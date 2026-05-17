# Admin Dashboard Design Spec

**Date**: 2026-03-27
**Status**: Approved
**Epic**: #103 (Phase 1.5)
**Subissues**: #104, #105, #106, #107

## Goal

Give the Charcoal admin (Bryan) the ability to pre-seed protected Bluesky
users by handle, trigger scans on their behalf, and view their scored data
— all from the web UI. This enables testing the scoring pipeline against
diverse user profiles without requiring those users to sign up.

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Admin identity | `CHARCOAL_ADMIN_DIDS` env var | Simplest approach; no schema change for roles. Only one admin for now. |
| Pre-seed scope | Register + build fingerprint | First scan needs overlap data to be useful. Fingerprint built in background. |
| UI organization | Single `/admin` page | Four admin actions fit on one page. Sub-routes can come later if needed. |
| Impersonation | `?as_user=` param on existing endpoints | Reuses all existing pages/components. No template duplication. |
| Scan concurrency | Per-user status tracking, global one-at-a-time gate | Models are memory-intensive on a single container. Data model supports future concurrency by removing the global gate. |

## 0. New Database Trait Methods

The following methods must be added to the `Database` trait in
`src/db/traits.rs`, with implementations in both `SqliteDatabase` and
`PgDatabase`:

| Method | Signature | Purpose |
|--------|-----------|---------|
| `list_users` | `async fn list_users(&self) -> Result<Vec<UserRow>>` | Return all rows from `users` table (DID, handle, created_at, last_login_at) |
| `get_scored_account_count` | `async fn get_scored_account_count(&self, user_did: &str) -> Result<i64>` | Count rows in `account_scores` for a user |
| `has_fingerprint` | `async fn has_fingerprint(&self, user_did: &str) -> Result<bool>` | Check if `topic_fingerprint` row exists for user |
| `delete_user_data` | `async fn delete_user_data(&self, user_did: &str) -> Result<()>` | Cascade delete from all user-scoped tables then `users`, in FK order |
| `update_last_login` | `async fn update_last_login(&self, did: &str) -> Result<()>` | Set `last_login_at = now()` on user row |

## 1. Admin Identity & Authorization

### Config

New env var: `CHARCOAL_ADMIN_DIDS` (comma-separated DIDs).

Parsed in `Config` alongside existing `allowed_did`. Empty = no admins
(admin endpoints return 403 for everyone).

### Middleware

New `require_admin` extractor in `src/web/auth.rs`:

1. Runs existing `require_auth` (validates session, extracts DID)
2. Checks if DID is in `Config::admin_dids`
3. Returns 403 if not an admin

### Impersonation Middleware

Modify the `AuthUser` extractor to support impersonation:

```rust
pub struct AuthUser {
    pub did: String,           // Original authenticated DID
    pub effective_did: String,  // Impersonated DID (or same as did)
    pub is_admin: bool,
}
```

When `as_user` query param is present:
1. Verify requester is admin (403 if not)
2. Verify target DID exists in `users` table (404 if not)
3. Set `effective_did` to the `as_user` value

All existing handlers use `auth.effective_did` for DB queries (no handler
changes needed). Write endpoints check `if auth.did != auth.effective_did`
and return 400.

This is a single middleware layer — not per-handler validation.

### Identity Endpoint

`GET /api/me` — protected endpoint, returns the authenticated user's identity:

```json
{
  "did": "did:plc:h3wpawnrlptr4534chevddo6",
  "handle": "chaosgreml.in",
  "is_admin": true
}
```

Frontend uses this to conditionally show the Admin nav link and to know
whether impersonation is available.

## 2. Admin API Endpoints

All endpoints require `require_admin`. All return JSON.

### GET /api/admin/users

List all registered users.

**Response** (200):
```json
{
  "users": [
    {
      "did": "did:plc:xxx",
      "handle": "chaosgreml.in",
      "has_fingerprint": true,
      "fingerprint_building": false,
      "last_scan_at": "2026-03-27T04:30:00Z",
      "scored_accounts": 1035,
      "last_login_at": "2026-03-27T03:00:00Z"
    }
  ]
}
```

