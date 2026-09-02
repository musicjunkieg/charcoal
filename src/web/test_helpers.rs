// Test infrastructure: builds an in-memory Axum app for integration tests.
// Only compiled under #[cfg(test)] — never ships in production binaries.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use atproto_identity::key::{generate_key, KeyType};
use tokio::sync::RwLock;

use crate::config::Config;
use crate::db::schema::create_tables;
use crate::db::sqlite::SqliteDatabase;
use crate::web::scan_job::{ScanManager, ScanModels};
use crate::web::{build_router, AppState};

pub const TEST_SECRET: &str = "test_session_secret_at_least_32_chars!";
pub const TEST_DID: &str = "did:plc:testalloweddid0000000000";
pub const TEST_CLIENT_ID: &str = "https://test.example.com/oauth-client-metadata.json";

/// Fixed AES-256-GCM key for tests (64 hex chars). Never used outside tests.
pub const TEST_TOKEN_KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

/// Load the shared ONNX models used by test `AppState`s, once per test binary.
///
/// `AppState::models` is `Arc<ScanModels>`, not optional (#257) — boot fails
/// fast when models are missing, and tests build the same `AppState` shape.
/// But the ~500MB model volume must not be a precondition for running the
/// web test suite at all, so this mirrors the `nli_files_present` skip-gate
/// used throughout the suite (e.g. tests/unit_nli.rs, tests/web_oauth.rs):
/// `None` when the files aren't present, rather than panicking.
///
/// Cached in a `OnceLock` — dozens of tests call `build_test_app[_with_db]`,
/// and reloading the models per call would be both slow and memory-hungry.
fn test_models() -> Option<Arc<ScanModels>> {
    static MODELS: OnceLock<Option<Arc<ScanModels>>> = OnceLock::new();
    MODELS
        .get_or_init(|| {
            let base = crate::toxicity::download::resolve_model_dir();
            let present = crate::toxicity::download::model_files_present(&base)
                && crate::toxicity::download::embedding_files_present(&base)
                && crate::toxicity::download::nli_files_present(&base);
            if !present {
                eprintln!(
                    "SKIP: models not present at {} — web tests needing AppState will skip",
                    base.display()
                );
                return None;
            }
            Some(Arc::new(
                ScanModels::load(&base).expect("model load should succeed when files are present"),
            ))
        })
        .clone()
}

/// Build an in-memory Axum router and DB suitable for integration tests.
/// Uses Config::test_defaults() — override fields as needed for specific tests.
///
/// Returns `None` when the ONNX model files aren't present locally (see
/// `test_models`); callers should skip cleanly rather than unwrap.
pub fn build_test_app_with_db() -> Option<(axum::Router, Arc<dyn crate::db::Database>)> {
    build_app(TEST_DID)
}

/// Build a test app with OPEN access — `CHARCOAL_ALLOWED_DID` empty, which is
/// how production runs since the open-signup ruling on #256.
///
/// Needed because the default helper pins `allowed_did` to `TEST_DID`, so a
/// second user is rejected by `require_auth` before any handler runs — and
/// "what happens to the SECOND user" is the entire subject of #257.
pub fn build_open_test_app_with_db() -> Option<(axum::Router, Arc<dyn crate::db::Database>)> {
    build_app_with_admins("", "")
}

/// Build a test app with OPEN access in which `TEST_DID` is an admin.
///
/// The admin handlers gate on `AuthUser::is_admin`, which is derived from
/// `config.admin_dids` — with the default empty list every admin endpoint
/// answers 403 before its body runs, so no test could reach one.
pub fn build_admin_test_app_with_db() -> Option<(axum::Router, Arc<dyn crate::db::Database>)> {
    build_app_with_admins("", TEST_DID)
}

/// Build a test app AND hand back the full `AppState` (default gate: only
/// `TEST_DID` is allowed).
///
/// Needed by tests that must drive state the router's handlers read/write
/// but that plain HTTP requests can't reach — e.g. seeding
/// `state.pending_oauth` so the OAuth callback's post-token-exchange gate
/// logic (#309) can be exercised against a wiremock stub instead of a live
/// PDS. `AppState` is `Clone` (all fields are `Arc`-wrapped), so the returned
/// state shares the exact same underlying data the router was built with —
/// mutating it through this handle is visible to in-flight requests.
pub fn build_test_app_with_state() -> Option<(axum::Router, AppState)> {
    build_app_with_admins_state(TEST_DID, "")
}

