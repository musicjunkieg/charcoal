// Test infrastructure: builds an in-memory Axum app for integration tests.
// Only compiled under #[cfg(test)] — never ships in production binaries.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::config::Config;
use crate::db::schema::create_tables;
use crate::db::sqlite::SqliteDatabase;
use crate::web::scan_job::ScanStatus;
use crate::web::{build_router, AppState};

pub const TEST_SECRET: &str = "test_session_secret_at_least_32_chars!";
pub const TEST_DID: &str = "did:plc:testalloweddid0000000000";
pub const TEST_CLIENT_ID: &str = "https://test.example.com/oauth-client-metadata.json";

/// Build an in-memory Axum router suitable for integration tests.
/// Uses Config::test_defaults() — override fields as needed for specific tests.
pub fn build_test_app() -> axum::Router {
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

    let state = AppState {
        db,
        config: Arc::new(config),
        scan_status: Arc::new(RwLock::new(ScanStatus::default())),
        pending_oauth: Arc::new(RwLock::new(HashMap::new())),
        oauth_tokens: Arc::new(RwLock::new(None)),
    };

    build_router(state)
}
