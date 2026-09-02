//! SessionStore: encrypted persistence, refresh-before-use, refresh
//! serialization, disconnect (#315, spec §3.4–§3.7).
#![cfg(feature = "web")]

use std::sync::Arc;

use atproto_identity::key::{generate_key, KeyType};
use atproto_oauth::workflow::{OAuthClient, TokenResponse};
use charcoal::config::Config;
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use charcoal::web::actions::scope::write_scope;
use charcoal::web::actions::session::{SessionError, SessionStore};
use rusqlite::Connection;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DID: &str = "did:plc:sessiontest000000000000";

fn setup_db() -> Arc<dyn Database> {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

fn store() -> SessionStore {
    let config = Config::test_defaults();
    SessionStore::from_config(&config).expect("test_defaults carries a token key")
}

fn oauth_client() -> OAuthClient {
    OAuthClient {
        redirect_uri: "https://charcoal.test/api/auth/callback".to_string(),
        client_id: "https://charcoal.test/client-metadata.json".to_string(),
        private_signing_key_data: generate_key(KeyType::P256Private).unwrap(),
    }
}

fn tokens(access: &str, refresh: &str, expires_in: u32) -> TokenResponse {
    tokens_with_scope(access, refresh, expires_in, &write_scope())
}

fn tokens_with_scope(access: &str, refresh: &str, expires_in: u32, scope: &str) -> TokenResponse {
    TokenResponse {
        access_token: access.to_string(),
        token_type: "DPoP".to_string(),
        refresh_token: Some(refresh.to_string()),
        scope: scope.to_string(),
        expires_in,
        sub: Some(DID.to_string()),
        extra: Default::default(),
    }
}

/// A mock PDS that serves the two discovery documents the refresh path reads
/// and a token endpoint at `/token`.
async fn mock_pds() -> MockServer {
    let mock = MockServer::start().await;
    let base = mock.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-protected-resource"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "resource": base, "authorization_servers": [base]
        })))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issuer": base,
            "token_endpoint": format!("{base}/token"),
            "authorization_endpoint": format!("{base}/authorize"),
            "pushed_authorization_request_endpoint": format!("{base}/par"),
            "revocation_endpoint": format!("{base}/revoke"),
            "scopes_supported": ["atproto", "transition:generic"],
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none", "private_key_jwt"],
            "token_endpoint_auth_signing_alg_values_supported": ["ES256"],
            "dpop_signing_alg_values_supported": ["ES256"],
            "authorization_response_iss_parameter_supported": true,
            "require_pushed_authorization_requests": true,
            "client_id_metadata_document_supported": true
        })))
        .mount(&mock)
        .await;
    mock
}

#[tokio::test]
async fn store_then_load_without_refresh_when_fresh() {
    let db = setup_db();
    let s = store();
    let key = generate_key(KeyType::P256Private).unwrap();
    s.store(
        db.as_ref(),
        DID,
        "https://pds.test",
        &key,
        &tokens("acc1", "ref1", 3600),
    )
    .await
    .unwrap();

    // Ciphertext at rest: the raw token never appears in the row.
    let row = db.get_oauth_session(DID).await.unwrap().unwrap();
    assert!(!row.access_token_enc.windows(4).any(|w| w == b"acc1"));
    assert!(!row
        .dpop_key_enc
        .windows(4)
        .any(|w| w == key.to_string().as_bytes()[..4].to_vec()));

    let http = reqwest::Client::new();
    let ws = s
        .load_for_write(db.as_ref(), &http, &oauth_client(), DID)
        .await
        .unwrap();
    assert_eq!(ws.access_token, "acc1");
    assert_eq!(ws.pds_url, "https://pds.test");
    assert_eq!(ws.did, DID);
    assert_eq!(ws.dpop_key.to_string(), key.to_string());
}

