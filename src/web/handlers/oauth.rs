// AT Protocol OAuth flow handlers:
//   GET  /oauth-client-metadata.json  — client metadata document (required by AT Protocol OAuth)
//   POST /api/auth/initiate           — start the OAuth flow, return redirect URL
//   GET  /api/auth/callback           — PDS posts back here; exchange code for tokens

use atproto_identity::key::{generate_key, to_public, KeyType};
use atproto_identity::resolve::resolve_handle_http;
use atproto_oauth::jwk::generate as generate_jwk;
use atproto_oauth::resources::pds_resources;
use atproto_oauth::workflow::{
    oauth_complete, oauth_init, OAuthClient, OAuthRequest, OAuthRequestState,
};
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use chrono::{Duration, Utc};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use rand::distr::{Alphanumeric, SampleString};
use serde::Deserialize;

use crate::web::{api_error, AppState};

/// Data stored between /api/auth/initiate and /api/auth/callback.
pub struct PendingOAuth {
    /// The full OAuth request state needed for token exchange.
    pub oauth_request: OAuthRequest,
    /// The authorization server metadata (needed by oauth_complete).
    pub authorization_server: atproto_oauth::resources::AuthorizationServer,
    /// The user-input handle from the initiate request.
    /// Stored here so the callback can register the user in the DB.
    pub handle: String,
}

// ---- Client metadata ----

/// GET /oauth-client-metadata.json
///
/// Serves the AT Protocol OAuth client metadata document. The `client_id` must be the
/// public URL of this document — that's what CHARCOAL_OAUTH_CLIENT_ID is set to.
pub async fn client_metadata(State(state): State<AppState>) -> Response {
    let client_id = &state.config.oauth_client_id;
    let redirect_uri = derive_redirect_uri(client_id);
    let base_uri = derive_base_url(client_id);

    // Derive public key from signing key and convert to proper JWK format
    let public_key = match to_public(&state.signing_key) {
        Ok(pk) => pk,
        Err(e) => {
            tracing::error!("Failed to derive public key: {e}");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Server key configuration error",
            );
        }
    };

    let jwk = match generate_jwk(&public_key) {
        Ok(k) => k,
        Err(e) => {
            tracing::error!("Failed to generate JWK from public key: {e}");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Server key configuration error",
            );
        }
    };

    let metadata = serde_json::json!({
        "client_id": client_id,
        "client_name": "Charcoal",
        "client_uri": base_uri,
        "redirect_uris": [redirect_uri],
        "scope": "atproto",
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "private_key_jwt",
        "token_endpoint_auth_signing_alg": "ES256",
        "application_type": "web",
        "dpop_bound_access_tokens": true,
        "jwks": {
            "keys": [jwk]
        }
    });

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        Json(metadata),
    )
        .into_response()
}

fn derive_base_url(client_id: &str) -> String {
    if let Some(scheme_end) = client_id.find("://") {
        let after_scheme = &client_id[scheme_end + 3..];
        let host = after_scheme.split('/').next().unwrap_or(after_scheme);
        let scheme = &client_id[..scheme_end];
        format!("{scheme}://{host}")
    } else {
        client_id.to_string()
    }
}

fn derive_redirect_uri(client_id: &str) -> String {
    format!("{}/api/auth/callback", derive_base_url(client_id))
}

// ---- Initiate ----

#[derive(Deserialize)]
pub struct InitiateRequest {
    pub handle: String,
}

