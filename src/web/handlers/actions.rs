//! Mute/block action endpoints (#315, spec §8).
//!
//! Reads use `AuthUser::effective_did`, so an admin can look at another
//! person's action log through `?as_user=`. Writes refuse impersonation
//! before doing anything else: nobody acts with someone else's credentials.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::traits::{ActionBatchRow, ActionRow, NewAction, ScoreSnapshot};
use crate::web::actions::scope::write_scope;
use crate::web::actions::session::SessionStore;
use crate::web::handlers::oauth::{begin_oauth, oauth_client};
use crate::web::{AppState, AuthUser};

/// Upper bound on targets per request — a whole tier fits comfortably.
const MAX_TARGETS: usize = 5_000;

/// `{"error": …, "code": …}` — the error shape clients branch on.
pub fn api_error_code(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": message, "code": code }))).into_response()
}

fn disabled() -> Response {
    api_error_code(
        StatusCode::SERVICE_UNAVAILABLE,
        "actions_disabled",
        "Mute and block actions are not enabled on this server",
    )
}

// `Response` is the error type for nearly every fallible function in this
// file (async handlers included) — it is what an axum handler must return
// either way, so boxing it here would only add an indirection at every call
// site without shrinking anything that matters.
#[allow(clippy::result_large_err)]
fn sessions(state: &AppState) -> Result<Arc<SessionStore>, Response> {
    state.sessions.clone().ok_or_else(disabled)
}

/// Writes act with the caller's own credentials only.
#[allow(clippy::result_large_err)]
fn writer(auth: &AuthUser) -> Result<(), Response> {
    if auth.is_impersonating() {
        return Err(api_error_code(
            StatusCode::FORBIDDEN,
            "impersonation_forbidden",
            "You can view this account's actions, but not act on its behalf",
        ));
    }
    Ok(())
}

fn db_error(e: anyhow::Error) -> Response {
    tracing::error!(error = %format!("{e:#}"), "actions endpoint database failure");
    api_error_code(
        StatusCode::INTERNAL_SERVER_ERROR,
        "db_error",
        "Something went wrong — please try again",
    )
}

fn not_found() -> Response {
    api_error_code(StatusCode::NOT_FOUND, "not_found", "Not found")
}

fn not_connected() -> Response {
    api_error_code(
        StatusCode::CONFLICT,
        "not_connected",
        "Connect your Bluesky account before muting or blocking",
    )
}

/// Require a stored write session for `did` (the runner does the real
/// load-with-refresh; here we only need to know one exists).
async fn require_connected(
    sessions: &SessionStore,
    state: &AppState,
    did: &str,
) -> Result<(), Response> {
    match sessions.status(&*state.db, did).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(not_connected()),
        Err(e) => Err(db_error(anyhow::Error::new(e))),
    }
}

fn wake(state: &AppState, batch_id: i64) {
    if let Some(wake) = &state.action_wake {
        // A full channel means a wake is already pending; the runner re-scans
        // every unfinished batch on each wake, so dropping this one is safe.
        let _ = wake.try_send(batch_id);
    }
}

/// A forward action Charcoal currently holds on a target: applied (or found
/// already done) and not itself an undo. An undo row that succeeded is also
/// `applied` with `kind = mute|block` — without the `undo_of` filter it would
/// read as an active mute.
fn is_active_forward(r: &ActionRow) -> bool {
    r.undo_of.is_none() && matches!(r.status.as_str(), "applied" | "skipped_already_done")
}

fn accepted(batch_id: i64) -> Response {
    (StatusCode::ACCEPTED, Json(json!({ "batch_id": batch_id }))).into_response()
}

// ---- JSON shapes ----

fn drift(r: &ActionRow, snap: Option<&ScoreSnapshot>) -> (Option<String>, bool) {
    let current = snap.and_then(|s| s.threat_tier.clone());
    let drifted = matches!((&r.tier_at_action, &current), (Some(then), Some(now)) if then != now);
    (current, drifted)
}

