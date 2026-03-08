// Auth handlers — POST /api/logout.
//
// The login flow has moved to AT Protocol OAuth (see handlers/oauth.rs).
// This file retains only the logout handler, which clears the session cookie.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::web::auth::clear_cookie_header;

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