/// Like `build_test_app_with_state`, but the caller supplies an
/// already-constructed `Database`.
///
/// Exists so tests can pre-sabotage a `SqliteDatabase` (e.g. drop a table)
/// BEFORE it's wired into the router, to exercise fail-closed paths a healthy
/// DB can never reach — e.g. `check_access`'s `Err` arm inside the OAuth
/// callback gate (#309). A plain in-memory DB built through the trait object
/// can't be sabotaged from outside the crate (`SqliteDatabase`'s connection
/// is private), so this takes the DB as-is instead of building one itself.
pub fn build_test_app_with_state_and_db(
    db: Arc<dyn crate::db::Database>,
    allowed_did: &str,
    admin_dids: &str,
) -> Option<(axum::Router, AppState)> {
    build_app_with_admins_state_and_db(db, allowed_did, admin_dids)
}

fn build_app(allowed_did: &str) -> Option<(axum::Router, Arc<dyn crate::db::Database>)> {
    build_app_with_admins(allowed_did, "")
}

fn build_app_with_admins(
    allowed_did: &str,
    admin_dids: &str,
) -> Option<(axum::Router, Arc<dyn crate::db::Database>)> {
    let (router, state) = build_app_with_admins_state(allowed_did, admin_dids)?;
    Some((router, state.db))
}

fn build_app_with_admins_state(
    allowed_did: &str,
    admin_dids: &str,
) -> Option<(axum::Router, AppState)> {
    let conn =
        rusqlite::Connection::open_in_memory().expect("in-memory SQLite should always succeed");
    create_tables(&conn).expect("schema creation should succeed");
    let db = Arc::new(SqliteDatabase::new(conn)) as Arc<dyn crate::db::Database>;
    build_app_with_admins_state_and_db(db, allowed_did, admin_dids)
}

fn build_app_with_admins_state_and_db(
    db: Arc<dyn crate::db::Database>,
    allowed_did: &str,
    admin_dids: &str,
) -> Option<(axum::Router, AppState)> {
    let models = test_models()?;

    let config = Config {
        allowed_did: allowed_did.to_string(),
        admin_dids: admin_dids.to_string(),
        oauth_client_id: TEST_CLIENT_ID.to_string(),
        session_secret: TEST_SECRET.to_string(),
        token_key: Some(TEST_TOKEN_KEY.to_string()),
        ..Config::test_defaults()
    };

    let signing_key =
        generate_key(KeyType::P256Private).expect("P-256 key generation should succeed");

    // Computed before `config` moves into `Arc::new` in the AppState literal.
    let sessions = crate::web::actions::session::SessionStore::from_config(&config).map(Arc::new);

    let state = AppState {
        db,
        config: Arc::new(config),
        scan_manager: Arc::new(RwLock::new(ScanManager::new())),
        pending_oauth: Arc::new(RwLock::new(HashMap::new())),
        sessions,
        action_wake: None,
        signing_key,
        http: reqwest::Client::new(),
        typeahead_limiter: crate::web::handlers::typeahead::build_limiter(),
        models,
        // No admitter behind a test AppState — nothing to wake.
        scan_wake: None,
    };

    Some((build_router(state.clone()), state))
}

/// Like `build_test_app_with_db` but with NO token key: the actions feature
/// is disabled and every /api/actions endpoint must answer 503.
pub fn build_test_app_actions_disabled() -> Option<(axum::Router, Arc<dyn crate::db::Database>)> {
    let conn =
        rusqlite::Connection::open_in_memory().expect("in-memory SQLite should always succeed");
    create_tables(&conn).expect("schema creation should succeed");
    let db: Arc<dyn crate::db::Database> = Arc::new(SqliteDatabase::new(conn));
    let models = test_models()?;
    let config = Config {
        allowed_did: TEST_DID.to_string(),
        admin_dids: String::new(),
        oauth_client_id: TEST_CLIENT_ID.to_string(),
        session_secret: TEST_SECRET.to_string(),
        token_key: None,
        ..Config::test_defaults()
    };
    let signing_key =
        generate_key(KeyType::P256Private).expect("P-256 key generation should succeed");
    let state = AppState {
        db: db.clone(),
        config: Arc::new(config),
        scan_manager: Arc::new(RwLock::new(ScanManager::new())),
        pending_oauth: Arc::new(RwLock::new(HashMap::new())),
        sessions: None,
        action_wake: None,
        signing_key,
        http: reqwest::Client::new(),
        typeahead_limiter: crate::web::handlers::typeahead::build_limiter(),
        models,
        scan_wake: None,
    };
    Some((build_router(state), db))
}

/// Build an in-memory Axum router for tests that don't need DB access.
///
/// Returns `None` when the ONNX model files aren't present locally — see
/// `build_test_app_with_db`.
pub fn build_test_app() -> Option<axum::Router> {
    let (router, _db) = build_test_app_with_db()?;
    Some(router)
}
