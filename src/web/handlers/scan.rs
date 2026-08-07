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
        Ok(None) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "User not found — re-authenticate",
            )
        }
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "DB error looking up user handle");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error");
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
