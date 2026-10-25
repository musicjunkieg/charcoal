//! Admin cost endpoint (#393): GET /api/admin/costs.
#![cfg(feature = "web")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use charcoal::web::auth::{create_token, COOKIE_NAME};
use charcoal::web::test_helpers::{build_admin_test_app_with_db, TEST_DID, TEST_SECRET};
use serde_json::Value;
use tower::ServiceExt;

const MODELS_REQUIRED: &str = "ONNX models required — run with CHARCOAL_MODEL_DIR=./models";

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
async fn non_admin_gets_403() {
    let (app, _db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    let (status, _) = call(
        &app,
        "GET",
        "/api/admin/costs",
        "did:plc:notanadmin000000000000",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn reports_per_user_totals_windows_and_the_onboarding_flag() {
    let (app, db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    let now = chrono::Utc::now();
    let at = |days: i64| {
        (now - chrono::Duration::days(days)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    };
    let row = |did: &str, kind: &str, started: String, cents: i64| charcoal::db::ScanCostRow {
        user_did: did.into(),
        kind: kind.into(),
        outcome: "completed".into(),
        finished_at: started.clone(),
        started_at: started,
        classifications: 1,
        classifier_cache_hits: 0,
        metered_worker_secs: 1.0,
        peak_in_flight: 1,
        est_cents: cents,
        rate_cents_per_hour: 329,
    };
    db.insert_scan_cost(&row("did:plc:big", "full", at(20), 250))
        .await
        .unwrap();
    // Two full-scan rows (a capped chunk and its resume), neither over $2.00
    // alone: the flag judges the full-scan cost to date, so this one is over.
    db.insert_scan_cost(&row("did:plc:split", "full", at(40), 120))
        .await
        .unwrap();
    db.insert_scan_cost(&row("did:plc:split", "full", at(39), 120))
        .await
        .unwrap();
    db.insert_scan_cost(&row("did:plc:small", "full", at(1), 90))
        .await
        .unwrap();
    db.insert_scan_cost(&row("did:plc:small", "refresh", at(1), 4))
        .await
        .unwrap();

    let (status, body) = call(&app, "GET", "/api/admin/costs", TEST_DID).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["last_7d_cents"], 94);
    assert_eq!(body["last_30d_cents"], 344);
    assert_eq!(body["onboarding_target_cents"], 200);
    let user = |did: &str| -> Value {
        body["users"]
            .as_array()
            .expect("users array")
            .iter()
            .find(|u| u["user_did"] == did)
            .unwrap_or_else(|| panic!("{did} listed"))
            .clone()
    };
    assert_eq!(body["users"][0]["user_did"], "did:plc:big");
    assert_eq!(user("did:plc:big")["full_scan_cents"], 250);
    assert_eq!(user("did:plc:big")["over_onboarding_target"], true);
    assert_eq!(user("did:plc:split")["full_scan_cents"], 240);
    assert_eq!(user("did:plc:split")["over_onboarding_target"], true);
    assert_eq!(user("did:plc:small")["full_scan_cents"], 90);
    assert_eq!(user("did:plc:small")["over_onboarding_target"], false);
    assert!(
        user("did:plc:small").get("first_full_scan_cents").is_none(),
        "the old field name is gone"
    );
}
