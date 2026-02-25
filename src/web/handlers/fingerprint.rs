// GET /api/fingerprint — the protected user's topic fingerprint.
//
// Returns the stored fingerprint JSON along with its metadata
// (post count and last updated timestamp).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;

use crate::web::{api_error, AppState};

/// GET /api/fingerprint — return the stored topic fingerprint.
pub async fn get_fingerprint(State(state): State<AppState>) -> impl IntoResponse {
    match state.db.get_fingerprint().await {
        Ok(Some((json, post_count, updated_at))) => {
            // Parse the fingerprint JSON to return it as a structured object.
            let fingerprint: serde_json::Value =
                serde_json::from_str(&json).unwrap_or(serde_json::json!(null));
            Json(serde_json::json!({
                "fingerprint": fingerprint,
                "post_count": post_count,
                "updated_at": updated_at,
            }))
            .into_response()
        }
        Ok(None) => api_error(
            StatusCode::NOT_FOUND,
            "No fingerprint found. Run `charcoal fingerprint` first.",
        ),
        Err(e) => {
            tracing::error!(error = %e, "DB error fetching fingerprint");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        }
    }
}