fn action_json(r: &ActionRow, snap: Option<&ScoreSnapshot>) -> Value {
    let (current_tier, drifted) = drift(r, snap);
    json!({
        "id": r.id,
        "batch_id": r.batch_id,
        "target_did": r.target_did,
        "handle": snap.map(|s| s.handle.clone()),
        "kind": r.kind,
        "status": r.status,
        "record_uri": r.record_uri,
        "undo_of": r.undo_of,
        "error": r.error,
        "score_at_action": r.score_at_action,
        "tier_at_action": r.tier_at_action,
        "current_tier": current_tier,
        "drifted": drifted,
        "applied_at": r.applied_at,
        "undone_at": r.undone_at,
    })
}

fn batch_summary(
    b: &ActionBatchRow,
    rows: &[ActionRow],
    by_did: &HashMap<&str, &ScoreSnapshot>,
) -> Value {
    let mut counts: BTreeMap<&str, i64> = BTreeMap::new();
    for r in rows {
        *counts.entry(r.status.as_str()).or_default() += 1;
    }
    let drifted = rows
        .iter()
        .any(|r| drift(r, by_did.get(r.target_did.as_str()).copied()).1);
    json!({
        "id": b.id,
        "kind": b.kind,
        "source": b.source,
        "requested": b.requested,
        "status": b.status,
        "error": b.error,
        "created_at": b.created_at,
        "started_at": b.started_at,
        "finished_at": b.finished_at,
        "counts": counts,
        "drifted": drifted,
    })
}

async fn snapshots(state: &AppState, did: &str) -> Result<Vec<ScoreSnapshot>, Response> {
    state.db.list_score_snapshots(did).await.map_err(db_error)
}

fn index(snaps: &[ScoreSnapshot]) -> HashMap<&str, &ScoreSnapshot> {
    snaps.iter().map(|s| (s.did.as_str(), s)).collect()
}

/// The batch, if it exists AND belongs to the effective user. Anything else
/// is 404 — never reveal that someone else's id exists.
async fn owned_batch(
    state: &AppState,
    auth: &AuthUser,
    id: i64,
) -> Result<ActionBatchRow, Response> {
    match state.db.get_action_batch(id).await {
        Ok(Some(b)) if b.user_did == auth.effective_did => Ok(b),
        Ok(_) => Err(not_found()),
        Err(e) => Err(db_error(e)),
    }
}

async fn rows_for(state: &AppState, batch_id: i64) -> Result<Vec<ActionRow>, Response> {
    state
        .db
        .list_actions_for_batch(batch_id)
        .await
        .map_err(db_error)
}

/// An undo row for `orig`: same kind and snapshot, pointing back at it.
fn undo_row(orig: &ActionRow) -> NewAction {
    NewAction {
        target_did: orig.target_did.clone(),
        kind: orig.kind.clone(),
        undo_of: Some(orig.id),
        score_at_action: orig.score_at_action,
        tier_at_action: orig.tier_at_action.clone(),
    }
}

// ---- status / connect / disconnect ----

/// GET /api/actions/status
pub async fn get_status(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
) -> Response {
    let Some(sessions) = state.sessions.as_ref() else {
        return Json(json!({ "enabled": false, "connected": false })).into_response();
    };
    match sessions.status(&*state.db, &auth.effective_did).await {
        Ok(Some(s)) => Json(json!({
            "enabled": true,
            "connected": true,
            "scope": s.scope,
            "pds_url": s.pds_url,
            "connected_at": s.connected_at,
        }))
        .into_response(),
        Ok(None) => Json(json!({ "enabled": true, "connected": false })).into_response(),
        Err(e) => db_error(anyhow::Error::new(e)),
    }
}

#[derive(Deserialize)]
pub struct ConnectRequest {
    /// What the person was about to do: `mute`, `block`, or `undo`.
    pub kind: String,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub handle: Option<String>,
}

fn plain(s: &str, extra: &[char]) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
}

/// Where the browser lands after consent. Computed here, never accepted from
/// the client, and always a relative path — so it cannot become an open
/// redirect. Unknown or unsafe input falls back to `/actions`.
pub(crate) fn return_to(body: &ConnectRequest) -> String {
    let kind = body.kind.as_str();
    if !matches!(kind, "mute" | "block" | "undo") {
        return "/actions".to_string();
    }
    if kind != "undo" {
        if let Some(tier) = body.tier.as_deref().filter(|t| plain(t, &[])) {
            return format!("/accounts?tier={tier}&resume={kind}");
        }
    }
    if let Some(handle) = body.handle.as_deref().filter(|h| plain(h, &['.', '-'])) {
        return format!("/accounts/{handle}?resume={kind}");
    }
    "/actions".to_string()
}

