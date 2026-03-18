// tests/web_oauth.rs
// Integration tests for the AT Protocol OAuth endpoints.
//
// Tests that require a real PDS (full OAuth flow) are marked #[ignore].
// All other tests run in CI against a local in-memory test server.
//
// Run all: cargo test --features web --test web_oauth
// Run ignored (manual): cargo test --features web --test web_oauth -- --ignored

#[cfg(feature = "web")]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use tower::ServiceExt; // for .oneshot()

    use charcoal::web::auth::{create_token, COOKIE_NAME};
    use charcoal::web::test_helpers::{
        build_test_app, build_test_app_with_db, TEST_DID, TEST_SECRET,
    };

    fn session_cookie(did: &str) -> String {
        format!("{}={}", COOKIE_NAME, create_token(TEST_SECRET, did))
    }

    // ---- Client metadata endpoint ----

    #[tokio::test]
    async fn client_metadata_returns_200_with_correct_fields() {
        let app = build_test_app();
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/oauth-client-metadata.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);

        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).expect("response should be valid JSON");

        // Required fields per AT Protocol OAuth spec
        assert!(json["client_id"].is_string(), "client_id must be a string");
        assert!(
            json["redirect_uris"].is_array(),
            "redirect_uris must be an array"
        );
        assert_eq!(json["scope"], "atproto");
        assert_eq!(json["token_endpoint_auth_method"], "private_key_jwt");
        assert_eq!(json["application_type"], "web");
        assert_eq!(json["dpop_bound_access_tokens"], true);
        assert!(
            json["grant_types"]
                .as_array()
                .unwrap()
                .contains(&Value::String("authorization_code".to_string())),
            "grant_types must include authorization_code"
        );
    }

    #[tokio::test]
    async fn client_metadata_content_type_is_json() {
        let app = build_test_app();
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/oauth-client-metadata.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let ct = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.contains("application/json"),
            "content-type should be application/json, got: {ct}"
        );
    }

    // ---- Initiate endpoint ----

    #[tokio::test]
    async fn initiate_rejects_empty_handle() {
        let app = build_test_app();
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/initiate")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"handle": ""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn initiate_rejects_whitespace_only_handle() {
        let app = build_test_app();
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/initiate")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"handle": "   "}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn initiate_rejects_missing_handle_field() {
        let app = build_test_app();
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/initiate")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Axum returns 422 for missing required fields in Json extractor
        assert!(
            res.status() == StatusCode::BAD_REQUEST
                || res.status() == StatusCode::UNPROCESSABLE_ENTITY,
            "Expected 400 or 422, got: {}",
            res.status()
        );
    }

    // Full initiate flow with a real PDS — manual only
    #[tokio::test]
    #[ignore = "requires a live PDS — run manually with BLUESKY_HANDLE set"]
    async fn initiate_with_real_handle_returns_redirect_url() {
        let app = build_test_app();
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/initiate")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"handle": "chaosgreml.in"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["redirect_url"].is_string(),
            "response should have redirect_url"
        );
        let url = json["redirect_url"].as_str().unwrap();
        assert!(url.starts_with("https://"), "redirect_url should be https");
    }

    // ---- Callback endpoint ----

    #[tokio::test]
    async fn callback_rejects_missing_state_param() {
        let app = build_test_app();
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/callback?code=somecode")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn callback_rejects_missing_code_param() {
        let app = build_test_app();
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/callback?state=somestate")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn callback_rejects_unknown_state() {
        // state param is present but not in the pending_oauth map → 400
        let app = build_test_app();
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/callback?code=fakecode&state=unknownstate123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn callback_surfaces_pds_error_param() {
        // PDS can redirect back with ?error=access_denied
        let app = build_test_app();
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/callback?error=access_denied&error_description=User+denied")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    // ---- Protected route authentication ----

    #[tokio::test]
    async fn protected_route_returns_401_with_no_cookie() {
        let app = build_test_app();
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn protected_route_returns_403_for_wrong_did() {
        // Session cookie is valid but belongs to a DID that isn't CHARCOAL_ALLOWED_DID.
        let app = build_test_app();
        let cookie = session_cookie("did:plc:intruder00000000000000000");

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn protected_route_returns_200_for_allowed_did() {
        let app = build_test_app();
        let cookie = session_cookie(TEST_DID);

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);
    }

    // ---- Scan endpoint requires registered user ----

    #[tokio::test]
    async fn scan_fails_when_user_not_registered() {
        // This test documents the bug: without a user row in the DB,
        // POST /api/scan returns 500 "User not found".
        let app = build_test_app();
        let cookie = session_cookie(TEST_DID);

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/scan")
                    .method("POST")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["error"].as_str().unwrap_or("").contains("not found"),
            "Error should mention user not found"
        );
    }

    #[tokio::test]
    async fn scan_succeeds_when_user_registered_in_db() {
        // This test proves the fix: if the user IS in the DB (as the
        // fixed OAuth callback will do), POST /api/scan should not
        // return "User not found". It will return 202 Accepted.
        let (app, db) = build_test_app_with_db();

        // Simulate what the fixed OAuth callback should do
        db.upsert_user(TEST_DID, "test.bsky.social")
            .await
            .expect("upsert_user should succeed");

        let cookie = session_cookie(TEST_DID);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/scan")
                    .method("POST")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // 202 Accepted — scan started successfully
        assert_eq!(
            res.status(),
            StatusCode::ACCEPTED,
            "Scan should return 202 Accepted for registered users"
        );
    }

    // ---- Logout ----

    #[tokio::test]
    async fn logout_clears_session_cookie() {
        let app = build_test_app();
        let cookie = session_cookie(TEST_DID);

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/logout")
                    .method("POST")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);

        let set_cookie = res
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            set_cookie.contains("Max-Age=0"),
            "Logout should set Max-Age=0 to expire the cookie. Got: {set_cookie}"
        );
    }
}
