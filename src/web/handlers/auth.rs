// Auth handlers — POST /api/login and POST /api/logout.
//
// Login: validates CHARCOAL_WEB_PASSWORD from the request body, then sets a
// signed HMAC session cookie. Uses constant-time comparison to prevent
// timing attacks on the password check.
//
// Logout: clears the session cookie.

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::web::auth::{clear_cookie_header, create_token, set_cookie_header};
use crate::web::{api_error, AppState};

#[derive(Deserialize)]
pub struct LoginRequest {
    password: String,
}

/// POST /api/login — authenticate with CHARCOAL_WEB_PASSWORD.
///
/// On success: returns 200 with a signed session cookie.
/// On failure: returns 401.
pub async fn login(State(state): State<AppState>, Json(body): Json<LoginRequest>) -> Response {
    // Constant-time comparison to prevent timing attacks.
    let expected = &state.config.web_password;
    let provided = &body.password;

    // Constant-time comparison: always XOR all bytes regardless of length so
    // the branch timing doesn't reveal whether the lengths matched.
    // Strategy: XOR byte-by-byte over the longer length, substituting 0 for
    // missing bytes in the shorter string, then OR in a length-mismatch flag.
    let max_len = expected.len().max(provided.len());
    let expected_bytes = expected.as_bytes();
    let provided_bytes = provided.as_bytes();
    let mut diff = if expected.len() == provided.len() {
        0u8
    } else {
        1u8
    };
    for i in 0..max_len {
        let a = expected_bytes.get(i).copied().unwrap_or(0);
        let b = provided_bytes.get(i).copied().unwrap_or(0);
        diff |= a ^ b;
    }
    let passwords_match = diff == 0;

    if !passwords_match || expected.is_empty() {
        return api_error(StatusCode::UNAUTHORIZED, "Invalid password");
    }

    let token = create_token(&state.config.session_secret);
    // Use Secure flag only over HTTPS (not needed for local dev).
    // In production on Railway, Railway provides HTTPS termination.
    let secure = false; // stateless server can't detect TLS; rely on Railway's proxy
    let cookie = set_cookie_header(&token, secure);

    (
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(serde_json::json!({ "message": "Authenticated" })),
    )
        .into_response()
}

/// POST /api/logout — clear the session cookie.
pub async fn logout() -> Response {
    let cookie = clear_cookie_header();
    (
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(serde_json::json!({ "message": "Logged out" })),
    )
        .into_response()
}