/// POST /api/actions/connect — begin the write-consent round-trip.
pub async fn connect(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
    Json(body): Json<ConnectRequest>,
) -> Response {
    if let Err(r) = writer(&auth) {
        return r;
    }
    if state.sessions.is_none() {
        return disabled();
    }
    // login_hint for the PDS; the DID is what binds the callback.
    let handle = match state.db.get_user_handle(&auth.did).await {
        Ok(Some(h)) => h,
        Ok(None) => auth.did.clone(),
        Err(e) => return db_error(e),
    };
    match begin_oauth(
        &state,
        &state.http,
        &handle,
        &auth.did,
        &write_scope(),
        Some(return_to(&body)),
    )
    .await
    {
        Ok(url) => Json(json!({ "redirect_url": url })).into_response(),
        Err(response) => response,
    }
}

/// POST /api/actions/disconnect
pub async fn disconnect(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
) -> Response {
    if let Err(r) = writer(&auth) {
        return r;
    }
    let sessions = match sessions(&state) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match sessions
        .disconnect(&*state.db, &state.http, &oauth_client(&state), &auth.did)
        .await
    {
        Ok(existed) => Json(json!({ "disconnected": existed })).into_response(),
        Err(e) => db_error(anyhow::Error::new(e)),
    }
}

// ---- batches ----

#[derive(Deserialize)]
pub struct CreateBatchRequest {
    pub kind: String,
    /// Free text for the log, e.g. `tier:High` or `account:a.test`.
    #[serde(default)]
    pub source: String,
    pub targets: Vec<String>,
}