/// POST /api/auth/initiate
///
/// Accept { handle }, resolve to a DID, discover the user's PDS and its
/// authorization server, perform a PAR request, and return the redirect URL
/// for the browser to follow.
pub async fn initiate(
    State(state): State<AppState>,
    Json(body): Json<InitiateRequest>,
) -> Response {
    let handle = body.handle.trim().to_string();
    if handle.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "handle is required");
    }

    let http_client = reqwest::Client::new();

    // Step 1: Resolve handle to DID
    // Try HTTP well-known first, fall back to AppView resolveHandle API
    // (some domains return Cloudflare errors on /.well-known/atproto-did)
    let did = match resolve_handle_http(&http_client, &handle).await {
        Ok(d) => d,
        Err(_) => match resolve_handle_appview(&http_client, &handle).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("Handle resolution failed for '{handle}': {e}");
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "Could not resolve Bluesky handle — check spelling",
                );
            }
        },
    };

    // Step 2: Resolve DID to document to get PDS endpoint
    let pds_endpoint = match resolve_did_to_pds(&http_client, &did).await {
        Ok(pds) => pds,
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, &msg),
    };

    // Step 3: Discover authorization server from PDS
    let (_, authorization_server) = match pds_resources(&http_client, &pds_endpoint).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("PDS resource discovery failed for '{pds_endpoint}': {e}");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not discover authorization server from PDS",
            );
        }
    };

    // Step 4: Generate PKCE, state, nonce, DPoP key
    let (pkce_verifier, code_challenge) = atproto_oauth::pkce::generate();
    let oauth_state = Alphanumeric.sample_string(&mut rand::rng(), 32);
    let nonce = Alphanumeric.sample_string(&mut rand::rng(), 32);

    let dpop_key = match generate_key(KeyType::P256Private) {
        Ok(k) => k,
        Err(e) => {
            tracing::error!("DPoP key generation failed: {e}");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not generate security key",
            );
        }
    };

    // Step 5: Build OAuth client config
    let redirect_uri = derive_redirect_uri(&state.config.oauth_client_id);
    let oauth_client = OAuthClient {
        client_id: state.config.oauth_client_id.clone(),
        redirect_uri,
        private_signing_key_data: state.signing_key.clone(),
    };

    let oauth_request_state = OAuthRequestState {
        state: oauth_state.clone(),
        nonce: nonce.clone(),
        code_challenge,
        scope: "atproto".to_string(),
    };

    // Step 6: Perform PAR (Pushed Authorization Request)
    let par_response = match oauth_init(
        &http_client,
        &oauth_client,
        &dpop_key,
        Some(&handle),
        &authorization_server,
        &oauth_request_state,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("PAR request failed for '{handle}': {e}");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not start sign-in with Bluesky — please try again",
            );
        }
    };

    // Step 7: Build the OAuthRequest to store for callback
    let public_signing_key = match to_public(&state.signing_key) {
        Ok(pk) => pk,
        Err(e) => {
            tracing::error!("Public key derivation failed: {e}");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Server key configuration error",
            );
        }
    };

    let now = Utc::now();
    let oauth_request = OAuthRequest {
        oauth_state: oauth_state.clone(),
        issuer: authorization_server.issuer.clone(),
        authorization_server: authorization_server.issuer.clone(),
        nonce,
        pkce_verifier,
        signing_public_key: public_signing_key.to_string(),
        dpop_private_key: dpop_key.to_string(),
        created_at: now,
        expires_at: now + Duration::hours(1),
    };

    // Step 8: Store pending state for callback
    state.pending_oauth.write().await.insert(
        oauth_state,
        PendingOAuth {
            oauth_request,
            authorization_server: authorization_server.clone(),
            handle: handle.clone(),
        },
    );

    // Step 9: Build authorization URL and return (percent-encode query params)
    let auth_url = format!(
        "{}?client_id={}&request_uri={}",
        authorization_server.authorization_endpoint,
        utf8_percent_encode(&oauth_client.client_id, NON_ALPHANUMERIC),
        utf8_percent_encode(&par_response.request_uri, NON_ALPHANUMERIC),
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({ "redirect_url": auth_url })),
    )
        .into_response()
}

/// Resolve a handle to a DID via the public AppView API.
/// Fallback for when the HTTP well-known method fails (e.g. Cloudflare SSL issues).
async fn resolve_handle_appview(
    http_client: &reqwest::Client,
    handle: &str,
) -> Result<String, String> {
    let url = format!(
        "https://public.api.bsky.app/xrpc/com.atproto.identity.resolveHandle?handle={}",
        handle
    );
    let resp: serde_json::Value = http_client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("AppView resolveHandle failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Invalid resolveHandle response: {e}"))?;

    resp["did"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "resolveHandle response missing 'did' field".to_string())
}

/// Resolve a DID to its PDS endpoint URL.
async fn resolve_did_to_pds(http_client: &reqwest::Client, did: &str) -> Result<String, String> {
    // For did:plc, query PLC directory. For did:web, use HTTP resolution.
    let doc = if did.starts_with("did:plc:") {
        atproto_identity::plc::query(http_client, "plc.directory", did)
            .await
            .map_err(|e| format!("Could not resolve DID '{did}': {e}"))?
    } else if did.starts_with("did:web:") {
        // did:web resolution via HTTP
        let domain = did.strip_prefix("did:web:").unwrap_or(did);
        let url = format!("https://{}/.well-known/did.json", domain);
        http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Could not resolve did:web '{did}': {e}"))?
            .json()
            .await
            .map_err(|e| format!("Invalid DID document for '{did}': {e}"))?
    } else {
        return Err(format!("Unsupported DID method: {did}"));
    };

    doc.pds_endpoints()
        .first()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("No PDS endpoint found in DID document for '{did}'"))
}

