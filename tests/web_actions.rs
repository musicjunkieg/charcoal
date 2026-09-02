//! Mute/block action endpoints (#315): auth, impersonation, feature gate,
//! batch shapes, undo/retry, and the write-consent OAuth callback branch.
//!
//! The test AppState has `action_wake: None`, so no runner ever picks a
//! batch up — every row stays exactly as the handlers wrote it.
#![cfg(feature = "web")]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use charcoal::db::models::AccountScore;
use charcoal::db::schema::create_tables;
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::traits::NewAction;
use charcoal::db::Database;
use charcoal::web::auth::{create_token, COOKIE_NAME};
use charcoal::web::test_helpers::{
    build_test_app_actions_disabled, build_test_app_with_state_and_db, TEST_DID, TEST_SECRET,
};
use charcoal::web::AppState;
use serde_json::{json, Value};
use tower::ServiceExt;

const MODELS_REQUIRED: &str = "ONNX models required — run with CHARCOAL_MODEL_DIR=./models";
const OTHER: &str = "did:plc:otheruser000000000000000";
const TARGET_A: &str = "did:plc:targeta00000000000000000";
const TARGET_B: &str = "did:plc:targetb00000000000000000";
const TARGET_C: &str = "did:plc:targetc00000000000000000";

fn session_cookie(did: &str) -> String {
    format!("{}={}", COOKIE_NAME, create_token(TEST_SECRET, did))
}

