# AT Protocol OAuth (v0.4) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the CHARCOAL_WEB_PASSWORD password gate with AT Protocol OAuth using the `atproto-oauth-axum` Rust crate, so the backend holds real AT Protocol tokens for future muting/blocking work.

**Architecture:** Backend-driven OAuth flow — Axum initiates PAR (Pushed Authorization Request), handles the PDS callback, validates the returned DID against `CHARCOAL_ALLOWED_DID`, then issues an HMAC session cookie with the DID embedded. Tokens are stored in-memory (`RwLock<Option<TokenSet>>`) on `AppState`; the user re-authenticates on restart. Session cookie format gains a DID field: `{timestamp}.{did_b64}.{nonce}.{hmac}`.

**Tech Stack:** Rust/Axum, `atproto-oauth` + `atproto-oauth-axum` + `atproto-identity` (ngerakines.me git crates, listed on Bluesky's official SDKs page), SvelteKit 5, `cimd-service.fly.dev` for the dev OAuth `client_id`.

**Current branch:** `feat/v0.4-atproto-oauth` (already created; design doc committed)

---

## Before you start

Understand these files — they are the core of what changes:

| File | What it does now |
|------|-----------------|
| `src/config.rs` | Loads env vars into `Config` struct. Has `web_password` and `session_secret` under `#[cfg(feature = "web")]`. |
| `src/web/mod.rs` | `AppState`, `build_router()`, `run_server()`. Routes `/api/login` (public) + protected routes behind `require_auth` middleware. |
| `src/web/auth.rs` | `create_token(secret)` / `verify_token(secret, token)` — HMAC session tokens in format `{timestamp}.{nonce}.{hmac}`. `require_auth` middleware. |
| `src/web/handlers/auth.rs` | `login()` handler (validates CHARCOAL_WEB_PASSWORD). `logout()` handler (clears cookie). |
| `web/src/routes/login/+page.svelte` | Login page with password field and "Continue" button. |
| `web/src/lib/api.ts` | `login(password)` function that POSTs to `/api/login`. |

**The key rule throughout:** write the failing test first, verify it fails, then implement to make it pass.

**Run all tests with:**
```bash
cargo test --features web --all-targets 2>&1
```

---

## Task 1: Add Cargo dependencies

No tests here — just setup so the crate is available.

**Files:**
- Modify: `Cargo.toml`

**Step 1: Read the current Cargo.toml [features] section**

```bash
grep -A 20 '^\[features\]' Cargo.toml
```

Expected: You'll see a `web` feature that lists its optional deps.

**Step 2: Add the three atproto crates to `[dependencies]` as optional**

Find the `# Web server` or `# web feature` comment block in `[dependencies]` and add:

```toml
# AT Protocol OAuth (required for the web feature — backend-driven OAuth flow)
atproto-oauth      = { git = "https://tangled.org/ngerakines.me/atproto-crates", optional = true }
atproto-oauth-axum = { git = "https://tangled.org/ngerakines.me/atproto-crates", optional = true }
atproto-identity   = { git = "https://tangled.org/ngerakines.me/atproto-crates", optional = true }
# base64 encoding for DID in session tokens
base64 = { version = "0.22", optional = true }
```

**Step 3: Add these deps to the `web` feature list in `[features]`**

Find `web = [...]` and add all four optional deps:

```toml
web = [
    # ... existing entries ...
    "dep:atproto-oauth",
    "dep:atproto-oauth-axum",
    "dep:atproto-identity",
    "dep:base64",
]
```

**Step 4: Verify the project resolves and compiles**

```bash
cargo check --features web 2>&1 | head -40
```

Expected: Fetches the git crates, compiles without errors. If you see "network error" or a 503 from tangled.org, wait and retry — the forge can be slow. If the crate names within the git repo are different (e.g. `atproto_oauth` with underscore), the error message will tell you.

> **If the git crates fail to resolve:** Check `tangled.org/ngerakines.me/atproto-crates` for the actual crate names in `Cargo.toml` files within that repo. The `package` key in each crate's `Cargo.toml` gives the real name.

**Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m 'chore: add atproto-oauth, atproto-oauth-axum, atproto-identity, base64 deps'
```

---

## Task 2: Write failing unit tests for the DID-aware session token

These tests define the new token format **before** we change the code. They will fail to compile — that's correct.

**Files:**
- Create: `tests/unit_oauth.rs`

**Step 1: Create the test file**

```rust
// tests/unit_oauth.rs
// Unit tests for DID-aware session tokens and the DID gate check.
//
// These tests drive the changes to src/web/auth.rs.
// They MUST FAIL (compile error) until Task 3 is complete.
//
// Run: cargo test --features web --test unit_oauth

#[cfg(feature = "web")]
mod token_tests {
    use charcoal::web::auth::{create_token, verify_token_did};

    const SECRET: &str = "test_session_secret_at_least_32_bytes!";
    const TEST_DID: &str = "did:plc:h3wpawnrlptr4534chevddo6";

    #[test]
    fn token_with_did_roundtrip() {
        let token = create_token(SECRET, TEST_DID);
        let result = verify_token_did(SECRET, &token);
        assert!(result.is_some(), "verify_token_did should return Some(did) for a fresh token");
        assert_eq!(result.unwrap(), TEST_DID);
    }

    #[test]
    fn wrong_secret_rejected() {
        let token = create_token(SECRET, TEST_DID);
        let result = verify_token_did("wrong_secret_also_32_bytes_long!!", &token);
        assert!(result.is_none(), "Wrong secret should return None");
    }

    #[test]
    fn tampered_hmac_rejected() {
        let token = create_token(SECRET, TEST_DID);
        // Flip the last byte of the token (the HMAC suffix is hex so it's ASCII)
        let mut bytes = token.into_bytes();
        let last = bytes.len() - 1;
        bytes[last] = if bytes[last] == b'0' { b'1' } else { b'0' };
        let tampered = String::from_utf8(bytes).unwrap();
        assert!(verify_token_did(SECRET, &tampered).is_none(), "Tampered HMAC should be rejected");
    }

    #[test]
    fn future_dated_token_rejected() {
        // Build a token manually with a future timestamp so checked_sub returns None.
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let future_ts = u64::MAX - 1;
        let did_b64 = URL_SAFE_NO_PAD.encode(TEST_DID.as_bytes());
        let nonce = "deadbeefdeadbeef";
        let payload = format!("{future_ts}.{did_b64}.{nonce}");
        let mut mac = HmacSha256::new_from_slice(SECRET.as_bytes()).unwrap();
        mac.update(payload.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());
        let token = format!("{payload}.{sig}");

        assert!(
            verify_token_did(SECRET, &token).is_none(),
            "Future-dated token should be rejected by checked_sub"
        );
    }

    #[test]
    fn malformed_token_rejected() {
        assert!(verify_token_did(SECRET, "").is_none());
        assert!(verify_token_did(SECRET, "only.three.parts").is_none());
        assert!(verify_token_did(SECRET, "a.b.c.d.e").is_none()); // too many segments
    }
}

#[cfg(feature = "web")]
mod gate_tests {
    use charcoal::web::auth::did_is_allowed;

    const ALLOWED: &str = "did:plc:h3wpawnrlptr4534chevddo6";

    #[test]
    fn allowed_did_passes() {
        assert!(did_is_allowed(ALLOWED, ALLOWED));
    }

    #[test]
    fn disallowed_did_rejected() {
        assert!(!did_is_allowed("did:plc:attacker00000000000000000", ALLOWED));
    }

    #[test]
    fn empty_allowed_did_rejects_everything() {
        // If CHARCOAL_ALLOWED_DID is not set, no DID should pass.
        assert!(!did_is_allowed(ALLOWED, ""));
    }

    #[test]
    fn did_comparison_is_exact() {
        // No prefix matching or substring matching.
        let prefix = &ALLOWED[..ALLOWED.len() - 1];
        assert!(!did_is_allowed(prefix, ALLOWED));
    }
}
```

**Step 2: Verify the tests FAIL (expected)**

```bash
cargo test --features web --test unit_oauth 2>&1 | head -20
```

Expected: Compile error — `create_token` takes one argument (we'll add `did`), and `verify_token_did` / `did_is_allowed` don't exist yet.

**Step 3: Commit the failing tests**

```bash
git add tests/unit_oauth.rs
git commit -m 'test: add failing unit tests for DID-aware session tokens (Task 3 will fix)'
```

---

## Task 3: Update session token format to include DID

This makes the `unit_oauth.rs` tests pass.

**Files:**
- Modify: `src/web/auth.rs`

**Background:** The current token format is `{timestamp}.{nonce_hex}.{hmac_hex}` (3 parts, split on `.`). The new format is `{timestamp}.{did_b64}.{nonce_hex}.{hmac_hex}` (4 parts). The HMAC now covers all three non-sig segments: `{timestamp}.{did_b64}.{nonce_hex}`. The `did_b64` is URL-safe base64 (no padding) of the DID string.

**Step 1: Add `base64` import and update `create_token` signature**

At the top of `src/web/auth.rs`, add:

```rust
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
```

Replace `create_token`:

```rust
/// Build a new session token with the authenticated DID embedded.
///
/// Token format: `{timestamp}.{did_b64}.{nonce_hex}.{hmac_hex}`
/// The HMAC covers `"{timestamp}.{did_b64}.{nonce_hex}"`.
/// `did_b64` is URL-safe base64 (no padding) of the DID UTF-8 bytes.
pub fn create_token(secret: &str, did: &str) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let did_b64 = URL_SAFE_NO_PAD.encode(did.as_bytes());

    let mut nonce_bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = hex::encode(nonce_bytes);

    let payload = format!("{timestamp}.{did_b64}.{nonce}");
    let sig = hmac_sign(secret, &payload);

    format!("{payload}.{sig}")
}
```

**Step 2: Add `verify_token_did` (keeps the old `verify_token` for backward compatibility with existing tests)**

Add this new function after `create_token`:

```rust
/// Verify a session token and extract the embedded DID.
///
/// Returns `Some(did)` if:
/// - The token has exactly 4 `.`-separated segments
/// - The HMAC is valid (constant-time check)
/// - The token age is within SESSION_TTL_SECS (using checked_sub to reject future-dated tokens)
/// - The did_b64 segment decodes to valid UTF-8
///
/// Returns `None` otherwise.
pub fn verify_token_did(secret: &str, token: &str) -> Option<String> {
    // Format: {timestamp}.{did_b64}.{nonce}.{hmac}
    // splitn(4) gives exactly 4 parts even if the hmac contains dots (it won't, it's hex).
    let parts: Vec<&str> = token.splitn(4, '.').collect();
    if parts.len() != 4 {
        return None;
    }
    let timestamp_str = parts[0];
    let did_b64 = parts[1];
    let nonce = parts[2];
    let provided_sig = parts[3];

    // Reject if there are extra dots in the final segment (would mean > 4 total parts).
    // splitn(4) puts everything from the 4th dot onward into parts[3], so check for dots there.
    if provided_sig.contains('.') {
        return None;
    }

    // Verify HMAC first (covers timestamp + did_b64 + nonce)
    let payload = format!("{timestamp_str}.{did_b64}.{nonce}");
    let expected_sig = hmac_sign(secret, &payload);
    if !constant_time_eq(provided_sig, &expected_sig) {
        return None;
    }

    // Verify age — checked_sub rejects future-dated tokens
    let timestamp = timestamp_str.parse::<u64>().ok()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let age = now.checked_sub(timestamp)?;
    if age >= SESSION_TTL_SECS {
        return None;
    }

    // Decode DID
    let did_bytes = URL_SAFE_NO_PAD.decode(did_b64).ok()?;
    String::from_utf8(did_bytes).ok()
}

