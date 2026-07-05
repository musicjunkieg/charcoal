// GET /api/status — returns scan status and threat tier counts.
//
// Combines the live ScanStatus (running, progress) with DB-derived
// tier counts so the dashboard can show "High: 12, Elevated: 34, ..."
// without a separate round-trip.
//
// While the scan is in its heavy scoring stage, the in-memory phase is just
// `Scoring`; this handler refines it using the `scan_phase` marker and the
// progress denominators the pipeline persists in `scan_state`, so the
// dashboard can render "gathering / classifying / finalizing" with live
// "X of Y" counts.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};

use crate::db::Database;
use crate::web::scan_job::WebScanPhase;
use crate::web::{api_error, AppState, AuthUser};

/// Map the in-memory phase + persisted pipeline state into the API phase
/// string and optional progress block.
///
/// Only refines while a scan is running in the `Scoring` stage AND the DB
/// marker points at a live phase (gather/burst/finalize). A marker of
/// "done" (or missing/unrecognised) means it's left over from a previous
/// run — report plain "scoring" with no counts rather than stale numbers.
fn refine_phase(
    mem_phase: WebScanPhase,
    running: bool,
    db_scan_phase: Option<&str>,
    candidates_total: Option<i64>,
    classifications_total: Option<i64>,
    pending: Option<i64>,
) -> (&'static str, Option<serde_json::Value>) {
    if !(running && mem_phase == WebScanPhase::Scoring) {
        return (mem_phase.as_str(), None);
    }
    let refined = match db_scan_phase {
        Some("gather") => "gathering",
        Some("burst") => "classifying",
        Some("finalize") => "finalizing",
        _ => return ("scoring", None),
    };
    // classifications_total is only meaningful once the burst denominator has
    // been recorded (at burst entry) — during gather it would be a leftover
    // from the previous run, so suppress it there.
    let (cls_total, cls_done) = if refined == "gathering" {
        (None, None)
    } else {
        match (classifications_total, pending) {
            (Some(total), Some(pending)) => (Some(total), Some((total - pending).max(0))),
            (total, _) => (total, None),
        }
    };
    let progress = serde_json::json!({
        "candidates_total": candidates_total,
        "classifications_total": cls_total,
        "classifications_done": cls_done,
    });
    (refined, Some(progress))
}

/// Read an integer scan_state value; missing keys, DB errors, and malformed
/// values all degrade to None — progress is decoration, never a 500.
async fn read_count(db: &dyn Database, user_did: &str, key: &str) -> Option<i64> {
    match db.get_scan_state(user_did, key).await {
        Ok(value) => value.and_then(|v| v.parse::<i64>().ok()),
        Err(e) => {
            tracing::warn!(error = %e, key, "failed to read scan progress state");
            None
        }
    }
}

