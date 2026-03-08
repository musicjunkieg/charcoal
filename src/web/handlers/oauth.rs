// AT Protocol OAuth flow handlers:
//   GET  /oauth-client-metadata.json  — client metadata document (required by AT Protocol OAuth)
//   POST /api/auth/initiate           — start the OAuth flow, return redirect URL
//   GET  /api/auth/callback           — PDS posts back here; exchange code for tokens

use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::web::{api_error, AppState};

// ---- Client metadata ----

/// GET /oauth-client-metadata.json
///
/// Serves the AT Protocol OAuth client metadata document. The `client_id` must be the
/// public URL of this document — that's what CHARCOAL_OAUTH_CLIENT_ID is set to.
pub async fn client_metadata(State(state): State<AppState>) -> Response {
    let client_id = &state.config.oauth_client_id;

    // Derive the redirect URI from the client_id URL.
    let redirect_uri = derive_redirect_uri(client_id);
    let base_uri = derive_base_url(client_id);

    let metadata = serde_json::json!({
        "client_id": client_id,
        "client_name": "Charcoal",
        "client_uri": base_uri,
        "redirect_uris": [redirect_uri],
        "scope": "atproto",
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "application_type": "web",
        "dpop_bound_access_tokens": true
    });

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        Json(metadata),
    )
        .into_response()
}

fn derive_base_url(client_id: &str) -> String {
    // Strip the path, keep scheme + host
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
/// Accept { handle }, resolve to DID, start PAR flow with user's PDS,
/// return { redirect_url } for the browser to follow.
pub async fn initiate(
    State(_state): State<AppState>,
    Json(body): Json<InitiateRequest>,
) -> Response {
    let handle = body.handle.trim().to_string();
    if handle.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "handle is required");
    }

    // Full OAuth initiation implemented in Task 7.
    // This returns 501 so the integration test for the full flow
    // (marked #[ignore]) documents the expected behavior.
    api_error(
        StatusCode::NOT_IMPLEMENTED,
        "OAuth flow not yet implemented — coming in Task 7",
    )
}

// ---- Callback ----

#[derive(Deserialize)]
pub struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
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
    let _code = match &params.code {
        Some(c) if !c.is_empty() => c.clone(),
        _ => return api_error(StatusCode::BAD_REQUEST, "missing required parameter: code"),
    };
    let state_param = match &params.state {
        Some(s) if !s.is_empty() => s.clone(),
        _ => return api_error(StatusCode::BAD_REQUEST, "missing required parameter: state"),
    };

    // Look up and remove the pending OAuth state.
    // If it's not there, the state is invalid or expired.
    {
        let mut pending = state.pending_oauth.write().await;
        if pending.remove(&state_param).is_none() {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid or expired OAuth state — please start sign-in again",
            );
        }
    }

    // Full token exchange implemented in Task 7.
    api_error(
        StatusCode::NOT_IMPLEMENTED,
        "Token exchange not yet implemented — coming in Task 7",
    )
}
