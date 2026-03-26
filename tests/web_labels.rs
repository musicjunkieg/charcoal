//! Integration tests for label API endpoints.
//! Run: cargo test --features web --test web_labels

#[cfg(feature = "web")]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use tower::ServiceExt;

    use charcoal::db::models::AccountScore;
    use charcoal::web::auth::{create_token, COOKIE_NAME};
    use charcoal::web::test_helpers::{build_test_app_with_db, TEST_DID, TEST_SECRET};

    fn session_cookie(did: &str) -> String {
        format!("{}={}", COOKIE_NAME, create_token(TEST_SECRET, did))
    }

    async fn seed_account(
        db: &std::sync::Arc<dyn charcoal::db::Database>,
        did: &str,
        handle: &str,
        score: f64,
        tier: &str,
    ) {
        let account = AccountScore {
            did: did.to_string(),
            handle: handle.to_string(),
            toxicity_score: Some(0.5),
            topic_overlap: Some(0.3),
            threat_score: Some(score),
            threat_tier: Some(tier.to_string()),
            posts_analyzed: 10,
            top_toxic_posts: vec![],
            scored_at: "2026-03-19T12:00:00Z".to_string(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
        };
        db.upsert_account_score(TEST_DID, &account).await.unwrap();
    }

    // ---- POST /api/accounts/{did}/label ----

    #[tokio::test]
    async fn label_account_returns_200() {
        let (app, db) = build_test_app_with_db();
        db.upsert_user(TEST_DID, "test.bsky.social").await.unwrap();
        seed_account(&db, "did:plc:target1", "target1.bsky.social", 40.0, "High").await;

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/accounts/did:plc:target1/label")
                    .header("cookie", session_cookie(TEST_DID))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"label": "high", "notes": "known troll"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);

        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["label"], "high");
        assert_eq!(json["notes"], "known troll");
        assert_eq!(json["target_did"], "did:plc:target1");
    }

    #[tokio::test]
    async fn label_account_updates_existing() {
        let (app, db) = build_test_app_with_db();
        db.upsert_user(TEST_DID, "test.bsky.social").await.unwrap();
        seed_account(&db, "did:plc:target1", "target1.bsky.social", 40.0, "High").await;

        // First label
        db.upsert_user_label(TEST_DID, "did:plc:target1", "high", None)
            .await
            .unwrap();

        // Update label
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/accounts/did:plc:target1/label")
                    .header("cookie", session_cookie(TEST_DID))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"label": "safe", "notes": "actually a friend"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);

        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["label"], "safe");
        assert_eq!(json["notes"], "actually a friend");
    }

    #[tokio::test]
    async fn label_rejects_invalid_tier() {
        let (app, db) = build_test_app_with_db();
        db.upsert_user(TEST_DID, "test.bsky.social").await.unwrap();

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/accounts/did:plc:target1/label")
                    .header("cookie", session_cookie(TEST_DID))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"label": "INVALID"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn label_requires_auth() {
        let (app, _db) = build_test_app_with_db();

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/accounts/did:plc:target1/label")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"label": "high"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    // ---- GET /api/review ----

    #[tokio::test]
    async fn review_queue_returns_unlabeled() {
        let (app, db) = build_test_app_with_db();
        db.upsert_user(TEST_DID, "test.bsky.social").await.unwrap();

        // Seed two accounts, label one
        seed_account(&db, "did:plc:a", "a.bsky.social", 40.0, "High").await;
        seed_account(&db, "did:plc:b", "b.bsky.social", 20.0, "Elevated").await;
        db.upsert_user_label(TEST_DID, "did:plc:a", "high", None)
            .await
            .unwrap();

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/review?limit=10")
                    .header("cookie", session_cookie(TEST_DID))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);

        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let accounts = json["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0]["handle"], "b.bsky.social");
    }

    #[tokio::test]
    async fn review_queue_sorted_by_threat_score_desc() {
        let (app, db) = build_test_app_with_db();
        db.upsert_user(TEST_DID, "test.bsky.social").await.unwrap();

        seed_account(&db, "did:plc:low", "low.bsky.social", 5.0, "Low").await;
        seed_account(&db, "did:plc:high", "high.bsky.social", 40.0, "High").await;
        seed_account(&db, "did:plc:mid", "mid.bsky.social", 20.0, "Elevated").await;

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/review?limit=10")
                    .header("cookie", session_cookie(TEST_DID))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);

        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let accounts = json["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 3);
        assert_eq!(accounts[0]["handle"], "high.bsky.social");
        assert_eq!(accounts[1]["handle"], "mid.bsky.social");
        assert_eq!(accounts[2]["handle"], "low.bsky.social");
    }

    // ---- GET /api/accuracy ----

    #[tokio::test]
    async fn accuracy_returns_metrics() {
        let (app, db) = build_test_app_with_db();
        db.upsert_user(TEST_DID, "test.bsky.social").await.unwrap();

        // Seed accounts and label them
        seed_account(&db, "did:plc:a", "a.bsky.social", 40.0, "High").await;
        seed_account(&db, "did:plc:b", "b.bsky.social", 20.0, "Elevated").await;
        db.upsert_user_label(TEST_DID, "did:plc:a", "high", None)
            .await
            .unwrap();
        db.upsert_user_label(TEST_DID, "did:plc:b", "safe", None)
            .await
            .unwrap();

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/accuracy")
                    .header("cookie", session_cookie(TEST_DID))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);

        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["total_labeled"], 2);
        assert!(json["accuracy"].is_number());
    }

    // ---- GET /api/accounts/{handle} includes label ----

    #[tokio::test]
    async fn account_detail_includes_label() {
        let (app, db) = build_test_app_with_db();
        db.upsert_user(TEST_DID, "test.bsky.social").await.unwrap();
        seed_account(&db, "did:plc:target1", "target1.bsky.social", 40.0, "High").await;
        db.upsert_user_label(TEST_DID, "did:plc:target1", "high", Some("known troll"))
            .await
            .unwrap();

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/accounts/target1.bsky.social")
                    .header("cookie", session_cookie(TEST_DID))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);

        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["user_label"]["label"], "high");
        assert_eq!(json["user_label"]["notes"], "known troll");
    }

    #[tokio::test]
    async fn account_detail_without_label_has_null() {
        let (app, db) = build_test_app_with_db();
        db.upsert_user(TEST_DID, "test.bsky.social").await.unwrap();
        seed_account(&db, "did:plc:target1", "target1.bsky.social", 40.0, "High").await;

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/accounts/target1.bsky.social")
                    .header("cookie", session_cookie(TEST_DID))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);

        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json["user_label"].is_null());
    }
}
