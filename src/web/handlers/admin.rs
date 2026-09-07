// Admin API handlers — identity, user management, and admin-triggered operations.
//
// All admin-only endpoints check `auth.is_admin` and return 403 Forbidden if false.
// The `/api/me` endpoint is available to all authenticated users.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Extension;
use axum::Json;
use serde::Deserialize;

use crate::bluesky::client::PublicAtpClient;
use crate::web::scan_job;
use crate::web::AppState;
use crate::web::AuthUser;

#[derive(Debug, Deserialize)]
pub struct PreSeedRequest {
    pub handle: String,
}

/// GET /api/me — return the authenticated user's identity.
pub async fn get_identity(
    Extension(auth): Extension<AuthUser>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let handle = state
        .db
        .get_user_handle(&auth.did)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    // Whether CHARCOAL_ALLOWED_DID is configured at all. When it is empty the
    // gate is open and access-table decisions are inert (clause 1 short-circuits
    // before the table is read) — the admin UI warns about exactly that (#309).
    let access_gate_active = !state.config.allowed_did.trim().is_empty();
    Json(serde_json::json!({
        "did": auth.did,
        "handle": handle,
        "is_admin": auth.is_admin,
        "access_gate_active": access_gate_active,
    }))
}

/// Render one `scan_queue` row for the dashboard (#288).
///
/// `handle` is joined in from the user list rather than stored on the row: the
/// queue panel lists DIDs, and no operator can read a DID. It is `null` for the
/// vanishingly rare case of a queue row whose user has been deleted — the row
/// still deserves to be visible.
fn scan_row_json(row: &crate::db::traits::ScanQueueRow, handle: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "user_did": row.user_did,
        "handle": handle,
        "status": row.status,
        "position": row.position,
        "enqueued_at": row.enqueued_at,
        "started_at": row.started_at,
        "finished_at": row.finished_at,
        "last_error": row.last_error,
    })
}

