# Allowlist Admin UI + Minimal Scan Cooldown — Design Spec

**Date:** 2026-08-24
**Chainlink:** #309 (this feature), partially delivers #258 (cooldown)
**Branch:** `feat/allowlist-admin-ui`
**Status:** Approved by Bryan in brainstorming session; ready for implementation planning

## Problem

Production is gated: `CHARCOAL_ALLOWED_DID` limits sign-in to Bryan's DID while
onboarding is controlled. Growing the allowlist currently means editing a
Railway env var and redeploying. Worse, a person who is not on the list
completes full Bluesky OAuth and then lands on a bare page of raw JSON
(`{"error":"This Bluesky account is not authorized..."}`) — no styling, no
explanation, no path forward. Someone de-allowlisted mid-session sees a broken
dashboard rather than any explanation.

Separately (#258), there is no per-user scan cooldown: any allowed user can
re-trigger a ~4-hour scan back to back.

## Decision summary (settled with Bryan)

| Question | Decision |
|---|---|
| Denied OAuth UX | **Auto-waitlist**: completing OAuth while not allowed creates a pending access request; user sees a styled waitlist page |
| Approve action | **Two buttons**: "Approve" (grant access only) and "Approve + scan" (grant + pre-seed + enqueue first scan) |
| Deny semantics | **Sticky and invisible**: denied users see the same waitlist page as pending users; reversible by admin |
| Removing an allowed user | **Revoke access, keep data**; data deletion remains the separate existing admin action |
| Cooldown | **24h default**, `CHARCOAL_SCAN_COOLDOWN_HOURS` env (0 disables), admin trigger path bypasses, failed scans don't count |
| Architecture | **Approach 1**: one `access_requests` table, per-request PK lookup, env var + admin DIDs as lockout-proof bootstrap OR'd with the table |

Rejected alternatives: in-memory cached allowlist (cache-coherence complexity
for a lookup that is free at current scale; only works single-process);
env-var-authoritative with table-as-waitlist-only (every approval would be a
redeploy).

## Data model

New table `access_requests`, schema **v14**, both backends (SQLite mirror in
`src/db/schema.rs` `create_tables`, Postgres
`migrations/postgres/0014_access_requests.sql` which must self-record its
version per the standing rule).

| column | type | meaning |
|---|---|---|
| `did` | TEXT PRIMARY KEY | one row per person, ever |
| `handle` | TEXT NOT NULL | snapshot at request time; refreshed on each sign-in attempt |
| `status` | TEXT NOT NULL CHECK (`pending`/`allowed`/`denied`) | current access state |
| `requested_at` | TEXT NOT NULL (RFC3339) | when the row was created |
| `decided_at` | TEXT NULL (RFC3339) | last admin decision time |
| `decided_by` | TEXT NULL | admin DID who decided |

Notes:

- Rows are created two ways: automatically (disallowed DID completes OAuth →
  `pending`) or by admin grant-by-handle (→ `allowed` immediately, with
  `decided_at`/`decided_by` set).
- `denied` covers both "denied from waitlist" and "revoked after having
  access". Anyone with a non-`allowed` row sees the waitlist page; the page
  never reveals which state they're in.
- `delete_user_data` does **not** cascade to `access_requests`. The row is the
  admin's grant/deny record (DID + public handle only), not user content.
  "Delete data" and "revoke access" are independent actions.
- Timestamps as RFC3339 TEXT on both backends, matching `ScanQueueRow`.

### Trait surface (`src/db/traits.rs`)

A row struct (`AccessRequestRow`: plain `String`/`Option<String>` fields,
matching the `ScanQueueRow` idiom) and methods, implemented in both
`sqlite.rs` and `postgres.rs`:

- `get_access_request(did) -> Option<AccessRequestRow>`
- `upsert_access_request_pending(did, handle, now)` — creates `pending` if no
  row; if a row exists, refreshes `handle` only (a `denied` row stays
  `denied`, an `allowed` row stays `allowed`)
- `set_access_status(did, status, decided_by, now)` — admin decision;
  idempotent (setting the current status again is a no-op success). For
  grant-by-handle of a DID with no row, inserts directly as `allowed`
- `list_access_requests() -> Vec<AccessRequestRow>`

Exact method shapes may be adjusted during implementation planning to match
trait conventions; behavior above is the contract.

## The gate

`did_is_allowed` (currently sync, `src/web/auth.rs:151`) becomes an async
check with three OR'd clauses, evaluated in order:

1. **Env bootstrap** (unchanged constant-time compare against
   `CHARCOAL_ALLOWED_DID`) — lockout-proof: DB state can never lock Bryan out.
2. **Admin implies allowed**: any DID in `CHARCOAL_ADMIN_DIDS` passes — an
   admin cannot revoke their own access via the table.
3. **DB row** with `status = 'allowed'` (single primary-key lookup).

Open-access semantics are preserved *and sharpened*: if `CHARCOAL_ALLOWED_DID`
is empty/whitespace, the gate returns allowed without consulting the table
(current production behavior when ungated). The table only governs access when
the env gate is active.

Enforcement sites are unchanged: the OAuth callback (pre-cookie) and
`require_auth` (every authenticated request). Both are already async and hold
`state.db`.

**Failure mode:** a DB error during the gate check fails **closed** (500 via
the existing `api_error` pattern, no access granted). Never fail open.

## Flows

### Denied OAuth (the auto-waitlist)

In `src/web/handlers/oauth.rs` callback, replacing the raw-JSON 403: when the
verified DID is not allowed —

1. `upsert_access_request_pending(did, handle, now)` (refreshes handle on
   existing rows, never downgrades/upgrades status)
2. HTTP 302 redirect to `/waitlist` (this is a top-level browser navigation,
   so a redirect — not a JSON body — is the correct shape)
3. No session cookie is set; no `users` row is created

### Mid-session revocation

`require_auth`'s existing 403 for a valid-cookie-but-disallowed DID gains a
machine-readable body: `{"error": "...", "code": "access_revoked"}`.
`web/src/lib/api.ts` recognizes `code == "access_revoked"` (analogous to its
401 → `AuthError` → `/login` handling) and navigates to `/waitlist`.

### Admin endpoints

Five new routes under the existing `protected_api` router, each with the same
hand-rolled `if !auth.is_admin { 403 }` guard as the existing admin handlers:

| Route | Behavior |
|---|---|
| `GET /api/admin/access` | All rows grouped by status: `{pending: [], allowed: [], denied: []}` |
| `POST /api/admin/access` `{handle}` | Resolve handle → DID (copy the pre-seed endpoint's resolution + error mapping exactly: 404 "Handle not found", 502 for infra failures), upsert as `allowed` |
| `POST /api/admin/access/{did}/approve` | → `allowed` |
| `POST /api/admin/access/{did}/approve-scan` | → `allowed`, then pre-seed user + enqueue scan via the existing admin-scan machinery. Partial failure is reported honestly: `{access: "granted", scan: "failed to queue: ..."}` — never silent partial success |
| `POST /api/admin/access/{did}/deny` | → `denied` (doubles as revoke for currently-allowed DIDs) |

All decision endpoints are idempotent: repeating a decision returns success
without error.

### Scan cooldown (#258, minimal)

In `trigger_scan` (`src/web/handlers/scan.rs`), immediately before
`enqueue_scan`:

- Read the user's `scan_queue` row. If `status == 'done'` and `finished_at`
  is within `CHARCOAL_SCAN_COOLDOWN_HOURS` (default **24**, `0` disables),
  return **429** with `{error: <plain-language message>, retry_at: <RFC3339>}`.
- Failed scans do not start a cooldown (a user whose scan failed may retry
  immediately).
- The existing already-queued/already-running handling is untouched and
  checked first.
- `trigger_admin_scan` performs **no** cooldown check — the admin bypass.
- Config: new `Config` field read from `CHARCOAL_SCAN_COOLDOWN_HOURS`;
  document in `.env.example`. (Also document the pre-existing undocumented
  `CHARCOAL_ADMIN_DIDS` while in that file.)

## Frontend

Deliberately UI-light: this spec fixes information architecture and states;
**visual design, layout, and componentry are owned by the impeccable skill
during implementation**, working within the existing Hearth Watch design
system (`DESIGN.md`, `web/src/lib/website/styles/tokens.css` — promote
existing values into tokens, never invent new hex).

### `/waitlist` (new public SPA route)

- Reachable by redirect from the OAuth callback and by the `access_revoked`
  handler; also directly (deep link) — content is static, no auth, no API call.
- Content contract: confirms the person is on the list / access is not
  currently active; gives no status detail; pending and denied are
  indistinguishable here by design (do not provoke the population Charcoal
  exists to guard against).

### Admin page: new "Access" area

Lives on the existing `(protected)/admin/+page.svelte` alongside the current
sections (scan queue / add protected user / users table). Contains:

- **Pending requests**: handle + requested-at per row; actions: Approve,
  Approve + scan, Deny. `confirm()` before Deny, matching the page's
  destructive-action idiom.
- **Access list**: allowed and denied entries with decided-at/by; actions:
  Revoke (on allowed), Approve (on denied).
- **Grant access by handle**: the page's existing `@`-input + button + inline
  `msg-error`/`msg-success` idiom.

Known IA wrinkle, explicitly delegated to impeccable: the page will contain
two add-by-handle forms (existing pre-seed "Add Protected User" vs. new
"Grant access"). Impeccable may merge, relabel, or reposition them; the spec
requires only that both capabilities exist and are distinguishable.

Refresh behavior matches the page's existing pattern (poll only while
relevant; a simple refetch-after-action is acceptable).

### Plumbing

- `web/src/lib/api.ts`: wrappers for the five endpoints; `access_revoked`
  403 → navigate `/waitlist`.
- `web/src/lib/types.ts`: mirrored response types.
- Cooldown 429 on the dashboard renders as a calm "next scan available at
  ⟨local time⟩" notice, not an error state.
- Any non-trivial frontend logic (403 routing decision, access-list state
  shaping) is extracted to plain `.ts` modules with vitest coverage, matching
  the existing `dashboard-state.ts` pattern.

## Error handling summary

- Gate DB failure → fail closed.
- Handle resolution: 404 vs 502 mapping copied from pre-seed; shown inline.
- Approve+scan partial failure reported explicitly, never silently.
- Decision endpoints idempotent.
- OAuth denial path must never 500 on DB failure in the upsert — log and
  still redirect to `/waitlist` (the person's experience does not depend on
  our bookkeeping succeeding; the gate itself already denied them).

## Testing (TDD — tests written first, per house mandate)

**Gate unit tests** (`src/web/auth.rs` inline + integration):
- env-var member passes; admin passes without any row; `allowed` row passes;
  `pending`/`denied`/no-row fail (when env gate active)
- empty env var → open access, table not consulted
- DB error → fail closed
- revoked-mid-session: valid cookie + denied row → 403 with `access_revoked`

**Handler tests** (new `tests/` file modeled on `unit_admin.rs`, using
`build_admin_test_app_with_db`):
- all five endpoints; non-admin → 403 on each
- state machine: pending→allowed, pending→denied, denied→allowed (reversal),
  allowed→denied (revoke), idempotent repeats
- grant-by-handle: resolution success, 404, 502 paths
- approve-scan: success, and approve-succeeds/enqueue-fails reporting

**OAuth callback tests** (extend existing oauth test files):
- disallowed DID → row created `pending`, 302 to `/waitlist`, no cookie
- second attempt while `pending` → handle refreshed, still `pending`
- attempt while `denied` → stays `denied`, same redirect
- upsert failure → still redirects

**Cooldown tests**:
- within window → 429 with `retry_at`; outside window → 202
- last scan `failed` → no cooldown
- `CHARCOAL_SCAN_COOLDOWN_HOURS=0` → disabled
- admin trigger path unaffected
- queued/running precedence over cooldown check

**Schema/parity**:
- version-pin tests bumped to 14 (`schema.rs` count + version-list tests)
- Postgres parity for the new trait methods in `tests/db_postgres.rs`

**Frontend (vitest)**: extracted logic modules; token test untouched unless a
new color is promoted.

## Out of scope

- Notifying people when approved (no DM/email channel exists) — they simply
  sign in again.
- #259 privacy policy page (separate prerequisite for stranger onboarding).
- Removing the env var entirely — it stays as the bootstrap layer.
- Any change to scan admission/`admit_ready` (cooldown lives at enqueue only,
  per #257's single-admission-path invariant).
