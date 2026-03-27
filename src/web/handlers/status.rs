// GET /api/status — returns scan status and threat tier counts.
//
// Combines the live ScanStatus (running, progress) with DB-derived
// tier counts so the dashboard can show "High: 12, Elevated: 34, ..."
// without a separate round-trip.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};

use crate::web::{api_error, AppState, AuthUser};

pub async fn get_status(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
) -> Response {
    // Snapshot scan status fields and release the lock before awaiting the DB.
    // Holding the read guard across an async DB call would block writers (e.g.
    // the scan job updating progress) for the duration of the query.
    let (scan_running, started_at, progress_message, last_error) = {
        let mgr = state.scan_manager.read().await;
        match mgr.get_status(&auth.effective_did) {
            Some(s) => (
                s.running,
                s.started_at.clone(),
                s.progress_message.clone(),
                s.last_error.clone(),
            ),
            None => (false, None, String::new(), None),
        }
    };

    // Compute tier counts from DB. threat_tier is stored as Option<String>.
    let threats = match state.db.get_ranked_threats(&auth.effective_did, 0.0).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "DB error in get_status");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error");
        }
    };
    let mut high = 0u32;
    let mut elevated = 0u32;
    let mut watch = 0u32;
    let mut low = 0u32;
    for account in &threats {
        match account.threat_tier.as_deref() {
            Some("High") => high += 1,
            Some("Elevated") => elevated += 1,
            Some("Watch") => watch += 1,
            _ => low += 1,
        }
    }

    Json(serde_json::json!({
        "scan_running": scan_running,
        "started_at": started_at,
        "progress_message": progress_message,
        "last_error": last_error,
        "tier_counts": {
            "high": high,
            "elevated": elevated,
            "watch": watch,
            "low": low,
            "total": threats.len(),
        }
    }))
    .into_response()
}
