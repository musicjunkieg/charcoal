# Single-action toast, bulk Done banner, runner timing (#332, #333, #318)

**Date:** 2026-09-04 · **Branch:** `feat/332-single-action-toast` → `staging`
**Decided by:** Bryan, from three interactive mockups (option B) and the
#318 disposition ("intentional" for the type ramp and the consent rule).
**Deciduous:** goal 704, decisions 708 / 711 / 712.

## 1. Problem

The #315 spike on staging passed every step, with two pieces of feedback:

1. Muting or blocking **one** account sends you to `/actions/<id>`, shows
   `Pending`, then `Done`, and leaves you there. For a single account that
   page is a detour, not a receipt. Bulk actions land on the same page and
   also just go quiet when finished — no "done", no way back.
2. Actions feel slow. The runner is event-driven (no polling delay on the
   server side), but every PDS call pays the DPoP nonce challenge as a second
   round trip, the batch page polls at 3 s so a 1 s action reads as 3 s+, and
   there is no timing instrumentation at all — nothing tells us where the
   seconds actually go.

## 2. Scope

| In | Out |
|---|---|
| Single mute/block/undo finishes in place with a toast (§3) | Any change to the batch/row data model or the `POST /api/actions/batches` contract |
| Batch page Done/failed banner + return link, 1 s polling (§4) | Replacing `getMutes`/`getBlocks` reconcile with `getProfiles` — a follow-up **only if** §5's timings show reconcile dominates |
| Runner `elapsed_ms` tracing + DPoP nonce cache (§5) | Changing runner pacing, retry, or rate-limit behaviour |
| #318: `scaleX`, token fallbacks, DESIGN.md ramp, one waiver (§6) | Collapsing the 0.875 / 0.9375 rem steps (a visible re-style; separate decision) |

## 3. Single action: the toast (#332)

### 3.1 Behaviour

From `ActionButtons.svelte`, on confirm:

1. `createActionBatch(kind, "account:<handle>", [did])` exactly as today.
   **No `goto`.**
2. `batch_id === null` → notice "That action is already in place." (unchanged).
3. Otherwise the button enters a `working` state (`Muting…` / `Blocking…`
   with the existing spinner idiom; the other button is disabled) and a
   toast is raised: **"Muting @handle…"**.
4. The toast polls `GET /api/actions/batches/{id}` every **1 s** until the
   single row's status leaves `pending`, or the batch leaves
   `queued`/`running` (whichever first), or 60 s elapse.
