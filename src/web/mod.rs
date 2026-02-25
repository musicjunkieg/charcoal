// Web server — Axum-based single-user dashboard backend.
//
// The server embeds the SvelteKit SPA at compile time via include_dir!.
// All /api/* routes serve JSON; all other paths serve the SPA's index.html
// so client-side routing works correctly.
//
// Auth: stateless HMAC-SHA256 session cookies. No session table in the DB.

use std::sync::Arc;

use anyhow::Result;
use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use include_dir::{include_dir, Dir};
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::config::Config;
use crate::db::Database;

pub mod auth;
pub mod handlers;
pub mod scan_job;

// Embed the SvelteKit build output at compile time.
// web/build/ must exist before `cargo build --features web` runs.
// Run `cd web && npm ci && npm run build` first.
static ASSETS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/web/build");

/// Shared application state threaded through all Axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<dyn Database>,
    pub config: Arc<Config>,
    pub scan_status: Arc<RwLock<scan_job::ScanStatus>>,
}

/// Start the Axum web server and block until it exits.
pub async fn run_server(
    config: Config,
    db: Arc<dyn Database>,
    port: u16,
    bind: &str,
) -> Result<()> {
    let state = AppState {
        db,
        config: Arc::new(config),
        scan_status: Arc::new(RwLock::new(scan_job::ScanStatus::default())),
    };

    let app = build_router(state);

    let addr = format!("{bind}:{port}");
    info!("Charcoal dashboard listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn build_router(state: AppState) -> Router {
    // Authenticated API routes (require valid session cookie)
    let protected_api = Router::new()
        .route("/api/status", get(handlers::status::get_status))
        .route("/api/accounts", get(handlers::accounts::list_accounts))
        .route(
            "/api/accounts/:handle",
            get(handlers::accounts::get_account),
        )
        .route("/api/events", get(handlers::events::list_events))
        .route(
            "/api/fingerprint",
            get(handlers::fingerprint::get_fingerprint),
        )
        .route("/api/scan", post(handlers::scan::trigger_scan))
        .route("/api/logout", post(handlers::auth::logout))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    // Public routes (no auth)
    let public_api = Router::new()
        .route("/health", get(health))
        .route("/api/login", post(handlers::auth::login));

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
#[derive(Clone)]
pub struct AuthUser;
