// GET /api/fingerprint — the protected user's topic fingerprint.
//
// Returns the stored fingerprint JSON along with its metadata
// (post count and last updated timestamp).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};

use crate::web::{api_error, AppState, AuthUser};

/// GET /api/fingerprint — return the stored topic fingerprint.
pub async fn get_fingerprint(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
) -> Response {
    match state.db.get_fingerprint(&auth.did).await {
        Ok(Some((json, post_count, updated_at))) => {
            // Parse the fingerprint JSON to return it as a structured object.
            let fingerprint: serde_json::Value = match serde_json::from_str(&json) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(error = %e, "Corrupt fingerprint JSON in DB");
                    return api_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Corrupt fingerprint data",
                    );
                }
            };
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