- `has_fingerprint`: whether `topic_fingerprint` exists for this user
- `fingerprint_building`: whether the pre-seed background job is still running
- `last_login_at`: null if pre-seeded but user has never OAuth'd in

### POST /api/admin/users

Pre-seed a new protected user.

**Request**:
```json
{ "handle": "someone.bsky.social" }
```

**Flow**:
1. Resolve handle to DID via AT Protocol (`com.atproto.identity.resolveHandle`)
2. `upsert_user(did, handle)` — insert into users table
3. Spawn background task: fetch user's posts, build TF-IDF fingerprint,
   compute sentence embeddings
4. Return 202 immediately

**Response** (202):
```json
{ "did": "did:plc:yyy", "handle": "someone.bsky.social" }
```

**Errors**:
- 400: invalid handle format
- 404: handle does not resolve to a DID (verified non-existent)
- 409: user already exists in the system
- 502: handle resolution failed due to network/DNS/PLC error (retryable)

Frontend polls `GET /api/admin/users` every 3 seconds to see when
`fingerprint_building` becomes false and `has_fingerprint` becomes true.
Stops polling once no users have `fingerprint_building: true`.

### POST /api/admin/users/{did}/scan

Trigger a scan for a specific protected user.

**Flow**:
1. Verify user exists in `users` table
2. Check global scan gate — 409 if any scan is currently running
3. Launch scan using existing `launch_scan()` pipeline with the target
   user's DID and handle (not the admin's)
4. Return 202

**Response** (202):
```json
{ "message": "Scan started", "user_did": "did:plc:yyy" }
```

**Errors**:
- 404: user DID not found in users table
- 409: a scan is already running (global gate)

### DELETE /api/admin/users/{did}

Remove a pre-seeded user and all their data.

**Flow**:
1. Verify user exists and is not the requesting admin
2. Delete from: `account_scores`, `amplification_events`, `topic_fingerprint`,
   `scan_state`, `user_labels`, `inferred_pairs`, `users` (in order for FK constraints)
3. Return 200

**Response** (200):
```json
{ "deleted": "did:plc:yyy" }
```

**Errors**:
- 400: cannot delete yourself
- 404: user not found
- 409: a scan is currently running for this user (must wait for completion)

## 3. Impersonation (View-As-User)

### Backend

All existing protected endpoints accept an optional `as_user` query parameter:

```
GET /api/status?as_user=did:plc:yyy
GET /api/accounts?as_user=did:plc:yyy
GET /api/accounts/{handle}?as_user=did:plc:yyy
GET /api/events?as_user=did:plc:yyy
GET /api/fingerprint?as_user=did:plc:yyy
GET /api/review?as_user=did:plc:yyy
GET /api/accuracy?as_user=did:plc:yyy
```

When `as_user` is present:
1. Middleware verifies the requester is an admin
2. The effective `user_did` for DB queries becomes the `as_user` value
3. Non-admins sending `as_user` get 403

Impersonation is **read-only** — `POST /api/scan` and
`POST /api/accounts/{did}/label` reject requests with `as_user` (400).
Scans must go through the admin scan endpoint. Labels are per-user and
should only be set by the actual user or via their own session.

### Frontend

When `as_user` is set in the URL:
- All API calls from `api.ts` append `?as_user=` to requests
- An amber banner appears at the top of every page:
  "Viewing as **@handle** (read-only)" with an "Exit" button
- The "Exit" button clears `as_user` from the URL and redirects to `/admin`
- The `as_user` value is stored as a URL search param (not session state)
  so it's shareable and doesn't persist unexpectedly

## 4. Scan Status Refactor

### Current State

`AppState::scan_status` is `Arc<RwLock<ScanStatus>>` — single global status.

### New State

```rust
pub struct ScanManager {
    /// Per-user scan status
    statuses: HashMap<String, ScanStatus>,
    /// DIDs currently building fingerprints
    fingerprint_building: HashSet<String>,
    /// Global gate: only one scan at a time (for now)
    any_running: bool,
}
```

Wrapped in `Arc<RwLock<ScanManager>>` on `AppState`.

