// Test infrastructure: builds an in-memory Axum app for integration tests.
// Only compiled under #[cfg(test)] — never ships in production binaries.

use std::collections::HashMap;
use std::sync::Arc;

use atproto_identity::key::{generate_key, KeyType};
use tokio::sync::RwLock;

use crate::config::Config;
use crate::db::schema::create_tables;
use crate::db::sqlite::SqliteDatabase;
use crate::web::scan_job::ScanManager;
use crate::web::{build_router, AppState};

pub const TEST_SECRET: &str = "test_session_secret_at_least_32_chars!";
pub const TEST_DID: &str = "did:plc:testalloweddid0000000000";
pub const TEST_CLIENT_ID: &str = "https://test.example.com/oauth-client-metadata.json";

/// Build an in-memory Axum router and DB suitable for integration tests.
/// Uses Config::test_defaults() — override fields as needed for specific tests.
pub fn build_test_app_with_db() -> (axum::Router, Arc<dyn crate::db::Database>) {
    let config = Config {
        allowed_did: TEST_DID.to_string(),
        oauth_client_id: TEST_CLIENT_ID.to_string(),
        session_secret: TEST_SECRET.to_string(),
        ..Config::test_defaults()
    };

    let conn =
        rusqlite::Connection::open_in_memory().expect("in-memory SQLite should always succeed");
    create_tables(&conn).expect("schema creation should succeed");
    let db = Arc::new(SqliteDatabase::new(conn)) as Arc<dyn crate::db::Database>;

    let signing_key =
        generate_key(KeyType::P256Private).expect("P-256 key generation should succeed");

    let state = AppState {
        db: db.clone(),
        config: Arc::new(config),
        scan_manager: Arc::new(RwLock::new(ScanManager::new())),
        pending_oauth: Arc::new(RwLock::new(HashMap::new())),
        oauth_tokens: Arc::new(RwLock::new(None)),
        signing_key,
    };

    (build_router(state), db)
}

/// Build an in-memory Axum router for tests that don't need DB access.
pub fn build_test_app() -> axum::Router {
    let (router, _db) = build_test_app_with_db();
    router
}