/// Check whether a DID is allowed to authenticate.
///
/// Returns `false` if `allowed_did` is empty (CHARCOAL_ALLOWED_DID not configured).
/// Uses constant-time comparison to avoid timing oracle on the DID.
pub fn did_is_allowed(did: &str, allowed_did: &str) -> bool {
    !allowed_did.is_empty() && constant_time_eq(did, allowed_did)
}
```

**Step 3: Keep the old `verify_token` as a thin wrapper (existing tests use it)**

The existing tests in `src/web/auth.rs` call `verify_token`. Keep it so they still compile:

```rust
/// Verify a session token (ignores the embedded DID).
/// Kept for backward compatibility with existing tests.
/// New code should use verify_token_did.
pub fn verify_token(secret: &str, token: &str) -> bool {
    verify_token_did(secret, token).is_some()
}
```

**Step 4: Update `require_auth` middleware to extract DID and gate on it**

The middleware currently reads `state.config.web_password`. That field is going away in Task 4. Prepare for it now by switching to a pattern that will work after Task 4:

```rust
pub async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let session_secret = &state.config.session_secret;
    let allowed_did = &state.config.allowed_did; // will be added in Task 4

    match extract_session_did(&request, session_secret) {
        None => super::api_error(axum::http::StatusCode::UNAUTHORIZED, "Authentication required"),
        Some(did) if !did_is_allowed(&did, allowed_did) => {
            super::api_error(axum::http::StatusCode::FORBIDDEN, "Access denied")
        }
        Some(did) => {
            request.extensions_mut().insert(AuthUser { did });
            next.run(request).await
        }
    }
}

