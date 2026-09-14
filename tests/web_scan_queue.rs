//! The #257 regression: a second user must be QUEUED, not refused.
//!
//! These drive the real HTTP handlers rather than the `Database` methods
//! underneath them. Asserting on `enqueue_scan` + `scan_queue_entry` directly
//! would pass just as happily against the old `any_running` gate, because that
//! gate lived in the handler — a test that never issues `POST /api/scan` cannot
//! observe the bug it claims to cover.
#![cfg(feature = "web")]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use charcoal::db::{Database, FinishCompletion, ScanKind};
use charcoal::web::admitter::{LiveScans, LEASE_SECS};
use charcoal::web::auth::{create_token, COOKIE_NAME};
use charcoal::web::scan_job::{
    record_full_scan_completion, run_under_slot, QueueSlot, ScanCompletion, ScanManager,
    ScanReport, SlotExit,
};
use charcoal::web::test_helpers::{build_open_test_app_with_db, TEST_SECRET};
use serde_json::Value;
use tokio::sync::RwLock;
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

/// Drive `run_under_slot` with a scan future that reports `completion` (and
/// writes the marker exactly as `run_scan` would), then POST /api/scan.
///
/// Composed rather than helper-level on purpose (V3-02): the defect this
/// guards against — a cooldown anchored on the queue row's `done` status
/// rather than on the completion marker — lives in the seam between the slot
/// lifecycle and the handler. A test that pokes `finish_queued_scan` and then
/// reads the row cannot see it.
async fn finish_then_post(
    app: &axum::Router,
    db: &Arc<dyn Database>,
    did: &str,
    completion: ScanCompletion,
) -> StatusCode {
    db.enqueue_scan(did).await.expect("enqueue");
    let claim = db
        .claim_next_scan(1, LEASE_SECS)
        .await
        .expect("claim")
        .expect("claimed");
    let mgr = Arc::new(RwLock::new({
        let mut m = ScanManager::new();
        m.begin_admitted_scan(did, &claim.claim_id);
        m
    }));
    let slot = QueueSlot {
        claim_id: claim.claim_id.clone(),
        wake: tokio::sync::mpsc::channel(1).0,
    };
    let live = LiveScans::new().try_register(did).expect("fresh registry");
    let scan_db = db.clone();
    let scan_did = did.to_string();
    let scan_claim = claim.claim_id.clone();
    let exit = run_under_slot(
        async move {
            record_full_scan_completion(scan_db.as_ref(), &scan_did, &scan_claim, completion).await;
            Ok(ScanReport {
                completion: completion.into(),
            })
        },
        db.clone(),
        mgr,
        did.to_string(),
        slot,
        live,
        Duration::from_millis(10),
    )
    .await;
    assert_eq!(exit, SlotExit::Completed);
    post_scan(app, did).await.0
}

/// #258/#309: a successful scan starts a per-user cooldown window. The 429
/// carries a `retry_at` so the client can tell the caller when to come back.
#[tokio::test]
async fn a_completed_scan_starts_the_cooldown() {
    let (app, db) = build_open_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(USER_A, "a.bsky.social").await.expect("user");
    assert_eq!(
        finish_then_post(&app, &db, USER_A, ScanCompletion::Complete).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    let (_, body) = post_scan(&app, USER_A).await;
    assert!(body["retry_at"].is_string(), "retry_at present: {body}");
}

/// A failed scan is not a "successful scan" for cooldown purposes — the user
/// should be able to retry immediately.
#[tokio::test]
async fn a_failed_scan_does_not_start_the_cooldown() {
    let (app, db) = build_open_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(USER_A, "a.bsky.social").await.expect("user");
    db.enqueue_scan(USER_A).await.expect("enqueue");
    let claim = db
        .claim_next_scan(1, 600)
        .await
        .expect("claim")
        .expect("claimed");
    db.finish_queued_scan(
        USER_A,
        &claim.claim_id,
        FinishCompletion::Failed,
        Some("boom"),
    )
    .await
    .expect("finish");

    let (status, _) = post_scan(&app, USER_A).await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "failed scans may retry immediately"
    );
}

