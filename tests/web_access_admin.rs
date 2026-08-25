//! Admin allowlist endpoints (#309): list, approve, deny.
#![cfg(feature = "web")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use charcoal::web::auth::{create_token, COOKIE_NAME};
use charcoal::web::test_helpers::{build_admin_test_app_with_db, TEST_DID, TEST_SECRET};
use serde_json::Value;
use tower::ServiceExt;

const MODELS_REQUIRED: &str = "ONNX models required — run with CHARCOAL_MODEL_DIR=./models";
const WAITER: &str = "did:plc:waiter000000000000000000";
const NON_ADMIN: &str = "did:plc:regular00000000000000000";

fn session_cookie(did: &str) -> String {
    format!("{}={}", COOKIE_NAME, create_token(TEST_SECRET, did))
}

async fn call(app: &axum::Router, method: &str, uri: &str, did: &str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .method(method)
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

#[tokio::test]
async fn non_admin_gets_403_on_every_access_endpoint() {
    let (app, db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(NON_ADMIN, "r.bsky.social")
        .await
        .expect("user");
    for (m, u) in [
        ("GET", "/api/admin/access"),
        ("POST", "/api/admin/access/did:plc:x/approve"),
        ("POST", "/api/admin/access/did:plc:x/deny"),
    ] {
        let (status, _) = call(&app, m, u, NON_ADMIN).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{m} {u}");
    }
}

#[tokio::test]
async fn list_groups_rows_by_status() {
    let (app, db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_access_request_pending(WAITER, "w.bsky.social")
        .await
        .expect("row");
    db.grant_access(
        "did:plc:granted00000000000000000",
        "g.bsky.social",
        TEST_DID,
    )
    .await
    .expect("row");
    let (status, body) = call(&app, "GET", "/api/admin/access", TEST_DID).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pending"][0]["did"], WAITER);
    assert_eq!(body["allowed"][0]["handle"], "g.bsky.social");
    assert!(body["denied"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn approve_deny_flip_status_and_404_without_a_row() {
    let (app, db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_access_request_pending(WAITER, "w.bsky.social")
        .await
        .expect("row");

    let uri = format!("/api/admin/access/{WAITER}/approve");
    let (status, body) = call(&app, "POST", &uri, TEST_DID).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "allowed");
    assert_eq!(
        db.get_access_request(WAITER).await.unwrap().unwrap().status,
        "allowed"
    );

    let uri = format!("/api/admin/access/{WAITER}/deny");
    let (status, _) = call(&app, "POST", &uri, TEST_DID).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        db.get_access_request(WAITER).await.unwrap().unwrap().status,
        "denied"
    );

    let (status, _) = call(
        &app,
        "POST",
        "/api/admin/access/did:plc:norow/approve",
        TEST_DID,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
