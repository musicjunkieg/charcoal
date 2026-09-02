# #315 — Tier-based mute/block actions

**Status:** design approved by Bryan 2026-09-01, awaiting written-spec review.
**Chainlink:** #315. **Prerequisite doc:** #261 (self-protective invariant), written as
the first task of this work.

## 1. Goal

Let a user act on Charcoal's output from inside Charcoal: mute or block every
account in a chosen tier in one confirmed step, mute or block a single account
from its detail page, see everything Charcoal has done on their behalf, and
undo any of it. Every action is reviewable and reversible (PRODUCT.md
principle 3).

Bryan's stated bar: *"I want this done well. Full confidential client. These
are the expectations today."*

### In scope

- Manual bulk mute / block by tier from the accounts list, with a confirm sheet.
- Per-account mute / block / undo on the account detail page.
- A new `/actions` page: write-access status, action log, undo, retry.
- Incremental OAuth consent: login stays read-only; write scopes are requested
  on the first action, using **granular scopes only**.
- Per-user OAuth sessions persisted with all secrets **encrypted at rest**, with
  refresh-token rotation.
- Reconciliation with blocks/mutes the user already has.
- `#261` invariant document.

### Out of scope (separate issues)

- Automatic action policies ("auto-block High"). The action log and undo model
  here are designed so that feature layers on top without schema changes.
- Charcoal-managed moderation lists, exports, or any shared/named list.
- A broad-scope (`transition:generic`) fallback for PDSes that reject granular
  scopes — tracked as a follow-up, opt-in if ever built.
- Privacy-page wording for stored tokens and the action log — belongs to #259,
  which gains this as a dependency.

## 2. The invariant (#261)

Written first, as `docs/self-protective-invariant.md`, and linked from
PRODUCT.md:

> Charcoal's outputs act only on the user's own experience and reach.

Consequences that bind this feature:

- A block is a public but **self-scoped** record in the user's own repo — the
  same record the Bluesky app creates when the user taps Block. Charcoal never
  attaches a reason, tier, score, or its own name to anything it writes.
- A mute is private to the user.
- Charcoal never creates a list, export, or share surface from its results.
- Charcoal never acts with one user's credentials on another user's behalf
  (see impersonation, §7).

## 3. OAuth: consent, scopes, storage

### 3.1 Current state

Charcoal is already a confidential client (`private_key_jwt`, ES256, DPoP-bound
tokens; `src/web/handlers/oauth.rs`). Tokens are currently stashed in a single
global in-memory slot (`AppState.oauth_tokens`) that is not per-user, not
persisted, and never refreshed. That slot is **removed** by this work.

### 3.2 Incremental consent

- Login is unchanged: scope `atproto`, read-only.
- The first Mute or Block click with no write session for the user redirects
  to the PDS authorization endpoint requesting **exactly** the write abilities
  needed: create/delete on the `app.bsky.graph.block` collection and the
  `app.bsky.graph.muteActor` / `app.bsky.graph.unmuteActor` RPCs. The scope
  string is built by one function (`write_scope()`), so it exists in one place.
- **Spike task (first implementation task):** verify the exact granular scope
  syntax against a live Bluesky-hosted PDS with a throwaway account, and record
  the working string in the spec's implementation notes. The syntax is newer
  than the vendored crate's docs, so it is confirmed, not assumed.
- The pending action (which page, which tier or handle, which kind) is stashed
  server-side keyed by the OAuth `state` parameter — the same shape as
  `pending_oauth` — so after consent the user lands back on the confirm sheet
  they started from.

### 3.3 When the PDS rejects granular scopes

If authorization fails with `invalid_scope` (expected on some older self-hosted
PDSes), Charcoal shows: *"Your Bluesky host doesn't support fine-grained
permissions yet, so Charcoal can't mute or block for you. You can still use
everything else."* No session row is written. Charcoal does **not** escalate
to `transition:generic`; least privilege is the point of the confidential
client.

### 3.4 `oauth_sessions` table

One row per user (primary key `user_did`).

| column | notes |
|---|---|
| `user_did` | PK, FK-equivalent to `users` |
| `pds_url` | authorization server / PDS base |
| `scope` | scope string actually granted (from the token response) |
| `access_token_enc` | AES-256-GCM ciphertext, nonce-prefixed |
| `refresh_token_enc` | same |
| `dpop_key_enc` | the DPoP private key, same |
| `access_expires_at` | unix seconds |
| `created_at`, `updated_at` | |

### 3.5 Encryption at rest