// ---- Callback ----

#[derive(Deserialize)]
pub struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
    pub iss: Option<String>,
}

/// GET /api/auth/callback
///
/// PDS redirects here after user authenticates. Exchange the code for tokens,
/// validate the DID, set session cookie, redirect to /dashboard.
pub async fn callback(
    State(state): State<AppState>,
    Query(params): Query<CallbackParams>,
) -> Response {
    // Surface any error from the PDS
    if let Some(err) = &params.error {
        let desc = params
            .error_description
            .as_deref()
            .unwrap_or("no description");
        return api_error(
            StatusCode::BAD_REQUEST,
            &format!("OAuth error from PDS: {err} — {desc}"),
        );
    }

    // Both code and state are required
    let code = match &params.code {
        Some(c) if !c.is_empty() => c.clone(),
        _ => return api_error(StatusCode::BAD_REQUEST, "missing required parameter: code"),
    };
    let state_param = match &params.state {
        Some(s) if !s.is_empty() => s.clone(),
        _ => return api_error(StatusCode::BAD_REQUEST, "missing required parameter: state"),
    };

    // Consume the pending OAuth state (one-time use)
    let pending = {
        let mut map = state.pending_oauth.write().await;
        match map.remove(&state_param) {
            Some(p) => p,
            None => {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "invalid or expired OAuth state — please start sign-in again",
                )
            }
        }
    };

    // Reconstruct the DPoP key from the stored serialized form
    let dpop_key =
        match atproto_identity::key::identify_key(&pending.oauth_request.dpop_private_key) {
            Ok(k) => k,
            Err(e) => {
                tracing::error!("Failed to deserialize DPoP key: {e}");
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal error — please try signing in again",
                );
            }
        };

    // Build the OAuth client config
    let redirect_uri = derive_redirect_uri(&state.config.oauth_client_id);
    let oauth_client = OAuthClient {
        client_id: state.config.oauth_client_id.clone(),
        redirect_uri,
        private_signing_key_data: state.signing_key.clone(),
    };

    let http_client = reqwest::Client::new();

    // Exchange authorization code for tokens
    let token_response = match oauth_complete(
        &http_client,
        &oauth_client,
        &dpop_key,
        &code,
        &pending.oauth_request,
        &pending.authorization_server,
    )
    .await
    {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("Token exchange failed: {e}");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not complete sign-in — please try again",
            );
        }
    };

    // Extract the authenticated DID from the token response
    let authenticated_did = match &token_response.sub {
        Some(did) if !did.is_empty() => did.clone(),
        _ => {
            tracing::error!("Token response missing 'sub' claim (DID)");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Bluesky did not return your identity — please try again",
            );
        }
    };

    // Gate on CHARCOAL_ALLOWED_DID
    if !crate::web::auth::did_is_allowed(&authenticated_did, &state.config.allowed_did) {
        tracing::warn!(
            "Login attempt from disallowed DID '{}' (allowed: '{}')",
            authenticated_did,
            state.config.allowed_did
        );
        return api_error(
            StatusCode::FORBIDDEN,
            "This Bluesky account is not authorized to access this dashboard",
        );
    }

    // Register the authenticated user in the database.
    // Uses the handle from the initiate step. If the user logs in again
    // with a different handle, upsert_user's ON CONFLICT updates it.
    if let Err(e) = state
        .db
        .upsert_user(&authenticated_did, &pending.handle)
        .await
    {
        tracing::error!("Failed to register user: {e}");
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "Could not register user");
    }

    // Store tokens in-memory for future XRPC calls (muting/blocking milestone).
    // TokenResponse doesn't derive Serialize, so store the fields we need manually.
    *state.oauth_tokens.write().await = Some(serde_json::json!({
        "access_token": token_response.access_token,
        "token_type": token_response.token_type,
        "refresh_token": token_response.refresh_token,
        "scope": token_response.scope,
        "expires_in": token_response.expires_in,
        "sub": token_response.sub,
    }));

    // Issue session cookie with DID embedded
    let token = crate::web::auth::create_token(&state.config.session_secret, &authenticated_did);
    let cookie = crate::web::auth::set_cookie_header(&token, true);

    // Redirect to dashboard with session cookie set
    ([(header::SET_COOKIE, cookie)], Redirect::to("/dashboard")).into_response()
}
