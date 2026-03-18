# Onboarding Flow Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the OAuth callback to register users in the database, then add a first-run welcome screen that guides new users through their first scan.

**Architecture:** Two sequential changes. First, thread the user's handle through the OAuth `PendingOAuth` struct so the callback can call `upsert_user()`. Second, add a client-side conditional in the dashboard Svelte component that shows a welcome screen when no accounts exist and no scan is running.

**Tech Stack:** Rust (Axum backend), SvelteKit (frontend SPA), SQLite (database)

**Spec:** `docs/superpowers/specs/2026-03-17-onboarding-flow-design.md`

---

## Chunk 1: Bug Fix — OAuth callback registers user (#121)

### Task 1: Write failing tests that prove the bug exists

**Files:**
- Modify: `src/web/test_helpers.rs` (add `build_test_app_with_db` helper)
- Modify: `tests/web_oauth.rs` (add failing tests)

- [ ] **Step 1: Add a helper that returns the DB alongside the app**

In `src/web/test_helpers.rs`, add a function that returns both the router and the DB so tests can inspect DB state:

```rust
/// Like `build_test_app`, but also returns the DB handle for state inspection.
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
        scan_status: Arc::new(RwLock::new(ScanStatus::default())),
        pending_oauth: Arc::new(RwLock::new(HashMap::new())),
        oauth_tokens: Arc::new(RwLock::new(None)),
        signing_key,
    };

    (build_router(state), db)
}
```

- [ ] **Step 2: Write tests that demonstrate the bug**

In `tests/web_oauth.rs`, update the import line to include the new helper:

```rust
    use charcoal::web::test_helpers::{build_test_app, build_test_app_with_db, TEST_DID, TEST_SECRET};
```

Then add two tests. The first proves the bug exists (scan fails when no user is registered — this is the current broken state). The second proves the fix works (scan succeeds when user IS registered via `upsert_user`):

```rust
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
            json["error"]
                .as_str()
                .unwrap_or("")
                .contains("not found"),
            "Error should mention user not found"
        );
    }

    #[tokio::test]
    async fn scan_succeeds_when_user_registered_in_db() {
        // This test proves the fix: if the user IS in the DB (as the
        // fixed OAuth callback will do), POST /api/scan should not
        // return "User not found". It will return 202 Accepted.
        let (app, db) = charcoal::web::test_helpers::build_test_app_with_db();

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

        // 202 Accepted (scan started) — NOT 500 with "User not found"
        assert_ne!(
            res.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "Scan should not fail with 'User not found' when user is registered"
        );
    }
```

- [ ] **Step 3: Run the tests to verify expected results**

Run: `cargo test --features web --test web_oauth -- --nocapture`

Expected:
- `scan_fails_when_user_not_registered` — PASSES (confirms the bug exists)
- `scan_succeeds_when_user_registered_in_db` — PASSES (upsert_user is called directly in the test, proving the contract works)
- All existing tests — PASS (no regressions)

Note: Both tests should pass at this stage because `scan_fails_when_user_not_registered` documents the current broken behavior, and `scan_succeeds_when_user_registered_in_db` calls `upsert_user` directly to prove the contract. The real TDD value is that the positive test validates our fix will work once we wire `upsert_user` into the OAuth callback.

- [ ] **Step 4: Commit the tests**

```bash
git add src/web/test_helpers.rs tests/web_oauth.rs
git commit -m 'test: add tests for user registration and scan endpoint contract

Add build_test_app_with_db helper that returns both router and DB.
Add two tests: one documenting the bug (scan fails with no user row),
one proving the fix contract (scan works when user is registered).'
```

### Task 2: Implement the fix — thread handle through OAuth callback

**Files:**
- Modify: `src/web/handlers/oauth.rs:25-30` (PendingOAuth struct)
- Modify: `src/web/handlers/oauth.rs:242-248` (PendingOAuth construction in initiate)
- Modify: `src/web/handlers/oauth.rs:441` (upsert_user call in callback)