/// POST /api/actions/batches
pub async fn create_batch(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
    Json(body): Json<CreateBatchRequest>,
) -> Response {
    if let Err(r) = writer(&auth) {
        return r;
    }
    let sessions = match sessions(&state) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !matches!(body.kind.as_str(), "mute" | "block") {
        return api_error_code(
            StatusCode::BAD_REQUEST,
            "invalid_kind",
            "kind must be \"mute\" or \"block\"",
        );
    }
    let mut seen = HashSet::new();
    let targets: Vec<String> = body
        .targets
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty() && seen.insert(t.clone()))
        .collect();
    if targets.is_empty() || targets.len() > MAX_TARGETS {
        return api_error_code(
            StatusCode::BAD_REQUEST,
            "invalid_targets",
            "targets must list between 1 and 5000 DIDs",
        );
    }
    let source: String = body.source.trim().chars().take(64).collect();
    let source = if source.is_empty() {
        "manual".to_string()
    } else {
        source
    };

    if let Err(r) = require_connected(&sessions, &state, &auth.did).await {
        return r;
    }

    // Targets must be accounts Charcoal scored FOR THIS USER (spec §7).
    let snaps = match snapshots(&state, &auth.did).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let by_did = index(&snaps);
    let unknown: Vec<&str> = targets
        .iter()
        .map(String::as_str)
        .filter(|d| !by_did.contains_key(d))
        .collect();
    if !unknown.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Some targets are not in your scored accounts",
                "code": "unknown_target",
                "unknown": unknown,
            })),
        )
            .into_response();
    }

    // Skip targets Charcoal already holds this kind of action on.
    let active: HashSet<String> = match state.db.active_actions(&auth.did).await {
        Ok(rows) => rows
            .into_iter()
            .filter(|r| r.kind == body.kind && is_active_forward(r))
            .map(|r| r.target_did)
            .collect(),
        Err(e) => return db_error(e),
    };
    let rows: Vec<NewAction> = targets
        .iter()
        .filter(|d| !active.contains(*d))
        .map(|d| {
            let s = by_did[d.as_str()];
            NewAction {
                target_did: d.clone(),
                kind: body.kind.clone(),
                undo_of: None,
                score_at_action: s.threat_score,
                tier_at_action: s.threat_tier.clone(),
            }
        })
        .collect();
    let skipped_active = targets.len() - rows.len();
    if rows.is_empty() {
        return Json(json!({ "batch_id": null, "requested": 0, "skipped_active": skipped_active }))
            .into_response();
    }
    let id = match state
        .db
        .create_action_batch(&auth.did, &body.kind, &source, &rows)
        .await
    {
        Ok(id) => id,
        Err(e) => return db_error(e),
    };
    tracing::info!(did = %auth.did, batch = id, kind = %body.kind, requested = rows.len(), "action batch created");
    wake(&state, id);
    (
        StatusCode::ACCEPTED,
        Json(json!({ "batch_id": id, "requested": rows.len(), "skipped_active": skipped_active })),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

/// GET /api/actions/batches?limit&offset
pub async fn list_batches(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
    Query(q): Query<ListQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let offset = q.offset.unwrap_or(0).max(0);
    let batches = match state
        .db
        .list_action_batches(&auth.effective_did, limit, offset)
        .await
    {
        Ok(b) => b,
        Err(e) => return db_error(e),
    };
    let snaps = match snapshots(&state, &auth.effective_did).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let by_did = index(&snaps);
    // One row query per batch; `limit` caps this at 100 small queries.
    let mut out = Vec::with_capacity(batches.len());
    for b in &batches {
        let rows = match rows_for(&state, b.id).await {
            Ok(r) => r,
            Err(r) => return r,
        };
        out.push(batch_summary(b, &rows, &by_did));
    }
    Json(json!({ "batches": out, "limit": limit, "offset": offset })).into_response()
}

/// GET /api/actions/batches/{id}
pub async fn get_batch(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
    Path(id): Path<i64>,
) -> Response {
    let batch = match owned_batch(&state, &auth, id).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let rows = match rows_for(&state, id).await {
        Ok(r) => r,
        Err(r) => return r,
    };
    let snaps = match snapshots(&state, &auth.effective_did).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let by_did = index(&snaps);
    let actions: Vec<Value> = rows
        .iter()
        .map(|r| action_json(r, by_did.get(r.target_did.as_str()).copied()))
        .collect();
    Json(json!({ "batch": batch_summary(&batch, &rows, &by_did), "actions": actions }))
        .into_response()
}

/// POST /api/actions/batches/{id}/undo — one undo batch over every active row.
pub async fn undo_batch(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
    Path(id): Path<i64>,
) -> Response {
    if let Err(r) = writer(&auth) {
        return r;
    }
    let sessions = match sessions(&state) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let batch = match owned_batch(&state, &auth, id).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    if let Err(r) = require_connected(&sessions, &state, &auth.did).await {
        return r;
    }
    let rows = match rows_for(&state, id).await {
        Ok(r) => r,
        Err(r) => return r,
    };
    let undo: Vec<NewAction> = if batch.kind == "undo" {
        Vec::new()
    } else {
        rows.iter()
            .filter(|r| is_active_forward(r))
            .map(undo_row)
            .collect()
    };
    if undo.is_empty() {
        return api_error_code(
            StatusCode::BAD_REQUEST,
            "nothing_to_undo",
            "There is nothing in this batch to undo",
        );
    }
    let source = format!("undo:batch:{id}");
    match state
        .db
        .create_action_batch(&auth.did, "undo", &source, &undo)
        .await
    {
        Ok(new_id) => {
            wake(&state, new_id);
            accepted(new_id)
        }
        Err(e) => db_error(e),
    }
}

/// POST /api/actions/batches/{id}/retry — a NEW batch over the failed rows.
pub async fn retry_batch(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
    Path(id): Path<i64>,
) -> Response {
    if let Err(r) = writer(&auth) {
        return r;
    }
    let sessions = match sessions(&state) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let batch = match owned_batch(&state, &auth, id).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    if let Err(r) = require_connected(&sessions, &state, &auth.did).await {
        return r;
    }
    let rows = match rows_for(&state, id).await {
        Ok(r) => r,
        Err(r) => return r,
    };
    let again: Vec<NewAction> = rows
        .iter()
        .filter(|r| r.status == "failed")
        .map(|r| NewAction {
            target_did: r.target_did.clone(),
            kind: r.kind.clone(),
            undo_of: r.undo_of,
            score_at_action: r.score_at_action,
            tier_at_action: r.tier_at_action.clone(),
        })
        .collect();
    if again.is_empty() {
        return api_error_code(
            StatusCode::BAD_REQUEST,
            "nothing_to_retry",
            "This batch has no failed rows",
        );
    }
    let source = format!("retry:{id}");
    match state
        .db
        .create_action_batch(&auth.did, &batch.kind, &source, &again)
        .await
    {
        Ok(new_id) => {
            wake(&state, new_id);
            accepted(new_id)
        }
        Err(e) => db_error(e),
    }
}

/// POST /api/actions/{action_id}/undo — undo one row.
pub async fn undo_action(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
    Path(action_id): Path<i64>,
) -> Response {
    if let Err(r) = writer(&auth) {
        return r;
    }
    let sessions = match sessions(&state) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let row = match state.db.get_action(action_id).await {
        Ok(Some(r)) if r.user_did == auth.did => r,
        Ok(_) => return not_found(),
        Err(e) => return db_error(e),
    };
    if let Err(r) = require_connected(&sessions, &state, &auth.did).await {
        return r;
    }
    if !is_active_forward(&row) {
        return api_error_code(
            StatusCode::BAD_REQUEST,
            "nothing_to_undo",
            "This action is not currently in effect",
        );
    }
    let source = format!("undo:action:{action_id}");
    match state
        .db
        .create_action_batch(&auth.did, "undo", &source, &[undo_row(&row)])
        .await
    {
        Ok(new_id) => {
            wake(&state, new_id);
            accepted(new_id)
        }
        Err(e) => db_error(e),
    }
}

/// GET /api/accounts/{handle}/actions — what Charcoal currently holds on one
/// target, for the account page's button state.
pub async fn account_actions(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
    Path(handle): Path<String>,
) -> Response {
    let account = match state
        .db
        .get_account_by_handle(&auth.effective_did, &handle)
        .await
    {
        Ok(Some(a)) => a,
        Ok(None) => return not_found(),
        Err(e) => return db_error(e),
    };
    let rows = match state.db.active_actions(&auth.effective_did).await {
        Ok(r) => r,
        Err(e) => return db_error(e),
    };
    let snap = ScoreSnapshot {
        did: account.did.clone(),
        handle: account.handle.clone(),
        threat_score: account.threat_score,
        threat_tier: account.threat_tier.clone(),
    };
    let actions: Vec<Value> = rows
        .iter()
        .filter(|r| r.target_did == account.did && is_active_forward(r))
        .map(|r| action_json(r, Some(&snap)))
        .collect();
    Json(json!({ "did": account.did, "actions": actions })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(kind: &str, tier: Option<&str>, handle: Option<&str>) -> ConnectRequest {
        ConnectRequest {
            kind: kind.to_string(),
            tier: tier.map(str::to_string),
            handle: handle.map(str::to_string),
        }
    }

    #[test]
    fn return_to_prefers_the_tier_page_for_bulk_actions() {
        assert_eq!(
            return_to(&req("mute", Some("High"), None)),
            "/accounts?tier=High&resume=mute"
        );
        assert_eq!(
            return_to(&req("block", Some("High"), Some("a.test"))),
            "/accounts?tier=High&resume=block"
        );
    }

    #[test]
    fn return_to_uses_the_account_page_for_single_and_undo() {
        assert_eq!(
            return_to(&req("mute", None, Some("a.test"))),
            "/accounts/a.test?resume=mute"
        );
        assert_eq!(
            return_to(&req("undo", Some("High"), Some("a-b.test"))),
            "/accounts/a-b.test?resume=undo"
        );
    }

    #[test]
    fn return_to_falls_back_on_anything_unsafe() {
        assert_eq!(return_to(&req("mute", Some("High&x=1"), None)), "/actions");
        assert_eq!(
            return_to(&req("mute", None, Some("//evil.example"))),
            "/actions"
        );
        assert_eq!(return_to(&req("mute", None, Some("a.test?x"))), "/actions");
        assert_eq!(return_to(&req("nuke", Some("High"), None)), "/actions");
        assert_eq!(return_to(&req("undo", None, None)), "/actions");
    }
}
