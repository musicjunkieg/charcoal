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
use axum::Json;
use chrono::Utc;

use crate::web::scan_job::launch_scan;
use crate::web::AppState;

/// POST /api/scan — start a background threat scan.
pub async fn trigger_scan(State(state): State<AppState>) -> impl IntoResponse {
    let mut status = state.scan_status.write().await;

    if status.running {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "A scan is already running" })),
        )
            .into_response();
    }

    status.running = true;
    status.started_at = Some(Utc::now().to_rfc3339());
    status.progress_message = "Starting scan…".to_string();
    status.last_error = None;
    drop(status);

    launch_scan(
        state.config.clone(),
        state.db.clone(),
        state.scan_status.clone(),
    );

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "message": "Scan started" })),
    )
        .into_response()
}