- Algorithm: AES-256-GCM via the `aes-gcm` crate (RustCrypto). Each value is
  stored as `nonce(12) || ciphertext || tag`, with the column name bound as
  associated data so a ciphertext cannot be moved between columns.
- Key: `CHARCOAL_TOKEN_KEY`, 32 bytes hex (64 chars). Generated once per
  environment (`openssl rand -hex 32`), **never derived** from
  `CHARCOAL_SESSION_SECRET`, so rotating one does not invalidate the other.
- Missing or malformed key: the web server logs a warning at startup and
  disables the actions feature (all action endpoints return 503 with
  `actions_disabled`; the UI hides the buttons). Login and scanning are
  unaffected. It never crashes the process.
- Rotation: out of scope for launch. The plan is a v2 with a key-id byte
  prefix; noted here so the byte layout leaves room (the first byte of the
  stored blob is a format version, currently `1`).

### 3.6 Refresh

- Before any PDS call, if `access_expires_at` is within 60 seconds, refresh via
  `atproto_oauth::workflow::oauth_refresh`.
- AT Protocol refresh tokens are **single-use**. Two concurrent refreshes would
  race, and the loser's stale refresh token would log the user out. Refresh is
  therefore serialized per user: a Postgres advisory lock keyed on the DID for
  `PgDatabase`, an in-process per-DID `tokio::sync::Mutex` for SQLite. The new
  token pair is written in one transaction.
- A refresh failure with `invalid_grant` (expired or revoked upstream) deletes
  the session row; see §6.

### 3.7 Disconnect and deletion

- `/actions` has **Disconnect** — deletes the row and calls the PDS token
  revocation endpoint (best effort; the row is gone regardless).
- `delete_user_data` wipes `oauth_sessions`, `action_batches`, and `actions`.

## 4. Actions engine

### 4.1 Tables

**`action_batches`** — one row per user request.

| column | notes |
|---|---|
| `id` | PK |
| `user_did` | |
| `kind` | `mute` \| `block` \| `undo` |
| `source` | `tier:High` etc., `single`, `undo:<batch_id>`, or `retry:<batch_id>` |
| `requested` | count of targets |
| `status` | `queued` \| `running` \| `done` \| `partial` \| `failed` |
| `error` | batch-level error, nullable |
| `created_at`, `started_at`, `finished_at` | |

**`actions`** — one row per target account.

| column | notes |
|---|---|
| `id` | PK |
| `batch_id` | FK |
| `user_did` | denormalized for per-user queries |
| `target_did` | |
| `kind` | `mute` \| `block` |
| `status` | `pending` \| `applied` \| `skipped_already_done` \| `failed` \| `undone` |
| `record_uri` | the `app.bsky.graph.block` record Charcoal created, nullable |
| `undo_of` | for rows in an `undo` batch: the original action id; null otherwise |
| `error` | nullable |
| `score_at_action`, `tier_at_action` | snapshot so the log can explain itself after rescans |
| `applied_at`, `undone_at` | |

Indexes: `actions(user_did, target_did, kind)` for reconciliation and the
detail-page state; `actions(batch_id)`; `action_batches(user_did, created_at)`.

Both backends. One migration (v15). The Postgres migration self-records its
version (`INSERT INTO schema_version … ON CONFLICT DO NOTHING`).

### 4.2 Execution

Confirming a batch inserts the batch and its `pending` rows, then hands the
batch id to a background task (`ActionRunner`, same shape as scan execution) so
the request returns immediately and the page polls `/api/actions/<id>`.

The runner, per batch:

1. Loads the session, refreshing if needed (§3.6). No session → §6.
2. **Reconciles first.** Fetches the user's current blocks (`getBlocks`) or
   mutes (`getMutes`) and marks any target already in place as
   `skipped_already_done`. Charcoal never re-creates something the user set up
   themselves, and (because `record_uri` stays null) never later deletes it.
3. **Blocks:** `com.atproto.repo.applyWrites` in chunks of ≤200 creates. Each
   returned URI is stored on its row as `applied`.
   **Mutes:** no batch endpoint exists; `muteActor` is called one target at a
   time with a ~100 ms gap. A 500-account mute batch takes about a minute and
   shows as live progress.
4. Each row is written as it settles. A failure on one account never aborts the
   batch. The batch ends `done` (all applied/skipped), `partial` (some failed),
   or `failed` (nothing could be attempted, e.g. a reconcile read that never
   succeeded). `partial` and `failed` batches expose **Retry failed**, which
   creates a new batch over the rows that failed *or* never ran — a batch that
   died before the write step leaves every row `pending`, and those are exactly
   the work still outstanding.

