//! The #257 regression: a second user must be QUEUED, not refused.
//!
//! These drive the real HTTP handlers rather than the `Database` methods
//! underneath them. Asserting on `enqueue_scan` + `scan_queue_entry` directly
//! would pass just as happily against the old `any_running` gate, because that
//! gate lived in the handler — a test that never issues `POST /api/scan` cannot
//! observe the bug it claims to cover.
#![cfg(feature = "web")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use charcoal::web::auth::{create_token, COOKIE_NAME};
use charcoal::web::test_helpers::{build_open_test_app_with_db, TEST_SECRET};
use serde_json::Value;
use tower::ServiceExt;

const USER_A: &str = "did:plc:queuetestaaaaaaaaaaaaaaaa";
const USER_B: &str = "did:plc:queuetestbbbbbbbbbbbbbbbb";

/// Missing models are a broken test environment, not a reason to pass.
///
/// These used to `return` early when `build_open_test_app_with_db` found no
/// models, which reports `ok` having asserted nothing — the failure mode that
/// already let five tests on this branch claim guarantees they never
/// exercised. #257 is a launch blocker; a green run that never issued
/// `POST /api/scan` is worse than a red one. CI downloads the models and sets
/// `CHARCOAL_MODEL_DIR` (`.github/workflows/ci.yml`), so this only fires on a
/// local run that forgot to.
const MODELS_REQUIRED: &str = "ONNX models are required to build the test AppState. Run \
    `charcoal download-model`, then run the tests with \
    `CHARCOAL_MODEL_DIR=./models cargo test --features web` — test binaries do \
    not load .env. These #257 queue-admission tests must never silently pass.";

fn session_cookie(did: &str) -> String {
    format!("{}={}", COOKIE_NAME, create_token(TEST_SECRET, did))
}

async fn post_scan(app: &axum::Router, did: &str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/scan")
                .method("POST")
                .header("cookie", session_cookie(did))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn get_status(app: &axum::Router, did: &str) -> Value {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/status")
                .header("cookie", session_cookie(did))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("body reads");
    serde_json::from_slice(&bytes).expect("status body is JSON")
}

/// Before #257 this returned 409 "Another scan is already in progress on this
/// server". Under open signup, with scans that take 22 minutes to 2 hours, that
/// was the second user's entire experience of Charcoal.
#[tokio::test]
async fn a_second_user_is_queued_not_refused() {
    let (app, db) = build_open_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(USER_A, "a.bsky.social").await.expect("user");
    db.upsert_user(USER_B, "b.bsky.social").await.expect("user");

    let (status_a, body_a) = post_scan(&app, USER_A).await;
    assert_eq!(status_a, StatusCode::ACCEPTED);
    assert_eq!(body_a["status"], "queued");
    assert_eq!(body_a["position"], 1);

    let (status_b, body_b) = post_scan(&app, USER_B).await;
    assert_eq!(
        status_b,
        StatusCode::ACCEPTED,
        "the second user must be queued, not refused: {body_b}"
    );
    assert_eq!(body_b["status"], "queued");
    assert_eq!(
        body_b["position"], 2,
        "second user is position 2, not rejected"
    );
}

/// A double-click must not book a second scan or produce an error. `user_did`
/// is the queue's primary key, so the enqueue is a no-op and the caller just
/// learns where they already are.
///
/// Ordering is A, B, A-again — not A, A, B. With A the only row queued when B
/// arrives, B lands at position 2 whether or not the repeated A request reset
/// `enqueued_at`, so that ordering cannot tell an idempotent re-post from one
/// that silently re-queued A behind B. Posting A a second time AFTER B is
/// queued is the case that actually discriminates: if the repeat pushed A's
/// `enqueued_at` forward, A would move to position 2 and B to position 1.
#[tokio::test]
async fn a_repeated_request_is_idempotent() {
    let (app, db) = build_open_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(USER_A, "a.bsky.social").await.expect("user");
    db.upsert_user(USER_B, "b.bsky.social").await.expect("user");

    let (_, first) = post_scan(&app, USER_A).await;
    let (_, b) = post_scan(&app, USER_B).await;
    let (status, second) = post_scan(&app, USER_A).await;

    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(first["position"], second["position"]);

    // A is still position 1 and B is still position 2 — the double-click did
    // not push A back behind B's row.
    assert_eq!(
        second["position"], 1,
        "A must stay at the front of the queue"
    );
    assert_eq!(b["position"], 2, "B must not be displaced by A's repeat");
}

/// GET /api/status must report the wait, since that is all a queued user has
/// to look at. The block is absent for everyone else, so existing clients see
/// no change.
#[tokio::test]
async fn status_reports_the_queue_position_only_while_queued() {
    let (app, db) = build_open_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(USER_A, "a.bsky.social").await.expect("user");
    db.upsert_user(USER_B, "b.bsky.social").await.expect("user");

    // Never enqueued: no queue block at all.
    let idle = get_status(&app, USER_A).await;
    assert!(idle.get("queue").is_none(), "{idle}");
    assert_eq!(idle["scan_running"], false);

    post_scan(&app, USER_A).await;
    post_scan(&app, USER_B).await;

    let queued = get_status(&app, USER_B).await;
    assert_eq!(queued["phase"], "queued");
    assert_eq!(
        queued["scan_running"], true,
        "a queued user is waiting on a scan — the dashboard must keep polling"
    );
    assert_eq!(queued["queue"]["position"], 2);
    assert!(queued["queue"]["enqueued_at"].is_string());

    // No admitter runs behind a test AppState, so drive the transition by hand:
    // once the row is claimed the user is running, not queued, and the block
    // must disappear rather than report a stale position.
    let claim = db
        .claim_next_scan(2, 120)
        .await
        .expect("claim")
        .expect("a queued row exists");
    assert_eq!(claim.user_did, USER_A);

    let running = get_status(&app, USER_A).await;
    assert!(
        running.get("queue").is_none(),
        "a running scan has no queue position: {running}"
    );
    assert_eq!(running["scan_running"], true);
}

/// The queue row is the authority on whether a scan is live (#257/#274). A
/// process that never registered the scan in memory — a restart, or another
/// replica — must still report it as running rather than idle.
#[tokio::test]
async fn status_reports_a_running_row_even_with_no_in_memory_scan() {
    let (app, db) = build_open_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(USER_A, "a.bsky.social").await.expect("user");

    post_scan(&app, USER_A).await;
    let claim = db
        .claim_next_scan(2, 120)
        .await
        .expect("claim")
        .expect("row");

    let json = get_status(&app, USER_A).await;
    assert_eq!(json["scan_running"], true);
    assert_eq!(
        json["phase"], "starting",
        "a running row with no in-memory entry must not read as idle"
    );

    // And once the row reaches a terminal state, the scan is over — even though
    // nothing in this process ever wrote a status entry.
    assert!(db
        .finish_queued_scan(USER_A, &claim.claim_id, None)
        .await
        .expect("finish"));
    let json = get_status(&app, USER_A).await;
    assert_eq!(json["scan_running"], false);
    assert_eq!(json["phase"], "idle");
}