**Scan methods** (all operate within a single write lock to prevent TOCTOU):

- `try_start_scan(user_did) -> Result<(), ScanGateError>` — atomically
  checks `any_running` and sets it + inserts per-user status. Returns error
  if gate is closed. Single write lock = no race condition.
- `finish_scan(user_did)` — sets `any_running = false`, updates per-user
  status with completion time
- `get_status(user_did)` — returns status for a specific user
- `is_scan_running_for(user_did)` — checks if a specific user has a running scan

**Fingerprint tracking methods:**

- `start_fingerprint_build(user_did)` — adds DID to `fingerprint_building` set
- `finish_fingerprint_build(user_did)` — removes DID from set
- `is_fingerprint_building(user_did) -> bool` — check status

`GET /api/status` uses the effective user DID (own or impersonated).
`GET /api/admin/users` checks both `has_fingerprint` (DB) and
`fingerprint_building` (ScanManager) for each user.

Future: remove the `any_running` gate to enable concurrent scans per user.

## 5. Schema Changes

### users table

Add column:

```sql
ALTER TABLE users ADD COLUMN last_login_at TEXT;
```

Updated on each successful OAuth callback. `NULL` means pre-seeded but
never logged in.

Schema version bumps to **v7**. Migration adds the column via the existing
versioned migration system in `src/db/schema.rs` (SQLite) and
`migrations/postgres/0007_last_login_at.sql` (Postgres).

## 6. OAuth Match (#107)

When a user signs in via OAuth:
1. `upsert_user(did, handle)` already runs — creates or updates the user row
2. Set `last_login_at = now()` on the user row
3. All pre-seeded data (fingerprint, scan results, scores) is already keyed
   by DID — the user sees it immediately

No merge logic needed. The pre-seeded data is already the user's data.

## 7. Frontend: Admin Page

Single route at `/admin` in the `(protected)` layout group.

### Sections

1. **Pre-seed form**: Handle input + "Add User" button. Shows inline
   success/error feedback. Disabled while a pre-seed operation is in progress.

2. **Protected Users table**: Lists all users from `GET /api/admin/users`.
   Columns: Handle, Fingerprint status (Ready/Building), Last Scan,
   Scored Accounts, Actions (Scan/View buttons). Scan button disabled if
   fingerprint not ready or a scan is running. View button navigates to
   `/dashboard?as_user=did`.

3. **Impersonation banner**: Shown in the root `(protected)/+layout.svelte`
   when `as_user` URL param is present. Amber background, "Viewing as
   @handle (read-only)" text, Exit button.

### Navigation

Admin nav link appears in the header only when `GET /api/me` returns
`is_admin: true`. Uses the existing nav pattern in `+layout.svelte`.

## 8. Testing Strategy

### Backend

- Unit tests for `require_admin` middleware (admin DID passes, non-admin 403)
- Unit tests for `as_user` extraction (admin + as_user works, non-admin + as_user 403)
- Integration tests for each admin endpoint (pre-seed, list, scan trigger, delete)
- Integration test for OAuth match (pre-seeded user logs in, sees data)
- Test global scan gate (second scan returns 409)

### Frontend

- Manual testing of admin page flow: pre-seed → poll → scan → view
- Verify impersonation banner appears/disappears correctly
- Verify non-admin users don't see Admin nav link

## 9. Security Considerations

- Admin endpoints gated by DID check, not just authentication
- Impersonation is read-only — no writes through `as_user`
- `DELETE /api/admin/users/{did}` prevents self-deletion
- `as_user` param is validated against the users table (can't impersonate
  a non-existent user)
- Admin DID list is env-var only — no runtime modification
- All admin actions logged via `tracing::info!` with admin DID and target DID
  (pre-seed, scan trigger, delete, impersonation start)

### Known Limitations

- `oauth_tokens` in `AppState` is a single `Option<Value>` slot, not per-user.
  This does not affect admin dashboard (read-only impersonation, no XRPC writes).
  Will need refactoring to `HashMap<String, Value>` when Phase 4 (block/mute)
  arrives.
