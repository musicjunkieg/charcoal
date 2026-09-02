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
        build_test_app, build_test_app_with_db, build_test_app_with_state, TEST_DID, TEST_SECRET,
    };

    fn session_cookie(did: &str) -> String {
        format!("{}={}", COOKIE_NAME, create_token(TEST_SECRET, did))
    }

    // ---- Client metadata endpoint ----

    #[tokio::test]
    async fn client_metadata_returns_200_with_correct_fields() {
        let Some(app) = build_test_app() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
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
        assert_eq!(
            json["scope"],
            charcoal::web::actions::scope::client_scope(),
            "client metadata must advertise the union of login + write scopes"
        );
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
        let Some(app) = build_test_app() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
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
        let Some(app) = build_test_app() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
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
        let Some(app) = build_test_app() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
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
        let Some(app) = build_test_app() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
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
        let Some(app) = build_test_app() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
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
        let Some(app) = build_test_app() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
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
        let Some(app) = build_test_app() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
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
        let Some(app) = build_test_app() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
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
        let Some(app) = build_test_app() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
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
        let Some(app) = build_test_app() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
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
        let Some(app) = build_test_app() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
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
        let Some(app) = build_test_app() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
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

    #[tokio::test]
    async fn status_json_shape_is_backward_compatible_with_phase_fields() {
        // Guard the /api/status contract: all pre-existing keys must survive,
        // and the additive phase/progress fields report idle before any scan.
        let Some(app) = build_test_app() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
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

        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).expect("response should be valid JSON");

        // Pre-existing contract.
        assert_eq!(json["scan_running"], false);
        assert!(json["started_at"].is_null());
        assert!(json["progress_message"].is_string());
        assert!(json["last_error"].is_null());
        assert!(json["tier_counts"]["total"].is_number());
        // Additive fields.
        assert_eq!(json["phase"], "idle");
        assert!(json["progress"].is_null());
    }

    #[tokio::test]
    async fn status_surfaces_not_assessed_count_in_tier_counts() {
        // #222 language abstention: accounts whose score was withheld
        // (unsupported language) are stored with threat_tier="NotAssessed"
        // and a NULL score, so get_ranked_threats(0.0) never sees them.
        // The authoritative count must come from count_not_assessed and
        // show up in tier_counts.not_assessed rather than silently
        // vanishing or being folded into "low".
        use charcoal::db::models::AccountScore;

        let Some((app, db)) = build_test_app_with_db() else {
            eprintln!("SKIP: models not present, cannot build test AppState");

            return;
        };

        let not_assessed = AccountScore {
            did: "did:plc:notassessed".to_string(),
            handle: "unsupported.bsky.social".to_string(),
            toxicity_score: None,
            topic_overlap: None,
            overlap_legacy: None,
            threat_score: None,
            threat_tier: Some("NotAssessed".to_string()),
            posts_analyzed: 5,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
            fingerprint_quality: None,
            scoring_confidence: None,
        };
        db.upsert_account_score(TEST_DID, &not_assessed)
            .await
            .unwrap();

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

        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).expect("response should be valid JSON");

        assert_eq!(json["tier_counts"]["not_assessed"], 1);
        // Must not have leaked into "low" (get_ranked_threats(0.0) filters
        // out NULL-score rows before the tier loop even runs).
        assert_eq!(json["tier_counts"]["low"], 0);
        // Total spans the NotAssessed population too (#222, CodeRabbit C2):
        // 0 ranked + 1 not_assessed == 1, so the tier counts reconcile.
        assert_eq!(json["tier_counts"]["total"], 1);
    }

    // ---- Scan endpoint requires registered user ----

    #[tokio::test]
    async fn scan_fails_when_user_not_registered() {
        // Without a user row in the DB, POST /api/scan returns 404 — a valid
        // session with a missing user row is a client-actionable
        // "re-authenticate" state, not a server error. This used to return
        // 500, mis-routing it into server-error alerting; the equivalent
        // condition in trigger_admin_scan (admin.rs) already returned 404.
        // (#307, CodeRabbit PR #103)
        let Some(app) = build_test_app() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
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

        assert_eq!(res.status(), StatusCode::NOT_FOUND);
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
        let Some((app, db)) = build_test_app_with_db() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };

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
        let Some(app) = build_test_app() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
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

    // ---- Callback gate: denied sign-in auto-waitlists (#309, Task 5) ----
    //
    // The callback handler exchanges the code for tokens via `oauth_complete`,
    // which makes a REAL HTTP POST to `authorization_server.token_endpoint` —
    // there is no seam to fake that response in-process. So driving the gate
    // block (which runs AFTER the token exchange) requires either a live PDS
    // or a stand-in HTTP server for the token endpoint.
    //
    // `build_test_app_with_state` (added for this task) hands back the live
    // `AppState` so a `PendingOAuth` can be seeded directly into
    // `state.pending_oauth` — the same map `/api/auth/initiate` populates —
    // pointing `authorization_server.token_endpoint` at a `wiremock`
    // `MockServer`. `wiremock` is already a project dependency exercised the
    // same way in tests/unit_classifier.rs, so this reuses existing test
    // infrastructure rather than inventing a new mocking framework. A plain
    // 200 JSON response satisfies `oauth_complete`: its `DpopRetry` middleware
    // only intervenes on 400/401 (nonce-challenge retry), so no DPoP-nonce
    // choreography is needed for the happy path this handler expects.
    //
    // This drives the actual `/api/auth/callback` HTTP handler end-to-end,
    // so all three behavioral contracts are asserted at the real HTTP layer:
    // pending row + redirect + no cookie/no user row; handle refresh keeps
    // denied sticky; a denied row stays denied and still redirects. The
    // DB-error fail-closed path is asserted the same way, by sabotaging the
    // DB.
    //
    // NOTE: the redirect status is 303 See Other, not 302 Found — that's what
    // `axum::response::Redirect::to()` sends (see axum's redirect.rs), the
    // same helper the pre-existing `/dashboard` success redirect uses. The
    // task brief's sample assertion said `StatusCode::FOUND`; verified against
    // actual axum behavior and left as `SEE_OTHER` here instead of changing
    // production code to deviate from the codebase's established redirect
    // convention.

    /// Seed `state.pending_oauth[state_param]` with a `PendingOAuth` whose
    /// token exchange resolves against a fresh `wiremock` server returning
    /// `sub: did`. Returns the (kept-alive) mock server — it must outlive the
    /// callback request or the POST to its token endpoint will fail to connect.
    async fn seed_callback_state(
        state: &charcoal::web::AppState,
        state_param: &str,
        did: &str,
        handle: &str,
    ) -> wiremock::MockServer {
        use atproto_identity::key::{generate_key, KeyType};
        use atproto_oauth::resources::AuthorizationServer;
        use atproto_oauth::workflow::OAuthRequest;
        use charcoal::web::handlers::oauth::PendingOAuth;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        let token_response = serde_json::json!({
            "access_token": "test-access-token",
            "token_type": "DPoP",
            "refresh_token": "test-refresh-token",
            "scope": "atproto",
            "expires_in": 3600,
            "sub": did,
        });
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_response))
            .mount(&mock)
            .await;

        let dpop_key =
            generate_key(KeyType::P256Private).expect("DPoP key generation should succeed");
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
                handle: handle.to_string(),
                did: did.to_string(),
            },
        );

        mock
    }

    /// Like `seed_callback_state`, but the handle at initiate resolved to a
    /// DIFFERENT DID than the one the token exchange authenticates as — the
    /// DID-binding mismatch case (#309 fast-follow, Fix 1).
    async fn seed_callback_state_mismatched_did(
        state: &charcoal::web::AppState,
        state_param: &str,
        pending_did: &str,
        authenticated_did: &str,
        handle: &str,
    ) -> wiremock::MockServer {
        use atproto_identity::key::{generate_key, KeyType};
        use atproto_oauth::resources::AuthorizationServer;
        use atproto_oauth::workflow::OAuthRequest;
        use charcoal::web::handlers::oauth::PendingOAuth;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        let token_response = serde_json::json!({
            "access_token": "test-access-token",
            "token_type": "DPoP",
            "refresh_token": "test-refresh-token",
            "scope": "atproto",
            "expires_in": 3600,
            "sub": authenticated_did,
        });
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_response))
            .mount(&mock)
            .await;

        let dpop_key =
            generate_key(KeyType::P256Private).expect("DPoP key generation should succeed");
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
                handle: handle.to_string(),
                did: pending_did.to_string(),
            },
        );

        mock
    }

    async fn call_callback(app: axum::Router, state_param: &str) -> axum::response::Response {
        app.oneshot(
            Request::builder()
                .uri(format!(
                    "/api/auth/callback?code=fakecode&state={state_param}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn callback_denies_new_did_upserts_pending_redirects_no_cookie_no_user() {
        let Some((app, state)) = build_test_app_with_state() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
        const DENIED_DID: &str = "did:plc:denied0000000000000000001";

        let _mock =
            seed_callback_state(&state, "state-new-denial", DENIED_DID, "denied.bsky.social").await;
        let res = call_callback(app, "state-new-denial").await;

        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        assert_eq!(res.headers().get("location").unwrap(), "/waitlist");
        assert!(
            res.headers().get("set-cookie").is_none(),
            "a denied sign-in must not set a session cookie"
        );

        let row = state
            .db
            .get_access_request(DENIED_DID)
            .await
            .unwrap()
            .expect("gate denial should upsert a pending access_requests row");
        assert_eq!(row.status, "pending");
        assert_eq!(row.handle, "denied.bsky.social");
        assert!(
            state
                .db
                .get_user_handle(DENIED_DID)
                .await
                .unwrap()
                .is_none(),
            "a denied sign-in must not create a users row"
        );
    }

    #[tokio::test]
    async fn callback_second_denied_attempt_refreshes_handle_keeps_pending_status() {
        let Some((app, state)) = build_test_app_with_state() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
        const DENIED_DID: &str = "did:plc:denied0000000000000000002";

        let _mock1 =
            seed_callback_state(&state, "state-first", DENIED_DID, "first.bsky.social").await;
        let res1 = call_callback(app.clone(), "state-first").await;
        assert_eq!(res1.status(), StatusCode::SEE_OTHER);

        let row1 = state
            .db
            .get_access_request(DENIED_DID)
            .await
            .unwrap()
            .expect("first denied attempt should record a pending row");
        assert_eq!(row1.status, "pending");
        assert_eq!(row1.handle, "first.bsky.social");

        // Second sign-in attempt from the same DID, different handle.
        let _mock2 =
            seed_callback_state(&state, "state-second", DENIED_DID, "second.bsky.social").await;
        let res2 = call_callback(app, "state-second").await;
        assert_eq!(res2.status(), StatusCode::SEE_OTHER);
        assert_eq!(res2.headers().get("location").unwrap(), "/waitlist");

        let row2 = state
            .db
            .get_access_request(DENIED_DID)
            .await
            .unwrap()
            .expect("second denied attempt should still have a row");
        assert_eq!(
            row2.handle, "second.bsky.social",
            "the upsert should refresh the handle"
        );
        assert_eq!(
            row2.status, "pending",
            "a second sign-in attempt must not change status on its own"
        );
    }

    #[tokio::test]
    async fn callback_denied_row_stays_denied_and_still_redirects() {
        let Some((app, state)) = build_test_app_with_state() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
        const DENIED_DID: &str = "did:plc:denied0000000000000000003";

        // Pre-seed a row an admin has explicitly denied.
        state
            .db
            .upsert_access_request_pending(DENIED_DID, "original.bsky.social")
            .await
            .unwrap();
        state
            .db
            .set_access_status(DENIED_DID, "denied", TEST_DID)
            .await
            .unwrap();

        let _mock = seed_callback_state(
            &state,
            "state-still-denied",
            DENIED_DID,
            "retry.bsky.social",
        )
        .await;
        let res = call_callback(app, "state-still-denied").await;

        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        assert_eq!(res.headers().get("location").unwrap(), "/waitlist");
        assert!(res.headers().get("set-cookie").is_none());

        let row = state
            .db
            .get_access_request(DENIED_DID)
            .await
            .unwrap()
            .expect("row should still exist");
        assert_eq!(
            row.status, "denied",
            "a sign-in attempt must never move a denied row back to pending"
        );
        assert_eq!(
            row.handle, "retry.bsky.social",
            "the handle still refreshes even while denied"
        );
        assert!(
            state
                .db
                .get_user_handle(DENIED_DID)
                .await
                .unwrap()
                .is_none(),
            "a denied DID must never get a users row, no matter how many attempts"
        );
    }

    #[tokio::test]
    async fn callback_rejects_did_mismatch_no_row_no_cookie_no_user() {
        // The handle typed at /api/auth/initiate resolved to PENDING_DID, but
        // the account that actually completed the OAuth dance authenticates
        // as a different DID entirely. The callback must reject this before
        // touching the DB or the access gate — no access_requests row, no
        // session cookie, no users row (Fix 1, #309 fast-follow).
        let Some((app, state)) = build_test_app_with_state() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
        const PENDING_DID: &str = "did:plc:typedhandleowner0000001";
        const AUTHENTICATED_DID: &str = "did:plc:actualsignerxxxxxxxxx001";

        let _mock = seed_callback_state_mismatched_did(
            &state,
            "state-did-mismatch",
            PENDING_DID,
            AUTHENTICATED_DID,
            "someone-elses-handle.bsky.social",
        )
        .await;
        let res = call_callback(app, "state-did-mismatch").await;

        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        assert!(
            res.headers().get("set-cookie").is_none(),
            "a DID mismatch must never set a session cookie"
        );
        assert!(
            state
                .db
                .get_access_request(PENDING_DID)
                .await
                .unwrap()
                .is_none(),
            "a DID mismatch must not write an access_requests row for the typed handle's DID"
        );
        assert!(
            state
                .db
                .get_access_request(AUTHENTICATED_DID)
                .await
                .unwrap()
                .is_none(),
            "a DID mismatch must not write an access_requests row for the authenticated DID either"
        );
        assert!(
            state
                .db
                .get_user_handle(AUTHENTICATED_DID)
                .await
                .unwrap()
                .is_none(),
            "a DID mismatch must not create a users row"
        );
    }

    #[tokio::test]
    async fn callback_gate_check_db_error_fails_closed_with_500() {
        // Sabotage the access_requests table so check_access's clause 3 query
        // errors out. The gate must fail CLOSED: 500, not a redirect, not a
        // session cookie — auth.rs's check_access_db_error_propagates_as_err_never_allow
        // already proves this for check_access in isolation; this proves the
        // callback's new Err arm actually wires that into a safe HTTP response.
        use charcoal::db::sqlite::SqliteDatabase;
        use charcoal::web::test_helpers::build_test_app_with_state_and_db;

        let conn =
            rusqlite::Connection::open_in_memory().expect("in-memory SQLite should always succeed");
        charcoal::db::schema::create_tables(&conn).expect("schema creation should succeed");
        conn.execute("DROP TABLE access_requests", [])
            .expect("table should exist and drop successfully");
        let db = std::sync::Arc::new(SqliteDatabase::new(conn))
            as std::sync::Arc<dyn charcoal::db::Database>;

        // allowed_did = TEST_DID, requester is a different DID and not an
        // admin — forces check_access into clause 3 (the sabotaged table).
        let Some((app, state)) = build_test_app_with_state_and_db(db, TEST_DID, "") else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };
        const OUTSIDER_DID: &str = "did:plc:outsider000000000000000";

        let _mock = seed_callback_state(
            &state,
            "state-db-error",
            OUTSIDER_DID,
            "outsider.bsky.social",
        )
        .await;
        let res = call_callback(app, "state-db-error").await;

        assert_eq!(
            res.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "a gate-check DB error must fail closed with 500, never allow or redirect"
        );
        assert!(
            res.headers().get("set-cookie").is_none(),
            "a failed-closed gate check must never set a session cookie"
        );
    }
}
