// Admin allowlist handlers (#309) — the DB-backed layer of the access gate.
//
// Same authorization idiom as handlers/admin.rs: each handler checks
// `auth.is_admin` at the top and returns 403. Decision endpoints are
// idempotent — repeating a decision is a success, not an error.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};

use crate::db::traits::AccessRequestRow;
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