### 4.3 Undo

An undo is a batch of kind `undo`; each of its rows carries the original
row's `kind` and points at it via `undo_of`.

- Blocks: `applyWrites` deletes over the stored `record_uri`s **only** — never
  over anything discovered via `getBlocks`.
- Mutes: `unmuteActor` per target. If the user already unmuted them, it is a
  no-op success, not an error.
- Original rows flip to `undone` with `undone_at` only when something was
  actually removed.
- **Undo is offered only for rows with status `applied`** — the ones Charcoal
  itself applied. A `skipped_already_done` row is the user's own mute or
  block: it is shown as in force (it greys the button and dedupes new
  batches), but it is never undone. Undo is available per row and per batch.

### 4.4 Tier drift

Later scans may move an acted-on account below the tier it was acted on at.
Nothing is auto-reversed in manual mode. `/actions` shows a *"since dropped to
Watch"* note by comparing `tier_at_action` with the current `account_scores`
row. That comparison is the hook a future auto-policy grows from.

### 4.5 Resume after restart

On web-server boot, the runner re-enqueues any batch in `queued` or `running`.
Reconcile-first makes a re-run idempotent: rows already `applied` are skipped by
status, and anything the PDS already has is skipped by reconciliation.

## 5. UI

Follows DESIGN.md; reuses the tier pills, `tierClass`, and table patterns in
`web/src/routes/(protected)/accounts/`.

### 5.1 Accounts list (`/accounts`)

When a tier filter is active, a bulk bar appears above the table:
*"N accounts in High · [Mute all] [Block all]"*. Either button opens a
**confirm sheet**:

- One plain-language sentence per kind. Mute: *"You stop seeing them. They
  won't know."* Block: *"They can't see, reply to, or quote you. Blocks are
  visible to anyone who looks."*
- The account list with checkboxes, all pre-checked. Each row shows handle,
  tier, and the top signal so unchecking is an informed choice (principle 2:
  never act on a tier alone).
- Accounts Charcoal has already acted on (an `applied` or
  `skipped_already_done` row in `actions`) show greyed *"already muted"* and
  are not counted. Blocks or mutes the user made outside Charcoal are not
  known until the batch runs; the runner's reconciliation step (§4.2) catches
  those and reports them as skipped.
- Confirm starts the batch and routes to `/actions/<id>`.

### 5.2 Account detail (`/accounts/[handle]`)

An action row with Mute and Block. Each flips to *Muted ✓ · Undo* once applied.
The one-line explanation appears on first use.

### 5.3 Actions (`/actions`, new route in the nav)

- Top: write-access status — *"Connected to your Bluesky account for mute and
  block (fine-grained permissions) · Disconnect"* or *"Not connected"*. This is
  the only place the grant is surfaced.
- Batches newest-first; each expands to per-account rows with status,
  tier-at-time, the drift note, and Undo per row or per batch.
- A running batch shows live progress via polling. `partial` batches show
  failures with Retry.

### 5.4 Consent interstitial

Clicking Mute or Block without a write session shows: *"Charcoal needs
permission to mute or block on your behalf. You'll approve exactly these two
abilities on Bluesky — nothing else."* then redirects. On return the user lands
on the confirm sheet they started from (§3.2).

Copy tone: quiet-hearth, no exclamation points.

## 6. Failure handling

| situation | behaviour |
|---|---|
| Consent denied, or `invalid_scope` | Return to origin page with the one-line message; no session row. |
| Refresh token expired / revoked (`invalid_grant`) | Session row deleted before any write. Batch stays `queued`; `/actions` shows *"Not connected — reconnect to continue"*; batch resumes after reconnect. |
| 429 mid-batch | Back off using the PDS `ratelimit-reset` header, then continue. A 429 never fails a row until the retry ceiling (`RATE_LIMIT_MAX_WAITS`). |
| Per-row 4xx (e.g. target DID gone) | Row `failed` with the error; batch continues. |
| Transport error / 5xx | Retry the row up to 3× with backoff, then `failed`. |
| Server restart | §4.5. |
| `CHARCOAL_TOKEN_KEY` missing | Feature disabled (503 `actions_disabled`), buttons hidden, startup warning. |

Errors are stringified after redaction; logs never contain tokens, DPoP keys,
or key material.

## 7. Security edges

