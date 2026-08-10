//! #288: the admin dashboard must read scans from the durable `scan_queue`.
//!
//! These drive `GET /api/admin/users` rather than `Database::list_scan_queue`
//! underneath it. Asserting on the DB method alone would pass just as happily
//! against the old handler, because the bug lived in the handler: it built its
//! only scan column from the process-local `ScanManager`, so an
//! admin-triggered scan — which since #257 is *enqueued*, not launched — was
//! invisible until some worker in *this* process picked it up.
#![cfg(feature = "web")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use charcoal::web::auth::{create_token, COOKIE_NAME};
use charcoal::web::test_helpers::{build_admin_test_app_with_db, TEST_DID, TEST_SECRET};
use serde_json::Value;
use tower::ServiceExt;

const OTHER_USER: &str = "did:plc:admintest288otheruser";

/// Missing models are a broken test environment, not a reason to pass. See the
/// same constant in tests/web_scan_queue.rs.
const MODELS_REQUIRED: &str = "ONNX models are required to build the test AppState. Run \
    `charcoal download-model`, then run the tests with \
    `CHARCOAL_MODEL_DIR=./models cargo test --features web` — test binaries do \
    not load .env. These #288 admin-visibility tests must never silently pass.";

async fn get_admin_users(app: &axum::Router) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/users")
                .header(
                    "cookie",
                    format!("{}={}", COOKIE_NAME, create_token(TEST_SECRET, TEST_DID)),
                )
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("body reads");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn user_row<'a>(body: &'a Value, did: &str) -> &'a Value {
    body["users"]
        .as_array()
        .expect("users is an array")
        .iter()
        .find(|u| u["did"] == did)
        .unwrap_or_else(|| panic!("{did} must appear in the user list"))
}

/// A QUEUED scan is the state the old dashboard could not see at all: nothing
/// has started, so `ScanManager` has no status and the row rendered blank.
#[tokio::test]
async fn queued_scans_are_visible_to_the_admin_dashboard() {
    let (app, db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(TEST_DID, "admin.bsky.social").await.unwrap();
    db.upsert_user(OTHER_USER, "other.bsky.social")
        .await
        .unwrap();
    db.enqueue_scan(OTHER_USER).await.unwrap();

    let (status, body) = get_admin_users(&app).await;
    assert_eq!(status, StatusCode::OK);

    let row = user_row(&body, OTHER_USER);
    assert_eq!(
        row["scan"]["status"], "queued",
        "a queued scan must be reported; the ScanManager-derived view showed nothing"
    );
    assert_eq!(row["scan"]["position"], 1);
    assert!(
        row["scan"]["enqueued_at"].is_string(),
        "the dashboard needs to say how long it has been waiting"
    );
    assert!(row["scan"]["started_at"].is_null());
    assert_eq!(
        row["last_scan_at"],
        Value::Null,
        "queued is not started — last_scan_at must stay null"
    );

    // A user with no queue row at all is distinguishable from a queued one.
    assert_eq!(
        user_row(&body, TEST_DID)["scan"],
        Value::Null,
        "a user who has never been enqueued has no scan object"
    );

    // The queue panel sees the same row, and the cap it is measured against.
    let queue = &body["queue"];
    assert_eq!(queue["queued"], 1);
    assert_eq!(queue["running"], 0);
    assert!(
        queue["concurrency_limit"].as_u64().unwrap_or(0) >= 1,
        "the panel is meaningless without the cap the depth is measured against"
    );
    let active = queue["active"].as_array().expect("active is an array");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["user_did"], OTHER_USER);
    assert_eq!(
        active[0]["handle"], "other.bsky.social",
        "the panel lists DIDs no human can read without the handle"
    );
}

/// The failure case is the other half of #288: "the last scan failed" and
/// "this user has never scanned" rendered identically before.
#[tokio::test]
async fn failed_scans_report_their_error_and_leave_the_active_list() {
    let (app, db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(OTHER_USER, "other.bsky.social")
        .await
        .unwrap();
    db.enqueue_scan(OTHER_USER).await.unwrap();
    let claim = db.claim_next_scan(1, 120).await.unwrap().expect("claim");

    // Running first: the row is active and last_scan_at is now real.
    let (_, body) = get_admin_users(&app).await;
    let row = user_row(&body, OTHER_USER);
    assert_eq!(row["scan"]["status"], "running");
    assert_eq!(row["scan"]["position"], 0, "a running row holds a slot");
    let started_at = row["scan"]["started_at"]
        .as_str()
        .expect("a running scan has started")
        .to_string();
    assert_eq!(
        row["last_scan_at"], started_at,
        "last_scan_at is the durable start of the most recent scan"
    );
    assert_eq!(body["queue"]["running"], 1);
    assert_eq!(body["queue"]["active"].as_array().unwrap().len(), 1);

    db.finish_queued_scan(OTHER_USER, &claim.claim_id, Some("gather exploded"))
        .await
        .unwrap();

    let (_, body) = get_admin_users(&app).await;
    let row = user_row(&body, OTHER_USER);
    assert_eq!(row["scan"]["status"], "failed");
    assert_eq!(row["scan"]["last_error"], "gather exploded");
    assert!(row["scan"]["finished_at"].is_string());
    assert_eq!(
        row["last_scan_at"], started_at,
        "a failed scan still started, so last_scan_at survives the failure"
    );
    assert_eq!(
        body["queue"]["active"].as_array().unwrap().len(),
        0,
        "a finished row is history, not queue — it must leave the active panel"
    );
    assert_eq!(body["queue"]["running"], 0);
    assert_eq!(body["queue"]["queued"], 0);
}

/// `fingerprint_building` is genuinely process-local — it tracks a
/// `tokio::spawn` this process owns, with no durable row anywhere. #288 moves
/// the SCAN fields off `ScanManager`; this one has to stay.
#[tokio::test]
async fn fingerprint_building_still_comes_from_the_scan_manager() {
    let (app, db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(OTHER_USER, "other.bsky.social")
        .await
        .unwrap();

    let (_, body) = get_admin_users(&app).await;
    assert_eq!(
        user_row(&body, OTHER_USER)["fingerprint_building"],
        false,
        "the key must survive the move to scan_queue, defaulting to false"
    );
}