fn extract_session_did(request: &Request, session_secret: &str) -> Option<String> {
    let cookie_header = request.headers().get(header::COOKIE)?.to_str().ok()?;
    for pair in cookie_header.split(';') {
        let pair = pair.trim();
        if let Some((name, value)) = pair.split_once('=') {
            if name.trim() == COOKIE_NAME {
                return verify_token_did(session_secret, value.trim());
            }
        }
    }
    None
}
```

**Step 5: Update `AuthUser` in `src/web/mod.rs` to carry the DID**

Find the `AuthUser` struct at the bottom of `src/web/mod.rs` and add the DID field:

```rust
/// Marker type indicating the request passed session authentication.
/// Inserted into request extensions by `require_auth` middleware.
/// Handlers can extract it to learn who is authenticated.
#[derive(Clone)]
pub struct AuthUser {
    pub did: String,
}
```

**Step 6: Run the unit_oauth tests — they should pass now**

```bash
cargo test --features web --test unit_oauth 2>&1
```

Expected: All 9 tests pass. If any fail, read the error and fix the implementation before continuing.

**Step 7: Check for compilation errors across the whole project**

```bash
cargo check --features web 2>&1
```

Some errors are expected: `state.config.web_password` references in `mod.rs` and `allowed_did` doesn't exist yet on `Config`. These will be fixed in Task 4. Note which errors appear so you know what to fix next.

**Step 8: Run the existing auth tests to ensure they still pass**

```bash
cargo test --features web --lib 2>&1 | grep -E '(test|FAILED|ok)'
```

Expected: The existing unit tests in `src/web/auth.rs` (token roundtrip, wrong_secret, tampered, malformed) still pass.

**Step 9: Commit**

```bash
git add src/web/auth.rs src/web/mod.rs
git commit -m 'feat: DID-aware session token — create_token/verify_token_did/did_is_allowed'
```

---

## Task 4: Update Config — add CHARCOAL_ALLOWED_DID, remove CHARCOAL_WEB_PASSWORD

This fixes the compilation errors left from Task 3 and wires up the new env vars.

**Files:**
- Modify: `src/config.rs`
- Modify: `src/web/mod.rs` (startup validation)
- Modify: `src/web/handlers/auth.rs` (remove login handler, update logout to not need password)

**Step 1: Update the `Config` struct in `src/config.rs`**

Under `#[cfg(feature = "web")]`, replace `web_password: String` with:

```rust
/// DID that is allowed to authenticate (CHARCOAL_ALLOWED_DID env var).
/// Find your DID at: bsky.app → Settings → Account
#[cfg(feature = "web")]
pub allowed_did: String,

/// Public URL of the OAuth client metadata document (CHARCOAL_OAUTH_CLIENT_ID env var).
/// Dev: register at cimd-service.fly.dev to get a URL like https://cimd-service.fly.dev/clients/xxx
/// Production: https://{RAILWAY_PUBLIC_DOMAIN}/oauth-client-metadata.json
#[cfg(feature = "web")]
pub oauth_client_id: String,

/// HMAC signing key for session cookies (CHARCOAL_SESSION_SECRET env var — unchanged)
#[cfg(feature = "web")]
pub session_secret: String,
```

**Step 2: Update `Config::load()` in `src/config.rs`**

Replace the `web_password` loading block with:

```rust
#[cfg(feature = "web")]
let allowed_did = env::var("CHARCOAL_ALLOWED_DID").unwrap_or_default();
#[cfg(feature = "web")]
let oauth_client_id = env::var("CHARCOAL_OAUTH_CLIENT_ID").unwrap_or_default();
#[cfg(feature = "web")]
let session_secret = env::var("CHARCOAL_SESSION_SECRET").unwrap_or_default();
```

In the `Ok(Self { ... })` block, replace `web_password` with:

```rust
#[cfg(feature = "web")]
allowed_did,
#[cfg(feature = "web")]
oauth_client_id,
#[cfg(feature = "web")]
session_secret,
```

**Step 3: Update startup validation in `src/web/mod.rs`**

In `run_server()`, replace the `web_password` check:

```rust
// Fail fast if required OAuth config is missing.
if config.allowed_did.is_empty() {
    anyhow::bail!(
        "CHARCOAL_ALLOWED_DID is not set. Add your Bluesky DID to your .env file.\n\
         Find it at: https://bsky.app → Settings → Account\n\
         It looks like: did:plc:xxxxxxxxxxxxxxxxxxxx"
    );
}
if config.oauth_client_id.is_empty() {
    anyhow::bail!(
        "CHARCOAL_OAUTH_CLIENT_ID is not set.\n\
         For dev: POST your client metadata to https://cimd-service.fly.dev/clients\n\
         For production: set to https://{{RAILWAY_PUBLIC_DOMAIN}}/oauth-client-metadata.json"
    );
}
if config.session_secret.len() < 32 {
    anyhow::bail!(
        "CHARCOAL_SESSION_SECRET must be at least 32 characters (currently {} chars).\n\
         Generate one with: openssl rand -hex 32",
        config.session_secret.len()
    );
}
```

**Step 4: Delete the `login` handler from `src/web/handlers/auth.rs`**

Remove the `LoginRequest` struct and the `login()` async function entirely. Keep only `logout()`.

Also remove the `create_token` import (logout no longer needs it — it only calls `clear_cookie_header`):

```rust
use crate::web::auth::clear_cookie_header;
```