- [ ] **Step 1: Add `handle` field to `PendingOAuth` struct**

In `src/web/handlers/oauth.rs`, add a `handle` field to the `PendingOAuth` struct:

```rust
/// Data stored between /api/auth/initiate and /api/auth/callback.
pub struct PendingOAuth {
    /// The full OAuth request state needed for token exchange.
    pub oauth_request: OAuthRequest,
    /// The authorization server metadata (needed by oauth_complete).
    pub authorization_server: atproto_oauth::resources::AuthorizationServer,
    /// The user-input handle from the initiate request.
    /// Stored here so the callback can register the user in the DB.
    pub handle: String,
}
```

- [ ] **Step 2: Store handle when constructing PendingOAuth in `initiate`**

In the `initiate` handler, around line 242-248, add the `handle` field to the `PendingOAuth` construction:

```rust
    // Step 8: Store pending state for callback
    state.pending_oauth.write().await.insert(
        oauth_state,
        PendingOAuth {
            oauth_request,
            authorization_server: authorization_server.clone(),
            handle: handle.clone(),
        },
    );
```

The `handle` variable already exists at line 122 of `initiate` — it's the trimmed user input.

- [ ] **Step 3: Add `upsert_user` call in `callback` after DID gate**

In the `callback` handler, after the DID gate check (after line 441), add the user registration call before the token storage:

```rust
    // Register the authenticated user in the database.
    // Uses the handle from the initiate step. If the user logs in again
    // with a different handle, upsert_user's ON CONFLICT updates it.
    if let Err(e) = state
        .db
        .upsert_user(&authenticated_did, &pending.handle)
        .await
    {
        tracing::error!("Failed to register user: {e}");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not register user",
        );
    }
```

This goes between the DID gate block (ending line 441) and the token storage block (starting line 443).

- [ ] **Step 4: Verify it compiles**

Run: `cargo check --features web`
Expected: Compiles with no errors.

- [ ] **Step 5: Run all tests to verify they pass**

Run: `cargo test --features web`
Expected: All tests pass, including the two new ones from Task 1. No clippy warnings.

- [ ] **Step 6: Commit the fix**

```bash
git add src/web/handlers/oauth.rs
git commit -m 'fix: register user in DB during OAuth callback

The OAuth callback authenticated users and set session cookies but
never called upsert_user(), so the users table had no row for them.
This caused POST /api/scan to fail with "User not found".

Thread the handle through PendingOAuth and call upsert_user after
the DID gate passes. Closes #121.'
```

---

## Chunk 2: First-Run Welcome Screen (#113)

### Task 3: Add welcome screen conditional to dashboard

**Files:**
- Modify: `web/src/routes/(protected)/dashboard/+page.svelte:154-233` (add welcome conditional)

The welcome screen is purely client-side (Svelte). There is no backend test to TDD against — the detection logic is a simple conditional on data already returned by `GET /api/status`. Manual verification after build.

- [ ] **Step 1: Add first-run detection and welcome UI**

In `web/src/routes/(protected)/dashboard/+page.svelte`, replace the `{:else if status}` block (lines 158-233) with a conditional that checks for first-run state. The welcome screen shows when `tier_counts.total === 0` and no scan is running.

Wrap everything inside `{:else if status}` in an `{#if}/{:else}` block:

```svelte
	{:else if status}
		{#if status.tier_counts.total === 0 && !status.scan_running}
			<!-- First-run welcome screen -->
			<div class="welcome">
				<h2 class="welcome-title">Welcome to Charcoal</h2>
				<p class="welcome-text">
					Charcoal scans your Bluesky posting history to identify accounts
					that may engage with your content in hostile ways — before it happens.
				</p>
				<p class="welcome-text">
					Your first scan will analyze your recent posts, find who's amplifying
					them, and score each account for toxicity and topic overlap. This
					usually takes a few minutes.
				</p>
				<button class="btn-scan btn-scan-welcome" onclick={handleScan} disabled={scanning}>
					{scanning ? 'Starting…' : 'Start your first scan'}
				</button>
				{#if scanError}
					<p class="scan-error">{scanError}</p>
				{/if}
			</div>
		{:else}
			<!-- Normal dashboard content -->
			<!-- (all existing tier cards, search box, events section go here unchanged) -->
		{/if}
	{/if}
```

