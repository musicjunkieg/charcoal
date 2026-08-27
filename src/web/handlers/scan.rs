// POST /api/scan — put the caller in the scan queue.
//
// Returns 202 Accepted with the caller's position and ETA. It no longer starts
// anything: the background admitter (src/web/admitter.rs) claims queued rows
// while the running count is under CHARCOAL_SCAN_CONCURRENCY, which is the only
// way the cap can hold across replicas and across a redeploy.
//
// This used to return 409 "Another scan is already in progress on this server"
// whenever any scan anywhere was running (#257). With open signup (#256) and
// scans that take 22 minutes to 2 hours, that was the second user's entire
// experience of Charcoal, all day.
//
// Callers poll GET /api/status, which reports the queue position while waiting
// and the live pipeline phase once the scan starts.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};

use crate::web::{api_error, AppState, AuthUser};

/// If the last successful scan finished inside the cooldown window, the
/// RFC3339 instant at which the next scan becomes available. None = no
/// cooldown (elapsed, disabled, or unparseable timestamp — never block on
/// bad data).
pub(crate) fn cooldown_retry_at(
    finished_at: &str,
    now: chrono::DateTime<chrono::Utc>,
    cooldown_hours: u64,
) -> Option<String> {
    if cooldown_hours == 0 {
        return None;
    }
    let finished = chrono::DateTime::parse_from_rfc3339(finished_at).ok()?;
    let retry_at = finished + chrono::Duration::hours(cooldown_hours as i64);
    if now < retry_at {
        Some(retry_at.to_rfc3339())
    } else {
        None
    }
}

/// POST /api/scan — queue a background threat scan for the caller.
pub async fn trigger_scan(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
) -> impl IntoResponse {
    // Checked up front rather than left to the admitter: the admitter's only
    // recourse for an unknown user is to mark the row failed, which tells the
    // user "your scan failed" when the real answer is "re-authenticate".
    match state.db.get_user_handle(&auth.did).await {
        Ok(Some(_)) => {}
        Ok(None) => return api_error(StatusCode::NOT_FOUND, "User not found — re-authenticate"),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "DB error looking up user handle");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error");
        }
    }

    // #258: one successful scan per user per cooldown window. Failed scans
    // don't count, and the admin trigger path (handlers/admin.rs) deliberately
    // has no such check — that is the operator's bypass.
    match state.db.list_scan_queue().await {
        Ok(rows) => {
            if let Some(row) = rows.iter().find(|r| r.user_did == auth.did) {
                if row.status == "done" {
                    if let Some(finished_at) = &row.finished_at {
                        if let Some(retry_at) = cooldown_retry_at(
                            finished_at,
                            chrono::Utc::now(),
                            state.config.scan_cooldown_hours,
                        ) {
                            // Word the limit to match the configured window —
                            // "one per day" is only true at the 24h default.
                            let window = match state.config.scan_cooldown_hours {
                                24 => "one per day".to_string(),
                                h => format!("one every {h} hours"),
                            };
                            return (
                                StatusCode::TOO_MANY_REQUESTS,
                                Json(serde_json::json!({
                                    "error": format!(
                                        "You scanned recently — scans are limited to {window}"
                                    ),
                                    "retry_at": retry_at,
                                })),
                            )
                                .into_response();
                        }
                    }
                }
            }
        }
        Err(e) => {
            // A cooldown is an abuse guard, not a correctness gate: if we
            // cannot read the queue, let the enqueue proceed rather than
            // refusing service on a DB blip.
            tracing::warn!(error = %format!("{e:#}"), "cooldown check skipped — could not read scan queue");
        }
    }

    // Idempotent by construction: `enqueue_scan` is a no-op while the user is
    // queued or running (user_did is the primary key), so a double-click
    // returns the current position instead of booking a second scan.
    if let Err(e) = state.db.enqueue_scan(&auth.did).await {
        tracing::error!(error = %format!("{e:#}"), "enqueue failed");
        return api_error(StatusCode::SERVICE_UNAVAILABLE, "Could not queue the scan");
    }

    // Wake the admitter so a free slot is taken now, not on the next 30s tick.
    if let Some(wake) = &state.scan_wake {
        let _ = wake.try_send(());
    }

    // The enqueue already succeeded, so the answer is still 202 — but a failed
    // read of the row must not be dressed up as a real one. `position: 0` is a
    // genuine value (it is what a *running* scan reports), so falling back to it
    // would make "we could not read the queue" indistinguishable from "your scan
    // is already running", with the error thrown away on top. Log it and send
    // `position: null` instead, which no successful read ever produces.
    let entry = match state
        .db
        .scan_queue_entry(&auth.did, crate::web::admitter::scan_concurrency())
        .await
    {
        Ok(entry) => entry,
        Err(e) => {
            tracing::error!(
                did = %auth.did,
                error = %format!("{e:#}"),
                "queued the scan but could not read back its queue entry"
            );
            None
        }
    };

    // The row's own status, not a hardcoded "queued": a user who already had a
    // scan running gets a no-op enqueue, and telling them "queued, position 0"
    // would be a lie about the scan they are watching.
    let (status, position, eta) = match entry {
        Some(e) => (e.status, Some(e.position), e.eta_seconds),
        None => ("queued".to_string(), None, None),
    };

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "status": status,
            "position": position,
            "eta_seconds": eta,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    #[test]
    fn inside_window_returns_retry_at() {
        let finished = (Utc::now() - Duration::hours(1)).to_rfc3339();
        let retry = cooldown_retry_at(&finished, Utc::now(), 24);
        assert!(retry.is_some());
        let expected =
            chrono::DateTime::parse_from_rfc3339(&finished).unwrap() + Duration::hours(24);
        assert_eq!(retry.unwrap(), expected.to_rfc3339());
    }

    #[test]
    fn outside_window_and_disabled_return_none() {
        let finished = (Utc::now() - Duration::hours(25)).to_rfc3339();
        assert!(cooldown_retry_at(&finished, Utc::now(), 24).is_none());
        let recent = (Utc::now() - Duration::hours(1)).to_rfc3339();
        assert!(
            cooldown_retry_at(&recent, Utc::now(), 0).is_none(),
            "0 disables"
        );
    }

    #[test]
    fn unparseable_finished_at_never_blocks() {
        assert!(cooldown_retry_at("not-a-timestamp", Utc::now(), 24).is_none());
    }
}