/// V3-02: an interrupted full scan finishes `done` like any other, so a
/// cooldown keyed on the row's status would lock the user out of the retry
/// that resumes their own staging. The marker is the only anchor.
#[tokio::test]
async fn an_interrupted_full_scan_does_not_start_a_cooldown() {
    let (app, db) = build_open_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(USER_A, "a.bsky.social").await.expect("user");
    assert_eq!(
        finish_then_post(&app, &db, USER_A, ScanCompletion::Resumable).await,
        StatusCode::ACCEPTED,
        "resume immediately"
    );
    let row = db
        .list_scan_queue()
        .await
        .expect("queue")
        .into_iter()
        .find(|r| r.user_did == USER_A)
        .expect("row");
    assert_eq!(row.status, "queued", "the retry click was accepted");
    assert!(db
        .get_scan_state(USER_A, "last_full_scan_finished_at")
        .await
        .expect("marker read")
        .is_none());
}

/// V6-01: a scan that finished with skipped accounts still CARRIED OUT the
/// user's request — the gaps are per-account, and the cooldown applies.
#[tokio::test]
async fn a_scan_completed_with_skips_starts_the_cooldown() {
    let (app, db) = build_open_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(USER_A, "a.bsky.social").await.expect("user");
    assert_eq!(
        finish_then_post(
            &app,
            &db,
            USER_A,
            ScanCompletion::CompleteWithSkips { n: 2 }
        )
        .await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

/// R13: a nightly refresh reuses the user's one queue row. It must neither
/// start a cooldown nor move the one a full scan started.
#[tokio::test]
async fn a_completed_full_scan_enforces_cooldown_and_a_refresh_does_not_reset_it() {
    let (app, db) = build_open_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(USER_A, "a.bsky.social").await.expect("user");
    assert_eq!(
        finish_then_post(&app, &db, USER_A, ScanCompletion::Complete).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    let marker = db
        .get_scan_state(USER_A, "last_full_scan_finished_at")
        .await
        .expect("marker read")
        .expect("marker written");

    // The 429'd click enqueued nothing, so the helper's row is already `done`
    // and the obligation it carried was cleared by the fulfilled completion —
    // which is why the refresh enqueue below stays a refresh rather than being
    // re-queued as owed full work.
    let row = db
        .list_scan_queue()
        .await
        .expect("queue")
        .into_iter()
        .find(|r| r.user_did == USER_A)
        .expect("row");
    assert_eq!(
        (row.status.as_str(), row.full_requested_at.as_deref()),
        ("done", None)
    );

    // A refresh runs and finishes; the marker and the 429 are unchanged.
    db.enqueue_refresh_scan(USER_A).await.expect("refresh");
    let claim = db
        .claim_next_scan(1, LEASE_SECS)
        .await
        .expect("claim")
        .expect("claimed");
    assert_eq!(claim.kind, ScanKind::Refresh);
    db.finish_queued_scan(USER_A, &claim.claim_id, FinishCompletion::Complete, None)
        .await
        .expect("finish");
    assert_eq!(
        db.get_scan_state(USER_A, "last_full_scan_finished_at")
            .await
            .expect("marker read")
            .expect("still there"),
        marker,
        "a refresh never touches the full-scan cooldown anchor"
    );
    assert_eq!(
        post_scan(&app, USER_A).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

/// R09: the 202 body says which of the three things actually happened, so the
/// dashboard can tell "your scan starts when the refresh finishes" from
/// "you're in line".
#[tokio::test]
async fn the_202_body_names_what_happened_to_the_request() {
    let (app, db) = build_open_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(USER_A, "a.bsky.social").await.expect("user");

    let (status, body) = post_scan(&app, USER_A).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["queued"], "now", "{body}");

    // A full scan of their own is running: still queued, already running.
    let claim = db
        .claim_next_scan(1, LEASE_SECS)
        .await
        .expect("claim")
        .expect("claimed");
    let (_, body) = post_scan(&app, USER_A).await;
    assert_eq!(body["queued"], "already_running", "{body}");
    db.finish_queued_scan(USER_A, &claim.claim_id, FinishCompletion::Resumable, None)
        .await
        .expect("finish");

    // A REFRESH is running: the request is recorded and honoured afterwards.
    db.enqueue_refresh_scan(USER_B).await.expect("refresh");
    db.upsert_user(USER_B, "b.bsky.social").await.expect("user");
    let claim = db
        .claim_next_scan(2, LEASE_SECS)
        .await
        .expect("claim")
        .expect("claimed");
    assert_eq!(
        (claim.user_did.as_str(), claim.kind),
        (USER_B, ScanKind::Refresh)
    );
    let (status, body) = post_scan(&app, USER_B).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["queued"], "after_refresh", "{body}");
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
        .finish_queued_scan(USER_A, &claim.claim_id, FinishCompletion::Complete, None)
        .await
        .expect("finish"));
    let json = get_status(&app, USER_A).await;
    assert_eq!(json["scan_running"], false);
    assert_eq!(json["phase"], "idle");
}