- Every action endpoint uses `AuthUser.effective_did` for **reads** and refuses
  with 403 `impersonation_forbidden` when `did != effective_did` for any
  **write** (start batch, undo, retry, connect, disconnect). Admins can view
  another user's `/actions`; they can never act with that user's credentials.
- No endpoint takes a user DID from the client; targets are DIDs already in the
  user's own `account_scores`.
- The OAuth `state` used for the write-consent round-trip is bound to the
  logged-in DID; a callback whose `sub` differs from the stashed DID is
  rejected (same DID-binding rule as login, from #309's review fix).
- Session cookie semantics are unchanged; the write session lives only
  server-side.

## 8. API surface

All under the existing `require_auth` layer.

| method | path | purpose |
|---|---|---|
| `GET` | `/api/actions/status` | write session present? scope, connected_at, feature enabled? |
| `POST` | `/api/actions/connect` | begin write-consent; body = pending action to resume |
| `GET` | `/oauth/callback` | existing callback, extended to complete write consent and resume |
| `POST` | `/api/actions/disconnect` | revoke + delete session |
| `POST` | `/api/actions/batches` | `{kind, source, targets:[did…]}` → batch id |
| `GET` | `/api/actions/batches?limit&offset` | list with counts and drift flags |
| `GET` | `/api/actions/batches/{id}` | batch + rows |
| `POST` | `/api/actions/batches/{id}/undo` | undo whole batch |
| `POST` | `/api/actions/batches/{id}/retry` | new batch over failed and never-run rows; 409 `batch_running` while the batch is still queued/running |
| `POST` | `/api/actions/{action_id}/undo` | undo one row |
| `GET` | `/api/accounts/{handle}/actions` | current mute/block state for the detail page |

## 9. Code layout

- `src/web/actions/` — new module: `crypto.rs` (encrypt/decrypt), `session.rs`
  (load/store/refresh with per-user locking), `scope.rs` (`write_scope()`),
  `runner.rs` (batch execution, reconcile, undo, resume), `pds.rs` (typed
  `applyWrites` / `muteActor` / `unmuteActor` / `getBlocks` / `getMutes`
  client over the DPoP-authenticated `reqwest` client).
- `src/web/handlers/actions.rs` — the endpoints above.
- `src/db/traits.rs` gains the `oauth_sessions`, `action_batches`, `actions`
  methods; implemented in `sqlite.rs` / `queries.rs` and `postgres.rs`.
- `web/src/routes/(protected)/actions/` and `actions/[id]/`; `web/src/lib/`
  gains `ConfirmSheet.svelte` and `ActionButtons.svelte`.

## 10. Testing (TDD, both backends)

- **Unit:** encrypt/decrypt round-trip; tamper and wrong-column AAD rejection;
  `write_scope()` string; chunking at 200; reconcile marks existing as skipped;
  undo selects only rows with status `applied`, and for blocks only a stored
  `record_uri` that is still the block in force; drift comparison.
- **Integration (fake PDS):** an in-test axum server serving `applyWrites`,
  `muteActor`, `unmuteActor`, `getBlocks`, `getMutes`, and the token endpoint.
  Cases: happy path both kinds; partial failure; 429 with `ratelimit-reset`;
  two concurrent refreshes persist exactly one token pair; restart resume
  (runner re-run over a half-applied batch creates no duplicates);
  `invalid_grant` pauses the batch and deletes the session.
- **Web:** 401 without session; 403 under impersonation on every write
  endpoint; consent redirect when no write session; 503 when the key is
  missing; JSON shapes for the list and detail endpoints.
- **DB:** migration v15 on SQLite and Postgres; `delete_user_data` cascade.
- **Vitest:** confirm-sheet counting; bulk bar visibility tied to tier filter;
  detail-page button state transitions.
- **Live spike on staging** with Bryan's account before any bulk run: mute one
  account → verify in the Bluesky app → undo → verify. Then the same for block.

## 11. Delivery

One spec, one plan. If the plan grows large it splits into two PRs into
`staging`, in this order:

1. #261 doc, `oauth_sessions` + crypto + refresh, tables, runner, API, `/actions`
   page, detail-page buttons.
2. Bulk bar + confirm sheet on `/accounts`.

Launch dependency noted: #259 (privacy page) must describe stored tokens,
Disconnect, and the action log before onboarding strangers.

## 12. Open items carried elsewhere

- Broad-scope fallback for PDSes without granular scopes (follow-up issue).
- Key rotation for `CHARCOAL_TOKEN_KEY` (follow-up issue).
- Auto-policy on top of this log (existing direction in PRODUCT.md).