/// Admin app (TEST_DID is admin, so `?as_user=` impersonation is reachable)
/// with the actions feature enabled and no runner.
fn app() -> (axum::Router, AppState) {
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
    create_tables(&conn).expect("schema");
    let db = Arc::new(SqliteDatabase::new(conn)) as Arc<dyn Database>;
    build_test_app_with_state_and_db(db, TEST_DID, TEST_DID).expect(MODELS_REQUIRED)
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    body: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut req = Request::builder().uri(uri).method(method);
    if let Some(c) = cookie {
        req = req.header("cookie", c);
    }
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let res = app
        .clone()
        .oneshot(
            req.body(Body::from(body.unwrap_or("").to_string()))
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn get(app: &axum::Router, uri: &str, did: &str) -> (StatusCode, Value) {
    let (s, _, v) = send(app, "GET", uri, Some(&session_cookie(did)), None).await;
    (s, v)
}

async fn post(app: &axum::Router, uri: &str, did: &str, body: Option<&str>) -> (StatusCode, Value) {
    let (s, _, v) = send(app, "POST", uri, Some(&session_cookie(did)), body).await;
    (s, v)
}

fn score_fixture(did: &str, handle: &str, score: f64, tier: &str) -> AccountScore {
    AccountScore {
        did: did.to_string(),
        handle: handle.to_string(),
        toxicity_score: Some(0.5),
        topic_overlap: Some(0.3),
        overlap_legacy: None,
        threat_score: Some(score),
        threat_tier: Some(tier.to_string()),
        posts_analyzed: 10,
        top_toxic_posts: vec![],
        scored_at: "2026-09-01T12:00:00Z".to_string(),
        behavioral_signals: None,
        context_score: None,
        graph_distance: None,
        fingerprint_quality: None,
        scoring_confidence: None,
    }
}

async fn seed_scores(db: &dyn Database, user: &str) {
    for (did, handle, score, tier) in [
        (TARGET_A, "a.test", 41.5, "High"),
        (TARGET_B, "b.test", 20.0, "Elevated"),
        (TARGET_C, "c.test", 9.0, "Watch"),
    ] {
        db.upsert_account_score(user, &score_fixture(did, handle, score, tier))
            .await
            .expect("score");
    }
}

/// Store a write session for `did` directly through the SessionStore.
async fn seed_session(state: &AppState, did: &str, pds_url: &str) {
    use atproto_identity::key::{generate_key, KeyType};
    use atproto_oauth::workflow::TokenResponse;
    let key = generate_key(KeyType::P256Private).expect("key");
    let tokens = TokenResponse {
        access_token: "test-access".to_string(),
        token_type: "DPoP".to_string(),
        refresh_token: Some("test-refresh".to_string()),
        scope: charcoal::web::actions::scope::write_scope(),
        expires_in: 3600,
        sub: Some(did.to_string()),
        extra: Default::default(),
    };
    state
        .sessions
        .as_ref()
        .expect("sessions enabled in test app")
        .store(&*state.db, did, pds_url, &key, &tokens)
        .await
        .expect("store session");
}

fn row(target: &str, kind: &str) -> NewAction {
    NewAction {
        target_did: target.to_string(),
        kind: kind.to_string(),
        undo_of: None,
        score_at_action: Some(41.5),
        tier_at_action: Some("High".to_string()),
    }
}

/// Create a batch for `user` and set each row's status in order. Returns
/// (batch_id, action ids).
async fn seed_batch(
    db: &dyn Database,
    user: &str,
    kind: &str,
    rows: &[NewAction],
    statuses: &[(&str, Option<&str>)],
) -> (i64, Vec<i64>) {
    let id = db
        .create_action_batch(user, kind, "test", rows)
        .await
        .expect("batch");
    let actions = db.list_actions_for_batch(id).await.expect("rows");
    for (a, (status, uri)) in actions.iter().zip(statuses) {
        db.update_action(a.id, status, *uri, None)
            .await
            .expect("update");
    }
    (id, actions.iter().map(|a| a.id).collect())
}

fn valid_body(kind: &str, targets: &[&str]) -> String {
    json!({ "kind": kind, "source": "tier:High", "targets": targets }).to_string()
}

// ---- auth / gates ----

#[tokio::test]
async fn every_actions_endpoint_requires_auth() {
    let (app, _) = app();
    for (m, u, b) in [
        ("GET", "/api/actions/status", None),
        ("POST", "/api/actions/connect", Some(r#"{"kind":"mute"}"#)),
        ("POST", "/api/actions/disconnect", None),
        ("GET", "/api/actions/batches", None),
        (
            "POST",
            "/api/actions/batches",
            Some(valid_body("mute", &[TARGET_A]).as_str()),
        ),
        ("GET", "/api/actions/batches/1", None),
        ("POST", "/api/actions/batches/1/undo", None),
        ("POST", "/api/actions/batches/1/retry", None),
        ("POST", "/api/actions/1/undo", None),
        ("GET", "/api/accounts/a.test/actions", None),
        ("GET", "/api/actions/active", None),
    ] {
        let (status, _, _) = send(&app, m, u, None, b).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{m} {u}");
    }
}

#[tokio::test]
async fn disabled_feature_reports_false_and_refuses_writes() {
    let (app, _db) = build_test_app_actions_disabled().expect(MODELS_REQUIRED);
    let (status, body) = get(&app, "/api/actions/status", TEST_DID).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], false);
    assert_eq!(body["connected"], false);

    for (u, b) in [
        ("/api/actions/connect", Some(r#"{"kind":"mute"}"#)),
        ("/api/actions/disconnect", None),
        (
            "/api/actions/batches",
            Some(valid_body("mute", &[TARGET_A]).as_str()),
        ),
        ("/api/actions/batches/1/undo", None),
        ("/api/actions/batches/1/retry", None),
        ("/api/actions/1/undo", None),
    ] {
        let (status, body) = post(&app, u, TEST_DID, b).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{u}: {body}");
        assert_eq!(body["code"], "actions_disabled", "{u}");
    }
    // Reads still work while disabled — the /actions page renders "not enabled".
    let (status, body) = get(&app, "/api/actions/batches", TEST_DID).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["batches"], json!([]));
}

#[tokio::test]
async fn writes_refuse_impersonation_before_anything_else() {
    let (app, state) = app();
    state
        .db
        .upsert_user(OTHER, "other.test")
        .await
        .expect("user");
    seed_session(&state, TEST_DID, "https://pds.test").await;
    for (u, b) in [
        ("/api/actions/connect?as_user=", Some(r#"{"kind":"mute"}"#)),
        ("/api/actions/disconnect?as_user=", None),
        (
            "/api/actions/batches?as_user=",
            Some(valid_body("mute", &[TARGET_A]).as_str()),
        ),
        // ids that do not exist: 403 must win over 404
        ("/api/actions/batches/999/undo?as_user=", None),
        ("/api/actions/batches/999/retry?as_user=", None),
        ("/api/actions/999/undo?as_user=", None),
    ] {
        let uri = format!("{u}{OTHER}");
        let (status, body) = post(&app, &uri, TEST_DID, b).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {body}");
        assert_eq!(body["code"], "impersonation_forbidden", "{uri}");
    }
    // Reads under impersonation are allowed (admin looking at another log).
    let uri = format!("/api/actions/status?as_user={OTHER}");
    let (status, body) = get(&app, &uri, TEST_DID).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["connected"], false, "OTHER has no session");
}

// ---- status / disconnect ----

#[tokio::test]
async fn status_flips_when_a_session_exists() {
    let (app, state) = app();
    let (status, body) = get(&app, "/api/actions/status", TEST_DID).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], true);
    assert_eq!(body["connected"], false);
    assert!(body.get("scope").is_none());

    seed_session(&state, TEST_DID, "https://pds.test").await;
    let (_, body) = get(&app, "/api/actions/status", TEST_DID).await;
    assert_eq!(body["connected"], true);
    assert_eq!(body["scope"], charcoal::web::actions::scope::write_scope());
    assert_eq!(body["pds_url"], "https://pds.test");
    assert!(body["connected_at"].is_string());
}

#[tokio::test]
async fn disconnect_deletes_the_session_even_when_revocation_fails() {
    let (app, state) = app();
    // A mock with nothing mounted: discovery 404s, revocation is best-effort.
    let mock = wiremock::MockServer::start().await;
    seed_session(&state, TEST_DID, &mock.uri()).await;

    let (status, body) = post(&app, "/api/actions/disconnect", TEST_DID, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["disconnected"], true);
    assert!(state
        .db
        .get_oauth_session(TEST_DID)
        .await
        .unwrap()
        .is_none());

    let (_, body) = post(&app, "/api/actions/disconnect", TEST_DID, None).await;
    assert_eq!(body["disconnected"], false);
}

// ---- create batch ----

#[tokio::test]
async fn create_batch_validates_input_then_requires_a_session() {
    let (app, state) = app();
    seed_scores(&*state.db, TEST_DID).await;

    let (status, body) = post(
        &app,
        "/api/actions/batches",
        TEST_DID,
        Some(&valid_body("nuke", &[TARGET_A])),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_kind");

    let (status, body) = post(
        &app,
        "/api/actions/batches",
        TEST_DID,
        Some(&valid_body("mute", &[])),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_targets");

    let (status, body) = post(
        &app,
        "/api/actions/batches",
        TEST_DID,
        Some(&valid_body("mute", &[TARGET_A])),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "not_connected");
}

#[tokio::test]
async fn create_batch_rejects_targets_outside_the_users_scores() {
    let (app, state) = app();
    seed_session(&state, TEST_DID, "https://pds.test").await;
    state
        .db
        .upsert_account_score(TEST_DID, &score_fixture(TARGET_A, "a.test", 41.5, "High"))
        .await
        .unwrap();
    // TARGET_B is scored for OTHER, not for TEST_DID — still unknown here.
    state
        .db
        .upsert_account_score(OTHER, &score_fixture(TARGET_B, "b.test", 20.0, "Elevated"))
        .await
        .unwrap();

    let (status, body) = post(
        &app,
        "/api/actions/batches",
        TEST_DID,
        Some(&valid_body("mute", &[TARGET_A, TARGET_B])),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "unknown_target");
    assert_eq!(body["unknown"], json!([TARGET_B]));
    assert!(
        state
            .db
            .list_action_batches(TEST_DID, 10, 0)
            .await
            .unwrap()
            .is_empty(),
        "nothing created"
    );
}

#[tokio::test]
async fn create_batch_snapshots_scores_skips_active_and_reports_drift() {
    let (app, state) = app();
    seed_session(&state, TEST_DID, "https://pds.test").await;
    seed_scores(&*state.db, TEST_DID).await;
    // B already muted by Charcoal; a successful UNDO row for C must NOT count as active.
    seed_batch(
        &*state.db,
        TEST_DID,
        "mute",
        &[row(TARGET_B, "mute")],
        &[("applied", None)],
    )
    .await;
    let undo_c = NewAction {
        undo_of: Some(1),
        ..row(TARGET_C, "mute")
    };
    seed_batch(
        &*state.db,
        TEST_DID,
        "undo",
        &[undo_c],
        &[("applied", None)],
    )
    .await;

    let body = json!({ "kind": "mute", "source": "tier:High", "targets": [TARGET_A, TARGET_B, TARGET_C, TARGET_A] }).to_string();
    let (status, created) = post(&app, "/api/actions/batches", TEST_DID, Some(&body)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{created}");
    assert_eq!(
        created["requested"], 2,
        "A and C; duplicate A collapsed, B active"
    );
    assert_eq!(created["skipped_active"], 1);
    let id = created["batch_id"].as_i64().expect("batch id");

    let (status, detail) = get(&app, &format!("/api/actions/batches/{id}"), TEST_DID).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["batch"]["kind"], "mute");
    assert_eq!(detail["batch"]["status"], "queued");
    assert_eq!(detail["batch"]["counts"], json!({ "pending": 2 }));
    assert_eq!(detail["batch"]["drifted"], false);
    let rows = detail["actions"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["target_did"], TARGET_A);
    assert_eq!(rows[0]["handle"], "a.test");
    assert_eq!(rows[0]["score_at_action"], 41.5);
    assert_eq!(rows[0]["tier_at_action"], "High");
    assert_eq!(rows[0]["current_tier"], "High");
    assert_eq!(rows[0]["drifted"], false);
    assert_eq!(rows[1]["target_did"], TARGET_C);

    // A drops to Watch after the batch was created.
    state
        .db
        .upsert_account_score(TEST_DID, &score_fixture(TARGET_A, "a.test", 9.0, "Watch"))
        .await
        .unwrap();
    let (_, detail) = get(&app, &format!("/api/actions/batches/{id}"), TEST_DID).await;
    assert_eq!(detail["actions"][0]["current_tier"], "Watch");
    assert_eq!(detail["actions"][0]["drifted"], true);
    assert_eq!(detail["batch"]["drifted"], true);

    // Listing: newest first, with counts.
    let (_, list) = get(&app, "/api/actions/batches", TEST_DID).await;
    let batches = list["batches"].as_array().unwrap();
    assert_eq!(batches[0]["id"], id);
    assert_eq!(batches[0]["counts"]["pending"], 2);

    // A different kind on an already-muted target is a new action.
    let (status, created) = post(
        &app,
        "/api/actions/batches",
        TEST_DID,
        Some(&valid_body("block", &[TARGET_B])),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(created["requested"], 1);
}

#[tokio::test]
async fn create_batch_with_only_active_targets_creates_nothing() {
    let (app, state) = app();
    seed_session(&state, TEST_DID, "https://pds.test").await;
    seed_scores(&*state.db, TEST_DID).await;
    seed_batch(
        &*state.db,
        TEST_DID,
        "mute",
        &[row(TARGET_A, "mute")],
        &[("skipped_already_done", None)],
    )
    .await;

    let (status, body) = post(
        &app,
        "/api/actions/batches",
        TEST_DID,
        Some(&valid_body("mute", &[TARGET_A])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["batch_id"].is_null());
    assert_eq!(body["requested"], 0);
    assert_eq!(body["skipped_active"], 1);
    assert_eq!(
        state
            .db
            .list_action_batches(TEST_DID, 10, 0)
            .await
            .unwrap()
            .len(),
        1,
        "no empty batch"
    );
}

// ---- ownership ----

#[tokio::test]
async fn batches_are_scoped_to_their_owner() {
    let (app, state) = app();
    seed_session(&state, TEST_DID, "https://pds.test").await;
    let (theirs, ids) = seed_batch(
        &*state.db,
        OTHER,
        "mute",
        &[row(TARGET_A, "mute")],
        &[("applied", None)],
    )
    .await;

    let (status, body) = get(&app, &format!("/api/actions/batches/{theirs}"), TEST_DID).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
    for u in [
        format!("/api/actions/batches/{theirs}/undo"),
        format!("/api/actions/batches/{theirs}/retry"),
        format!("/api/actions/{}/undo", ids[0]),
    ] {
        let (status, _) = post(&app, &u, TEST_DID, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{u}");
    }
    let (_, list) = get(&app, "/api/actions/batches", TEST_DID).await;
    assert_eq!(list["batches"], json!([]));
}

// ---- undo / retry ----

#[tokio::test]
async fn undo_batch_targets_only_rows_charcoal_applied() {
    let (app, state) = app();
    seed_session(&state, TEST_DID, "https://pds.test").await;
    let (orig, ids) = seed_batch(
        &*state.db,
        TEST_DID,
        "block",
        &[
            row(TARGET_A, "block"),
            row(TARGET_B, "block"),
            row(TARGET_C, "block"),
        ],
        &[
            (
                "applied",
                Some("at://did:plc:testalloweddid0000000000/app.bsky.graph.block/aaa"),
            ),
            ("failed", None),
            ("skipped_already_done", None),
        ],
    )
    .await;

    let (status, body) = post(
        &app,
        &format!("/api/actions/batches/{orig}/undo"),
        TEST_DID,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let undo_id = body["batch_id"].as_i64().unwrap();
    let batch = state.db.get_action_batch(undo_id).await.unwrap().unwrap();
    assert_eq!(batch.kind, "undo");
    assert_eq!(batch.source, format!("undo:batch:{orig}"));
    let rows = state.db.list_actions_for_batch(undo_id).await.unwrap();
    assert_eq!(
        rows.len(),
        1,
        "only the applied row: the failed row did nothing and the \
         skipped_already_done row is the user's own block (#261)"
    );
    assert_eq!(rows[0].target_did, TARGET_A);
    assert_eq!(rows[0].undo_of, Some(ids[0]));
    assert_eq!(rows[0].kind, "block");
    assert_eq!(rows[0].status, "pending");
    assert_eq!(
        rows[0].tier_at_action.as_deref(),
        Some("High"),
        "snapshot copied"
    );

    // An undo batch cannot itself be undone.
    let (status, body) = post(
        &app,
        &format!("/api/actions/batches/{undo_id}/undo"),
        TEST_DID,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "nothing_to_undo");
}

#[tokio::test]
async fn undo_single_action_needs_a_row_charcoal_applied() {
    let (app, state) = app();
    seed_session(&state, TEST_DID, "https://pds.test").await;
    let (_, ids) = seed_batch(
        &*state.db,
        TEST_DID,
        "mute",
        &[
            row(TARGET_A, "mute"),
            row(TARGET_B, "mute"),
            row(TARGET_C, "mute"),
        ],
        &[
            ("applied", None),
            ("failed", None),
            ("skipped_already_done", None),
        ],
    )
    .await;

    let (status, body) = post(
        &app,
        &format!("/api/actions/{}/undo", ids[0]),
        TEST_DID,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let undo_id = body["batch_id"].as_i64().unwrap();
    let batch = state.db.get_action_batch(undo_id).await.unwrap().unwrap();
    assert_eq!(batch.source, format!("undo:action:{}", ids[0]));
    let rows = state.db.list_actions_for_batch(undo_id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].undo_of, Some(ids[0]));

    let (status, body) = post(
        &app,
        &format!("/api/actions/{}/undo", ids[1]),
        TEST_DID,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "nothing_to_undo");

    // The mute the user made themselves: in force, but not Charcoal's to
    // remove (#261).
    let (status, body) = post(
        &app,
        &format!("/api/actions/{}/undo", ids[2]),
        TEST_DID,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "nothing_to_undo");

    let (status, _) = post(&app, "/api/actions/999/undo", TEST_DID, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A batch that only ever found things already done has nothing Charcoal may
/// undo — but those rows are still IN FORCE, so `/api/actions/active` must
/// keep listing them for the confirm sheet's greying (spec §5.1).
#[tokio::test]
async fn undo_batch_of_only_already_done_rows_is_refused_but_stays_active() {
    let (app, state) = app();
    seed_scores(&*state.db, TEST_DID).await;
    seed_session(&state, TEST_DID, "https://pds.test").await;
    let (orig, _) = seed_batch(
        &*state.db,
        TEST_DID,
        "mute",
        &[row(TARGET_A, "mute"), row(TARGET_B, "mute")],
        &[
            ("skipped_already_done", None),
            ("skipped_already_done", None),
        ],
    )
    .await;

    let (status, body) = post(
        &app,
        &format!("/api/actions/batches/{orig}/undo"),
        TEST_DID,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "nothing_to_undo");

    let (status, body) = get(&app, "/api/actions/active", TEST_DID).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["active"].as_array().map(Vec::len),
        Some(2),
        "already-done rows are in force even though they cannot be undone"
    );
}

#[tokio::test]
async fn undo_requires_a_session() {
    let (app, state) = app();
    let (orig, _) = seed_batch(
        &*state.db,
        TEST_DID,
        "mute",
        &[row(TARGET_A, "mute")],
        &[("applied", None)],
    )
    .await;
    let (status, body) = post(
        &app,
        &format!("/api/actions/batches/{orig}/undo"),
        TEST_DID,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "not_connected");
}

#[tokio::test]
async fn retry_creates_a_new_batch_over_failed_rows() {
    let (app, state) = app();
    seed_session(&state, TEST_DID, "https://pds.test").await;
    let (orig, _) = seed_batch(
        &*state.db,
        TEST_DID,
        "mute",
        &[row(TARGET_A, "mute"), row(TARGET_B, "mute")],
        &[("failed", None), ("applied", None)],
    )
    .await;
    state
        .db
        .set_action_batch_status(orig, "partial", None)
        .await
        .unwrap();

    let (status, body) = post(
        &app,
        &format!("/api/actions/batches/{orig}/retry"),
        TEST_DID,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let retry_id = body["batch_id"].as_i64().unwrap();
    assert_ne!(retry_id, orig);
    let batch = state.db.get_action_batch(retry_id).await.unwrap().unwrap();
    assert_eq!(batch.kind, "mute");
    assert_eq!(batch.source, format!("retry:{orig}"));
    assert_eq!(batch.status, "queued");
    let rows = state.db.list_actions_for_batch(retry_id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].target_did, TARGET_A);
    assert_eq!(rows[0].status, "pending");
    // The original is untouched.
    let orig_rows = state.db.list_actions_for_batch(orig).await.unwrap();
    assert_eq!(orig_rows[0].status, "failed");

    // A batch with nothing left to try — every row settled — is refused.
    let (settled, _) = seed_batch(
        &*state.db,
        TEST_DID,
        "mute",
        &[row(TARGET_C, "mute")],
        &[("applied", None)],
    )
    .await;
    state
        .db
        .set_action_batch_status(settled, "done", None)
        .await
        .unwrap();
    let (status, body) = post(
        &app,
        &format!("/api/actions/batches/{settled}/retry"),
        TEST_DID,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "nothing_to_retry");
}

/// A batch the runner gave up on before writing anything is stored `failed`
/// with every row still `pending`. Retry has to reach exactly those rows —
/// otherwise the one recoverable failure shape is a dead end (I2).
#[tokio::test]
async fn retry_reaches_a_failed_batch_whose_rows_never_ran() {
    let (app, state) = app();
    seed_session(&state, TEST_DID, "https://pds.test").await;
    let (orig, _) = seed_batch(
        &*state.db,
        TEST_DID,
        "block",
        &[row(TARGET_A, "block"), row(TARGET_B, "block")],
        &[("pending", None), ("pending", None)],
    )
    .await;
    state
        .db
        .set_action_batch_status(orig, "failed", Some("getBlocks: server error 503"))
        .await
        .unwrap();

    let (status, body) = post(
        &app,
        &format!("/api/actions/batches/{orig}/retry"),
        TEST_DID,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let retry_id = body["batch_id"].as_i64().unwrap();
    let batch = state.db.get_action_batch(retry_id).await.unwrap().unwrap();
    assert_eq!(batch.status, "queued");
    assert_eq!(batch.kind, "block");
    let rows = state.db.list_actions_for_batch(retry_id).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.status == "pending"));
}

/// Retry counts `pending` rows as work to redo, so a batch the runner has not
/// finished with must be refused — otherwise the API alone could clone a live
/// batch into a second one over the same targets.
#[tokio::test]
async fn retry_refuses_a_batch_that_is_still_running() {
    let (app, state) = app();
    seed_session(&state, TEST_DID, "https://pds.test").await;
    let (orig, _) = seed_batch(
        &*state.db,
        TEST_DID,
        "mute",
        &[row(TARGET_A, "mute")],
        &[("pending", None)],
    )
    .await;
    for status in ["queued", "running"] {
        state
            .db
            .set_action_batch_status(orig, status, None)
            .await
            .unwrap();
        let (code, body) = post(
            &app,
            &format!("/api/actions/batches/{orig}/retry"),
            TEST_DID,
            None,
        )
        .await;
        assert_eq!(code, StatusCode::CONFLICT, "{status}: {body}");
        assert_eq!(body["code"], "batch_running");
    }
    // No retry batch was created.
    let batches = state.db.list_action_batches(TEST_DID, 50, 0).await.unwrap();
    assert_eq!(batches.len(), 1);
}

// ---- account detail ----

#[tokio::test]
async fn account_actions_lists_active_forward_rows_for_that_target() {
    let (app, state) = app();
    seed_scores(&*state.db, TEST_DID).await;
    seed_batch(
        &*state.db,
        TEST_DID,
        "mute",
        &[row(TARGET_A, "mute")],
        &[("applied", None)],
    )
    .await;
    seed_batch(
        &*state.db,
        TEST_DID,
        "block",
        &[row(TARGET_A, "block"), row(TARGET_B, "block")],
        &[
            ("applied", Some("at://x/app.bsky.graph.block/1")),
            ("failed", None),
        ],
    )
    .await;

    let (status, body) = get(&app, "/api/accounts/a.test/actions", TEST_DID).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["did"], TARGET_A);
    let kinds: Vec<&str> = body["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["mute", "block"]);
    assert_eq!(
        body["actions"][1]["record_uri"],
        "at://x/app.bsky.graph.block/1"
    );

    let (_, body) = get(&app, "/api/accounts/b.test/actions", TEST_DID).await;
    assert_eq!(body["actions"], json!([]), "failed rows are not active");

    let (status, body) = get(&app, "/api/accounts/nobody.test/actions", TEST_DID).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn active_actions_lists_every_active_forward_row() {
    let (app, state) = app();
    seed_scores(&*state.db, TEST_DID).await;
    seed_batch(
        &*state.db,
        TEST_DID,
        "mute",
        &[row(TARGET_A, "mute")],
        &[("applied", None)],
    )
    .await;
    seed_batch(
        &*state.db,
        TEST_DID,
        "block",
        &[row(TARGET_A, "block"), row(TARGET_B, "block")],
        &[
            ("applied", Some("at://x/app.bsky.graph.block/1")),
            ("failed", None),
        ],
    )
    .await;
    seed_batch(
        &*state.db,
        TEST_DID,
        "mute",
        &[row(TARGET_B, "mute")],
        &[("skipped_already_done", None)],
    )
    .await;

    let (status, body) = get(&app, "/api/actions/active", TEST_DID).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut pairs: Vec<(String, String)> = body["active"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["did"].as_str().unwrap().to_string(),
                a["kind"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            (TARGET_A.to_string(), "block".to_string()),
            (TARGET_A.to_string(), "mute".to_string()),
            (TARGET_B.to_string(), "mute".to_string()),
        ],
        "failed rows are not active; skipped_already_done rows are"
    );

    // Another user sees nothing of this. (OTHER needs an access grant to pass
    // the auth gate at all — the endpoint under test isn't reachable through
    // `allowed_did`/admin like TEST_DID is, so this is setup, not the thing
    // being asserted.)
    state
        .db
        .grant_access(OTHER, "other.test", TEST_DID)
        .await
        .expect("grant access");
    let (status, body) = get(&app, "/api/actions/active", OTHER).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["active"], json!([]));
}

// ---- write-consent callback ----

/// Seed `state.pending_oauth[state_param]` as a WRITE-CONSENT round-trip for
/// `pending_did`, whose token exchange (against a fresh wiremock) returns
/// `sub: authenticated_did` with `granted_scope`. Mirrors
/// `seed_callback_state` in tests/web_oauth.rs.
async fn seed_write_consent(
    state: &AppState,
    state_param: &str,
    pending_did: &str,
    authenticated_did: &str,
    granted_scope: &str,
    return_to: &str,
) -> wiremock::MockServer {
    use atproto_identity::key::{generate_key, KeyType};
    use atproto_oauth::resources::AuthorizationServer;
    use atproto_oauth::workflow::OAuthRequest;
    use charcoal::web::handlers::oauth::{PendingOAuth, WriteConsent};
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "consent-access-token",
            "token_type": "DPoP",
            "refresh_token": "consent-refresh-token",
            "scope": granted_scope,
            "expires_in": 3600,
            "sub": authenticated_did,
        })))
        .mount(&mock)
        .await;

    let dpop_key = generate_key(KeyType::P256Private).expect("DPoP key");
    let now = chrono::Utc::now();
    let oauth_request = OAuthRequest {
        oauth_state: state_param.to_string(),
        issuer: "https://mock-authz.test".to_string(),
        authorization_server: "https://mock-authz.test".to_string(),
        nonce: "test-nonce".to_string(),
        pkce_verifier: "test-verifier".to_string(),
        signing_public_key: "unused-by-callback".to_string(),
        dpop_private_key: dpop_key.to_string(),
        created_at: now,
        expires_at: now + chrono::Duration::hours(1),
    };
    let authorization_server = AuthorizationServer {
        issuer: "https://mock-authz.test".to_string(),
        token_endpoint: format!("{}/token", mock.uri()),
        ..Default::default()
    };
    state.pending_oauth.write().await.insert(
        state_param.to_string(),
        PendingOAuth {
            oauth_request,
            authorization_server,
            handle: "me.test".to_string(),
            did: pending_did.to_string(),
            write_consent: Some(WriteConsent {
                pds_url: mock.uri(),
                return_to: return_to.to_string(),
            }),
        },
    );
    mock
}

const RETURN_TO: &str = "/accounts?tier=High&resume=mute";

#[tokio::test]
async fn write_consent_callback_stores_the_session_and_returns() {
    let (app, state) = app();
    let _mock = seed_write_consent(
        &state,
        "consent-ok",
        TEST_DID,
        TEST_DID,
        &charcoal::web::actions::scope::write_scope(),
        RETURN_TO,
    )
    .await;

    let (status, headers, _) = send(
        &app,
        "GET",
        "/api/auth/callback?code=abc&state=consent-ok",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers.get("location").unwrap(), RETURN_TO);
    assert!(
        headers.get("set-cookie").is_none(),
        "consent never issues a login cookie"
    );

    let session = state
        .db
        .get_oauth_session(TEST_DID)
        .await
        .unwrap()
        .expect("session stored");
    assert!(session.scope.contains("repo:app.bsky.graph.block"));
    assert!(
        state.pending_oauth.read().await.is_empty(),
        "state consumed"
    );
}

#[tokio::test]
async fn write_consent_callback_rejects_a_downgraded_scope() {
    let (app, state) = app();
    let _mock = seed_write_consent(
        &state,
        "consent-weak",
        TEST_DID,
        TEST_DID,
        "atproto",
        RETURN_TO,
    )
    .await;

    let (status, headers, _) = send(
        &app,
        "GET",
        "/api/auth/callback?code=abc&state=consent-weak",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers.get("location").unwrap(),
        "/accounts?tier=High&resume=mute&actions_error=invalid_scope"
    );
    assert!(
        state
            .db
            .get_oauth_session(TEST_DID)
            .await
            .unwrap()
            .is_none(),
        "no session row"
    );
}

#[tokio::test]
async fn write_consent_callback_rejects_a_different_account() {
    let (app, state) = app();
    let _mock = seed_write_consent(
        &state,
        "consent-mismatch",
        TEST_DID,
        OTHER,
        &charcoal::web::actions::scope::write_scope(),
        RETURN_TO,
    )
    .await;
    let (status, _, _) = send(
        &app,
        "GET",
        "/api/auth/callback?code=abc&state=consent-mismatch",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(state
        .db
        .get_oauth_session(TEST_DID)
        .await
        .unwrap()
        .is_none());
    assert!(state.db.get_oauth_session(OTHER).await.unwrap().is_none());
}

#[tokio::test]
async fn write_consent_error_returns_to_origin_with_a_code() {
    let (app, state) = app();
    let _mock = seed_write_consent(
        &state,
        "consent-denied",
        TEST_DID,
        TEST_DID,
        &charcoal::web::actions::scope::write_scope(),
        "/accounts/a.test?resume=block",
    )
    .await;

    let (status, headers, _) = send(
        &app,
        "GET",
        "/api/auth/callback?error=access_denied&error_description=nope&state=consent-denied",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers.get("location").unwrap(),
        "/accounts/a.test?resume=block&actions_error=denied"
    );
    // One-time use: the same state now fails as unknown.
    let (status, _, body) = send(
        &app,
        "GET",
        "/api/auth/callback?code=abc&state=consent-denied",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // A plain-login error (no pending write consent) keeps the JSON 400.
    let (status, _, body) = send(
        &app,
        "GET",
        "/api/auth/callback?error=access_denied&state=unknown",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("access_denied"));
}