/// GET /api/admin/users — list all registered users with status info.
///
/// The scan columns come from the durable `scan_queue` (#288), not from the
/// process-local `ScanManager`. The old view only knew about scans *this*
/// process launched: it was empty after a redeploy, blind to a second replica,
/// had no concept of "queued" at all — so an admin-triggered scan, which since
/// #257 is enqueued rather than launched, showed nothing — and could not tell a
/// failed scan from one that never ran.
///
/// `fingerprint_building` deliberately stays on `ScanManager`. It tracks a
/// `tokio::spawn` this process owns and has no durable row anywhere, so
/// process-local is the correct scope for it.
pub async fn list_users(
    Extension(auth): Extension<AuthUser>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, StatusCode> {
    if !auth.is_admin {
        return Err(StatusCode::FORBIDDEN);
    }
    let users = state.db.list_users().await.map_err(|e| {
        tracing::error!("Failed to list users: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Fail the request rather than degrade to "nobody has ever scanned" — a
    // dashboard that quietly reports an empty queue during a database blip is
    // worse than one that reports an error, because an operator acts on it.
    let queue_rows = state.db.list_scan_queue().await.map_err(|e| {
        tracing::error!("Failed to list the scan queue: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let handles: std::collections::HashMap<&str, &str> = users
        .iter()
        .map(|u| (u.did.as_str(), u.handle.as_str()))
        .collect();
    let by_did: std::collections::HashMap<&str, &crate::db::traits::ScanQueueRow> = queue_rows
        .iter()
        .map(|row| (row.user_did.as_str(), row))
        .collect();

    // Counts come from the same snapshot the panel renders, not from a second
    // `scan_queue_depth` round-trip that could disagree with it.
    let running = queue_rows.iter().filter(|r| r.status == "running").count();
    let queued = queue_rows.iter().filter(|r| r.status == "queued").count();
    // Already ordered oldest-first by `list_scan_queue`, which is the order the
    // queue drains in.
    let active: Vec<serde_json::Value> = queue_rows
        .iter()
        .filter(|r| r.status == "queued" || r.status == "running")
        .map(|r| scan_row_json(r, handles.get(r.user_did.as_str()).copied()))
        .collect();

    let mgr = state.scan_manager.read().await;
    let mut response = Vec::new();
    for user in &users {
        let has_fp = state.db.has_fingerprint(&user.did).await.unwrap_or(false);
        let count = state
            .db
            .get_scored_account_count(&user.did)
            .await
            .unwrap_or(0);
        let fp_building = mgr.is_fingerprint_building(&user.did);
        let scan = by_did.get(user.did.as_str()).copied();
        response.push(serde_json::json!({
            "did": user.did,
            "handle": user.handle,
            "has_fingerprint": has_fp,
            "fingerprint_building": fp_building,
            // Unchanged key, unchanged meaning — "when did this user's most
            // recent scan start" — but sourced from the durable row, so it now
            // survives a restart and sees other replicas. Null while a scan is
            // only queued, because a queued scan has not started. Duplicated
            // inside `scan` so the panel and the table read one shape.
            "last_scan_at": scan.and_then(|r| r.started_at.clone()),
            "scan": scan.map(|r| scan_row_json(r, Some(user.handle.as_str()))),
            "scored_accounts": count,
            "last_login_at": user.last_login_at,
        }));
    }

    Ok(Json(serde_json::json!({
        "users": response,
        "queue": {
            "running": running,
            "queued": queued,
            "concurrency_limit": crate::web::admitter::scan_concurrency(),
            "active": active,
        },
    })))
}

/// POST /api/admin/users — pre-seed a user by Bluesky handle.
/// Resolves handle to DID, inserts user, and spawns background fingerprint build.
pub async fn pre_seed_user(
    Extension(auth): Extension<AuthUser>,
    State(state): State<AppState>,
    Json(body): Json<PreSeedRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if !auth.is_admin {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "Admin required"})),
        ));
    }
    let handle = body.handle.trim().to_string();
    if handle.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Handle is required"})),
        ));
    }

    // Resolve handle to DID via the public AT Protocol API
    let client = PublicAtpClient::new(&state.config.public_api_url).map_err(|e| {
        tracing::error!("Failed to create ATP client: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Internal error"})),
        )
    })?;
    let did = match client.resolve_handle(&handle).await {
        Ok(did) => did,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") || msg.contains("404") {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error": format!("Handle not found: {handle}")})),
                ));
            }
            return Err((
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("Failed to resolve handle: {msg}")})),
            ));
        }
    };

    // Check if user already exists
    if state
        .db
        .get_user_handle(&did)
        .await
        .ok()
        .flatten()
        .is_some()
    {
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "User already exists"})),
        ));
    }

    // Insert user record
    state.db.upsert_user(&did, &handle).await.map_err(|e| {
        tracing::error!("Failed to upsert user: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Database error"})),
        )
    })?;

    tracing::info!(
        admin_did = %auth.did,
        target_did = %did,
        target_handle = %handle,
        "Admin pre-seeded user"
    );

    // Spawn background fingerprint build
    let db = state.db.clone();
    let config = state.config.clone();
    let scan_mgr = state.scan_manager.clone();
    let fp_did = did.clone();
    let fp_handle = handle.clone();
    {
        let mut mgr = scan_mgr.write().await;
        mgr.start_fingerprint_build(&fp_did);
    }
    tokio::spawn(async move {
        let result = scan_job::build_user_fingerprint(&config, &*db, &fp_did, &fp_handle).await;
        let mut mgr = scan_mgr.write().await;
        mgr.finish_fingerprint_build(&fp_did);
        if let Err(e) = result {
            tracing::error!(target_did = %fp_did, "Fingerprint build failed: {e}");
        } else {
            tracing::info!(target_did = %fp_did, "Fingerprint build complete");
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({"did": did, "handle": handle})),
    ))
}

