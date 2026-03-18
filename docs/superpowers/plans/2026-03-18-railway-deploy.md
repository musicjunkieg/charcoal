# Railway Deployment Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prepare Charcoal for Railway deployment by adding auto-download of ONNX models on startup, making the DID gate optional for multi-user access, and deploying to Railway with a custom domain.

**Architecture:** Two code changes (auto-download models on serve startup, optional DID allowlist) plus Railway infrastructure setup (project, Postgres addon, persistent volume, custom domain, env vars).

**Tech Stack:** Rust (Axum), Railway (Nixpacks), Postgres (Railway addon), Porkbun DNS

**Spec:** `docs/superpowers/specs/2026-03-18-railway-deploy-design.md`

---

## Chunk 1: Code Changes

### Task 1: Write tests for optional DID gate

**Files:**
- Modify: `tests/unit_oauth.rs` (update existing gate tests, add new ones)

- [ ] **Step 1: Update the existing "empty allowed DID" test and add comma-separated tests**

In `tests/unit_oauth.rs`, the `gate_tests` module needs updates. The test
`empty_allowed_did_rejects_everything` at line 99 currently asserts that an
empty allowlist rejects all DIDs. Change it to assert the opposite (open access).
Add tests for comma-separated allowlist behavior.

```rust
    #[test]
    fn empty_allowed_did_allows_everyone() {
        // If CHARCOAL_ALLOWED_DID is not set, all DIDs pass (open access).
        assert!(did_is_allowed(ALLOWED, ""));
    }

    #[test]
    fn comma_separated_allowlist_first_entry() {
        let allowlist = "did:plc:h3wpawnrlptr4534chevddo6,did:plc:other000000000000000";
        assert!(did_is_allowed(ALLOWED, allowlist));
    }

    #[test]
    fn comma_separated_allowlist_second_entry() {
        let allowlist = "did:plc:other000000000000000,did:plc:h3wpawnrlptr4534chevddo6";
        assert!(did_is_allowed(ALLOWED, allowlist));
    }

    #[test]
    fn comma_separated_allowlist_rejects_unlisted() {
        let allowlist = "did:plc:other000000000000000,did:plc:another0000000000000";
        assert!(!did_is_allowed(ALLOWED, allowlist));
    }

    #[test]
    fn comma_separated_allowlist_trims_whitespace() {
        let allowlist = "did:plc:other000000000000000 , did:plc:h3wpawnrlptr4534chevddo6 ";
        assert!(did_is_allowed(ALLOWED, allowlist));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --features web --test unit_oauth -- gate_tests --nocapture`
Expected: `empty_allowed_did_allows_everyone` FAILS (currently returns false).
Comma-separated tests FAIL (currently does single-string comparison).

- [ ] **Step 3: Commit the tests**

```bash
git add tests/unit_oauth.rs
git commit -m 'test: add tests for optional DID gate and comma-separated allowlist

Update empty_allowed_did test to expect open access when unset.
Add tests for comma-separated DID allowlist behavior.'
```

### Task 2: Implement optional DID gate

**Files:**
- Modify: `src/web/auth.rs:119-121` (did_is_allowed function)
- Modify: `src/web/mod.rs:61-67` (remove bail guard)

- [ ] **Step 1: Update `did_is_allowed` in `src/web/auth.rs`**

Replace the current implementation at lines 115-121:

```rust
/// Check whether a DID is allowed to authenticate.
///
/// Returns `true` if `allowed_did` is empty (open access — no gate configured).
/// When `allowed_did` is set, supports comma-separated DIDs for allowlist.
/// Uses constant-time comparison to avoid timing oracle on the DID.
pub fn did_is_allowed(did: &str, allowed_did: &str) -> bool {
    if allowed_did.is_empty() {
        return true;
    }
    allowed_did
        .split(',')
        .any(|entry| constant_time_eq(did, entry.trim()))
}
```

- [ ] **Step 2: Remove the `bail!` guard in `src/web/mod.rs`**

Remove lines 61-67 (the `allowed_did.is_empty()` bail):

```rust
    // DELETE THIS BLOCK:
    // if config.allowed_did.is_empty() {
    //     anyhow::bail!(
    //         "CHARCOAL_ALLOWED_DID is not set. ..."
    //     );
    // }
```

Keep the other guards (`oauth_client_id` and `session_secret` checks).

- [ ] **Step 3: Run the DID gate tests**

Run: `cargo test --features web --test unit_oauth -- gate_tests --nocapture`
Expected: All gate tests pass, including the new ones.

- [ ] **Step 4: Run the full test suite**

Run: `cargo test --features web`
Expected: All tests pass. The OAuth integration tests in `web_oauth.rs` use
`TEST_DID` with `build_test_app` which sets `allowed_did: TEST_DID.to_string()`,
so the auth middleware still works (non-empty allowlist).

- [ ] **Step 5: Commit**

```bash
git add src/web/auth.rs src/web/mod.rs
git commit -m 'feat: make DID gate optional for multi-user open access

When CHARCOAL_ALLOWED_DID is unset or empty, all Bluesky users can
sign in via OAuth. When set, supports comma-separated DIDs for
allowlist. Remove the bail guard in run_server that prevented
startup without the env var.

Closes #111.'
```

### Task 3: Add auto-download of ONNX models on serve startup

**Files:**
- Modify: `src/main.rs:718-724` (serve command)

- [ ] **Step 1: Add model download check before server startup**

