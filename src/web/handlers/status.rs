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

/// Reconcile the in-memory scan status against the user's `scan_queue` row.
///
/// The row wins wherever they disagree (#257). `ScanManager` is process-local
/// and knows nothing about a scan that has not started yet, and it is left
/// saying "running" forever by a scan whose lease lapsed — the queue row is the
/// durable answer to both, so preferring it dissolves the stale-label problem
/// rather than managing it.
///
/// Returns the phase to report and whether a scan is live from the user's point
/// of view (queued counts: they asked for a scan and one is coming).
fn reconcile_with_queue(
    mem_phase: WebScanPhase,
    mem_running: bool,
    queue_status: Option<&str>,
) -> (WebScanPhase, bool) {
    match queue_status {
        // Never enqueued — nothing to reconcile against.
        None => (mem_phase, mem_running),
        Some("queued") => (WebScanPhase::Queued, true),
        // Another replica (or this one before a restart) is running it, so the
        // in-memory phase may be Idle while the scan is genuinely live.
        Some("running") if mem_phase == WebScanPhase::Idle => (WebScanPhase::Starting, true),
        Some("running") => (mem_phase, true),
        // Terminal. If memory still claims a live scan it is a zombie entry —
        // report the queue's verdict, not the abandoned pipeline's last words.
        Some("done") if mem_running => (WebScanPhase::Done, false),
        Some("failed") if mem_running => (WebScanPhase::Failed, false),
        Some("done") | Some("failed") => (mem_phase, false),
        // An unrecognised status is a schema the code doesn't know; don't
        // invent a verdict from it.
        Some(_) => (mem_phase, mem_running),
    }
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
    let (mem_running, started_at, mut progress_message, last_error, mem_phase) = {
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

    // The queue row is the authority on whether a scan is live (#257).
    // Failures here degrade to the in-memory view rather than 500 — a status
    // poll must keep working when the queue query does not.
    let queue = match state
        .db
        .scan_queue_entry(
            &auth.effective_did,
            crate::web::admitter::scan_concurrency(),
        )
        .await
    {
        Ok(entry) => entry,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "failed to read the scan queue entry");
            None
        }
    };
    let (mem_phase, scan_running) = reconcile_with_queue(
        mem_phase,
        mem_running,
        queue.as_ref().map(|e| e.status.as_str()),
    );
    if mem_phase == WebScanPhase::Queued {
        progress_message = "Waiting for a free scan slot…".to_string();
    }

    // Refine the coarse Scoring phase from pipeline state in the DB.
    let (phase, progress) = if scan_running && mem_phase == WebScanPhase::Scoring {
        let db = &*state.db;
        let did = &auth.effective_did;
        // The four reads are independent; join them so this frequently-polled
        // handler doesn't pay four sequential round-trips (SQLite serializes
        // behind its connection mutex anyway, but Postgres genuinely benefits).
        let (scan_phase_res, candidates_total, classifications_total, pending_res) = tokio::join!(
            db.get_scan_state(did, "scan_phase"),
            read_count(db, did, "candidates_total"),
            read_count(db, did, "classifications_total"),
            db.count_pending_classifications(did),
        );
        let db_scan_phase = scan_phase_res.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to read scan_phase marker");
            None
        });
        let pending = pending_res.map(Some).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to count pending classifications");
            None
        });
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
            Some("NotAssessed") => {} // counted separately via count_not_assessed below
            Some("Insufficient Data") => {} // pre-existing terminal, not a threat tier
            _ => low += 1,
        }
    }

    // get_ranked_threats(0.0) filters out NULL-score rows, so NotAssessed
    // accounts never reach the loop above — the authoritative count comes
    // from this dedicated query instead (#222 language abstention). A DB
    // failure here must NOT be silently coerced to 0: NotAssessed is a
    // safety signal ("we couldn't assess these accounts"), and a zero would
    // be indistinguishable from a real error, understating what's unknown.
    // Fail consistently with get_ranked_threats above (500), rather than
    // returning misleading tier_counts.
    let not_assessed = match state.db.count_not_assessed(&auth.effective_did).await {
        Ok(n) => n as u32,
        Err(e) => {
            tracing::error!(error = %e, "DB error counting not-assessed in get_status");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error");
        }
    };

    // Total is the sum of the exposed buckets, so it reconciles BY CONSTRUCTION
    // regardless of what get_ranked_threats returns. This deliberately excludes
    // the non-threat terminals the loop skips — `Insufficient Data` (and, were it
    // ever returned, `NotAssessed`) — which carry a NULL score and so aren't
    // returned by get_ranked_threats(0.0) today anyway. Computing total from
    // threats.len() would count those skipped rows in the total but in no bucket.
    let total = high + elevated + watch + low + not_assessed;

    // The queue block appears only while the user is actually waiting, so a
    // client that predates it sees no change at all.
    let mut body = serde_json::json!({
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
            "not_assessed": not_assessed,
            "total": total,
        }
    });

    if let Some(entry) = queue.filter(|e| e.status == "queued") {
        body["queue"] = serde_json::json!({
            "position": entry.position,
            "eta_seconds": entry.eta_seconds,
            "enqueued_at": entry.enqueued_at,
        });
    }

    Json(body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_with_no_queue_row_is_reported_from_memory_alone() {
        assert_eq!(
            reconcile_with_queue(WebScanPhase::Done, false, None),
            (WebScanPhase::Done, false)
        );
    }

    #[test]
    fn a_queued_user_is_reported_queued_and_live() {
        // Live from the user's point of view: they asked for a scan, one is
        // coming, and the dashboard must keep polling.
        assert_eq!(
            reconcile_with_queue(WebScanPhase::Idle, false, Some("queued")),
            (WebScanPhase::Queued, true)
        );
        // Even when memory still holds the previous run's terminal phase.
        assert_eq!(
            reconcile_with_queue(WebScanPhase::Done, false, Some("queued")),
            (WebScanPhase::Queued, true)
        );
    }

    #[test]
    fn a_running_row_beats_an_empty_memory() {
        // Another replica, or this one after a restart: the row says running
        // and memory has never heard of the scan.
        assert_eq!(
            reconcile_with_queue(WebScanPhase::Idle, false, Some("running")),
            (WebScanPhase::Starting, true)
        );
        // With a live entry the pipeline's own phase is the more specific one.
        assert_eq!(
            reconcile_with_queue(WebScanPhase::Scoring, true, Some("running")),
            (WebScanPhase::Scoring, true)
        );
    }

    /// The stale-"running" label an abandoned scan leaves behind (#274). Its
    /// ScanManager entry says running forever; the queue row is what actually
    /// knows the slot was released.
    #[test]
    fn a_terminal_row_overrides_a_zombie_running_label() {
        assert_eq!(
            reconcile_with_queue(WebScanPhase::Scoring, true, Some("failed")),
            (WebScanPhase::Failed, false)
        );
        assert_eq!(
            reconcile_with_queue(WebScanPhase::Scoring, true, Some("done")),
            (WebScanPhase::Done, false)
        );
        // Memory already agrees it is over — keep its more specific phase.
        assert_eq!(
            reconcile_with_queue(WebScanPhase::Failed, false, Some("done")),
            (WebScanPhase::Failed, false)
        );
    }

    #[test]
    fn an_unrecognised_status_changes_nothing() {
        assert_eq!(
            reconcile_with_queue(WebScanPhase::Scoring, true, Some("wat")),
            (WebScanPhase::Scoring, true)
        );
    }

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
