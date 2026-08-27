//! The three-clause access gate (#309): env bootstrap OR admin OR DB row.
#![cfg(feature = "web")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use charcoal::web::auth::{create_token, COOKIE_NAME};
use charcoal::web::test_helpers::{build_test_app_with_db, TEST_DID, TEST_SECRET};
use serde_json::Value;
use tower::ServiceExt;

const MODELS_REQUIRED: &str = "ONNX models required — run with CHARCOAL_MODEL_DIR=./models";
const OUTSIDER: &str = "did:plc:outsider0000000000000000";

fn session_cookie(did: &str) -> String {
    format!("{}={}", COOKIE_NAME, create_token(TEST_SECRET, did))
}

async fn get_me(app: &axum::Router, did: &str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/me")
                .header("cookie", session_cookie(did))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

// build_test_app_with_db pins CHARCOAL_ALLOWED_DID to TEST_DID, so the env
// gate is ACTIVE in these tests and OUTSIDER is not on it.

#[tokio::test]
async fn env_member_still_passes_with_empty_table() {
    let (app, _db) = build_test_app_with_db().expect(MODELS_REQUIRED);
    let (status, _) = get_me(&app, TEST_DID).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn outsider_with_valid_cookie_gets_access_revoked_403() {
    let (app, _db) = build_test_app_with_db().expect(MODELS_REQUIRED);
    let (status, body) = get_me(&app, OUTSIDER).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["code"], "access_revoked",
        "machine-readable code: {body}"
    );
}

#[tokio::test]
async fn allowed_db_row_passes_the_env_gate() {
    let (app, db) = build_test_app_with_db().expect(MODELS_REQUIRED);
    db.grant_access(OUTSIDER, "outsider.bsky.social", "did:plc:admin")
        .await
        .expect("grant");
    let (status, _) = get_me(&app, OUTSIDER).await;
    assert_eq!(status, StatusCode::OK, "allowed row must pass");
}

#[tokio::test]
async fn denied_and_pending_rows_do_not_pass() {
    let (app, db) = build_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_access_request_pending(OUTSIDER, "outsider.bsky.social")
        .await
        .expect("pending");
    let (status, _) = get_me(&app, OUTSIDER).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "pending is not allowed");

    db.set_access_status(OUTSIDER, "denied", "did:plc:admin")
        .await
        .expect("deny");
    let (status, _) = get_me(&app, OUTSIDER).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "denied is not allowed");
}
