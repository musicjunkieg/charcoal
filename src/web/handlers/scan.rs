// POST /api/scan — trigger a background scan.
//
// Returns 202 Accepted if the scan starts.
// Returns 409 Conflict if a scan is already running.
//
// The scan pipeline runs in a background tokio task — callers poll
// GET /api/status to track progress.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};

use crate::web::scan_job::launch_scan;
use crate::web::{api_error, AppState, AuthUser};

/// POST /api/scan — start a background threat scan.
pub async fn trigger_scan(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
) -> impl IntoResponse {
    let mut mgr = state.scan_manager.write().await;
    if let Err(msg) = mgr.try_start_scan(&auth.did) {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": msg })),
        )
            .into_response();
    }
    drop(mgr); // Release lock before spawning

    // Look up the authenticated user's handle for the scan pipeline.
    let actor_handle = match state.db.get_user_handle(&auth.did).await {
        Ok(Some(handle)) => handle,
        Ok(None) => {
            // Roll back the scan state since we can't proceed
            state.scan_manager.write().await.finish_scan(&auth.did);
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "User not found — re-authenticate",
            );
        }
        Err(e) => {
            state.scan_manager.write().await.finish_scan(&auth.did);
            tracing::error!(error = %e, "DB error looking up user handle");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error");
        }
    };

    launch_scan(
        state.config.clone(),
        state.db.clone(),
        state.scan_manager.clone(),
        auth.did,
        actor_handle,
    );

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "message": "Scan started" })),
    )
        .into_response()
}