/// POST /api/admin/users/{did}/scan — queue a scan for a specific user.
///
/// Enqueues exactly like `POST /api/scan` does, per Bryan's ruling on #257.
/// This used to call `launch_scan` directly, which never touched `scan_queue`
/// and was therefore invisible to `claim_next_scan`'s running count — real
/// concurrency would have been `CHARCOAL_SCAN_CONCURRENCY + admin scans`, and
/// the GPU spend with it. Admin gives up starting immediately; there is one
/// admission path and the cap holds absolutely.
pub async fn trigger_admin_scan(
    Extension(auth): Extension<AuthUser>,
    State(state): State<AppState>,
    Path(target_did): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if !auth.is_admin {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "Admin required"})),
        ));
    }
    // Existence check only — the admitter looks the handle up again when it
    // claims the row. Checking here turns "this DID was never registered" into
    // a 404 now rather than a failed queue row later.
    state
        .db
        .get_user_handle(&target_did)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Database error"})),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "User not found"})),
            )
        })?;

    state.db.enqueue_scan(&target_did).await.map_err(|e| {
        tracing::error!(error = %format!("{e:#}"), "admin enqueue failed");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Could not queue the scan"})),
        )
    })?;

    if let Some(wake) = &state.scan_wake {
        let _ = wake.try_send(());
    }

    tracing::info!(
        admin_did = %auth.did,
        target_did = %target_did,
        "Admin queued scan"
    );

    // Same shape as POST /api/scan, deliberately: the enqueue succeeded so this
    // stays 202, but a failed read-back reports `position: null` rather than 0.
    // 0 is what a *running* scan legitimately reports, so reusing it here would
    // hide a database error behind a plausible-looking answer.
    let entry = match state
        .db
        .scan_queue_entry(&target_did, crate::web::admitter::scan_concurrency())
        .await
    {
        Ok(entry) => entry,
        Err(e) => {
            tracing::error!(
                target_did = %target_did,
                error = %format!("{e:#}"),
                "admin queued the scan but could not read back its queue entry"
            );
            None
        }
    };
    let (status, position, eta) = match entry {
        Some(e) => (e.status, Some(e.position), e.eta_seconds),
        None => ("queued".to_string(), None, None),
    };

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "status": status,
            "position": position,
            "eta_seconds": eta,
            "user_did": target_did,
        })),
    ))
}

/// DELETE /api/admin/users/{did} — remove a pre-seeded user and all their data.
pub async fn delete_user(
    Extension(auth): Extension<AuthUser>,
    State(state): State<AppState>,
    Path(target_did): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if !auth.is_admin {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "Admin required"})),
        ));
    }
    if auth.did == target_did {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Cannot delete yourself"})),
        ));
    }
    if state
        .db
        .get_user_handle(&target_did)
        .await
        .ok()
        .flatten()
        .is_none()
    {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "User not found"})),
        ));
    }

    // Gate on the durable `scan_queue` row, not `ScanManager::is_scan_running_for`
    // (#278). The in-process status map only knows about scans THIS process
    // launched, so it misses a row that is merely queued, one running on
    // another replica, and anything left over a restart. `delete_user_data`
    // already clears `scan_queue` below, so nothing is orphaned either way —
    // this just makes the guard as durable as the rest of #257's admission
    // path instead of trusting a process-local view.
    //
    // A DB error here must NOT fall through to the delete. The guard it
    // replaced was an in-memory read that could not fail, so `if let Ok(..)`
    // would have quietly turned "we cannot tell" into "go ahead" — deleting a
    // user out from under a live scan is exactly what this check exists to
    // prevent, so an unreadable queue is a 500, not an implicit yes.
    let queue_entry = state
        .db
        .scan_queue_entry(&target_did, crate::web::admitter::scan_concurrency())
        .await
        .map_err(|e| {
            tracing::error!(target_did = %target_did, error = %e, "delete_user: scan_queue lookup failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Database error"})),
            )
        })?;
    if let Some(entry) = queue_entry {
        if matches!(entry.status.as_str(), "queued" | "running") {
            return Err((
                StatusCode::CONFLICT,
                Json(
                    serde_json::json!({"error": "Cannot delete user with queued or running scan"}),
                ),
            ));
        }
    }

    tracing::info!(
        admin_did = %auth.did,
        target_did = %target_did,
        "Admin deleted user"
    );

    state.db.delete_user_data(&target_did).await.map_err(|e| {
        tracing::error!("Failed to delete user data: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Database error"})),
        )
    })?;

    Ok(Json(serde_json::json!({"deleted": target_did})))
}