#[tokio::test]
async fn load_refreshes_when_expiring_and_persists_new_pair() {
    let db = setup_db();
    let s = store();
    let mock = mock_pds().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=ref1"))
        .and(body_string_contains("client_assertion_type=urn%3Aietf%3Aparams%3Aoauth%3Aclient-assertion-type%3Ajwt-bearer"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "acc2", "token_type": "DPoP", "refresh_token": "ref2",
            "scope": "atproto", "expires_in": 3600, "sub": DID
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let key = generate_key(KeyType::P256Private).unwrap();
    // expires_in = 30 s is inside the 60 s refresh threshold.
    s.store(
        db.as_ref(),
        DID,
        &mock.uri(),
        &key,
        &tokens("acc1", "ref1", 30),
    )
    .await
    .unwrap();

    let http = reqwest::Client::new();
    let ws = s
        .load_for_write(db.as_ref(), &http, &oauth_client(), DID)
        .await
        .unwrap();
    assert_eq!(ws.access_token, "acc2");

    // Second load: fresh now, no second refresh (expect(1) above enforces it).
    let ws2 = s
        .load_for_write(db.as_ref(), &http, &oauth_client(), DID)
        .await
        .unwrap();
    assert_eq!(ws2.access_token, "acc2");
}

#[tokio::test]
async fn concurrent_loads_refresh_exactly_once() {
    let db = setup_db();
    let s = Arc::new(store());
    let mock = mock_pds().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(200))
                .set_body_json(serde_json::json!({
                    "access_token": "acc2", "token_type": "DPoP", "refresh_token": "ref2",
                    "scope": "atproto", "expires_in": 3600, "sub": DID
                })),
        )
        .expect(1)
        .mount(&mock)
        .await;
    let key = generate_key(KeyType::P256Private).unwrap();
    s.store(
        db.as_ref(),
        DID,
        &mock.uri(),
        &key,
        &tokens("acc1", "ref1", 0),
    )
    .await
    .unwrap();

    let http = reqwest::Client::new();
    let oc = Arc::new(oauth_client());
    let mut handles = Vec::new();
    for _ in 0..5 {
        let (db, s, http, oc) = (db.clone(), s.clone(), http.clone(), oc.clone());
        handles.push(tokio::spawn(async move {
            s.load_for_write(db.as_ref(), &http, &oc, DID)
                .await
                .map(|w| w.access_token)
        }));
    }
    for h in handles {
        assert_eq!(h.await.unwrap().unwrap(), "acc2");
    }
}

#[tokio::test]
async fn invalid_grant_deletes_session_and_reports_not_connected() {
    let db = setup_db();
    let s = store();
    let mock = mock_pds().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant", "error_description": "refresh token revoked"
        })))
        .mount(&mock)
        .await;
    let key = generate_key(KeyType::P256Private).unwrap();
    s.store(
        db.as_ref(),
        DID,
        &mock.uri(),
        &key,
        &tokens("acc1", "ref1", 0),
    )
    .await
    .unwrap();

    let http = reqwest::Client::new();
    let err = s
        .load_for_write(db.as_ref(), &http, &oauth_client(), DID)
        .await
        .unwrap_err();
    assert!(matches!(err, SessionError::NotConnected), "got {err:?}");
    assert!(db.get_oauth_session(DID).await.unwrap().is_none());
}

#[tokio::test]
async fn transient_refresh_failure_keeps_session() {
    let db = setup_db();
    let s = store();
    let mock = mock_pds().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&mock)
        .await;
    let key = generate_key(KeyType::P256Private).unwrap();
    s.store(
        db.as_ref(),
        DID,
        &mock.uri(),
        &key,
        &tokens("acc1", "ref1", 0),
    )
    .await
    .unwrap();

    let http = reqwest::Client::new();
    let err = s
        .load_for_write(db.as_ref(), &http, &oauth_client(), DID)
        .await
        .unwrap_err();
    assert!(matches!(err, SessionError::Refresh(_)), "got {err:?}");
    assert!(db.get_oauth_session(DID).await.unwrap().is_some());
}

#[tokio::test]
async fn load_for_unknown_did_is_not_connected() {
    let db = setup_db();
    let s = store();
    let http = reqwest::Client::new();
    let err = s
        .load_for_write(db.as_ref(), &http, &oauth_client(), DID)
        .await
        .unwrap_err();
    assert!(matches!(err, SessionError::NotConnected));
    assert!(s.status(db.as_ref(), DID).await.unwrap().is_none());
}