In `src/main.rs`, in the `Commands::Serve` arm (around line 718-724), add a
model check before calling `run_server`. The current code is:

```rust
Commands::Serve { port, bind } => {
    let config = config::Config::load()?;
    let db = open_database(&config).await?;
    info!(port, %bind, "Starting Charcoal web server");
    charcoal::web::run_server(config, db, port, &bind).await?;
}
```

Change to:

```rust
Commands::Serve { port, bind } => {
    let config = config::Config::load()?;
    let db = open_database(&config).await?;

    // Auto-download ONNX models if not present (required for scans).
    // On Railway with a persistent volume, this only happens on first deploy.
    if !charcoal::toxicity::download::model_files_present(&config.model_dir)
        || !charcoal::toxicity::download::embedding_files_present(&config.model_dir)
    {
        info!("Checking ONNX models — some files missing, downloading...");
        charcoal::toxicity::download::download_model(&config.model_dir).await?;
        info!("ONNX models ready");
    }

    info!(port, %bind, "Starting Charcoal web server");
    charcoal::web::run_server(config, db, port, &bind).await?;
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check --features web,postgres`
Expected: Compiles with no errors.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test --features web`
Expected: All tests pass. The serve command isn't exercised in tests, but
the download functions are tested.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m 'feat: auto-download ONNX models on serve startup

Check for model files before starting the web server. If missing,
download from HuggingFace automatically. On Railway with a persistent
volume, this only runs on first deploy.

Closes #112.'
```

### Task 4: Commit spec, plan, and push

**Files:**
- Add: `docs/superpowers/specs/2026-03-18-railway-deploy-design.md`
- Add: `docs/superpowers/plans/2026-03-18-railway-deploy.md`

- [ ] **Step 1: Commit docs**

```bash
git add docs/superpowers/specs/2026-03-18-railway-deploy-design.md docs/superpowers/plans/2026-03-18-railway-deploy.md
git commit -m 'docs: add Railway deployment design spec and implementation plan'
```

- [ ] **Step 2: Update CHANGELOG**

Add under `## [Unreleased]`:
- `Added: Auto-download ONNX models on serve startup for Railway deployment`
- `Changed: CHARCOAL_ALLOWED_DID is now optional — open to all when unset`

- [ ] **Step 3: Commit and push**

```bash
git add CHANGELOG.md
git commit -m 'docs: update CHANGELOG for Railway deploy changes'
git push -u origin feat/railway-deploy
```

---

## Chunk 2: Railway Infrastructure Setup

These steps are manual (Railway dashboard + DNS), not code.

### Task 5: Create Railway project and configure services

- [ ] **Step 1: Create Railway project**

Run: `railway init` or create via Railway dashboard.

- [ ] **Step 2: Add Postgres addon**

In Railway dashboard, add a Postgres database. This auto-sets `DATABASE_URL`.
Verify pgvector is available: connect to the database and run
`CREATE EXTENSION IF NOT EXISTS vector;` — the Charcoal migrations will do
this automatically, but verify it works.

- [ ] **Step 3: Add persistent volume**

In Railway dashboard, add a volume:
- Mount path: `/data/models`
- Size: 1GB (models are ~216MB, room for growth)

- [ ] **Step 4: Set environment variables**

In Railway dashboard, set:
- `CHARCOAL_MODEL_DIR=/data/models`
- `CHARCOAL_OAUTH_CLIENT_ID=https://charcoal.watch/oauth-client-metadata.json`
- `CHARCOAL_SESSION_SECRET=<generate with: openssl rand -base64 32>`
- `RUST_LOG=charcoal=info`

Do NOT set: `CHARCOAL_ALLOWED_DID`, `BLUESKY_HANDLE`, `BLUESKY_APP_PASSWORD`,
`CHARCOAL_DB_PATH`.

- [ ] **Step 5: Connect GitHub repo and deploy**

In Railway dashboard, connect the `musicjunkieg/charcoal` GitHub repo.
Set deploy branch to `main`. Railway will auto-build using `railway.toml`.

- [ ] **Step 6: Wait for first boot**

Monitor Railway logs. Expect:
1. Nixpacks build (~5-10 min for Rust)
2. Model download (~60s)
3. "Charcoal dashboard listening on http://0.0.0.0:$PORT"
4. Health check goes green

### Task 6: Configure custom domain

- [ ] **Step 1: Generate Railway domain**

In Railway dashboard, go to the service settings → Networking → Custom Domain.
Add `charcoal.watch`. Railway will show the CNAME target.

- [ ] **Step 2: Configure DNS in Porkbun**

In Porkbun dashboard for `charcoal.watch`:
- If Railway provides a CNAME target: add an ALIAS record for `@` pointing to
  it (Porkbun supports ALIAS for apex domains — do NOT use CNAME on apex)
- If ALIAS is not available: add a CNAME for `www` pointing to Railway's target,
  then set up a redirect from the apex to `www.charcoal.watch`

- [ ] **Step 3: Wait for SSL provisioning**

Railway auto-provisions SSL via Let's Encrypt. May take a few minutes after
DNS propagation.

- [ ] **Step 4: Verify OAuth flow**

Visit `https://charcoal.watch`:
1. Landing page should load (splash page with "Engage with confidence")
2. Click "Sign in"
3. Enter a Bluesky handle, complete OAuth flow
4. Should land on dashboard with welcome screen
5. Click "Start your first scan" — should work end-to-end
