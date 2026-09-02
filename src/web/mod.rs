// Web server — Axum-based single-user dashboard backend.
//
// The server embeds the SvelteKit SPA at compile time via include_dir!.
// All /api/* routes serve JSON; all other paths serve the SPA's index.html
// so client-side routing works correctly.
//
// Auth: stateless HMAC-SHA256 session cookies. No session table in the DB.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use include_dir::{include_dir, Dir};
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::config::Config;
use crate::db::Database;

pub mod actions;
pub mod admitter;
pub mod auth;
pub mod handlers;
pub mod scan_job;
pub mod test_helpers;
pub mod typeahead;

// Embed the SvelteKit build output at compile time.
// web/build/ must exist before `cargo build --features web` runs.
// Run `cd web && npm ci && npm run build` first.
static ASSETS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/web/build");

/// Shared application state threaded through all Axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<dyn Database>,
    pub config: Arc<Config>,
    pub scan_manager: Arc<RwLock<scan_job::ScanManager>>,
    /// In-flight OAuth request states, keyed by the `state` parameter sent to the PDS.
    /// Populated by POST /api/auth/initiate; consumed by GET /api/auth/callback.
    pub pending_oauth: Arc<RwLock<HashMap<String, handlers::oauth::PendingOAuth>>>,
    /// OAuth write sessions (#315): `Some` only when `CHARCOAL_TOKEN_KEY` is
    /// valid. `None` means the actions feature is disabled — endpoints answer
    /// 503 `actions_disabled` and nothing else changes.
    pub sessions: Option<Arc<actions::session::SessionStore>>,
    /// Wake channel for the background action runner (#315). Send a batch id
    /// after inserting it so it runs now. `None` in test helpers that spawn
    /// no runner, and when the feature is disabled.
    pub action_wake: Option<tokio::sync::mpsc::Sender<i64>>,
    /// P-256 signing key for JWT client assertions. Generated at startup.
    pub signing_key: atproto_identity::key::KeyData,
    /// Shared HTTP client for outbound calls made by handlers (#227).
    /// One client so the connection pool is reused across requests.
    pub http: reqwest::Client,
    /// Per-caller rate limiter for the PUBLIC typeahead endpoint (#227).
    /// Shared so the limit is global to the process, not per-request.
    pub typeahead_limiter: Arc<typeahead::TypeaheadLimiter>,
    /// ONNX models, loaded once at boot and shared by every scan (#257).
    pub models: Arc<scan_job::ScanModels>,
    /// Wake channel for the background admitter (#257). Send `()` after an
    /// enqueue so a free slot is taken now rather than on the next 30s tick.
    ///
    /// `Option` because `test_helpers` builds an `AppState` with no admitter
    /// behind it — a test that never enqueues has nothing to wake.
    pub scan_wake: Option<tokio::sync::mpsc::Sender<()>>,
}

