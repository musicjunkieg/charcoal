# Onboarding Flow Design

**Date:** 2026-03-17
**Issues:** #121 (bug fix), #113 (onboarding)
**Status:** Approved

## Problem

The OAuth callback in `src/web/handlers/oauth.rs` authenticates users and sets
a session cookie, but never calls `db.upsert_user()` to register the DID and
handle in the `users` table. This causes `POST /api/scan` to fail with "User
not found — re-authenticate" because `get_user_handle()` returns `None`.

Additionally, first-time users land on an empty dashboard with no guidance on
what Charcoal does or how to start.

## Solution

Two changes, implemented in order:

### 1. Bug Fix: OAuth callback registers user (#121)

**Root cause:** The user's handle is resolved during the `initiate` handler but
not stored in the `PendingOAuth` struct. By the time `callback` runs, the
handle is gone.

**Changes to `src/web/handlers/oauth.rs`:**

- Add `handle: String` field to `PendingOAuth` struct (line 25-29)
- Store the user-input `handle` when building `PendingOAuth` in `initiate`
  (around line 242-248). We store the user-input handle (what they typed at
  login), not the canonical handle from the DID document. This is what the scan
  pipeline needs for API calls. If the user changes their handle later, the
  next login will update it via the `ON CONFLICT ... DO UPDATE` in upsert_user.
- After the DID gate passes in `callback` (after line 441), register the user.
  The callback returns `Response` (not `Result`), so use explicit error handling
  consistent with the rest of the file:
  ```rust
  if let Err(e) = state.db.upsert_user(&authenticated_did, &pending.handle).await {
      tracing::error!("Failed to register user: {e}");
      return api_error(StatusCode::INTERNAL_SERVER_ERROR, "Could not register user");
  }
  ```

**Known limitation:** If a user changes their Bluesky handle between logins,
the stored handle will be stale until they log in again.

**Testing:** Existing OAuth integration tests in `tests/web_oauth.rs` should be
updated to verify the user row exists after callback completes.

### 2. First-Run Welcome Screen (#113)

**Detection (client-side, no backend changes):**

After the dashboard fetches `GET /api/status`:
- If `status.tier_counts.total === 0` AND `!status.scan_running` → show welcome
- If a scan is running (even with zero results) → show dashboard with progress
- If accounts exist → show normal dashboard

**UI changes to `web/src/routes/(protected)/dashboard/+page.svelte`:**

A conditional `{#if}` block replaces the tier cards and account table with:

> **Welcome to Charcoal**
>
> Charcoal scans your Bluesky posting history to identify accounts that may
> engage with your content in hostile ways — before it happens.
>
> Your first scan will analyze your recent posts, find who's amplifying them,
> and score each account for toxicity and topic overlap. This usually takes a
> few minutes.
>
> **[Start your first scan]**

**Button behavior:**
- Calls `POST /api/scan` (same as existing trigger scan button)
- On 202 success: flip to normal dashboard view with scan progress indicator
- On error: show error message inline
- Button disables with spinner while POST is in flight

**Styling:** Centered on page, consistent with existing dashboard design
language. No new components or routes.

## Work Order

1. **#121** — Bug fix (high priority, blocker)
2. **#113** — Onboarding welcome screen (medium priority, blocked by #121)

## Out of Scope

- Multi-step wizard or tooltip walkthrough (can add later)
- Explanation of tier levels (dashboard itself makes this clear)
- Auto-refresh of account list after scan (#108, separate issue)
- Removing the CHARCOAL_ALLOWED_DID gate (#111, separate issue)