pub async fn get_status(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
) -> Response {
    // Snapshot scan status fields and release the lock before awaiting the DB.
    // Holding the read guard across an async DB call would block writers (e.g.
    // the scan job updating progress) for the duration of the query.
    let (scan_running, started_at, progress_message, last_error, mem_phase) = {
        let mgr = state.scan_manager.read().await;
        match mgr.get_status(&auth.effective_did) {
            Some(s) => (
                s.running,
                s.started_at.clone(),
                s.progress_message.clone(),
                s.last_error.clone(),
                s.phase,
            ),
            None => (false, None, String::new(), None, WebScanPhase::Idle),
        }
    };

    // Refine the coarse Scoring phase from pipeline state in the DB.
    let (phase, progress) = if scan_running && mem_phase == WebScanPhase::Scoring {
        let db = &*state.db;
        let did = &auth.effective_did;
        let db_scan_phase = match db.get_scan_state(did, "scan_phase").await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "failed to read scan_phase marker");
                None
            }
        };
        let candidates_total = read_count(db, did, "candidates_total").await;
        let classifications_total = read_count(db, did, "classifications_total").await;
        let pending = match db.count_pending_classifications(did).await {
            Ok(n) => Some(n),
            Err(e) => {
                tracing::warn!(error = %e, "failed to count pending classifications");
                None
            }
        };
        refine_phase(
            mem_phase,
            scan_running,
            db_scan_phase.as_deref(),
            candidates_total,
            classifications_total,
            pending,
        )
    } else {
        (mem_phase.as_str(), None)
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
        "phase": phase,
        "progress": progress,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_running_returns_mem_phase_no_progress() {
        let (phase, progress) = refine_phase(
            WebScanPhase::Done,
            false,
            Some("burst"),
            Some(5),
            Some(10),
            Some(2),
        );
        assert_eq!(phase, "done");
        assert!(progress.is_none());
    }

    #[test]
    fn running_setup_phases_pass_through_unrefined() {
        let (phase, progress) = refine_phase(
            WebScanPhase::Discovering,
            true,
            Some("burst"),
            None,
            None,
            None,
        );
        assert_eq!(phase, "discovering");
        assert!(progress.is_none());
    }

    #[test]
    fn scoring_with_burst_marker_reports_classifying_with_counts() {
        let (phase, progress) = refine_phase(
            WebScanPhase::Scoring,
            true,
            Some("burst"),
            Some(120),
            Some(400),
            Some(150),
        );
        assert_eq!(phase, "classifying");
        let p = progress.expect("progress block");
        assert_eq!(p["candidates_total"], 120);
        assert_eq!(p["classifications_total"], 400);
        assert_eq!(p["classifications_done"], 250);
    }

    #[test]
    fn scoring_with_finalize_marker_reports_done_equals_total() {
        let (phase, progress) = refine_phase(
            WebScanPhase::Scoring,
            true,
            Some("finalize"),
            Some(120),
            Some(400),
            Some(0),
        );
        assert_eq!(phase, "finalizing");
        let p = progress.expect("progress block");
        assert_eq!(p["classifications_done"], 400);
    }

    #[test]
    fn scoring_with_gather_marker_suppresses_stale_classification_counts() {
        // classifications_total left over from a prior run must not show
        // during gather — only the fresh candidates_total does.
        let (phase, progress) = refine_phase(
            WebScanPhase::Scoring,
            true,
            Some("gather"),
            Some(120),
            Some(999),
            Some(0),
        );
        assert_eq!(phase, "gathering");
        let p = progress.expect("progress block");
        assert_eq!(p["candidates_total"], 120);
        assert!(p["classifications_total"].is_null());
        assert!(p["classifications_done"].is_null());
    }

    #[test]
    fn scoring_with_stale_done_marker_reports_plain_scoring() {
        // Marker "done" from a previous run: the current run hasn't entered
        // the phased pipeline yet — never show last run's numbers.
        let (phase, progress) = refine_phase(
            WebScanPhase::Scoring,
            true,
            Some("done"),
            Some(120),
            Some(400),
            Some(0),
        );
        assert_eq!(phase, "scoring");
        assert!(progress.is_none());
    }

    #[test]
    fn scoring_with_missing_or_garbage_marker_reports_plain_scoring() {
        for marker in [None, Some("garbage")] {
            let (phase, progress) =
                refine_phase(WebScanPhase::Scoring, true, marker, None, None, None);
            assert_eq!(phase, "scoring");
            assert!(progress.is_none());
        }
    }

    #[test]
    fn missing_counts_degrade_to_nulls_without_panic() {
        let (phase, progress) =
            refine_phase(WebScanPhase::Scoring, true, Some("burst"), None, None, None);
        assert_eq!(phase, "classifying");
        let p = progress.expect("progress block");
        assert!(p["candidates_total"].is_null());
        assert!(p["classifications_total"].is_null());
        assert!(p["classifications_done"].is_null());
    }

    #[test]
    fn pending_exceeding_total_clamps_done_to_zero() {
        let (_, progress) = refine_phase(
            WebScanPhase::Scoring,
            true,
            Some("burst"),
            None,
            Some(10),
            Some(25),
        );
        let p = progress.expect("progress block");
        assert_eq!(p["classifications_done"], 0);
    }
}