/// Start the Axum web server and block until it exits.
pub async fn run_server(
    config: Config,
    db: Arc<dyn Database>,
    port: u16,
    bind: &str,
) -> Result<()> {
    // Fail fast if required OAuth config is missing.
    // Note: CHARCOAL_ALLOWED_DID is intentionally optional — when empty,
    // all Bluesky users can sign in (open access for multi-user deploys).
    if config.oauth_client_id.is_empty() {
        anyhow::bail!(
            "CHARCOAL_OAUTH_CLIENT_ID is not set.\n\
             For dev: register your client metadata at your OAuth client ID service.\n\
             For production: set to https://{{RAILWAY_PUBLIC_DOMAIN}}/oauth-client-metadata.json"
        );
    }
    if config.session_secret.len() < 32 {
        anyhow::bail!(
            "CHARCOAL_SESSION_SECRET must be at least 32 characters (currently {} chars).\n\
             Generate one with: openssl rand -hex 32",
            config.session_secret.len()
        );
    }

    // Derive a stable P-256 signing key from the session secret.
    // Using HMAC-SHA256 ensures the same key is produced on every restart,
    // which is critical because the PDS caches our client metadata (including
    // the JWKS public key). A new key on restart would cause `invalid_client`
    // errors until the PDS cache expires.
    let signing_key = {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(config.session_secret.as_bytes())
            .expect("HMAC accepts any key length");
        mac.update(b"charcoal-oauth-signing-key-v1");
        let derived = mac.finalize().into_bytes(); // 32 bytes — valid P-256 scalar
        atproto_identity::key::KeyData::new(
            atproto_identity::key::KeyType::P256Private,
            derived.to_vec(),
        )
    };
    info!("Derived stable P-256 signing key for OAuth client assertions");

    // Load models before binding the port. Fail-fast: a server that cannot
    // score is not usefully up, and this surfaces a broken model volume as a
    // failed deploy rather than as scans that fail one by one later.
    let models = Arc::new(
        scan_job::ScanModels::load(&config.model_dir)
            .context("model load failed at boot — the server will not start")?,
    );
    info!("Loaded ONNX models (toxicity + embedding + NLI) into shared state");

    // Computed before `config` moves into `Arc::new` below — `from_config`
    // only needs a borrow, and once wrapped in `Arc<Config>` a mutable move
    // is not the point; we just need the read to happen first.
    let sessions = actions::session::SessionStore::from_config(&config).map(Arc::new);

    let state = AppState {
        db,
        config: Arc::new(config),
        scan_manager: Arc::new(RwLock::new(scan_job::ScanManager::new())),
        pending_oauth: Arc::new(RwLock::new(HashMap::new())),
        sessions,
        action_wake: None,
        signing_key,
        http: reqwest::Client::new(),
        typeahead_limiter: handlers::typeahead::build_limiter(),
        models,
        scan_wake: None,
    };

    // One admitter per process. It owns launching scans from here on: it
    // reclaims rows orphaned by the previous deploy, then claims while the
    // running count is under CHARCOAL_SCAN_CONCURRENCY.
    let scan_wake = admitter::spawn_admitter(state.clone());
    let state = AppState {
        scan_wake: Some(scan_wake),
        ..state
    };

    // One action runner per process (#315). Only spawned when the feature is
    // enabled; it resumes any batch left queued/running by the last deploy.
    let action_wake = state
        .sessions
        .is_some()
        .then(|| actions::runner::spawn_runner(state.clone()));
    let state = AppState {
        action_wake,
        ..state
    };

    let app = build_router(state);

    let addr = format!("{bind}:{port}");
    info!("Charcoal dashboard listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

pub(crate) fn build_router(state: AppState) -> Router {
    // Authenticated API routes (require valid session cookie)
    let protected_api = Router::new()
        .route("/api/status", get(handlers::status::get_status))
        .route("/api/accounts", get(handlers::accounts::list_accounts))
        .route(
            "/api/accounts/{handle}",
            get(handlers::accounts::get_account),
        )
        .route("/api/events", get(handlers::events::list_events))
        .route(
            "/api/fingerprint",
            get(handlers::fingerprint::get_fingerprint),
        )
        .route("/api/scan", post(handlers::scan::trigger_scan))
        .route(
            "/api/accounts/{did}/label",
            post(handlers::labels::upsert_label),
        )
        .route("/api/review", get(handlers::labels::get_review_queue))
        .route("/api/accuracy", get(handlers::labels::get_accuracy))
        .route("/api/logout", post(handlers::auth::logout))
        .route("/api/me", get(handlers::admin::get_identity))
        .route(
            "/api/admin/users",
            get(handlers::admin::list_users).post(handlers::admin::pre_seed_user),
        )
        .route(
            "/api/admin/users/{did}/scan",
            post(handlers::admin::trigger_admin_scan),
        )
        .route(
            "/api/admin/users/{did}",
            delete(handlers::admin::delete_user),
        )
        .route(
            "/api/admin/access",
            get(handlers::access::list_access).post(handlers::access::grant_access_by_handle),
        )
        .route(
            "/api/admin/access/{did}/approve",
            post(handlers::access::approve_access),
        )
        .route(
            "/api/admin/access/{did}/deny",
            post(handlers::access::deny_access),
        )
        .route(
            "/api/admin/access/{did}/approve-scan",
            post(handlers::access::approve_access_and_scan),
        )
        .route("/api/actions/status", get(handlers::actions::get_status))
        .route("/api/actions/connect", post(handlers::actions::connect))
        .route(
            "/api/actions/disconnect",
            post(handlers::actions::disconnect),
        )
        .route(
            "/api/actions/batches",
            get(handlers::actions::list_batches).post(handlers::actions::create_batch),
        )
        .route(
            "/api/actions/batches/{id}",
            get(handlers::actions::get_batch),
        )
        .route(
            "/api/actions/batches/{id}/undo",
            post(handlers::actions::undo_batch),
        )
        .route(
            "/api/actions/batches/{id}/retry",
            post(handlers::actions::retry_batch),
        )
        .route(
            "/api/actions/{action_id}/undo",
            post(handlers::actions::undo_action),
        )
        .route(
            "/api/accounts/{handle}/actions",
            get(handlers::actions::account_actions),
        )
        .route(
            "/api/actions/active",
            get(handlers::actions::active_actions),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    // Public routes (no auth)
    let public_api = Router::new()
        .route(
            "/oauth-client-metadata.json",
            get(handlers::oauth::client_metadata),
        )
        .route("/health", get(health))
        .route("/api/auth/initiate", post(handlers::oauth::initiate))
        .route("/api/auth/callback", get(handlers::oauth::callback))
        // PUBLIC by necessity — the login screen is pre-auth, so the typeahead
        // backing it cannot sit behind require_auth. Guarded instead by query
        // validation, a per-caller rate limit, and an upstream timeout; see
        // handlers/typeahead.rs.
        .route("/api/typeahead", get(handlers::typeahead::suggest));

    Router::new()
        .merge(protected_api)
        .merge(public_api)
        .fallback(serve_spa)
        .layer(
            CorsLayer::new()
                .allow_origin(tower_http::cors::Any)
                .allow_methods([
                    axum::http::Method::GET,
                    axum::http::Method::POST,
                    axum::http::Method::DELETE,
                    axum::http::Method::OPTIONS,
                ])
                .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Railway health check — always returns 200 OK.
async fn health() -> impl IntoResponse {
    (
        StatusCode::OK,
        axum::Json(serde_json::json!({ "status": "ok" })),
    )
}

/// Serve the embedded SPA for all non-API paths.
/// Falls back to index.html for any path not found in the asset dir,
/// so SvelteKit client-side routing works correctly.
async fn serve_spa(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');

    // Try exact path first
    if let Some(file) = ASSETS.get_file(path) {
        return asset_response(file.contents(), path);
    }

    // For nested paths that don't exist as files, serve index.html
    // (SPA fallback for client-side routing)
    match ASSETS.get_file("index.html") {
        Some(index) => asset_response(index.contents(), "index.html"),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain")],
            Body::from("Web assets not found. Run: cd web && npm run build"),
        )
            .into_response(),
    }
}

fn asset_response(contents: &'static [u8], path: &str) -> Response {
    let mime = mime_type(path);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static(mime))
        .body(Body::from(contents))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn mime_type(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript",
        "css" => "text/css",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "json" => "application/json",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Typed JSON error response helper.
pub fn api_error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(serde_json::json!({ "error": message }))).into_response()
}

/// Marker type indicating the request passed session authentication.
/// Inserted into request extensions by `require_auth` middleware.
/// Handlers can extract it to learn who is authenticated.
#[derive(Debug, Clone)]
pub struct AuthUser {
    /// The DID of the authenticated user (from session cookie)
    pub did: String,
    /// The effective DID for DB queries (impersonated DID, or same as `did`)
    pub effective_did: String,
    /// Whether this user is an admin
    pub is_admin: bool,
}

impl AuthUser {
    /// Returns true if the user is viewing as someone else
    pub fn is_impersonating(&self) -> bool {
        self.did != self.effective_did
    }
}
