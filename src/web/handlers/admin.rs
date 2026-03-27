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
    Json(serde_json::json!({
        "did": auth.did,
        "handle": handle,
        "is_admin": auth.is_admin,
    }))
}

/// GET /api/admin/users — list all registered users with status info.
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
    let mgr = state.scan_manager.read().await;
    let mut response = Vec::new();
    for user in users {
        let has_fp = state.db.has_fingerprint(&user.did).await.unwrap_or(false);
        let count = state
            .db
            .get_scored_account_count(&user.did)
            .await
            .unwrap_or(0);
        let fp_building = mgr.is_fingerprint_building(&user.did);
        let last_scan = mgr.get_status(&user.did).and_then(|s| s.started_at.clone());
        response.push(serde_json::json!({
            "did": user.did,
            "handle": user.handle,
            "has_fingerprint": has_fp,
            "fingerprint_building": fp_building,
            "last_scan_at": last_scan,
            "scored_accounts": count,
            "last_login_at": user.last_login_at,
        }));
    }
    Ok(Json(serde_json::json!({ "users": response })))
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

/// POST /api/admin/users/{did}/scan — trigger a scan for a specific user.
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
    let handle = state
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

    {
        let mut mgr = state.scan_manager.write().await;
        mgr.try_start_scan(&target_did).map_err(|msg| {
            (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": msg})),
            )
        })?;
    }

    tracing::info!(
        admin_did = %auth.did,
        target_did = %target_did,
        "Admin triggered scan"
    );

    scan_job::launch_scan(
        state.config.clone(),
        state.db.clone(),
        state.scan_manager.clone(),
        target_did.clone(),
        handle,
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({"message": "Scan started", "user_did": target_did})),
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

    {
        let mgr = state.scan_manager.read().await;
        if mgr.is_scan_running_for(&target_did) {
            return Err((
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "Cannot delete user with running scan"})),
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