Keep all existing content (tier cards, search box, events section) inside the `{:else}` branch, unchanged.

- [ ] **Step 2: Add welcome screen styles**

Add the following CSS to the `<style>` block in the same file:

```css
	.welcome {
		display: flex;
		flex-direction: column;
		align-items: center;
		text-align: center;
		padding: 4rem 2rem;
		max-width: 520px;
		margin: 0 auto;
	}

	.welcome-title {
		font-family: 'Libre Baskerville', Georgia, serif;
		font-size: 1.5rem;
		font-weight: 400;
		color: #fffbeb;
		margin-bottom: 1.25rem;
	}

	.welcome-text {
		font-size: 0.9375rem;
		color: #a8a29e;
		line-height: 1.6;
		margin-bottom: 1rem;
	}

	.btn-scan-welcome {
		margin-top: 1rem;
		padding: 0.75rem 2rem;
		font-size: 1rem;
	}
```

- [ ] **Step 3: Build the SvelteKit SPA**

Run: `cd /Users/bryan.guffey/Code/charcoal/web && npm run build`
Expected: Build completes with no errors.

- [ ] **Step 4: Rebuild the Rust binary to embed updated SPA**

Run: `cargo build --features web`
Expected: Compiles successfully. The `include_dir!` macro picks up the new `web/build/` output.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test --features web`
Expected: All tests pass. No regressions.

- [ ] **Step 6: Commit**

```bash
git add web/src/routes/\(protected\)/dashboard/+page.svelte
git commit -m 'feat: add first-run welcome screen for new users

When the dashboard loads with zero scored accounts and no scan
running, show a centered welcome message explaining what Charcoal
does and a "Start your first scan" button. After the scan starts,
the view flips to the normal dashboard with progress indicator.

Closes #113.'
```

### Task 4: Commit the SvelteKit build output and spec/plan docs

**Files:**
- Modify: `web/build/` (rebuilt SPA assets)
- Add: `docs/superpowers/specs/2026-03-17-onboarding-flow-design.md`
- Add: `docs/superpowers/plans/2026-03-17-onboarding-flow.md`

- [ ] **Step 1: Stage and commit the build output**

```bash
git add web/build/
git commit -m 'chore: rebuild SvelteKit SPA with welcome screen'
```

- [ ] **Step 2: Commit spec and plan docs**

```bash
git add docs/superpowers/specs/2026-03-17-onboarding-flow-design.md docs/superpowers/plans/2026-03-17-onboarding-flow.md
git commit -m 'docs: add onboarding flow design spec and implementation plan'
```

- [ ] **Step 3: Push the branch**

```bash
git push -u origin feat/onboarding-flow
```

### Task 5: Update CLAUDE.md and CHANGELOG

**Files:**
- Modify: `CLAUDE.md` (update test count if changed, add onboarding to feature list)
- Modify: `CHANGELOG.md` (add entries under Unreleased)

- [ ] **Step 1: Update CHANGELOG.md**

Add under `## [Unreleased]`:
- `Fixed: OAuth callback now registers user in database (handle + DID), fixing "User not found" on scan trigger`
- `Added: First-run welcome screen guides new users through their initial scan`

- [ ] **Step 2: Update CLAUDE.md test count if tests were added**

Verify the actual test count first: `cargo test --features web 2>&1 | grep 'test result'`. Update the count in CLAUDE.md to match.

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md CHANGELOG.md
git commit -m 'docs: update CHANGELOG and test count for onboarding flow'
```

- [ ] **Step 4: Push**

```bash
git push
```
