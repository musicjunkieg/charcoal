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

async fn post_json(app: &axum::Router, uri: &str, did: &str, body: &str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .method("POST")
                .header("cookie", session_cookie(did))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
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
    // grant_access_by_handle takes a Json body, so the extractor runs before
    // admin_guard — a bodyless request (what `call` sends) 415s before the
    // handler ever checks admin status. Exercise the 403 path with a
    // well-formed body via post_json instead (deviation from the brief,
    // which assumed `call` would work for every route in this loop).
    let (status, _) = post_json(
        &app,
        "/api/admin/access",
        NON_ADMIN,
        r#"{"handle": "someone.bsky.social"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "POST /api/admin/access");
}

#[tokio::test]
async fn grant_by_handle_validates_input() {
    let (app, _db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    let (status, _) = post_json(&app, "/api/admin/access", TEST_DID, r#"{"handle": "  "}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
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

#[tokio::test]
async fn approve_scan_grants_seeds_and_queues() {
    let (app, db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_access_request_pending(WAITER, "w.bsky.social")
        .await
        .expect("row");

    let uri = format!("/api/admin/access/{WAITER}/approve-scan");
    let (status, body) = call(&app, "POST", &uri, TEST_DID).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["access"], "granted");
    assert_eq!(body["scan"], "queued");
    assert_eq!(
        db.get_access_request(WAITER).await.unwrap().unwrap().status,
        "allowed"
    );
    assert_eq!(
        db.get_user_handle(WAITER).await.unwrap().as_deref(),
        Some("w.bsky.social"),
        "user row pre-seeded from the access row's handle"
    );
    let queued = db
        .list_scan_queue()
        .await
        .unwrap()
        .into_iter()
        .any(|r| r.user_did == WAITER && r.status == "queued");
    assert!(queued, "scan enqueued");
}

#[tokio::test]
async fn approve_scan_404s_without_a_row() {
    let (app, _db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    let (status, _) = call(
        &app,
        "POST",
        "/api/admin/access/did:plc:norow/approve-scan",
        TEST_DID,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