**Step 5: Remove the `/api/login` route from `src/web/mod.rs`**

In `build_router()`, in the `public_api` router, remove:

```rust
.route("/api/login", post(handlers::auth::login))
```

Also remove the `handlers::auth::login` reference from the `use` imports if any.

**Step 6: Add `test_defaults()` constructor to `Config` for use in integration tests**

At the bottom of `src/config.rs`, add:

```rust
#[cfg(test)]
impl Config {
    /// Build a Config with safe test values. Used by integration test helpers.
    /// Individual fields can be overridden after construction.
    pub fn test_defaults() -> Self {
        Self {
            bluesky_handle: String::new(),
            bluesky_app_password: String::new(),
            public_api_url: "https://public.api.bsky.app".to_string(),
            perspective_api_key: String::new(),
            db_path: ":memory:".to_string(),
            database_url: None,
            scorer_backend: crate::config::ScorerBackend::Onnx,
            model_dir: std::path::PathBuf::from("/tmp/test_models"),
            constellation_url: "https://constellation.microcosm.blue".to_string(),
            #[cfg(feature = "web")]
            allowed_did: "did:plc:testalloweddid0000000000".to_string(),
            #[cfg(feature = "web")]
            oauth_client_id: "https://cimd-service.fly.dev/clients/test".to_string(),
            #[cfg(feature = "web")]
            session_secret: "test_session_secret_at_least_32_chars!".to_string(),
        }
    }
}
```

**Step 7: Compile check — must be clean**

```bash
cargo check --features web 2>&1
```

Expected: No errors. If you see `web_password` referenced somewhere, search for it and remove:

```bash
grep -rn "web_password" src/
```

**Step 8: Run all tests**

```bash
cargo test --features web --all-targets 2>&1 | tail -20
```

Expected: All tests pass. The `unit_oauth.rs` tests still pass.

**Step 9: Run clippy**

```bash
cargo clippy --features web -- -D warnings 2>&1
```

Expected: Clean.

**Step 10: Commit**

```bash
git add src/config.rs src/web/mod.rs src/web/handlers/auth.rs
git commit -m 'feat: replace CHARCOAL_WEB_PASSWORD with CHARCOAL_ALLOWED_DID + CHARCOAL_OAUTH_CLIENT_ID'
```

---

## Task 5: Write failing integration tests for the new OAuth endpoints

These tests define what the new endpoints must do, before the endpoints exist.

**Files:**
- Create: `tests/web_oauth.rs`

**Step 1: Create the test file**

```rust
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

    use charcoal::web::test_helpers::{build_test_app, TEST_DID, TEST_SECRET};
    use charcoal::web::auth::{create_token, COOKIE_NAME};

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

        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).expect("response should be valid JSON");

        // Required fields per AT Protocol OAuth spec
        assert!(json["client_id"].is_string(), "client_id must be a string");
        assert!(json["redirect_uris"].is_array(), "redirect_uris must be an array");
        assert_eq!(json["scope"], "atproto");
        assert_eq!(json["token_endpoint_auth_method"], "none");
        assert_eq!(json["application_type"], "web");
        assert_eq!(json["dpop_bound_access_tokens"], true);
        assert!(
            json["grant_types"].as_array().unwrap().contains(&Value::String("authorization_code".to_string())),
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

        let ct = res.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.contains("application/json"), "content-type should be application/json, got: {ct}");
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
            res.status() == StatusCode::BAD_REQUEST || res.status() == StatusCode::UNPROCESSABLE_ENTITY,
            "Expected 400 or 422, got: {}", res.status()
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
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json["redirect_url"].is_string(), "response should have redirect_url");
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
```

