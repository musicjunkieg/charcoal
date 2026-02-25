// GET /api/status — returns scan status and threat tier counts.
//
// Combines the live ScanStatus (running, progress) with DB-derived
// tier counts so the dashboard can show "High: 12, Elevated: 34, ..."
// without a separate round-trip.

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;

use crate::web::AppState;

pub async fn get_status(State(state): State<AppState>) -> impl IntoResponse {
    let scan_status = state.scan_status.read().await;

    // Compute tier counts from DB. threat_tier is stored as Option<String>.
    let threats = state.db.get_ranked_threats(0.0).await.unwrap_or_default();
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
        "scan_running": scan_status.running,
        "started_at": scan_status.started_at,
        "progress_message": scan_status.progress_message,
        "last_error": scan_status.last_error,
        "tier_counts": {
            "high": high,
            "elevated": elevated,
            "watch": watch,
            "low": low,
            "total": threats.len(),
        }
    }))
}