5. Settled states:

   | Row status | Toast | Button |
   |---|---|---|
   | `applied` | **Muted @handle** · Undo · Record — holds 6 s, then dismisses | `Muted ✓ · Undo` (existing `done` state via `refresh()`) |
   | `skipped_already_done` | **Already muted @handle** · Record — 6 s | `Muted ✓` (no Undo, #261) |
   | `failed` | **Couldn't mute @handle** · Retry · Record — stays until dismissed; error colour | back to `Mute` |
   | batch parked (`not_connected`) | **Reconnect to Bluesky to finish** · Reconnect — stays | back to `Mute` |
   | 60 s timeout | **Still working — check the record** · Record — stays | back to `Mute` |

   "Record" links to `/actions/{batch_id}`. "Retry" calls `retryBatch(id)`
   and re-enters step 3 with the returned batch id. "Reconnect" calls
   `startConsent('undo' | kind, { handle })`.
6. **Undo** (from the button or the toast) follows the same path:
   `undoAction(actionId)` → toast "Undoing…" → poll → **"Unmuted @handle"**
   (or "Unblocked"). The undo toast has no Undo of its own — redoing is the
   original button, which is back to `Mute`.
7. Copy uses the verb for the kind: mute → Muting/Muted/Unmuted, block →
   Blocking/Blocked/Unblocked. No bare status codes reach the screen
   (PRODUCT principle 1).

### 3.2 Components

- **`web/src/lib/toast.ts`** — a Svelte 5 store module: `toasts` (writable
  array), `raise(toast): id`, `update(id, patch)`, `dismiss(id)`. A toast is
  `{ id, tone: 'working' | 'ok' | 'error', text, actions: {label, onclick}[],
  href?: {label, url}, ttlMs?: number }`. `ttlMs` starts its dismiss timer
  when set (i.e. on the settled update, not on raise). Pure TS, unit-tested.
- **`web/src/lib/components/Toast.svelte`** — renders the store. Fixed,
  bottom-centre on narrow viewports, bottom-left ≥ 720 px; `role="status"`
  for `ok`/`working`, `role="alert"` for `error`; slide-up transition using
  `--ease-out-expo`, disabled under `prefers-reduced-motion`. Mounted **once**
  in `web/src/routes/(protected)/+layout.svelte` after `{@render children()}`.
  Styled entirely from `tokens.css` (`--charcoal-800` surface, `--status-ok`,
  `--status-error`, `--copper` for links). Stacks newest at the bottom; at
  most 3 visible.
- **`web/src/lib/action-progress.ts`** — pure helpers, unit-tested:
  - `settle(detail: ActionBatchDetail): Settled | null` — reads the single
    row (or the batch when it has none) and returns
    `{ kind: 'applied' | 'skipped' | 'failed' | 'parked', row? }` or `null`
    while still pending.
  - `toastCopy(kind: ActionBatchKind, handle: string, phase: 'working' | Settled['kind'] | 'timeout'): string`
    — the strings in §3.1, pinned by tests.
  - `pollUntilSettled(fetch, { intervalMs: 1000, timeoutMs: 60000 })` —
    generic: calls `fetch()` until `settle()` is non-null or timeout; returns
    the settled value or `'timeout'`. Takes an injectable `sleep` so tests
    run without real timers.
- `ActionButtons.svelte` gains a `working` button state (`states[kind]`
  extended by a local `inflight: ActionKind | 'undo' | null`), loses both
  `goto` calls, and calls `refresh()` after every settle so the button
  reflects the server.

### 3.3 Not changed

`ConfirmSheet.svelte` semantics, the consent round-trip (`?resume=`), the
impersonation guard, `/api/actions/*` handlers, and the accounts-list bulk
path (`confirmBulk` still navigates to the batch page — §4).

## 4. Bulk: the batch page finishes properly (#332)

`web/src/routes/(protected)/actions/[id]/+page.svelte`:

- Poll interval 3000 → **1000 ms** while `isRunning`. The 3 s value was a
  guess; a 1 s poll of one small JSON endpoint is negligible load and cuts
  up to 2 s off every perceived completion.
- When the batch is not running and not parked, a **banner** renders between
  the header and the table:
  - `done` → ok tone: **"Done · 14 muted, 0 failed"** [Undo all] [← Back to Watch accounts]
  - `partial` / `failed` → error tone: **"Finished with problems · 12 muted, 2 failed"** [Retry failed] [Undo all if any applied] [← Back …]
  - `undo` kind → **"Undone · 14 unmuted"** [← Back …]
  - The counts sentence comes from a new `bannerSummary(b)` in
    `action-status.ts` (pure, tested), reusing `n()`.
- **Return link** from `returnPath(source, asUserSuffix)` in
  `action-status.ts` (pure, tested):
  - `tier:<Tier>` → `/accounts?tier=<Tier>` — label "← Back to <Tier> accounts"
  - `account:<handle>` → `/accounts/<handle>` — label "← Back to @<handle>"
  - anything else → `/accounts` — label "← Back to accounts"
  - `as_user` suffix is appended as the page already does for its links.
- The existing Undo all / Retry failed controls move into the banner when it
  is shown (they stay in the header while running/parked, where the banner
  is absent). No duplicate controls.

## 5. Speed: measure, and remove the sure round-trip (#333)

### 5.1 Timing instrumentation — `src/web/actions/runner.rs`

`tracing::info!` lines with `elapsed_ms: u64` (from `std::time::Instant`) at:

| Span | Where |
|---|---|
| `load_session` | around `sessions.load_for_write` — includes any refresh |
| `reconcile` | around `get_mutes` / `get_blocks` in `run_mutes`/`run_blocks`/`run_undo` |
| `pds_call` | inside `call()` per attempt, with `attempt` and the nsid-ish `op` label passed by the caller (`"muteActor"`, `"applyWrites"`, …) |
| `batch` | on the existing "action batch finished" line, total wall time |

All are `info` so staging's default filter shows them. No new dependencies
(`tracing` is already there). `RunnerConfig` is untouched.

### 5.2 DPoP nonce cache — `src/web/actions/dpop_http.rs`, `pds.rs`

Today `send_dpop` mints a nonce-free proof, gets the `use_dpop_nonce`
challenge, and re-sends: **two round trips per call**, on every call, because
nothing remembers the nonce. AT Protocol servers return a `DPoP-Nonce` header
on every response (success included) and rotate it periodically, so:

- `send_dpop` gains a `nonce: &NonceCache` parameter.
  `pub struct NonceCache(std::sync::Mutex<Option<String>>)` with
  `get() -> Option<String>` and `set(&str)`. A `Mutex`, not a `RwLock` or
  atomics — it is a short critical section holding one small string, and
  the runner is single-flight per batch anyway.
- First attempt: if the cache holds a nonce, include it in the proof
  claims. After **every** response (any status), if a `DPoP-Nonce` header is
  present, `set` it.
- The existing challenge retry stays exactly as is (a stale cached nonce
  yields a challenge with a fresh nonce; the retry uses that one and caches
  it). So the worst case is unchanged (two round trips) and the steady state
  is one.
- `PdsClient` owns a `NonceCache` for its lifetime (one per batch — matches
  the session load granularity). `session.rs`'s token-refresh call passes a
  fresh cache of its own; the token endpoint's nonce is not the resource
  server's.
- Never log the nonce alongside a token; it is not a secret but the
  no-token-in-logs rule (#315) stays absolute and the simplest way to keep
  it is to log neither.

### 5.3 Evidence gate for the reconcile follow-up

After this ships to staging, one single-account mute is run and the four
spans read from Railway logs. If `reconcile` is the majority of `batch`,
open a chainlink issue for a `getProfiles`-based reconcile for batches of
≤ 25 rows (one call, returns `viewer.muted` / `viewer.blocking`). If it is
not, close #333 on the measurements. Either way the numbers go in the
chainlink #333 result comment.

## 6. Design-hook findings (#318)

| Finding | Action |
|---|---|
| `.signal-bar` `transition: width` (accounts/[handle] L397-402) | `transform: scaleX(<ratio>)` with `transform-origin: left`; markup passes `style="transform: scaleX({scoreBar(x) / 100})"`; `transition: transform 0.5s ease`. Same motion, no layout on each frame. |
| ConfirmSheet `var(--charcoal-100, #f5f5f4)` ×2, `var(--charcoal-900, #1c1917)` | `--charcoal-100` does not exist in `tokens.css` (the palette stops at `--charcoal-300`), so the fallback was the only thing painting that colour. Replace with `var(--cream-50)` — the site's existing off-white (headings use it) — and drop the `--charcoal-900` fallback, which does exist. Found while planning; the "drop the fallbacks" wording was wrong. |
| ConfirmSheet backdrop `rgb(0 0 0 / 0.4)` | `rgb(var(--charcoal-950-rgb) / 0.4)` — visually identical (#0c0a09 vs #000 at 40 %). |
| Font sizes 0.75 / 0.875 / 0.9375 / 1.125 / 1.875 rem | **Promote to the DESIGN.md ramp** (front-matter `typography:`): `caption` 0.75, `small` 0.875, `body-sm` 0.9375, `subtitle` 1.125, `stat` 1.875 (the score-card figure). Docs change; nothing visual moves. Rationale: 36 / 24 / 17 / 5 uses — these are the shipped system, the doc was behind. |
| Radius `10px` ×2, `6px` | Snap to `8px` (`rounded.sm`). |
| Radius `2px` ×2 on the 4 px signal bars | Add `rounded.xs: "2px"` to DESIGN.md — an 8 px corner on a 4 px bar is not a design. |
| `side-tab` on `.consent` (ConfirmSheet L145) | **Keep.** It marks the consent sentence as quoted terms. Bryan: intentional. Waive file-scoped: `/impeccable hooks ignore-value side-tab "*" --file web/src/lib/components/ConfirmSheet.svelte --shared --reason "Consent-terms rule, confirmed intentional 2026-09-04 (#318)"`. The only suppression in this work. |

The hook is expected to report **zero** findings on both files afterwards.

## 7. Testing

- **vitest** (`web/src/lib/*.test.ts`): `toast.test.ts` (raise/update/
  dismiss, ttl starts on settle, cap of 3), `action-progress.test.ts`
  (`settle` for each row status and the parked batch; `toastCopy` strings
  pinned; `pollUntilSettled` with an injected sleep — returns on first
  settled, returns `'timeout'`, stops calling after settle),
  `action-status.test.ts` extended (`bannerSummary`, `returnPath` for the
  three source shapes and the `as_user` suffix).
- **Rust** (`src/web/actions/dpop_http.rs` `#[cfg(test)]` + existing
  `tests/unit_actions_pds.rs` wiremock harness (which already exercises the nonce challenge)): a mock that demands a nonce
  sees exactly two requests on the first call and **one** on the second; a
  200 with a rotated `DPoP-Nonce` updates the cache; a stale nonce falls
  back to the challenge path and still succeeds. Runner tests unchanged and
  green; the timing lines are asserted only by compiling (no log-capture
  test — the value is in staging logs, not in a unit test).
- **Gates**: `npm --prefix web run test -- --run`, `npm --prefix web run
  check` (no new errors beyond the five pre-existing on
  accounts/[handle]), `npm --prefix web run build`, clippy `--features web`
  / `--features postgres` / default with `-D warnings`, `CHARCOAL_MODEL_DIR=./models
  cargo test --features web -- --show-output` with zero `SKIP:` lines,
  postgres suite against local `charcoal_test`.
- **Staging**: one mute, one undo, one bulk Watch mute — toast copy,
  banner, return link, and the four timing spans in the logs.

## 8. Rollout

One PR `feat/332-single-action-toast` → `staging` carrying #332, #333 (round
1), #318. CodeRabbit loop to APPROVED, merge, verify on staging per §7, then
`chainlink issue close 332 --no-changelog`, `333` or a follow-up per §5.3,
`318 --no-changelog`; handwritten CHANGELOG entries.
