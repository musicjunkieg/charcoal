// Admin allowlist handlers (#309) — the DB-backed layer of the access gate.
//
// Same authorization idiom as handlers/admin.rs: each handler checks
// `auth.is_admin` at the top and returns 403. Decision endpoints are
// idempotent — repeating a decision is a success, not an error.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};
use serde::Deserialize;

use crate::bluesky::client::PublicAtpClient;
use crate::db::traits::AccessRequestRow;
use crate::web::scan_job;
use crate::web::{AppState, AuthUser};

fn admin_guard(auth: &AuthUser) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if !auth.is_admin {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "Admin required"})),
        ));
    }
    Ok(())
}

fn row_json(r: &AccessRequestRow) -> serde_json::Value {
    serde_json::json!({
        "did": r.did,
        "handle": r.handle,
        "status": r.status,
        "requested_at": r.requested_at,
        "decided_at": r.decided_at,
        "decided_by": r.decided_by,
    })
}

/// GET /api/admin/access — every row, grouped by status, oldest first.
pub async fn list_access(
    Extension(auth): Extension<AuthUser>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    admin_guard(&auth)?;
    let rows = state.db.list_access_requests().await.map_err(|e| {
        tracing::error!(error = %format!("{e:#}"), "failed to list access requests");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Database error"})),
        )
    })?;
    let group = |s: &str| -> Vec<serde_json::Value> {
        rows.iter()
            .filter(|r| r.status == s)
            .map(row_json)
            .collect()
    };
    Ok(Json(serde_json::json!({
        "pending": group("pending"),
        "allowed": group("allowed"),
        "denied": group("denied"),
    })))
}

/// Shared body for approve/deny: set the status, 404 when no row exists.
///
/// Concrete return type (not `impl IntoResponse`) so Task 8 can call this with
/// `?` from another handler.
async fn decide(
    state: &AppState,
    auth: &AuthUser,
    did: &str,
    status: &str,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let found = state
        .db
        .set_access_status(did, status, &auth.did)
        .await
        .map_err(|e| {
            tracing::error!(error = %format!("{e:#}"), "failed to set access status");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Database error"})),
            )
        })?;
    if !found {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "No access request for that DID"})),
        ));
    }
    tracing::info!(admin_did = %auth.did, target_did = %did, status, "Access decision");
    Ok(Json(serde_json::json!({"did": did, "status": status})))
}

/// POST /api/admin/access/{did}/approve
pub async fn approve_access(
    Extension(auth): Extension<AuthUser>,
    State(state): State<AppState>,
    Path(did): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    admin_guard(&auth)?;
    decide(&state, &auth, &did, "allowed").await
}

/// POST /api/admin/access/{did}/deny — also the revoke path for allowed DIDs.
pub async fn deny_access(
    Extension(auth): Extension<AuthUser>,
    State(state): State<AppState>,
    Path(did): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    admin_guard(&auth)?;
    decide(&state, &auth, &did, "denied").await
}

#[derive(Debug, Deserialize)]
pub struct GrantRequest {
    pub handle: String,
}

/// POST /api/admin/access — grant access by Bluesky handle.
/// Resolution + error mapping copied from pre_seed_user (handlers/admin.rs):
/// 404 for unknown handles, 502 when resolution infrastructure fails.
pub async fn grant_access_by_handle(
    Extension(auth): Extension<AuthUser>,
    State(state): State<AppState>,
    Json(body): Json<GrantRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    admin_guard(&auth)?;
    let handle = body.handle.trim().trim_start_matches('@').to_string();
    if handle.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Handle is required"})),
        ));
    }

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

    state
        .db
        .grant_access(&did, &handle, &auth.did)
        .await
        .map_err(|e| {
            tracing::error!(error = %format!("{e:#}"), "failed to grant access");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Database error"})),
            )
        })?;

    tracing::info!(admin_did = %auth.did, target_did = %did, target_handle = %handle, "Access granted by handle");
    Ok(Json(
        serde_json::json!({"did": did, "handle": handle, "status": "allowed"}),
    ))
}

/// POST /api/admin/access/{did}/approve-scan — approve AND kick off onboarding.
///
/// Two operations reported honestly: approval commits first; a scan-side
/// failure downgrades the `scan` field, never the approval. The enqueue goes
/// through `enqueue_scan` like every other scan (#257: one admission path).
pub async fn approve_access_and_scan(
    Extension(auth): Extension<AuthUser>,
    State(state): State<AppState>,
    Path(did): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    admin_guard(&auth)?;
    // Reuse the approve path: 404 / decided_by / idempotency all match.
    let _ = decide(&state, &auth, &did, "allowed").await?;

    // The access row is the source of the handle for pre-seeding.
    let row = match state.db.get_access_request(&did).await {
        Ok(Some(row)) => row,
        Ok(None) | Err(_) => {
            // The decide() above just succeeded, so this is a read-back race
            // or DB blip — approval stands, scan does not happen.
            return Ok(Json(serde_json::json!({
                "did": did, "access": "granted", "scan": "failed to queue",
            })));
        }
    };

    // Pre-seed the user row if they have never signed in, spawning the same
    // background fingerprint build pre_seed_user does.
    let user_missing = state
        .db
        .get_user_handle(&did)
        .await
        .ok()
        .flatten()
        .is_none();
    if user_missing {
        if let Err(e) = state.db.upsert_user(&did, &row.handle).await {
            tracing::error!(error = %format!("{e:#}"), "approve-scan: upsert_user failed");
            return Ok(Json(serde_json::json!({
                "did": did, "access": "granted", "scan": "failed to queue",
            })));
        }
        let db = state.db.clone();
        let config = state.config.clone();
        let scan_mgr = state.scan_manager.clone();
        let fp_did = did.clone();
        let fp_handle = row.handle.clone();
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
    }

    let scan = match state.db.enqueue_scan(&did).await {
        Ok(()) => {
            if let Some(wake) = &state.scan_wake {
                let _ = wake.try_send(());
            }
            "queued"
        }
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "approve-scan: enqueue failed");
            "failed to queue"
        }
    };

    tracing::info!(admin_did = %auth.did, target_did = %did, scan, "Approve + scan");
    Ok(Json(
        serde_json::json!({"did": did, "access": "granted", "scan": scan}),
    ))
}