#[tokio::test]
async fn disconnect_revokes_best_effort_and_deletes_row() {
    let db = setup_db();
    let s = store();
    let mock = mock_pds().await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .and(body_string_contains("token=ref1"))
        .and(body_string_contains("token_type_hint=refresh_token"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock)
        .await;
    let key = generate_key(KeyType::P256Private).unwrap();
    s.store(
        db.as_ref(),
        DID,
        &mock.uri(),
        &key,
        &tokens("acc1", "ref1", 3600),
    )
    .await
    .unwrap();

    let http = reqwest::Client::new();
    assert!(s
        .disconnect(db.as_ref(), &http, &oauth_client(), DID)
        .await
        .unwrap());
    assert!(db.get_oauth_session(DID).await.unwrap().is_none());
    // Second disconnect: nothing to delete, still Ok(false), no error.
    assert!(!s
        .disconnect(db.as_ref(), &http, &oauth_client(), DID)
        .await
        .unwrap());
}

#[tokio::test]
async fn disconnect_deletes_row_even_when_revocation_fails() {
    let db = setup_db();
    let s = store();
    let mock = mock_pds().await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock)
        .await;
    let key = generate_key(KeyType::P256Private).unwrap();
    s.store(
        db.as_ref(),
        DID,
        &mock.uri(),
        &key,
        &tokens("acc1", "ref1", 3600),
    )
    .await
    .unwrap();
    let http = reqwest::Client::new();
    assert!(s
        .disconnect(db.as_ref(), &http, &oauth_client(), DID)
        .await
        .unwrap());
    assert!(db.get_oauth_session(DID).await.unwrap().is_none());
}

#[tokio::test]
async fn status_reports_scope_without_secrets() {
    let db = setup_db();
    let s = store();
    let key = generate_key(KeyType::P256Private).unwrap();
    s.store(
        db.as_ref(),
        DID,
        "https://pds.test",
        &key,
        &tokens("acc1", "ref1", 3600),
    )
    .await
    .unwrap();
    let st = s.status(db.as_ref(), DID).await.unwrap().unwrap();
    assert_eq!(st.scope, write_scope());
    assert_eq!(st.pds_url, "https://pds.test");
    assert!(!st.connected_at.is_empty());
}

/// #322: a row stored under an older `write_scope()` (the #315 grant had no
/// `rpc:` scopes for the reconcile reads) must read as NOT connected — from
/// both `status` (so the UI offers consent again) and `load_for_write` (so
/// the runner parks the batch as `not_connected` instead of failing it at
/// the first proxied read with a 403).
#[tokio::test]
async fn row_with_insufficient_scope_reads_as_not_connected() {
    let db = setup_db();
    let s = store();
    let key = generate_key(KeyType::P256Private).unwrap();
    let pre_322 = "atproto repo:app.bsky.graph.block?action=create&action=delete \
                   rpc:app.bsky.graph.muteActor?aud=did:web:api.bsky.app%23bsky_appview \
                   rpc:app.bsky.graph.unmuteActor?aud=did:web:api.bsky.app%23bsky_appview";
    s.store(
        db.as_ref(),
        DID,
        "https://pds.test",
        &key,
        &tokens_with_scope("acc1", "ref1", 3600, pre_322),
    )
    .await
    .unwrap();

    assert!(s.status(db.as_ref(), DID).await.unwrap().is_none());
    let http = reqwest::Client::new();
    let err = s
        .load_for_write(db.as_ref(), &http, &oauth_client(), DID)
        .await
        .unwrap_err();
    assert!(matches!(err, SessionError::NotConnected), "{err:?}");

    // Re-consent overwrites the stale row and the DID is connected again.
    s.store(
        db.as_ref(),
        DID,
        "https://pds.test",
        &key,
        &tokens("acc2", "ref2", 3600),
    )
    .await
    .unwrap();
    assert!(s.status(db.as_ref(), DID).await.unwrap().is_some());
}