**Step 2: Verify these tests FAIL (compile error — test_helpers doesn't exist)**

```bash
cargo test --features web --test web_oauth 2>&1 | head -20
```

Expected: Compile error about missing `charcoal::web::test_helpers` module.

**Step 3: Commit the failing tests**

```bash
git add tests/web_oauth.rs
git commit -m 'test: add failing integration tests for OAuth endpoints (Task 6 will fix)'
```

---

## Task 6: Add test_helpers module and implement the client metadata endpoint

This makes the integration tests compile, and makes the metadata + early-rejection tests pass.

**Files:**
- Create: `src/web/test_helpers.rs`
- Create: `src/web/handlers/oauth.rs`
- Modify: `src/web/handlers/mod.rs`
- Modify: `src/web/mod.rs`

**Step 1: Make `build_router` public so test_helpers can use it**

In `src/web/mod.rs`, change:

```rust
fn build_router(state: AppState) -> Router {
```

to:

```rust
pub(crate) fn build_router(state: AppState) -> Router {
```

**Step 2: Create `src/web/test_helpers.rs`**

```rust
// src/web/test_helpers.rs
// Test infrastructure: builds an in-memory Axum app for integration tests.
// Only compiled under #[cfg(test)] — never ships in production binaries.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::config::Config;
use crate::web::{build_router, AppState};
use crate::web::scan_job::ScanStatus;

pub const TEST_SECRET: &str = "test_session_secret_at_least_32_chars!";
pub const TEST_DID: &str = "did:plc:testalloweddid0000000000";
pub const TEST_CLIENT_ID: &str = "https://cimd-service.fly.dev/clients/test";

/// Build an in-memory Axum router suitable for integration tests.
/// Uses Config::test_defaults() — override fields as needed for specific tests.
pub fn build_test_app() -> axum::Router {
    let config = Config {
        #[cfg(feature = "web")]
        allowed_did: TEST_DID.to_string(),
        #[cfg(feature = "web")]
        oauth_client_id: TEST_CLIENT_ID.to_string(),
        #[cfg(feature = "web")]
        session_secret: TEST_SECRET.to_string(),
        ..Config::test_defaults()
    };

    // Use a shared in-memory fake DB. For most OAuth endpoint tests,
    // the DB isn't queried — but the /api/status endpoint needs it.
    // SqliteDatabase::open(":memory:") creates a fresh in-memory DB each call.
    let db = {
        // Inline import to keep it scoped to this helper.
        use crate::db::SqliteDatabase;
        use std::sync::Arc;
        Arc::new(
            SqliteDatabase::open(":memory:").expect("in-memory SQLite should always succeed"),
        ) as Arc<dyn crate::db::Database>
    };

    let state = AppState {
        db,
        config: Arc::new(config),
        scan_status: Arc::new(RwLock::new(ScanStatus::default())),
        pending_oauth: Arc::new(RwLock::new(HashMap::new())),
        oauth_tokens: Arc::new(RwLock::new(None)),
    };

    build_router(state)
}
```

> **Note:** If `SqliteDatabase` is not directly importable here (it may be behind a feature flag or private), adapt the import path. Check `src/db/mod.rs` for what's exported.

**Step 3: Expose test_helpers in `src/web/mod.rs`**

Near the bottom of `src/web/mod.rs`, after the `pub mod handlers;` line:

```rust
#[cfg(test)]
pub mod test_helpers;
```

**Step 4: Update `AppState` in `src/web/mod.rs` to include the new OAuth fields**

```rust
use std::collections::HashMap;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<dyn Database>,
    pub config: Arc<Config>,
    pub scan_status: Arc<RwLock<scan_job::ScanStatus>>,
    /// In-flight OAuth request states, keyed by the `state` parameter sent to the PDS.
    /// Populated by POST /api/auth/initiate; consumed by GET /api/auth/callback.
    pub pending_oauth: Arc<RwLock<HashMap<String, PendingOAuth>>>,
    /// AT Protocol tokens for the authenticated user.
    /// Stored in-memory for this milestone (lost on restart — user re-authenticates).
    pub oauth_tokens: Arc<RwLock<Option<serde_json::Value>>>,
}
```

The `PendingOAuth` type will be defined in `handlers/oauth.rs` in Task 7. For now, use `serde_json::Value` as a placeholder so it compiles:

```rust
pub type PendingOAuth = serde_json::Value; // replaced in Task 7
```

Initialize in `run_server()`:

```rust
let state = AppState {
    db,
    config: Arc::new(config),
    scan_status: Arc::new(RwLock::new(scan_job::ScanStatus::default())),
    pending_oauth: Arc::new(RwLock::new(HashMap::new())),
    oauth_tokens: Arc::new(RwLock::new(None)),
};
```

**Step 5: Create `src/web/handlers/oauth.rs` with the metadata handler and stubs**

```rust
// src/web/handlers/oauth.rs
// AT Protocol OAuth flow handlers:
//   GET  /oauth-client-metadata.json  — client metadata document (required by AT Protocol OAuth)
//   POST /api/auth/initiate           — start the OAuth flow, return redirect URL
//   GET  /api/auth/callback           — PDS posts back here; exchange code for tokens

use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::web::{api_error, AppState};

// ---- Client metadata ----

/// GET /oauth-client-metadata.json
///
/// Serves the AT Protocol OAuth client metadata document. The `client_id` must be the
/// public URL of this document — that's what CHARCOAL_OAUTH_CLIENT_ID is set to.
pub async fn client_metadata(State(state): State<AppState>) -> Response {
    let client_id = &state.config.oauth_client_id;

    // Derive the redirect URI from the client_id URL.
    // Production: client_id = "https://host/oauth-client-metadata.json"
    //             → redirect = "https://host/api/auth/callback"
    // Dev (cimd-service): client_id = "https://cimd-service.fly.dev/clients/xxx"
    //             → we still use our own /api/auth/callback (registered at cimd-service)
    let redirect_uri = derive_redirect_uri(client_id);
    let base_uri = derive_base_url(client_id);

    let metadata = serde_json::json!({
        "client_id": client_id,
        "client_name": "Charcoal",
        "client_uri": base_uri,
        "redirect_uris": [redirect_uri],
        "scope": "atproto",
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "application_type": "web",
        "dpop_bound_access_tokens": true
    });

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        Json(metadata),
    )
        .into_response()
}

fn derive_base_url(client_id: &str) -> String {
    // Strip the path, keep scheme + host
    if let Some(scheme_end) = client_id.find("://") {
        let after_scheme = &client_id[scheme_end + 3..];
        let host = after_scheme.split('/').next().unwrap_or(after_scheme);
        let scheme = &client_id[..scheme_end];
        format!("{scheme}://{host}")
    } else {
        client_id.to_string()
    }
}

fn derive_redirect_uri(client_id: &str) -> String {
    format!("{}/api/auth/callback", derive_base_url(client_id))
}

// ---- Initiate ----

#[derive(Deserialize)]
pub struct InitiateRequest {
    pub handle: String,
}

#[derive(Serialize)]
pub struct InitiateResponse {
    pub redirect_url: String,
}

/// POST /api/auth/initiate
///
/// Accept { handle }, resolve to DID, start PAR flow with user's PDS,
/// return { redirect_url } for the browser to follow.
pub async fn initiate(
    State(state): State<AppState>,
    Json(body): Json<InitiateRequest>,
) -> Response {
    let handle = body.handle.trim().to_string();
    if handle.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "handle is required");
    }

    // Full OAuth initiation implemented in Task 7.
    // This stub returns 501 so the integration test for the full flow
    // (marked #[ignore]) documents the expected behavior.
    let _ = state; // suppress unused warning until Task 7
    api_error(
        StatusCode::NOT_IMPLEMENTED,
        "OAuth flow not yet implemented — coming in Task 7",
    )
}

// ---- Callback ----

#[derive(Deserialize)]
pub struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// GET /api/auth/callback
///
/// PDS redirects here after user authenticates. Exchange the code for tokens,
/// validate the DID, set session cookie, redirect to /dashboard.
pub async fn callback(
    State(state): State<AppState>,
    Query(params): Query<CallbackParams>,
) -> Response {
    // Surface any error from the PDS
    if let Some(err) = &params.error {
        let desc = params
            .error_description
            .as_deref()
            .unwrap_or("no description");
        return api_error(
            StatusCode::BAD_REQUEST,
            &format!("OAuth error from PDS: {err} — {desc}"),
        );
    }

    // Both code and state are required
    let code = match &params.code {
        Some(c) if !c.is_empty() => c.clone(),
        _ => return api_error(StatusCode::BAD_REQUEST, "missing required parameter: code"),
    };
    let state_param = match &params.state {
        Some(s) if !s.is_empty() => s.clone(),
        _ => return api_error(StatusCode::BAD_REQUEST, "missing required parameter: state"),
    };

    // Look up and remove the pending OAuth state.
    // If it's not there, the state is invalid or expired.
    {
        let mut pending = state.pending_oauth.write().await;
        if pending.remove(&state_param).is_none() {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid or expired OAuth state — please start sign-in again",
            );
        }
    }

    // Full token exchange implemented in Task 7.
    let _ = (code, state);
    api_error(
        StatusCode::NOT_IMPLEMENTED,
        "Token exchange not yet implemented — coming in Task 7",
    )
}
```

**Step 6: Register the module in `src/web/handlers/mod.rs`**

Add:

```rust
pub mod oauth;
```

**Step 7: Add the new routes in `src/web/mod.rs`**

In `build_router()`, update `public_api`:

```rust
let public_api = Router::new()
    .route("/oauth-client-metadata.json", get(handlers::oauth::client_metadata))
    .route("/health", get(health))
    .route("/api/auth/initiate", post(handlers::oauth::initiate))
    .route("/api/auth/callback", get(handlers::oauth::callback));
```

(The old `/api/login` route was removed in Task 4.)

**Step 8: Compile check**

```bash
cargo check --features web 2>&1
```

Expected: Clean. If there are errors about `PendingOAuth` type, ensure the placeholder type alias is in place.

**Step 9: Run the integration tests**

```bash
cargo test --features web --test web_oauth 2>&1
```

Expected:
- `client_metadata_returns_200_with_correct_fields` — PASS
- `client_metadata_content_type_is_json` — PASS
- `initiate_rejects_empty_handle` — PASS
- `initiate_rejects_whitespace_only_handle` — PASS
- `initiate_rejects_missing_handle_field` — PASS
- `callback_rejects_missing_state_param` — PASS
- `callback_rejects_missing_code_param` — PASS
- `callback_rejects_unknown_state` — PASS
- `callback_surfaces_pds_error_param` — PASS
- `protected_route_returns_401_with_no_cookie` — PASS
- `protected_route_returns_403_for_wrong_did` — PASS
- `protected_route_returns_200_for_allowed_did` — PASS
- `logout_clears_session_cookie` — PASS

If any test fails, fix it before proceeding.

**Step 10: Run all tests**

```bash
cargo test --features web --all-targets 2>&1 | tail -20
```

Expected: All pass.

**Step 11: Commit**

```bash
git add src/web/mod.rs src/web/test_helpers.rs src/web/handlers/oauth.rs src/web/handlers/mod.rs
git commit -m 'feat: client metadata endpoint, OAuth handler stubs, test_helpers, AppState update'
```

---

## Task 7: Implement the full OAuth initiate and callback flow

> **Before writing any code in this task:** Open the actual crate documentation to confirm exact type names and function signatures:
>
> ```bash
> cargo doc --features web --open
> ```
>
> Navigate to `atproto_oauth::workflow` and `atproto_oauth_axum::state`. The code below uses the API shape from the crate README and v0.14.0 docs — verify field names before coding.

**Files:**
- Modify: `src/web/handlers/oauth.rs`
- Modify: `src/web/mod.rs` (update `PendingOAuth` type)

**Step 1: Define the `PendingOAuth` struct**

At the top of `src/web/handlers/oauth.rs`, add:

```rust
/// Data stored between /api/auth/initiate and /api/auth/callback.
/// Contains the DPoP key and OAuth state needed to complete the token exchange.
///
/// The exact fields depend on the atproto-oauth crate's types.
/// Check atproto_oauth::OAuthRequestState in docs.rs.
pub struct PendingOAuth {
    pub request_state: atproto_oauth::OAuthRequestState,
    pub dpop_key: atproto_oauth::DpopKey,  // or however the crate names it
}
```

Update the type alias in `src/web/mod.rs`:

```rust
pub type PendingOAuth = crate::web::handlers::oauth::PendingOAuth;
```

**Step 2: Implement the `initiate` handler**

Replace the stub with the real implementation. Key steps:
1. Resolve the handle to a DID document using `atproto_identity`
2. Build an `OAuthClient` config from `state.config`
3. Generate a DPoP key
4. Create an `OAuthRequestState`
5. Call `oauth_init()` to perform the PAR request → get back a `ParResponse` with a `redirect_url`
6. Store the pending state in `state.pending_oauth` keyed by the `state` parameter
7. Return `{ redirect_url }`

```rust
pub async fn initiate(
    State(state): State<AppState>,
    Json(body): Json<InitiateRequest>,
) -> Response {
    use atproto_identity::resolve_handle;
    use atproto_oauth::workflow::{oauth_init, OAuthClient};

    let handle = body.handle.trim().to_string();
    if handle.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "handle is required");
    }

    let http_client = reqwest::Client::new();

    // Step 1: Resolve handle to DID document (discovers PDS and auth server)
    let did_document = match resolve_handle(&http_client, &handle).await {
        Ok(doc) => doc,
        Err(e) => {
            tracing::warn!("Handle resolution failed for '{handle}': {e}");
            return api_error(StatusCode::BAD_REQUEST, "Could not resolve Bluesky handle — check spelling");
        }
    };

    // Step 2: Build OAuth client config
    let redirect_uri = derive_redirect_uri(&state.config.oauth_client_id);
    let oauth_client = OAuthClient {
        client_id: state.config.oauth_client_id.clone(),
        redirect_uri: redirect_uri.clone(),
        // private_signing_key_data: check the crate docs for the expected format
        // It may be a JWK, PEM bytes, or a generated key — confirm before coding.
    };

    // Step 3: Generate DPoP key
    // Check if atproto_oauth exports a key generation function, or if OAuthRequestState::new()
    // handles it internally. Confirm from cargo doc output.
    let dpop_key = atproto_oauth::DpopKey::generate();  // or similar

    // Step 4: Create in-flight OAuth state
    let oauth_request_state = atproto_oauth::OAuthRequestState::new();
    let state_param = oauth_request_state.state_param().to_string();

    // Step 5: Initiate PAR flow
    let par_response = match oauth_init(
        &http_client,
        &oauth_client,
        &dpop_key,
        &handle,
        &did_document,
        &oauth_request_state,
    ).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("PAR request to PDS failed for '{handle}': {e}");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not start sign-in with Bluesky — please try again",
            );
        }
    };

    // Step 6: Persist state for callback
    state.pending_oauth.write().await.insert(
        state_param,
        PendingOAuth {
            request_state: oauth_request_state,
            dpop_key,
        },
    );

    // Step 7: Return redirect URL
    (
        StatusCode::OK,
        Json(InitiateResponse {
            redirect_url: par_response.redirect_url, // confirm field name in docs
        }),
    )
        .into_response()
}
```

**Step 3: Implement the `callback` handler**

Replace the partial stub:

```rust
pub async fn callback(
    State(state): State<AppState>,
    Query(params): Query<CallbackParams>,
) -> Response {
    use atproto_oauth::workflow::{oauth_complete, OAuthClient};

    // Surface PDS errors
    if let Some(err) = &params.error {
        let desc = params.error_description.as_deref().unwrap_or("");
        return api_error(
            StatusCode::BAD_REQUEST,
            &format!("Sign-in rejected by Bluesky: {err} {desc}").trim().to_string(),
        );
    }

    let code = match &params.code {
        Some(c) if !c.is_empty() => c.clone(),
        _ => return api_error(StatusCode::BAD_REQUEST, "missing required parameter: code"),
    };
    let state_param = match &params.state {
        Some(s) if !s.is_empty() => s.clone(),
        _ => return api_error(StatusCode::BAD_REQUEST, "missing required parameter: state"),
    };

    // Consume the pending state (one-time use)
    let pending = {
        let mut map = state.pending_oauth.write().await;
        match map.remove(&state_param) {
            Some(p) => p,
            None => return api_error(
                StatusCode::BAD_REQUEST,
                "sign-in session expired or invalid — please start sign-in again",
            ),
        }
    };

    let redirect_uri = derive_redirect_uri(&state.config.oauth_client_id);
    let oauth_client = OAuthClient {
        client_id: state.config.oauth_client_id.clone(),
        redirect_uri,
        // private_signing_key_data: same as initiate handler
    };

    let http_client = reqwest::Client::new();

    // Exchange code for tokens using PKCE + DPoP
    let token_response = match oauth_complete(
        &http_client,
        &oauth_client,
        &pending.dpop_key,
        &code,
        &pending.request_state,
    ).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("Token exchange failed: {e}");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not complete sign-in — please try again",
            );
        }
    };

    // Extract DID from token (the `sub` claim is the authenticated DID)
    let authenticated_did = token_response.sub.clone(); // confirm field name in docs

    // Gate on CHARCOAL_ALLOWED_DID
    if !crate::web::auth::did_is_allowed(&authenticated_did, &state.config.allowed_did) {
        tracing::warn!(
            "Login attempt from disallowed DID '{}' (allowed: '{}')",
            authenticated_did,
            state.config.allowed_did
        );
        return api_error(
            StatusCode::FORBIDDEN,
            "This Bluesky account is not authorized to access this dashboard",
        );
    }

    // Store tokens in-memory for future XRPC calls (muting/blocking milestone)
    *state.oauth_tokens.write().await = Some(serde_json::to_value(&token_response).unwrap_or_default());

    // Issue session cookie with DID embedded
    let token = crate::web::auth::create_token(&state.config.session_secret, &authenticated_did);
    let cookie = crate::web::auth::set_cookie_header(&token, true); // secure=true in production

    // Redirect to dashboard with session cookie
    use axum::response::Redirect;
    (
        [(header::SET_COOKIE, cookie)],
        Redirect::to("/dashboard"),
    )
        .into_response()
}
```

**Step 4: Compile and fix**

```bash
cargo check --features web 2>&1
```

This step will likely have errors because the exact `atproto-oauth` API might differ from the code above. Use the error messages and `cargo doc --open` to find the correct type names and function signatures. Fix each error one at a time.

Common things to verify in the actual crate:
- Is the function called `oauth_init` or something else?
- How is `OAuthRequestState` constructed and how do you extract the `state` parameter?
- How is the DPoP key generated and attached to `OAuthRequestState`?
- What is `TokenResponse.sub` actually called?

**Step 5: Run all tests after fixing**

```bash
cargo test --features web --all-targets 2>&1 | tail -20
```

Expected: All existing tests still pass. The web_oauth integration tests still pass (they don't exercise the real OAuth flow — those are `#[ignore]`).

**Step 6: Run clippy**

```bash
cargo clippy --features web -- -D warnings
```

**Step 7: Commit**

```bash
git add src/web/handlers/oauth.rs src/web/mod.rs
git commit -m 'feat: implement OAuth initiate and callback handlers with atproto-oauth crate'
```

---

## Task 8: Update the SvelteKit login page

> **IMPORTANT:** Before editing any `.svelte` file, invoke the `svelte:svelte-code-writer` skill.

The password form is replaced with a handle input. Everything visual stays the same.

**Files:**
- Modify: `web/src/lib/api.ts`
- Modify: `web/src/routes/login/+page.svelte`

**Step 1: Update `web/src/lib/api.ts` — replace `login(password)` with `initiateAuth(handle)`**

Remove the `login` function and add:

```typescript
export async function initiateAuth(handle: string): Promise<string> {
    const res = await fetch('/api/auth/initiate', {
        method: 'POST',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ handle })
    });
    if (!res.ok) {
        const body = await res.json().catch(() => ({}));
        throw new Error(body.error ?? 'Sign-in failed — please try again');
    }
    const data = await res.json();
    return data.redirect_url as string;
}
```

**Step 2: Update `web/src/routes/login/+page.svelte` — script section**

Replace the `<script>` block (the HTML/CSS below it stays identical):

```svelte
<script lang="ts">
    import { initiateAuth } from '$lib/api.js';

    let handle = $state('');
    let isSubmitting = $state(false);
    let isFocused = $state(false);
    let errorMessage = $state('');

    async function handleSubmit(e: SubmitEvent) {
        e.preventDefault();
        if (!handle.trim() || isSubmitting) return;

        isSubmitting = true;
        errorMessage = '';

        try {
            const redirectUrl = await initiateAuth(handle.trim());
            window.location.href = redirectUrl;
        } catch (err) {
            errorMessage = err instanceof Error ? err.message : 'Sign-in failed';
        } finally {
            isSubmitting = false;
        }
    }
</script>
```

**Step 3: Update the form HTML inside the `<div class="card-inner">` block**

Change the label, input, and button:

```svelte
<form onsubmit={handleSubmit}>
    <div class="field">
        <label for="handle" class="label">Bluesky Handle</label>
        <div class="input-container" class:focused={isFocused}>
            <input
                type="text"
                id="handle"
                name="handle"
                bind:value={handle}
                onfocus={() => (isFocused = true)}
                onblur={() => (isFocused = false)}
                placeholder="yourhandle.bsky.social"
                autocomplete="username"
                required
                disabled={isSubmitting}
            />
        </div>
        {#if errorMessage}
            <p class="error">{errorMessage}</p>
        {:else}
            <p class="hint">Sign in with your Bluesky account</p>
        {/if}
    </div>

    <button type="submit" class="btn-continue" disabled={!handle.trim() || isSubmitting}>
        {#if isSubmitting}
            <span class="loading-pulse"></span>
            <span>Signing in...</span>
        {:else}
            <span>Sign in with Bluesky</span>
            <svg class="arrow" viewBox="0 0 20 20" fill="none">
                <path
                    d="M4 10h12m-4-4l4 4-4 4"
                    stroke="currentColor"
                    stroke-width="2"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                />
            </svg>
        {/if}
    </button>
</form>
```

**Step 4: Build the SvelteKit app to verify no TypeScript errors**

```bash
cd web && npm run build 2>&1 | tail -20
```

Expected: Build succeeds with no errors.

**Step 5: Type-check without building**

```bash
cd web && npm run check 2>&1
```

Expected: No errors.

**Step 6: Commit**

```bash
cd ..
git add web/src/routes/login/+page.svelte web/src/lib/api.ts
git commit -m 'feat: replace password login form with Bluesky handle field and OAuth redirect'
```

---

## Task 9: Update docs and env var reference

**Files:**
- Modify: `README.md`
- Modify: `CLAUDE.md`
- Modify: `.env.example` (if it exists)
- Modify: `railway.toml` (if it has env var docs)

**Step 1: Check what files need updating**

```bash
grep -rn "CHARCOAL_WEB_PASSWORD" . --include="*.md" --include="*.toml" --include="*.example"
```

Update every file that mentions `CHARCOAL_WEB_PASSWORD` — remove it and replace with the new vars.

**Step 2: New env var table (for README.md)**

| Variable | Required | Purpose |
|----------|----------|---------|
| `CHARCOAL_ALLOWED_DID` | Yes | Your Bluesky DID — only this account can sign in |
| `CHARCOAL_OAUTH_CLIENT_ID` | Yes | Public URL of your OAuth client metadata document |
| `CHARCOAL_SESSION_SECRET` | Yes | HMAC signing key for session cookies (min 32 chars) |
| `CHARCOAL_WEB_PASSWORD` | **Removed** | Replaced by AT Protocol OAuth |

**Finding your DID:**
```
Visit https://bsky.app → Settings → Account → your handle → copy the DID (starts with did:plc:)
```

**For development (cimd-service.fly.dev):**

Register once by POSTing your client metadata:

```bash
curl -X POST https://cimd-service.fly.dev/clients \
  -H 'Content-Type: application/json' \
  -d '{
    "client_name": "Charcoal (Dev)",
    "client_uri": "http://localhost:8080",
    "redirect_uris": ["http://localhost:8080/api/auth/callback"],
    "scope": "atproto",
    "grant_types": ["authorization_code", "refresh_token"],
    "response_types": ["code"],
    "token_endpoint_auth_method": "none",
    "application_type": "web",
    "dpop_bound_access_tokens": true
  }'
```

The response contains your `client_id` URL — set `CHARCOAL_OAUTH_CLIENT_ID` to that value.

**For production (Railway):**
Set `CHARCOAL_OAUTH_CLIENT_ID=https://{RAILWAY_PUBLIC_DOMAIN}/oauth-client-metadata.json`

**Step 3: Update CLAUDE.md current status section**

In the web GUI bullet, add: "v0.4 AT Protocol OAuth — backend-driven OAuth via `atproto-oauth-axum`, CHARCOAL_ALLOWED_DID gate, DID-embedded session cookies"

**Step 4: Commit**

```bash
git add README.md CLAUDE.md
# Add any other files changed
git commit -m 'docs: update env vars and setup guide for AT Protocol OAuth (v0.4)'
```

---

## Task 10: Manual smoke test

This is not automated. Do it once to verify the end-to-end flow works before opening the PR.

**Step 1: Register a dev client_id (one-time)**

If you haven't done this yet, run the `curl` command from Task 9 Step 2.

**Step 2: Set up .env**

```
CHARCOAL_ALLOWED_DID=did:plc:h3wpawnrlptr4534chevddo6
CHARCOAL_OAUTH_CLIENT_ID=https://cimd-service.fly.dev/clients/YOUR_CID_HERE
CHARCOAL_SESSION_SECRET=<output of: openssl rand -hex 32>
```

Remove `CHARCOAL_WEB_PASSWORD` from your .env.

**Step 3: Start the server**

```bash
cargo run --features web -- serve
```

Expected: Server starts without "CHARCOAL_ALLOWED_DID is not set" or other startup errors.

**Step 4: Verify client metadata**

```bash
curl http://localhost:8080/oauth-client-metadata.json | python3 -m json.tool
```

Expected: Valid JSON with `scope: "atproto"`, your `client_id`, etc.

**Step 5: Test the sign-in flow**

1. Visit `http://localhost:8080/login` in your browser
2. Enter your Bluesky handle (e.g. `chaosgreml.in`)
3. Click "Sign in with Bluesky"
4. Should redirect to your PDS (blacksky.app) login page
5. Authenticate
6. Should redirect back to `http://localhost:8080/dashboard` with session cookie set

**Step 6: Verify the session**

```bash
curl -c /tmp/cookies.txt http://localhost:8080/api/status
```

Expected: 401 (no cookie). After sign-in completes, run again with the cookie jar.

---

## Final verification before PR

Run everything one more time, clean:

```bash
# Rust
cargo test --features web --all-targets 2>&1 | tail -30
cargo clippy --features web -- -D warnings

# SvelteKit
cd web && npm run build && npm run check && cd ..
```

All must be green before opening the PR.

**PR title:** `feat: v0.4 — AT Protocol OAuth (replace password auth)`

**PR description:** Run `deciduous writeup` to generate from the decision graph nodes logged during this work.
