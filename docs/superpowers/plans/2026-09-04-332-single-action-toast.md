# Single-action toast, bulk Done banner, runner timing — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Spec:** `docs/superpowers/specs/2026-09-04-332-333-single-action-toast-design.md`
**Branch:** `feat/332-single-action-toast` → `staging` · **Issues:** #332, #333 (round 1), #318
**Deciduous:** goal 704, decisions 708 / 711 / 712

**Goal:** A single mute/block/undo finishes in place with a toast instead of navigating to `/actions/<id>`; the bulk batch page gets a Done/problems banner with a return link and a 1 s poll; the runner logs `elapsed_ms` spans and caches the DPoP nonce so the steady state is one round trip per PDS call; the #318 design-hook findings are resolved.

**Architecture:** Three pure TS modules (`toast.ts` store, `action-progress.ts` settle/poll/copy, `action-status.ts` additions) carry all the logic and are unit-tested under vitest; two Svelte files (`Toast.svelte`, `ActionButtons.svelte`) and the batch page are thin renderers over them. On the Rust side a `NonceCache` is threaded through `send_dpop` and owned by `PdsClient`; the runner wraps its existing call sites with `Instant` timing and `tracing::info!` lines. Nothing about the batch/row data model, the `/api/actions/*` contract, retry/rate-limit behaviour, or `RunnerConfig` changes.

**Tech Stack:** Svelte 5 runes + SvelteKit adapter-static SPA, vitest (`environment: 'node'`, pure TS only — no `.svelte` under test), `svelte/store` `writable`; Rust: `tracing`, `std::sync::Mutex`, `std::time::Instant`, wiremock 0.6 harness in `tests/unit_actions_pds.rs`, `base64` 0.22 (already a `web`-feature dep).

## Global Constraints

- **Toast copy (verbatim, spec §3.1):** mute → `Muting @h…` / `Muted @h` / `Already muted @h` / `Couldn't mute @h`; block → `Blocking @h…` / `Blocked @h` / `Already blocked @h` / `Couldn't block @h`; undo of a mute → `Unmuting @h…` / `Unmuted @h` / `Already unmuted @h` / `Couldn't unmute @h`; undo of a block → `Unblocking @h…` / `Unblocked @h` / `Already unblocked @h` / `Couldn't unblock @h`; parked → `Reconnect to Bluesky to finish`; timeout → `Still working — check the record`. Toast action labels: `Undo`, `Retry`, `Reconnect`; link label `Record` → `/actions/{batch_id}`. Notice for `batch_id === null`: `That action is already in place.` (unchanged). No bare status codes reach the screen.
- **Toast timing:** poll every **1000 ms**, give up at **60000 ms**; settled `ok` toasts hold **6000 ms** then dismiss; `error`/parked/timeout toasts stay until dismissed. `ttlMs` starts on the settled `update`, not on `raise`. At most **3** toasts visible (oldest dropped).
- **Toast placement:** fixed, bottom-centre below 720 px, bottom-left at ≥ 720 px; `role="status"` for `ok`/`working`, `role="alert"` for `error`; slide-up with `--ease-out-expo`, no motion under `prefers-reduced-motion`; styled only from `tokens.css` (`--charcoal-800` surface, `--status-ok`, `--status-error`, `--copper` links). Mounted **once** in `web/src/routes/(protected)/+layout.svelte` after `{@render children()}`.
- **Batch page:** poll interval **1000 ms** while `isRunning`. Banner shown only when `!isRunning(b) && !isParked(b)`. Titles: `Done` (status done), `Undone` (kind undo + status done), `Finished with problems` (partial/failed). Return link: `tier:<T>` → `/accounts?tier=<T>` "← Back to <T> accounts"; `account:<h>` → `/accounts/<h>` "← Back to @<h>"; else `/accounts` "← Back to accounts"; `as_user` suffix appended exactly as the page already does. Undo all / Retry failed live in the banner when it shows, in the header otherwise — never both.
- **Rust timing spans (spec §5.1):** `tracing::info!` with `elapsed_ms: u64` from `std::time::Instant`; span names exactly `load_session`, `reconcile`, `pds_call` (with `op` and `attempt`), `batch` (on the existing "action batch finished" line). No new dependencies; `RunnerConfig` untouched.
- **Nonce cache (spec §5.2):** `pub struct NonceCache(std::sync::Mutex<Option<String>>)` with `get() -> Option<String>` and `set(&str)`; `send_dpop` gains `nonce: &NonceCache`; cached nonce goes into the **first** proof; `DPoP-Nonce` from **every** response (any status) is `set`; the challenge retry path is unchanged; `PdsClient` owns one `NonceCache` (its `new()` signature does **not** change); `session.rs` refresh passes a fresh `NonceCache::default()`.
- **Secrets:** never log, Debug-print, or put in a fixture/commit/chat an access token, refresh token, DPoP key, `CHARCOAL_TOKEN_KEY`, or a nonce alongside a token. The timing lines carry only ids, names, counts, and durations.
- **#318 (spec §6):** `.signal-bar` moves from `transition: width` to `transform: scaleX(...)` with `transform-origin: left`; ConfirmSheet replaces `var(--charcoal-100, #f5f5f4)` with `var(--cream-50)` (there is **no** `--charcoal-100` token — the palette stops at `--charcoal-300`; `--cream-50` is the site's existing off-white) and `var(--charcoal-900, #1c1917)` with `var(--charcoal-900)`, and uses `rgb(var(--charcoal-950-rgb) / 0.4)` for the backdrop; radii `10px` and `6px` → `8px`; DESIGN.md `typography:` gains `caption` 0.75rem, `small` 0.875rem, `body-sm` 0.9375rem, `subtitle` 1.125rem, `stat` 1.875rem and `rounded:` gains `xs: "2px"`; `.impeccable/design.json` `typographyMeta` gains matching entries; the **only** suppression is the file-scoped `side-tab` waiver on `ConfirmSheet.svelte`, written by `hook-admin.mjs` (never by hand-editing `.impeccable/config.json`). The 0.875 / 0.9375 rem steps are **not** collapsed.
- **Repo rules:** never commit to `staging`/`main`; `git add` files by name only (no `-A`/`.`); no heredocs in shell commands; conventional commit messages; `npm --prefix web …` (never `cd web`); run the svelte MCP validator on every `.svelte` you touch; run `cargo` in the **foreground**; do not add `docs/superpowers/plans/2026-09-01-315-pr-body.md` to any commit; do not remove `web/.chainlink/` (human-reserved).
- **Gates (spec §7):** `npm --prefix web run test -- --run`; `npm --prefix web run check` (no new errors beyond the five pre-existing on `accounts/[handle]`); `npm --prefix web run build`; `cargo clippy --features web --all-targets -- -D warnings`, same for `--features postgres` and no features; `CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "^\s*SKIP:"` prints nothing; postgres suite `DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --all-targets --features postgres`.

---

## File map

| File | Responsibility | Task |
|---|---|---|
| `web/src/lib/toast.ts` (new) | Toast store: raise/update/dismiss, ttl timers, cap of 3 | 1 |
| `web/src/lib/toast.test.ts` (new) | Pins store behaviour with fake timers | 1 |
| `web/src/lib/action-progress.ts` (new) | `settle`, `toastCopy`, `pollUntilSettled` | 2 |
| `web/src/lib/action-progress.test.ts` (new) | Pins settle mapping, copy strings, poll loop | 2 |
| `web/src/lib/components/Toast.svelte` (new) | Renders the store | 3 |
| `web/src/routes/(protected)/+layout.svelte` | Mounts `<Toast />` once | 3 |
| `web/src/lib/components/ActionButtons.svelte` | Toast-driven single actions, no `goto` | 4 |
| `web/src/lib/action-status.ts` + `.test.ts` | `bannerSummary`, `returnPath` | 5 |
| `web/src/routes/(protected)/actions/[id]/+page.svelte` | Banner, return link, 1 s poll | 5 |
| `src/web/actions/runner.rs` | Timing spans | 6 |
| `src/web/actions/dpop_http.rs`, `pds.rs`, `session.rs`, `tests/unit_actions_pds.rs` | `NonceCache` | 7 |
| `web/src/routes/(protected)/accounts/[handle]/+page.svelte`, `ConfirmSheet.svelte`, `DESIGN.md`, `.impeccable/design.json`, `.impeccable/config.json` | #318 | 8 |
| `CHANGELOG.md` | Unreleased entries | 9 |

---

### Task 1: Toast store (`toast.ts`)

**Files:**
- Create: `web/src/lib/toast.ts`
- Test: `web/src/lib/toast.test.ts`

**Interfaces:**
- Consumes: `svelte/store` `writable`, `get` (already resolvable under vitest's node environment — `dashboard-state.test.ts` proves the import path works).
- Produces (used by Tasks 3 and 4):
  ```ts
  export type ToastTone = 'working' | 'ok' | 'error';
  export interface ToastAction { label: string; onclick: () => void }
  export interface Toast {
    id: number; tone: ToastTone; text: string;
    actions: ToastAction[]; href?: { label: string; url: string }; ttlMs?: number;
  }
  export type ToastInput = Omit<Toast, 'id'>;
  export const toasts: Readable<Toast[]>;          // newest LAST
  export function raise(t: ToastInput): number;    // returns id
  export function update(id: number, patch: Partial<ToastInput>): void;
  export function dismiss(id: number): void;
  export const MAX_TOASTS = 3;
  ```

- [ ] **Step 1: Write the failing tests**

Create `web/src/lib/toast.test.ts`:

```ts
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get } from 'svelte/store';
import { toasts, raise, update, dismiss, MAX_TOASTS } from './toast.js';

// The store is module-level state; every test starts from empty.
function clearAll() {
	for (const t of get(toasts)) dismiss(t.id);
}

describe('toast store', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		clearAll();
	});
	afterEach(() => {
		clearAll();
		vi.useRealTimers();
	});

	it('raise appends newest last and returns a unique id', () => {
		const a = raise({ tone: 'working', text: 'Muting @a…', actions: [] });
		const b = raise({ tone: 'working', text: 'Muting @b…', actions: [] });
		expect(a).not.toBe(b);
		expect(get(toasts).map((t) => t.id)).toEqual([a, b]);
		expect(get(toasts)[1].text).toBe('Muting @b…');
	});

	it('update patches in place and keeps the id', () => {
		const id = raise({ tone: 'working', text: 'Muting @a…', actions: [] });
		update(id, { tone: 'ok', text: 'Muted @a', href: { label: 'Record', url: '/actions/7' } });
		const t = get(toasts)[0];
		expect(t.id).toBe(id);
		expect(t.tone).toBe('ok');
		expect(t.text).toBe('Muted @a');
		expect(t.href).toEqual({ label: 'Record', url: '/actions/7' });
	});

	it('update of an unknown id is a no-op', () => {
		raise({ tone: 'ok', text: 'x', actions: [] });
		update(999, { text: 'y' });
		expect(get(toasts).map((t) => t.text)).toEqual(['x']);
	});

	it('dismiss removes only that toast', () => {
		const a = raise({ tone: 'ok', text: 'a', actions: [] });
		const b = raise({ tone: 'ok', text: 'b', actions: [] });
		dismiss(a);
		expect(get(toasts).map((t) => t.id)).toEqual([b]);
	});

	it('a raise without ttlMs never auto-dismisses', () => {
		raise({ tone: 'working', text: 'Muting @a…', actions: [] });
		vi.advanceTimersByTime(600_000);
		expect(get(toasts)).toHaveLength(1);
	});

	it('ttl starts when ttlMs is set on the settled update, not on raise', () => {
		const id = raise({ tone: 'working', text: 'Muting @a…', actions: [] });
		vi.advanceTimersByTime(30_000);
		update(id, { tone: 'ok', text: 'Muted @a', ttlMs: 6000 });
		vi.advanceTimersByTime(5999);
		expect(get(toasts)).toHaveLength(1);
		vi.advanceTimersByTime(1);
		expect(get(toasts)).toHaveLength(0);
	});

	it('ttlMs on raise dismisses after the ttl', () => {
		raise({ tone: 'ok', text: 'Muted @a', actions: [], ttlMs: 1000 });
		vi.advanceTimersByTime(1000);
		expect(get(toasts)).toHaveLength(0);
	});

	it('a second update with ttlMs restarts the timer', () => {
		const id = raise({ tone: 'ok', text: 'a', actions: [], ttlMs: 1000 });
		vi.advanceTimersByTime(900);
		update(id, { text: 'b', ttlMs: 1000 });
		vi.advanceTimersByTime(900);
		expect(get(toasts)).toHaveLength(1);
		vi.advanceTimersByTime(100);
		expect(get(toasts)).toHaveLength(0);
	});

	it('dismiss cancels a pending ttl timer (no double removal of a reused id)', () => {
		const id = raise({ tone: 'ok', text: 'a', actions: [], ttlMs: 1000 });
		dismiss(id);
		const b = raise({ tone: 'ok', text: 'b', actions: [] });
		vi.advanceTimersByTime(1000);
		expect(get(toasts).map((t) => t.id)).toEqual([b]);
	});

	it(`keeps at most ${MAX_TOASTS} toasts, dropping the oldest`, () => {
		const ids = [1, 2, 3, 4].map((i) => raise({ tone: 'ok', text: `t${i}`, actions: [] }));
		expect(get(toasts)).toHaveLength(MAX_TOASTS);
		expect(get(toasts).map((t) => t.id)).toEqual(ids.slice(1));
	});
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npm --prefix web run test -- --run src/lib/toast.test.ts`
Expected: FAIL — `Cannot find module './toast.js'` (or equivalent resolution error).

- [ ] **Step 3: Write the store**

Create `web/src/lib/toast.ts`:

```ts
// Toast store for in-place action feedback (#332, spec §3.2). Pure TS on
// `svelte/store` so vitest can drive it without a DOM; `Toast.svelte` is the
// only renderer. Newest toast is LAST in the array — the renderer stacks
// bottom-up, so the newest sits nearest the viewport edge.
import { writable, type Readable } from 'svelte/store';

export type ToastTone = 'working' | 'ok' | 'error';

export interface ToastAction {
	label: string;
	onclick: () => void;
}

export interface Toast {
	id: number;
	tone: ToastTone;
	text: string;
	actions: ToastAction[];
	/** A plain link rendered after the actions, e.g. "Record" → the batch page. */
	href?: { label: string; url: string };
	/** Auto-dismiss delay. The timer (re)starts whenever a raise or update
	 *  carries this field, so a settled update — not the working raise —
	 *  is what starts the clock. */
	ttlMs?: number;
}

export type ToastInput = Omit<Toast, 'id'>;

/** More than this and the stack hides content; the oldest goes first. */
export const MAX_TOASTS = 3;

const store = writable<Toast[]>([]);
export const toasts: Readable<Toast[]> = { subscribe: store.subscribe };

let nextId = 1;
const timers = new Map<number, ReturnType<typeof setTimeout>>();

function clearTimer(id: number) {
	const t = timers.get(id);
	if (t !== undefined) {
		clearTimeout(t);
		timers.delete(id);
	}
}

function armTimer(id: number, ttlMs: number | undefined) {
	clearTimer(id);
	if (ttlMs === undefined) return;
	timers.set(
		id,
		setTimeout(() => {
			timers.delete(id);
			dismiss(id);
		}, ttlMs)
	);
}

export function raise(input: ToastInput): number {
	const id = nextId++;
	store.update((list) => {
		const next = [...list, { ...input, id }];
		// Drop from the front: the oldest toasts are the least relevant.
		while (next.length > MAX_TOASTS) {
			const dropped = next.shift()!;
			clearTimer(dropped.id);
		}
		return next;
	});
	armTimer(id, input.ttlMs);
	return id;
}

export function update(id: number, patch: Partial<ToastInput>): void {
	let found = false;
	store.update((list) =>
		list.map((t) => {
			if (t.id !== id) return t;
			found = true;
			return { ...t, ...patch };
		})
	);
	if (found && patch.ttlMs !== undefined) armTimer(id, patch.ttlMs);
}

export function dismiss(id: number): void {
	clearTimer(id);
	store.update((list) => list.filter((t) => t.id !== id));
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `npm --prefix web run test -- --run src/lib/toast.test.ts`
Expected: PASS, 10 tests.

- [ ] **Step 5: Commit**

```bash
git add web/src/lib/toast.ts web/src/lib/toast.test.ts
git commit -m 'feat(332): toast store with ttl-on-settle and a cap of three'
```

---

### Task 2: Progress helpers (`action-progress.ts`)

**Files:**
- Create: `web/src/lib/action-progress.ts`
- Test: `web/src/lib/action-progress.test.ts`

**Interfaces:**
- Consumes: `ActionBatchDetail`, `ActionRowView`, `ActionKind` from `web/src/lib/types.ts`; `isParked`, `isRunning` from `web/src/lib/action-status.ts`.
- Produces (used by Task 4):
  ```ts
  export type SettledKind = 'applied' | 'skipped' | 'failed' | 'parked';
  export interface Settled { kind: SettledKind; row?: ActionRowView }
  export function settle(detail: ActionBatchDetail): Settled | null;
  export type ToastKind = 'mute' | 'block' | 'unmute' | 'unblock';
  export type ToastPhase = 'working' | SettledKind | 'timeout';
  export function toastCopy(kind: ToastKind, handle: string, phase: ToastPhase): string;
  export const POLL_INTERVAL_MS = 1000;
  export const POLL_TIMEOUT_MS = 60000;
  export interface PollOptions { intervalMs?: number; timeoutMs?: number; sleep?: (ms: number) => Promise<void>; now?: () => number }
  export function pollUntilSettled(fetch: () => Promise<ActionBatchDetail>, opts?: PollOptions): Promise<Settled | 'timeout'>;
  ```
- Design note: the spec writes `toastCopy(kind: ActionBatchKind, …)`. `ActionBatchKind` is `'mute' | 'block' | 'undo'`, but an undo toast needs the verb of the thing being undone ("Unmuted" vs "Unblocked"), which `'undo'` cannot carry. `ToastKind` is the resolved shape: the caller maps `undo` of a mute → `'unmute'`, of a block → `'unblock'`.

- [ ] **Step 1: Write the failing tests**

Create `web/src/lib/action-progress.test.ts`:

```ts
import { describe, it, expect } from 'vitest';
import { settle, toastCopy, pollUntilSettled, POLL_INTERVAL_MS, POLL_TIMEOUT_MS } from './action-progress.js';
import type { ActionBatchDetail, ActionBatchSummary, ActionRowView } from './types.js';

function summary(over: Partial<ActionBatchSummary> = {}): ActionBatchSummary {
	return {
		id: 7,
		kind: 'mute',
		source: 'account:alice.bsky.social',
		requested: 1,
		status: 'running',
		error: null,
		created_at: '2026-09-04T00:00:00Z',
		started_at: null,
		finished_at: null,
		counts: {},
		drifted: 0,
		...over
	};
}

function row(over: Partial<ActionRowView> = {}): ActionRowView {
	return {
		id: 1,
		batch_id: 7,
		target_did: 'did:plc:x',
		handle: 'alice.bsky.social',
		kind: 'mute',
		status: 'pending',
		record_uri: null,
		undo_of: null,
		error: null,
		score_at_action: null,
		tier_at_action: null,
		current_tier: null,
		drifted: false,
		applied_at: null,
		undone_at: null,
		...over
	};
}

function detail(b: Partial<ActionBatchSummary>, rows: ActionRowView[]): ActionBatchDetail {
	return { batch: summary(b), actions: rows };
}

describe('settle', () => {
	it('is null while the row is pending and the batch is running', () => {
		expect(settle(detail({ status: 'running' }, [row()]))).toBeNull();
		expect(settle(detail({ status: 'queued' }, [row()]))).toBeNull();
	});

	it('maps each settled row status', () => {
		const applied = row({ status: 'applied' });
		expect(settle(detail({ status: 'running' }, [applied]))).toEqual({ kind: 'applied', row: applied });
		const skipped = row({ status: 'skipped_already_done' });
		expect(settle(detail({ status: 'running' }, [skipped]))).toEqual({ kind: 'skipped', row: skipped });
		const failed = row({ status: 'failed', error: 'boom' });
		expect(settle(detail({ status: 'running' }, [failed]))).toEqual({ kind: 'failed', row: failed });
		// An undo row that succeeded is stored `undone`.
		const undone = row({ status: 'undone', kind: 'mute' });
		expect(settle(detail({ kind: 'undo', status: 'running' }, [undone]))).toEqual({ kind: 'applied', row: undone });
	});

	it('is parked when the batch is waiting for a reconnect, whatever the row says', () => {
		expect(settle(detail({ status: 'queued', error: 'not_connected' }, [row()]))).toEqual({ kind: 'parked' });
	});

	it('falls back to the batch status when there is no settled row', () => {
		expect(settle(detail({ status: 'done' }, []))).toEqual({ kind: 'applied' });
		expect(settle(detail({ status: 'failed' }, []))).toEqual({ kind: 'failed' });
		// A batch that failed before the write step leaves its row pending.
		expect(settle(detail({ status: 'failed' }, [row()]))).toEqual({ kind: 'failed' });
	});
});

describe('toastCopy', () => {
	it('mute', () => {
		expect(toastCopy('mute', 'alice', 'working')).toBe('Muting @alice…');
		expect(toastCopy('mute', 'alice', 'applied')).toBe('Muted @alice');
		expect(toastCopy('mute', 'alice', 'skipped')).toBe('Already muted @alice');
		expect(toastCopy('mute', 'alice', 'failed')).toBe("Couldn't mute @alice");
	});
	it('block', () => {
		expect(toastCopy('block', 'alice', 'working')).toBe('Blocking @alice…');
		expect(toastCopy('block', 'alice', 'applied')).toBe('Blocked @alice');
		expect(toastCopy('block', 'alice', 'skipped')).toBe('Already blocked @alice');
		expect(toastCopy('block', 'alice', 'failed')).toBe("Couldn't block @alice");
	});
	it('unmute / unblock', () => {
		expect(toastCopy('unmute', 'alice', 'working')).toBe('Unmuting @alice…');
		expect(toastCopy('unmute', 'alice', 'applied')).toBe('Unmuted @alice');
		expect(toastCopy('unmute', 'alice', 'skipped')).toBe('Already unmuted @alice');
		expect(toastCopy('unmute', 'alice', 'failed')).toBe("Couldn't unmute @alice");
		expect(toastCopy('unblock', 'alice', 'working')).toBe('Unblocking @alice…');
		expect(toastCopy('unblock', 'alice', 'applied')).toBe('Unblocked @alice');
		expect(toastCopy('unblock', 'alice', 'skipped')).toBe('Already unblocked @alice');
		expect(toastCopy('unblock', 'alice', 'failed')).toBe("Couldn't unblock @alice");
	});
	it('parked and timeout are kind-independent', () => {
		expect(toastCopy('mute', 'alice', 'parked')).toBe('Reconnect to Bluesky to finish');
		expect(toastCopy('block', 'alice', 'timeout')).toBe('Still working — check the record');
	});
});

describe('pollUntilSettled', () => {
	/** Fake clock: `sleep` advances `now` instead of waiting. */
	function clock() {
		let t = 0;
		const sleeps: number[] = [];
		return {
			now: () => t,
			sleep: async (ms: number) => {
				sleeps.push(ms);
				t += ms;
			},
			sleeps
		};
	}

	it('defaults are 1 s and 60 s', () => {
		expect(POLL_INTERVAL_MS).toBe(1000);
		expect(POLL_TIMEOUT_MS).toBe(60000);
	});

	it('returns the first settled value and stops fetching', async () => {
		const c = clock();
		const responses = [
			detail({ status: 'running' }, [row()]),
			detail({ status: 'running' }, [row()]),
			detail({ status: 'done' }, [row({ status: 'applied' })])
		];
		let calls = 0;
		const fetch = async () => responses[calls++];
		const out = await pollUntilSettled(fetch, { sleep: c.sleep, now: c.now });
		expect(out).toEqual({ kind: 'applied', row: responses[2].actions[0] });
		expect(calls).toBe(3);
		expect(c.sleeps).toEqual([1000, 1000]);
	});

	it('returns immediately without sleeping when the first fetch is settled', async () => {
		const c = clock();
		const fetch = async () => detail({ status: 'running' }, [row({ status: 'skipped_already_done' })]);
		const out = await pollUntilSettled(fetch, { sleep: c.sleep, now: c.now });
		expect(out).toMatchObject({ kind: 'skipped' });
		expect(c.sleeps).toEqual([]);
	});

	it("returns 'timeout' once timeoutMs has elapsed", async () => {
		const c = clock();
		let calls = 0;
		const fetch = async () => {
			calls++;
			return detail({ status: 'running' }, [row()]);
		};
		const out = await pollUntilSettled(fetch, { sleep: c.sleep, now: c.now, intervalMs: 1000, timeoutMs: 5000 });
		expect(out).toBe('timeout');
		// t=0,1,2,3,4,5 → six fetches; the 6th sees now >= timeout and stops.
		expect(calls).toBe(6);
	});

	it('a failed fetch is a blip: keep polling', async () => {
		const c = clock();
		let calls = 0;
		const fetch = async () => {
			calls++;
			if (calls === 1) throw new Error('network');
			return detail({ status: 'done' }, [row({ status: 'applied' })]);
		};
		const out = await pollUntilSettled(fetch, { sleep: c.sleep, now: c.now });
		expect(out).toMatchObject({ kind: 'applied' });
		expect(calls).toBe(2);
	});
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npm --prefix web run test -- --run src/lib/action-progress.test.ts`
Expected: FAIL — module `./action-progress.js` not found.

- [ ] **Step 3: Write the helpers**

Create `web/src/lib/action-progress.ts`:

```ts
// Single-action progress helpers (#332, spec §3.2). Pure: the toast copy and
// the settle mapping are pinned by vitest, and `pollUntilSettled` takes an
// injectable clock so the tests never wait.
import type { ActionBatchDetail, ActionRowView } from './types.js';
import { isParked, isRunning } from './action-status.js';

export type SettledKind = 'applied' | 'skipped' | 'failed' | 'parked';

export interface Settled {
	kind: SettledKind;
	row?: ActionRowView;
}

/** Read one single-row batch. `null` means keep polling. A parked batch
 *  (waiting on a reconnect) wins over anything the row says, because
 *  nothing will move until the person acts. */
export function settle(detail: ActionBatchDetail): Settled | null {
	const b = detail.batch;
	if (isParked(b)) return { kind: 'parked' };
	const row = detail.actions.find((r) => r.status !== 'pending');
	if (row) {
		switch (row.status) {
			case 'applied':
			case 'undone':
				return { kind: 'applied', row };
			case 'skipped_already_done':
				return { kind: 'skipped', row };
			case 'failed':
				return { kind: 'failed', row };
		}
	}
	if (isRunning(b)) return null;
	// The batch finished without touching the row: `done` with an empty
	// batch, or a failure before the write step (reconcile read, token
	// refresh) that leaves the row pending.
	return { kind: b.status === 'done' ? 'applied' : 'failed' };
}

/** The verb the toast conjugates. `undo` of a mute is `unmute`, of a block
 *  `unblock` — the batch kind alone cannot say which. */
export type ToastKind = 'mute' | 'block' | 'unmute' | 'unblock';
export type ToastPhase = 'working' | SettledKind | 'timeout';

const VERBS: Record<ToastKind, { ing: string; ed: string; bare: string }> = {
	mute: { ing: 'Muting', ed: 'Muted', bare: 'mute' },
	block: { ing: 'Blocking', ed: 'Blocked', bare: 'block' },
	unmute: { ing: 'Unmuting', ed: 'Unmuted', bare: 'unmute' },
	unblock: { ing: 'Unblocking', ed: 'Unblocked', bare: 'unblock' }
};

export function toastCopy(kind: ToastKind, handle: string, phase: ToastPhase): string {
	const v = VERBS[kind];
	const who = `@${handle}`;
	switch (phase) {
		case 'working':
			return `${v.ing} ${who}…`;
		case 'applied':
			return `${v.ed} ${who}`;
		case 'skipped':
			return `Already ${v.ed.toLowerCase()} ${who}`;
		case 'failed':
			return `Couldn't ${v.bare} ${who}`;
		case 'parked':
			return 'Reconnect to Bluesky to finish';
		case 'timeout':
			return 'Still working — check the record';
	}
}

export const POLL_INTERVAL_MS = 1000;
export const POLL_TIMEOUT_MS = 60000;

export interface PollOptions {
	intervalMs?: number;
	timeoutMs?: number;
	sleep?: (ms: number) => Promise<void>;
	now?: () => number;
}

const realSleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** Fetch until `settle()` is non-null or `timeoutMs` has elapsed. A fetch
 *  that throws is a dropped poll, not a verdict — the runner is still
 *  working behind it, so keep going. */
export async function pollUntilSettled(
	fetch: () => Promise<ActionBatchDetail>,
	opts: PollOptions = {}
): Promise<Settled | 'timeout'> {
	const intervalMs = opts.intervalMs ?? POLL_INTERVAL_MS;
	const timeoutMs = opts.timeoutMs ?? POLL_TIMEOUT_MS;
	const sleep = opts.sleep ?? realSleep;
	const now = opts.now ?? Date.now;
	const started = now();
	for (;;) {
		try {
			const settled = settle(await fetch());
			if (settled) return settled;
		} catch {
			// blip — fall through to the sleep
		}
		if (now() - started >= timeoutMs) return 'timeout';
		await sleep(intervalMs);
	}
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `npm --prefix web run test -- --run src/lib/action-progress.test.ts`
Expected: PASS, 11 tests.

- [ ] **Step 5: Commit**

```bash
git add web/src/lib/action-progress.ts web/src/lib/action-progress.test.ts
git commit -m 'feat(332): settle/toastCopy/pollUntilSettled helpers for single actions'
```

---

### Task 3: `Toast.svelte` and the layout mount

**Files:**
- Create: `web/src/lib/components/Toast.svelte`
- Modify: `web/src/routes/(protected)/+layout.svelte:108-111` (add `<Toast />` after `</main>`)

**Interfaces:**
- Consumes: `toasts`, `dismiss`, `Toast` type from Task 1.
- Produces: the one and only `<Toast />` mount. No props.

There is no vitest coverage for `.svelte` files (vitest runs in plain node without the SvelteKit plugin); this task's gate is `npm --prefix web run check`, `npm --prefix web run build`, and the svelte MCP validator (`mcp__svelte__svelte-autofixer`) reporting no issues on the new component.

- [ ] **Step 1: Write the component**

Create `web/src/lib/components/Toast.svelte`:

```svelte
<script lang="ts">
	// Renders the toast store (#332, spec §3.2). Mounted once in the
	// protected layout; every page raises through `$lib/toast`. Newest is
	// last in the store and sits nearest the viewport edge (column-reverse).
	import { fly } from 'svelte/transition';
	import { toasts, dismiss } from '$lib/toast';
	import '$lib/website/styles/tokens.css';

	// Respect reduced motion by zeroing the transition rather than branching
	// the markup — one code path, same DOM either way.
	const reduced =
		typeof window !== 'undefined' && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
	const flyOpts = { y: reduced ? 0 : 16, duration: reduced ? 0 : 320, easing: easeOutExpo };

	function easeOutExpo(t: number): number {
		// cubic-bezier(0.16, 1, 0.3, 1) ≈ 1 - 2^(-10t); `--ease-out-expo` in
		// tokens.css is the CSS twin for hover/transform transitions.
		return t === 1 ? 1 : 1 - Math.pow(2, -10 * t);
	}
</script>

<div class="stack" aria-live="polite">
	{#each $toasts as t (t.id)}
		<div
			class="toast"
			data-tone={t.tone}
			role={t.tone === 'error' ? 'alert' : 'status'}
			transition:fly={flyOpts}
		>
			{#if t.tone === 'working'}
				<span class="spinner" aria-hidden="true"></span>
			{/if}
			<span class="text">{t.text}</span>
			{#each t.actions as a (a.label)}
				<button class="action" onclick={a.onclick}>{a.label}</button>
			{/each}
			{#if t.href}
				<a class="link" href={t.href.url}>{t.href.label}</a>
			{/if}
			<button class="close" aria-label="Dismiss" onclick={() => dismiss(t.id)}>×</button>
		</div>
	{/each}
</div>

<style>
	.stack {
		position: fixed;
		bottom: 1rem;
		left: 50%;
		transform: translateX(-50%);
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		width: min(28rem, calc(100vw - 2rem));
		z-index: 60;
		pointer-events: none;
	}
	@media (min-width: 720px) {
		.stack {
			left: 1.5rem;
			transform: none;
		}
	}
	.toast {
		pointer-events: auto;
		display: flex;
		align-items: center;
		gap: 0.625rem;
		padding: 0.625rem 0.75rem 0.625rem 0.875rem;
		background: var(--charcoal-800);
		color: var(--cream-50);
		border: 1px solid rgb(var(--charcoal-400-rgb) / 0.15);
		border-left: 3px solid var(--charcoal-500);
		border-radius: 8px;
		font-size: 0.8125rem;
		box-shadow: 0 8px 24px -8px rgb(var(--charcoal-950-rgb) / 0.6);
	}
	.toast[data-tone='ok'] {
		border-left-color: var(--status-ok);
	}
	.toast[data-tone='error'] {
		border-left-color: var(--status-error);
	}
	.text {
		flex: 1;
		min-width: 0;
	}
	.action,
	.link {
		font: inherit;
		font-size: 0.8125rem;
		font-weight: 500;
		color: var(--copper);
		background: none;
		border: 0;
		padding: 0.125rem 0.25rem;
		cursor: pointer;
		text-decoration: none;
	}
	.action:hover,
	.link:hover {
		text-decoration: underline;
	}
	.close {
		font: inherit;
		font-size: 1rem;
		line-height: 1;
		color: var(--charcoal-500);
		background: none;
		border: 0;
		padding: 0 0.25rem;
		cursor: pointer;
	}
	.close:hover {
		color: var(--cream-50);
	}
	.spinner {
		width: 0.875rem;
		height: 0.875rem;
		flex: none;
		border: 2px solid rgb(var(--charcoal-400-rgb) / 0.2);
		border-top-color: var(--charcoal-400);
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}
	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.spinner {
			animation: none;
		}
	}
</style>
```

- [ ] **Step 2: Mount it once in the protected layout**

In `web/src/routes/(protected)/+layout.svelte`, add the import next to the other component imports at the top of the `<script>`:

```ts
	import Toast from '$lib/components/Toast.svelte';
```

and change the main block (currently lines 108-111) to:

```svelte
		<main class="main">
			{@render children()}
		</main>
		<Toast />
```

- [ ] **Step 3: Validate the component**

Run the svelte MCP validator on `web/src/lib/components/Toast.svelte` (`mcp__svelte__svelte-autofixer`), apply anything it flags, re-run until clean.

Run: `npm --prefix web run check`
Expected: no errors beyond the five pre-existing on `accounts/[handle]/+page.svelte`.

Run: `npm --prefix web run build`
Expected: build succeeds.

- [ ] **Step 4: Commit**

```bash
git add web/src/lib/components/Toast.svelte "web/src/routes/(protected)/+layout.svelte"
git commit -m 'feat(332): Toast renderer mounted once in the protected layout'
```

---

### Task 4: `ActionButtons.svelte` finishes in place

**Files:**
- Modify: `web/src/lib/components/ActionButtons.svelte` (full rewrite of the `<script>` and the button markup; CSS gains a `.working` rule)

**Interfaces:**
- Consumes: `raise`, `update`, `dismiss` (Task 1); `pollUntilSettled`, `toastCopy`, `type ToastKind` (Task 2); existing `createActionBatch`, `undoAction`, `retryBatch`, `getActionBatch`, `startConsent`, `NotConnectedError` from `$lib/api.js`; `buttonState` from `$lib/action-selection`.
- Produces: no new exports. Props are unchanged (`handle, did, tier, resume, actionsError, impersonating`). Both `goto` calls are gone; `$app/navigation` is no longer imported.

Behavioural contract (spec §3.1): confirm → `createActionBatch` → working toast + button label `Muting…`/`Blocking…` with the other button disabled → poll → `refresh()` → toast settles per the table (Undo only on a settled non-undo `applied`; Retry on `failed`; Reconnect on `parked`; `Record` link always) → ok toasts auto-dismiss at 6 s.

- [ ] **Step 1: Replace the `<script>` block**

Replace the whole `<script lang="ts">…</script>` in `web/src/lib/components/ActionButtons.svelte` with:

```svelte
<script lang="ts">
	// Per-account Mute / Block with confirm sheet (#315, spec §5.2). Hidden
	// entirely when the server has the feature off or the viewer is
	// impersonating — nobody acts with someone else's credentials.
	//
	// A single action finishes in place (#332): no navigation to the batch
	// page. The button shows `Muting…` while a toast polls the batch, then
	// both settle from the server's answer.
	import '$lib/website/styles/tokens.css';
	import { onMount } from 'svelte';
	import {
		getActionsStatus,
		getAccountActions,
		getActionBatch,
		createActionBatch,
		undoAction,
		retryBatch,
		startConsent,
		NotConnectedError
	} from '$lib/api.js';
	import { buttonState, type ActiveRow } from '$lib/action-selection';
	import { pollUntilSettled, toastCopy, type ToastKind } from '$lib/action-progress';
	import { raise, update, dismiss } from '$lib/toast';
	import ConfirmSheet from '$lib/components/ConfirmSheet.svelte';
	import type { ActionKind, ActionsStatus } from '$lib/types.js';

	interface Props {
		handle: string;
		did: string;
		tier: string | null;
		/** `?resume=mute|block|undo` from a consent round-trip. `mute`/`block`
		 *  reopen the confirm sheet; `undo` cannot (the round-trip doesn't carry
		 *  which action was being undone), so it surfaces a notice instead. */
		resume?: string | null;
		/** `?actions_error=` from a failed consent round-trip. */
		actionsError?: string | null;
		impersonating?: boolean;
	}

	let { handle, did, tier, resume = null, actionsError = null, impersonating = false }: Props = $props();

	let status = $state<ActionsStatus | null>(null);
	let active = $state<ActiveRow[]>([]);
	let busy = $state(false);
	/** Which button is mid-flight, so it can read `Muting…` and the other
	 *  can sit disabled. `'undo'` disables both without relabelling. */
	let inflight = $state<ActionKind | 'undo' | null>(null);
	let error = $state('');
	let notice = $state('');
	let sheet = $state<ActionKind | null>(null);

	const KINDS: ActionKind[] = ['mute', 'block'];
	let states = $derived(Object.fromEntries(KINDS.map((k) => [k, buttonState(active, k)])));

	const WORKING_LABEL: Record<ActionKind, string> = { mute: 'Muting…', block: 'Blocking…' };
	/** Settled ok toasts hold this long, then go (spec §3.1). */
	const OK_TTL_MS = 6000;

	const ERROR_COPY: Record<string, string> = {
		denied: "Bluesky didn't grant permission. Nothing was changed.",
		invalid_scope: "Bluesky granted different permissions than Charcoal asked for. Nothing was changed.",
		failed: "Something went wrong while connecting. Nothing was changed.",
		disabled: 'Mute and block actions are not enabled on this server.'
	};

	async function refresh() {
		const res = await getAccountActions(handle);
		active = res.actions.map((r) => ({ id: r.id, kind: r.kind, status: r.status }));
	}

	onMount(async () => {
		try {
			status = await getActionsStatus();
			if (!status.enabled) return;
			await refresh();
		} catch {
			status = null;
			return;
		}
		if (actionsError) error = ERROR_COPY[actionsError] ?? ERROR_COPY.failed;
		else if (resume === 'mute' || resume === 'block') sheet = resume;
		else if (resume === 'undo') notice = 'Connected to Bluesky. Click Undo again to finish.';
	});

	/** Raise the working toast for `batchId`, poll it to a verdict, refresh
	 *  the buttons, and settle the toast. `toastKind` picks the verb;
	 *  `buttonKind` is the button that can offer Undo (null for an undo
	 *  batch — redoing is the original button, which is back to `Mute`). */
	async function track(batchId: number, toastKind: ToastKind, buttonKind: ActionKind | null) {
		const record = { label: 'Record', url: `/actions/${batchId}` };
		const id = raise({ tone: 'working', text: toastCopy(toastKind, handle, 'working'), actions: [] });
		const settled = await pollUntilSettled(() => getActionBatch(batchId));
		try {
			await refresh();
		} catch {
			// The toast still tells the truth; the buttons catch up next load.
		}
		inflight = null;
		if (settled === 'timeout') {
			update(id, { tone: 'error', text: toastCopy(toastKind, handle, 'timeout'), actions: [], href: record });
			return;
		}
		switch (settled.kind) {
			case 'applied': {
				// Captured as a const so the narrowing survives into the closure.
				const bk = buttonKind;
				const actions =
					bk !== null
						? [{ label: 'Undo', onclick: () => { dismiss(id); void undo(bk); } }]
						: [];
				update(id, { tone: 'ok', text: toastCopy(toastKind, handle, 'applied'), actions, href: record, ttlMs: OK_TTL_MS });
				return;
			}
			case 'skipped':
				update(id, { tone: 'ok', text: toastCopy(toastKind, handle, 'skipped'), actions: [], href: record, ttlMs: OK_TTL_MS });
				return;
			case 'failed':
				update(id, {
					tone: 'error',
					text: toastCopy(toastKind, handle, 'failed'),
					actions: [{ label: 'Retry', onclick: () => { dismiss(id); void retry(batchId, toastKind, buttonKind); } }],
					href: record
				});
				return;
			case 'parked':
				update(id, {
					tone: 'error',
					text: toastCopy(toastKind, handle, 'parked'),
					actions: [{ label: 'Reconnect', onclick: () => { void startConsent(buttonKind ?? 'undo', { handle }); } }],
					href: record
				});
				return;
		}
	}

	async function retry(batchId: number, toastKind: ToastKind, buttonKind: ActionKind | null) {
		inflight = buttonKind ?? 'undo';
		error = '';
		try {
			const res = await retryBatch(batchId);
			await track(res.batch_id, toastKind, buttonKind);
		} catch (e) {
			inflight = null;
			if (e instanceof NotConnectedError) {
				await startConsent(buttonKind ?? 'undo', { handle });
				return;
			}
			error = e instanceof Error ? e.message : 'Something went wrong';
		}
	}

	async function confirm(kind: ActionKind) {
		sheet = null;
		busy = true;
		error = '';
		notice = '';
		try {
			const res = await createActionBatch(kind, `account:${handle}`, [did]);
			if (res.batch_id === null) {
				// The server returns batch_id: null when every target is already
				// in force (is_in_force: Charcoal applied it, or the user already
				// held it themselves) — never for "in progress" work.
				notice = 'That action is already in place.';
				await refresh();
				return;
			}
			inflight = kind;
			busy = false;
			await track(res.batch_id, kind, kind);
		} catch (e) {
			inflight = null;
			if (e instanceof NotConnectedError) {
				await startConsent(kind, { handle });
				return;
			}
			error = e instanceof Error ? e.message : 'Something went wrong';
		} finally {
			busy = false;
		}
	}

	async function undo(kind: ActionKind) {
		const id = states[kind].actionId;
		if (id === null) return;
		busy = true;
		error = '';
		notice = '';
		try {
			const res = await undoAction(id);
			inflight = 'undo';
			busy = false;
			await track(res.batch_id, kind === 'mute' ? 'unmute' : 'unblock', null);
		} catch (e) {
			inflight = null;
			if (e instanceof NotConnectedError) {
				await startConsent('undo', { handle });
				return;
			}
			error = e instanceof Error ? e.message : 'Something went wrong';
		} finally {
			busy = false;
		}
	}
</script>
```

- [ ] **Step 2: Replace the button markup**

Replace the `{#each KINDS as kind (kind)} … {/each}` block with:

```svelte
		{#each KINDS as kind (kind)}
			{@const s = states[kind]}
			{#if inflight === kind}
				<button class="act working" data-kind={kind} disabled>
					<span class="spinner" aria-hidden="true"></span>
					{WORKING_LABEL[kind]}
				</button>
			{:else if s.state === 'done'}
				<span class="done">{s.label}</span>
				<!-- No Undo when `actionId` is null: that mute or block is the
				     person's own, and Charcoal does not remove it (#261). -->
				{#if s.actionId !== null}
					<button class="undo" onclick={() => undo(kind)} disabled={busy || inflight !== null}>Undo</button>
				{/if}
			{:else}
				<button class="act" data-kind={kind} onclick={() => (sheet = kind)} disabled={busy || inflight !== null}>
					{s.label}
				</button>
			{/if}
		{/each}
```

The `{#if error} … {:else if notice}` block and the `<ConfirmSheet …/>` block stay exactly as they are.

- [ ] **Step 3: Add the working-state CSS**

Append inside `<style>`, after the `.act:disabled, .undo:disabled` rule:

```css
	.act.working {
		display: inline-flex;
		align-items: center;
		gap: 0.375rem;
		opacity: 1;
		cursor: progress;
	}
	.spinner {
		width: 0.75rem;
		height: 0.75rem;
		border: 2px solid rgb(var(--charcoal-400-rgb) / 0.2);
		border-top-color: var(--charcoal-400);
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}
	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.spinner {
			animation: none;
		}
	}
```

- [ ] **Step 4: Validate**

Run the svelte MCP validator on `web/src/lib/components/ActionButtons.svelte`; fix anything it flags.

Run: `rg -n "goto|app/navigation" web/src/lib/components/ActionButtons.svelte`
Expected: no output.

Run: `npm --prefix web run check`
Expected: no errors beyond the five pre-existing on `accounts/[handle]/+page.svelte`.

Run: `npm --prefix web run test -- --run`
Expected: all green (the store and helper suites from Tasks 1–2 plus the existing suites).

Run: `npm --prefix web run build`
Expected: succeeds.

- [ ] **Step 5: Commit**

```bash
git add web/src/lib/components/ActionButtons.svelte
git commit -m 'feat(332): single mute/block/undo settles in place with a toast'
```

---

### Task 5: Batch page banner, return link, 1 s poll

**Files:**
- Modify: `web/src/lib/action-status.ts` (append `bannerSummary`, `returnPath`)
- Modify: `web/src/lib/action-status.test.ts` (append two `describe` blocks)
- Modify: `web/src/routes/(protected)/actions/[id]/+page.svelte`

**Interfaces:**
- Consumes: existing `isRunning`, `isParked`, `canRetry`, `canUndo`, private `n()`.
- Produces:
  ```ts
  export interface BannerSummary { title: 'Done' | 'Undone' | 'Finished with problems'; detail: string; tone: 'ok' | 'error' }
  export function bannerSummary(b: ActionBatchSummary, rows: ActionRowView[]): BannerSummary;
  export interface ReturnPath { href: string; label: string }
  export function returnPath(source: string, asUserSuffix: string): ReturnPath;
  ```
- Design note: an `undo` batch's `counts` are keyed by row status, not by the kind undone, so `bannerSummary` takes the rows too and counts `undone`/`applied` rows by `row.kind` to say "3 unmuted, 1 unblocked". The batch detail page already has the rows in hand.

- [ ] **Step 1: Write the failing tests**

Append to `web/src/lib/action-status.test.ts` (reuse the file's existing `summary(over)` and `row(over)` fixture builders; add `bannerSummary, returnPath` to the import from `./action-status.js`):

```ts
describe('bannerSummary', () => {
	it('done mute batch', () => {
		const b = summary({ kind: 'mute', status: 'done', counts: { applied: 12, skipped_already_done: 2 } });
		expect(bannerSummary(b, [])).toEqual({ title: 'Done', detail: '14 muted, 0 failed', tone: 'ok' });
	});

	it('done block batch of one', () => {
		const b = summary({ kind: 'block', status: 'done', counts: { applied: 1 } });
		expect(bannerSummary(b, [])).toEqual({ title: 'Done', detail: '1 blocked, 0 failed', tone: 'ok' });
	});

	it('partial batch is a problem', () => {
		const b = summary({ kind: 'mute', status: 'partial', counts: { applied: 12, failed: 2 } });
		expect(bannerSummary(b, [])).toEqual({ title: 'Finished with problems', detail: '12 muted, 2 failed', tone: 'error' });
	});

	it('failed batch that never wrote is a problem with zero done', () => {
		const b = summary({ kind: 'block', status: 'failed', counts: { pending: 3 } });
		expect(bannerSummary(b, [])).toEqual({ title: 'Finished with problems', detail: '0 blocked, 0 failed', tone: 'error' });
	});

	it('undo batch counts rows by the kind undone', () => {
		const b = summary({ kind: 'undo', status: 'done', counts: { undone: 3, skipped_already_done: 1 } });
		const rows = [
			row({ kind: 'mute', status: 'undone' }),
			row({ kind: 'mute', status: 'undone' }),
			row({ kind: 'block', status: 'undone' }),
			row({ kind: 'mute', status: 'skipped_already_done' })
		];
		expect(bannerSummary(b, rows)).toEqual({ title: 'Undone', detail: '3 unmuted, 1 unblocked', tone: 'ok' });
	});

	it('undo batch with a failure', () => {
		const b = summary({ kind: 'undo', status: 'partial', counts: { undone: 1, failed: 1 } });
		const rows = [row({ kind: 'mute', status: 'undone' }), row({ kind: 'block', status: 'failed' })];
		expect(bannerSummary(b, rows)).toEqual({ title: 'Finished with problems', detail: '1 unmuted, 0 unblocked, 1 failed', tone: 'error' });
	});
});

describe('returnPath', () => {
	it('tier source goes back to the filtered list', () => {
		expect(returnPath('tier:Watch', '')).toEqual({ href: '/accounts?tier=Watch', label: '← Back to Watch accounts' });
	});

	it('account source goes back to the account', () => {
		expect(returnPath('account:alice.bsky.social', '')).toEqual({
			href: '/accounts/alice.bsky.social',
			label: '← Back to @alice.bsky.social'
		});
	});

	it('anything else goes back to the list', () => {
		expect(returnPath('', '')).toEqual({ href: '/accounts', label: '← Back to accounts' });
		expect(returnPath('retry:12', '')).toEqual({ href: '/accounts', label: '← Back to accounts' });
	});

	it('appends the as_user suffix, joining with & when there is already a query', () => {
		expect(returnPath('tier:High', '?as_user=x').href).toBe('/accounts?tier=High&as_user=x');
		expect(returnPath('account:bob', '?as_user=x').href).toBe('/accounts/bob?as_user=x');
		expect(returnPath('', '?as_user=x').href).toBe('/accounts?as_user=x');
	});

	it('encodes the tier and handle', () => {
		expect(returnPath('tier:Very High', '').href).toBe('/accounts?tier=Very%20High');
		expect(returnPath('account:a b', '').href).toBe('/accounts/a%20b');
	});
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npm --prefix web run test -- --run src/lib/action-status.test.ts`
Expected: FAIL — `bannerSummary is not a function` / `returnPath is not a function`.

- [ ] **Step 3: Implement**

Append to `web/src/lib/action-status.ts`:

```ts
export interface BannerSummary {
	title: 'Done' | 'Undone' | 'Finished with problems';
	detail: string;
	tone: 'ok' | 'error';
}

/** The finished-batch banner on the batch page (#332, spec §4). Only
 *  meaningful once `!isRunning(b) && !isParked(b)`. An undo batch's counts
 *  are by row status, so the rows are needed to say what was undone. */
export function bannerSummary(b: ActionBatchSummary, rows: ActionRowView[]): BannerSummary {
	const c = b.counts;
	const failed = c.failed ?? 0;
	const problem = b.status === 'partial' || b.status === 'failed';
	const tone = problem ? 'error' : 'ok';
	if (b.kind === 'undo') {
		const undone = rows.filter((r) => r.status === 'undone' || r.status === 'applied');
		const unmuted = undone.filter((r) => r.kind === 'mute').length;
		const unblocked = undone.filter((r) => r.kind === 'block').length;
		let detail = `${unmuted} unmuted, ${unblocked} unblocked`;
		if (failed) detail += `, ${failed} failed`;
		return { title: problem ? 'Finished with problems' : 'Undone', detail, tone };
	}
	const done = (c.applied ?? 0) + (c.skipped_already_done ?? 0);
	const past = b.kind === 'mute' ? 'muted' : 'blocked';
	return {
		title: problem ? 'Finished with problems' : 'Done',
		detail: `${done} ${past}, ${failed} failed`,
		tone
	};
}

export interface ReturnPath {
	href: string;
	label: string;
}

/** Where "← Back" goes from a finished batch: the place the batch was
 *  started from, read off `batch.source` (#332, spec §4). `asUserSuffix` is
 *  the page's existing `?as_user=…` string or ''. */
export function returnPath(source: string, asUserSuffix: string): ReturnPath {
	const tier = source.startsWith('tier:') ? source.slice('tier:'.length) : '';
	const handle = source.startsWith('account:') ? source.slice('account:'.length) : '';
	let href: string;
	let label: string;
	if (tier) {
		href = `/accounts?tier=${encodeURIComponent(tier)}`;
		label = `← Back to ${tier} accounts`;
	} else if (handle) {
		href = `/accounts/${encodeURIComponent(handle)}`;
		label = `← Back to @${handle}`;
	} else {
		href = '/accounts';
		label = '← Back to accounts';
	}
	if (asUserSuffix) {
		// The suffix already starts with '?'; when the href has a query of
		// its own, join with '&' instead.
		href += href.includes('?') ? `&${asUserSuffix.slice(1)}` : asUserSuffix;
	}
	return { href, label };
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `npm --prefix web run test -- --run src/lib/action-status.test.ts`
Expected: PASS (existing tests + 11 new).

- [ ] **Step 5: Commit the helpers**

```bash
git add web/src/lib/action-status.ts web/src/lib/action-status.test.ts
git commit -m 'feat(332): bannerSummary and returnPath for the finished batch page'
```

- [ ] **Step 6: Wire the batch page**

In `web/src/routes/(protected)/actions/[id]/+page.svelte`:

(a) Extend the `$lib/action-status` import:

```ts
	import {
		batchHeadline,
		driftNote,
		isRunning,
		isParked,
		canRetry,
		canUndo,
		bannerSummary,
		returnPath
	} from '$lib/action-status';
```

(b) Poll at 1 s. In `load()`, change the comment and the interval:

```ts
			// Reset both first: this runs every second while a batch is in
			// flight, and a single dropped poll must not latch the page into an
			// error state while the runner is still applying blocks behind it.
```

and

```ts
		if (running && !timer) timer = setInterval(load, POLL_MS);
```

with, next to the other module-level `let`s:

```ts
	/** 1 s: one small JSON read; the old 3 s made a 1 s action read as 3 s+ (#332). */
	const POLL_MS = 1000;
```

(c) Add a derived flag after `let timer …`:

```ts
	/** The banner replaces the header controls once the runner is done and
	 *  nobody is waiting on a reconnect (spec §4). */
	let finished = $derived(detail ? !isRunning(detail.batch) && !isParked(detail.batch) : false);
```

(d) Replace the `.controls` block inside `.header` so that Undo all / Retry failed render there **only while not finished**:

```svelte
			{#if !asUser && !finished}
				<div class="controls">
					{#if isParked(b)}
						<button onclick={() => startConsent('undo')} disabled={busy}>Reconnect</button>
					{/if}
				</div>
			{/if}
```

(Undo all and Retry failed cannot show while running/parked anyway — `canUndo`/`canRetry` return false there — so nothing is lost.)

(e) Insert the banner between `</div>` closing `.header` and `<table class="rows">`:

```svelte
		{#if finished}
			{@const s = bannerSummary(b, detail.actions)}
			{@const back = returnPath(b.source, asUserSuffix)}
			<div class="banner" data-tone={s.tone} role="status">
				<div class="banner-text">
					<strong>{s.title}</strong>
					<span class="banner-detail">· {s.detail}</span>
				</div>
				<div class="banner-actions">
					{#if !asUser && canRetry(b)}
						<button onclick={() => run(() => retryBatch(b.id), b.kind)} disabled={busy}>Retry failed</button>
					{/if}
					{#if !asUser && canUndo(b)}
						<button onclick={() => run(() => undoBatch(b.id), 'undo')} disabled={busy}>Undo all</button>
					{/if}
					<a class="banner-back" href={back.href}>{back.label}</a>
				</div>
			</div>
		{/if}
```

(f) Add CSS (keep the file's one-line-per-rule style), after the `.controls button:disabled, .link:disabled` line:

```css
	.banner { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 0.75rem; margin: 0 0 1.25rem; padding: 0.75rem 1rem; border-radius: 8px; border: 1px solid rgb(var(--status-ok-rgb) / 0.3); border-left: 3px solid var(--status-ok); background: rgb(var(--status-ok-rgb) / 0.06); font-size: 0.875rem; }
	.banner[data-tone='error'] { border-color: rgb(var(--status-error-rgb) / 0.3); border-left-color: var(--status-error); background: rgb(var(--status-error-rgb) / 0.06); }
	.banner-text strong { font-weight: 500; }
	.banner-detail { color: var(--charcoal-400); }
	.banner-actions { display: flex; align-items: center; gap: 0.5rem; }
	.banner-actions button { padding: 0.375rem 0.75rem; font: inherit; font-size: 0.8125rem; border: 1px solid rgb(var(--charcoal-400-rgb) / 0.15); border-radius: 8px; background: transparent; color: var(--charcoal-400); cursor: pointer; }
	.banner-actions button:disabled { opacity: 0.5; cursor: not-allowed; }
	.banner-back { font-size: 0.8125rem; color: var(--copper); text-decoration: none; }
	.banner-back:hover { text-decoration: underline; }
```

- [ ] **Step 7: Validate**

Run the svelte MCP validator on the page; fix anything flagged.

Run: `rg -n "3000" "web/src/routes/(protected)/actions/[id]/+page.svelte"`
Expected: no output.

Run: `npm --prefix web run check` — no new errors. Run: `npm --prefix web run build` — succeeds.

- [ ] **Step 8: Commit**

```bash
git add "web/src/routes/(protected)/actions/[id]/+page.svelte"
git commit -m 'feat(332): batch page finishes with a Done banner, return link, 1 s poll'
```

---

### Task 6: Runner timing spans

**Files:**
- Modify: `src/web/actions/runner.rs` (`run_batch_inner`, `finalize`, `call`, `run_mutes`, `run_blocks`, `run_undo`, `apply_chunked`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `call` gains a leading `op: &'static str` argument: `async fn call<T, F, Fut>(&self, op: &'static str, mut f: F) -> Result<Result<T, PdsError>, Halt>`. `finalize` gains `started: Instant`. Every existing `self.call(|| …)` site passes its nsid-ish label. No public API changes; `RunnerConfig` untouched.

Log lines (all `tracing::info!`, all with `elapsed_ms: u64`):

| Where | Line |
|---|---|
| after `load_for_write` returns (Ok or Err) | `info!(batch_id, span = "load_session", elapsed_ms, "timing")` |
| after each `get_mutes`/`get_blocks` in `run_mutes`/`run_blocks`/`run_undo` | `info!(span = "reconcile", op = "getMutes" \| "getBlocks", elapsed_ms, "timing")` |
| inside `call`, after **every** attempt | `info!(span = "pds_call", op, attempt, elapsed_ms, "timing")` |
| `finalize` | `info!(batch_id, status, ok, failed, span = "batch", elapsed_ms, "action batch finished")` |

The lines carry ids, labels, counts and durations only — never a token, key, nonce, or error body.

- [ ] **Step 1: Confirm the runner suite is green before touching it**

Run: `cargo test --features web --test unit_actions_runner`
Expected: PASS (this is the baseline; the timing lines are asserted by compilation only — spec §7).

- [ ] **Step 2: Import `Instant`**

Change line 8 of `src/web/actions/runner.rs`:

```rust
use std::time::{Duration, Instant};
```

- [ ] **Step 3: Time the session load and the whole batch in `run_batch_inner`**

Replace the body from `self.db.set_action_batch_status(batch_id, "running", None).await?;` down to `let pds = …` with:

```rust
        let batch_started = Instant::now();
        self.db
            .set_action_batch_status(batch_id, "running", None)
            .await?;

        // Includes any token refresh the session store does on the way.
        let load_started = Instant::now();
        let loaded = self
            .sessions
            .load_for_write(&*self.db, &self.http, &self.oauth_client, &batch.user_did)
            .await;
        info!(
            batch_id,
            span = "load_session",
            elapsed_ms = load_started.elapsed().as_millis() as u64,
            "timing"
        );
        let session = match loaded {
            Ok(s) => s,
            Err(SessionError::NotConnected) => {
                self.db
                    .set_action_batch_status(batch_id, "queued", Some("not_connected"))
                    .await?;
                return Ok(());
            }
            Err(e) => anyhow::bail!("load write session: {e}"),
        };
        let pds = session.pds_client(self.http.clone());
```

and change the last line of `run_batch_inner` to:

```rust
        self.finalize(batch_id, batch_started).await
```

- [ ] **Step 4: `finalize` reports the batch wall time**

```rust
    async fn finalize(&self, batch_id: i64, started: Instant) -> anyhow::Result<()> {
        let rows = self.db.list_actions_for_batch(batch_id).await?;
        let failed = rows.iter().filter(|a| a.status == "failed").count();
        let ok = rows
            .iter()
            .filter(|a| {
                matches!(
                    a.status.as_str(),
                    "applied" | "skipped_already_done" | "undone"
                )
            })
            .count();
        let status = if failed == 0 {
            "done"
        } else if ok == 0 {
            "failed"
        } else {
            "partial"
        };
        info!(
            batch_id,
            status,
            ok,
            failed,
            span = "batch",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "action batch finished"
        );
        self.db
            .set_action_batch_status(batch_id, status, None)
            .await
    }
```

- [ ] **Step 5: `call` takes an `op` label and times every attempt**

```rust
    /// One PDS call under the retry policy. `Err(Halt)` stops the batch;
    /// `Ok(Err(_))` fails this action only. `op` is the nsid-ish label for
    /// the timing line (`"muteActor"`, `"applyWrites"`, …) — #333 needs to
    /// see where the seconds go, per attempt.
    async fn call<T, F, Fut>(&self, op: &'static str, mut f: F) -> Result<Result<T, PdsError>, Halt>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, PdsError>>,
    {
        let mut transient = 0u32;
        let mut limited = 0u32;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let started = Instant::now();
            let result = f().await;
            info!(
                span = "pds_call",
                op,
                attempt,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "timing"
            );
            match result {
                Ok(v) => return Ok(Ok(v)),
                Err(PdsError::Auth) => return Err(Halt::NotConnected),
                Err(PdsError::RateLimited { reset_at }) if limited < RATE_LIMIT_MAX_WAITS => {
                    limited += 1;
                    let wait = reset_at
                        .map(|t| {
                            Duration::from_secs((t - chrono::Utc::now().timestamp()).max(1) as u64)
                        })
                        .unwrap_or(self.cfg.backoff * 4)
                        .min(self.cfg.max_wait);
                    info!(
                        wait_ms = wait.as_millis() as u64,
                        "rate limited by PDS — pausing"
                    );
                    tokio::time::sleep(wait).await;
                }
                Err(e) if e.is_retryable() && transient < TRANSIENT_RETRIES => {
                    tokio::time::sleep(self.cfg.backoff * 2u32.pow(transient)).await;
                    transient += 1;
                }
                Err(e) => return Ok(Err(e)),
            }
        }
    }
```

- [ ] **Step 6: Label every call site and time the reconcile reads**

`run_mutes` — replace the `let existing = …` statement:

```rust
        let started = Instant::now();
        let existing = self.call("getMutes", || pds.get_mutes()).await;
        info!(
            span = "reconcile",
            op = "getMutes",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "timing"
        );
        let existing = match existing {
            Ok(Ok(set)) => set,
            Ok(Err(e)) => anyhow::bail!("getMutes: {e}"),
            Err(h) => return Ok(Some(h)),
        };
```

and `self.call(|| pds.mute_actor(&a.target_did))` → `self.call("muteActor", || pds.mute_actor(&a.target_did))`.

`run_blocks` — same shape:

```rust
        let started = Instant::now();
        let existing = self.call("getBlocks", || pds.get_blocks()).await;
        info!(
            span = "reconcile",
            op = "getBlocks",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "timing"
        );
        let existing = match existing {
            Ok(Ok(map)) => map,
            Ok(Err(e)) => anyhow::bail!("getBlocks: {e}"),
            Err(h) => return Ok(Some(h)),
        };
```

`run_undo` — both conditional reads:

```rust
        let blocks = if pending.iter().any(|a| a.kind == "block") {
            let started = Instant::now();
            let got = self.call("getBlocks", || pds.get_blocks()).await;
            info!(
                span = "reconcile",
                op = "getBlocks",
                elapsed_ms = started.elapsed().as_millis() as u64,
                "timing"
            );
            match got {
                Ok(Ok(map)) => map,
                Ok(Err(e)) => anyhow::bail!("getBlocks: {e}"),
                Err(h) => return Ok(Some(h)),
            }
        } else {
            Default::default()
        };
        let mutes = if pending.iter().any(|a| a.kind == "mute") {
            let started = Instant::now();
            let got = self.call("getMutes", || pds.get_mutes()).await;
            info!(
                span = "reconcile",
                op = "getMutes",
                elapsed_ms = started.elapsed().as_millis() as u64,
                "timing"
            );
            match got {
                Ok(Ok(set)) => set,
                Ok(Err(e)) => anyhow::bail!("getMutes: {e}"),
                Err(h) => return Ok(Some(h)),
            }
        } else {
            Default::default()
        };
```

and `self.call(|| pds.unmute_actor(&a.target_did))` → `self.call("unmuteActor", || pds.unmute_actor(&a.target_did))`.

`apply_chunked` — both sites: `self.call("applyWrites", || pds.apply_writes(&writes))` and `self.call("applyWrites", || pds.apply_writes(&one))`.

- [ ] **Step 7: Verify nothing else calls `call` without a label**

Run: `rg -n "self\.call\(\|\|" src/web/actions/runner.rs`
Expected: no output.

- [ ] **Step 8: Build, lint, test**

Run: `cargo clippy --features web --all-targets -- -D warnings`
Expected: clean.

Run: `cargo test --features web --test unit_actions_runner`
Expected: PASS, same count as Step 1.

- [ ] **Step 9: Commit**

```bash
git add src/web/actions/runner.rs
git commit -m 'feat(333): elapsed_ms timing spans for load_session, reconcile, pds_call, batch'
```

---

### Task 7: DPoP nonce cache

**Files:**
- Modify: `src/web/actions/dpop_http.rs` (add `NonceCache`, thread it through `send_dpop`)
- Modify: `src/web/actions/pds.rs:84-107, 225-238, 261-268` (`PdsClient` owns a cache)
- Modify: `src/web/actions/session.rs:466-473` (refresh passes a fresh cache)
- Test: `tests/unit_actions_pds.rs` (three new wiremock tests + a `ProofNonce` matcher)

**Interfaces:**
- Produces:
  ```rust
  // src/web/actions/dpop_http.rs
  #[derive(Default)]
  pub struct NonceCache(std::sync::Mutex<Option<String>>);
  impl NonceCache { pub fn get(&self) -> Option<String>; pub fn set(&self, nonce: &str); }
  pub async fn send_dpop(http, key, method, url, access_token: Option<&str>, nonce: &NonceCache, build) -> Result<DpopResponse>;
  ```
  `PdsClient::new(http, pds_url, did, dpop_key, access_token)` is **unchanged**; the struct gains a private `nonce: NonceCache` field initialised with `NonceCache::default()`.
- Note: `session.rs` already imports `tokio::sync::Mutex`; the cache uses `std::sync::Mutex` fully qualified so the two never collide.

Wire protocol reminder for the tests: the `DPoP` request header is a JWT (`header.payload.signature`, each segment base64url without padding). The nonce, when present, is the `nonce` claim in the payload JSON. `base64` 0.22 is already a `web`-feature dependency (`base64::engine::general_purpose::URL_SAFE_NO_PAD`).

- [ ] **Step 1: Add the `ProofNonce` matcher and the three failing tests**

In `tests/unit_actions_pds.rs`, add after the `NoHeader` impl:

```rust
use base64::Engine as _;

/// Matches requests whose DPoP proof carries exactly this `nonce` claim
/// (`None` = no nonce claim at all). Decodes the JWT payload segment; the
/// signature is not checked — this is a routing matcher, not a verifier.
struct ProofNonce(Option<&'static str>);

impl Match for ProofNonce {
    fn matches(&self, request: &Request) -> bool {
        let Some(proof) = request.headers.get("DPoP").and_then(|v| v.to_str().ok()) else {
            return false;
        };
        let Some(payload) = proof.split('.').nth(1) else {
            return false;
        };
        let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload) else {
            return false;
        };
        let Ok(claims) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return false;
        };
        claims.get("nonce").and_then(|n| n.as_str()) == self.0
    }
}
```

and, after `dpop_nonce_challenge_is_retried_once`, the three tests:

```rust
/// Spec §5.2: the first call pays the challenge, the second does not.
#[tokio::test]
async fn cached_nonce_skips_the_challenge_on_the_second_call() {
    let mock = MockServer::start().await;
    // No nonce → challenge.
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .and(ProofNonce(None))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("WWW-Authenticate", "DPoP error=\"use_dpop_nonce\"")
                .insert_header("DPoP-Nonce", "n1")
                .set_body_json(serde_json::json!({ "error": "use_dpop_nonce" })),
        )
        .expect(1)
        .mount(&mock)
        .await;
    // Right nonce → success (the server echoes the same nonce back).
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .and(ProofNonce(Some("n1")))
        .respond_with(ResponseTemplate::new(200).insert_header("DPoP-Nonce", "n1"))
        .expect(2)
        .mount(&mock)
        .await;

    let c = client(&mock);
    c.mute_actor("did:plc:t1").await.unwrap();
    c.mute_actor("did:plc:t2").await.unwrap();

    // Two round trips for the first call, one for the second.
    assert_eq!(mock.received_requests().await.unwrap().len(), 3);
}

/// A 200 that carries a rotated `DPoP-Nonce` updates the cache, so the next
/// proof uses the new one.
#[tokio::test]
async fn rotated_nonce_on_success_updates_cache() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .and(ProofNonce(None))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("WWW-Authenticate", "DPoP error=\"use_dpop_nonce\"")
                .insert_header("DPoP-Nonce", "n1")
                .set_body_json(serde_json::json!({ "error": "use_dpop_nonce" })),
        )
        .expect(1)
        .mount(&mock)
        .await;
    // n1 is accepted, but the server hands out n2 on the way back.
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .and(ProofNonce(Some("n1")))
        .respond_with(ResponseTemplate::new(200).insert_header("DPoP-Nonce", "n2"))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .and(ProofNonce(Some("n2")))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock)
        .await;

    let c = client(&mock);
    c.mute_actor("did:plc:t1").await.unwrap();
    c.mute_actor("did:plc:t2").await.unwrap();
    assert_eq!(mock.received_requests().await.unwrap().len(), 3);
}

/// A cached nonce the server no longer accepts falls back to the existing
/// challenge path and still succeeds — the worst case is unchanged.
#[tokio::test]
async fn stale_cached_nonce_falls_back_to_the_challenge() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .and(ProofNonce(None))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("WWW-Authenticate", "DPoP error=\"use_dpop_nonce\"")
                .insert_header("DPoP-Nonce", "n1")
                .set_body_json(serde_json::json!({ "error": "use_dpop_nonce" })),
        )
        .expect(1)
        .mount(&mock)
        .await;
    // n1 works once (first call's retry), then the server has rotated: the
    // second call's cached n1 is challenged with n2. Order matters — the
    // `up_to_n_times(1)` mock is consumed first, then the 401 takes over.
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .and(ProofNonce(Some("n1")))
        .respond_with(ResponseTemplate::new(200).insert_header("DPoP-Nonce", "n1"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .and(ProofNonce(Some("n1")))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("WWW-Authenticate", "DPoP error=\"use_dpop_nonce\"")
                .insert_header("DPoP-Nonce", "n2")
                .set_body_json(serde_json::json!({ "error": "use_dpop_nonce" })),
        )
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .and(ProofNonce(Some("n2")))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock)
        .await;

    let c = client(&mock);
    c.mute_actor("did:plc:t1").await.unwrap();
    c.mute_actor("did:plc:t2").await.unwrap();
    assert_eq!(mock.received_requests().await.unwrap().len(), 4);
}
```

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `cargo test --features web --test unit_actions_pds cached_nonce rotated_nonce stale_cached`
Expected: the three tests FAIL — `cached_nonce_skips_the_challenge_on_the_second_call` sees 4 requests (both calls pay the challenge); `rotated_nonce_on_success_updates_cache` and `stale_cached_nonce_falls_back_to_the_challenge` panic on a wiremock `expect` mismatch or an unmatched request. (The `ProofNonce(Some(..))` mocks are only reachable with a cache.) If instead they fail to compile, `base64` is missing from the test's reach — it is a `web`-feature dep, so `--features web` is required.

- [ ] **Step 3: Add `NonceCache` and thread it through `send_dpop`**

Replace the whole of `src/web/actions/dpop_http.rs` from the `DpopResponse` struct through the end of `send_dpop` with:

```rust
pub struct DpopResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

/// The last `DPoP-Nonce` a server handed us (#333). Servers return one on
/// every response and rotate it now and then; sending it in the first proof
/// turns the usual two round trips per call into one. A plain `Mutex` — the
/// critical section is one small string and the runner is single-flight per
/// batch, so there is nothing for a `RwLock` or an atomic to win.
#[derive(Default)]
pub struct NonceCache(std::sync::Mutex<Option<String>>);

impl NonceCache {
    pub fn get(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set(&self, nonce: &str) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(nonce.to_owned());
    }

    /// Cache whatever nonce this response carries, whatever its status.
    fn remember(&self, r: &DpopResponse) {
        if let Some(n) = r.headers.get("DPoP-Nonce").and_then(|v| v.to_str().ok()) {
            self.set(n);
        }
    }
}

/// Send `method url` with a fresh DPoP proof (bound to `access_token` when
/// given). `build` adds body/query/headers to the request. The proof carries
/// the cached nonce when there is one. If the server still answers 400/401
/// with a `DPoP-Nonce` header and a `use_dpop_nonce` / `invalid_dpop_proof`
/// signal (WWW-Authenticate or JSON body), the request is re-signed with
/// that nonce and sent exactly once more. Every response's `DPoP-Nonce` is
/// cached for the next call.
pub async fn send_dpop(
    http: &reqwest::Client,
    key: &KeyData,
    method: &str,
    url: &str,
    access_token: Option<&str>,
    nonce: &NonceCache,
    build: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
) -> Result<DpopResponse> {
    let (mut proof, header, mut claims) = match access_token {
        Some(t) => request_dpop(key, method, url, t),
        None => auth_dpop(key, method, url),
    }
    .context("mint DPoP proof")?;

    if let Some(cached) = nonce.get() {
        claims
            .private
            .insert("nonce".to_string(), serde_json::Value::String(cached));
        proof = mint(key, &header, &claims).context("mint DPoP proof with cached nonce")?;
    }

    let first = send_once(http, method, url, access_token, &proof, &build).await?;
    nonce.remember(&first);
    let fresh = first
        .headers
        .get("DPoP-Nonce")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let (Some(fresh), true) = (fresh, is_nonce_challenge(&first)) else {
        return Ok(first);
    };

    claims
        .private
        .insert("nonce".to_string(), serde_json::Value::String(fresh));
    let proof = mint(key, &header, &claims).context("mint DPoP proof with nonce")?;
    let second = send_once(http, method, url, access_token, &proof, &build).await?;
    nonce.remember(&second);
    Ok(second)
}
```

`is_nonce_challenge` and `send_once` stay exactly as they are.

- [ ] **Step 4: `PdsClient` owns a cache**

In `src/web/actions/pds.rs`, change the import on line 20 to:

```rust
use super::dpop_http::{send_dpop, DpopResponse, NonceCache};
```

the struct and constructor to:

```rust
pub struct PdsClient {
    http: reqwest::Client,
    pds_url: String,
    did: String,
    dpop_key: KeyData,
    access_token: String,
    /// One per client, i.e. one per batch — the same granularity as the
    /// session load (#333).
    nonce: NonceCache,
}

impl PdsClient {
    pub fn new(
        http: reqwest::Client,
        pds_url: String,
        did: String,
        dpop_key: KeyData,
        access_token: String,
    ) -> Self {
        Self {
            http,
            pds_url: pds_url.trim_end_matches('/').to_string(),
            did,
            dpop_key,
            access_token,
            nonce: NonceCache::default(),
        }
    }
```

and both `send_dpop(` calls (`paginate` at ~L225 and `post` at ~L261) gain `&self.nonce,` immediately after `Some(&self.access_token),`:

```rust
            let resp = send_dpop(
                &self.http,
                &self.dpop_key,
                "GET",
                &url,
                Some(&self.access_token),
                &self.nonce,
                |r| {
```

```rust
        let resp = send_dpop(
            &self.http,
            &self.dpop_key,
            "POST",
            &url,
            Some(&self.access_token),
            &self.nonce,
            |r| with_proxy(nsid, r).json(body),
        )
```

- [ ] **Step 5: The token refresh uses a throwaway cache**

In `src/web/actions/session.rs`, change the import on line 22 to:

```rust
use super::dpop_http::{send_dpop, NonceCache};
```

and the refresh call (~L466) to:

```rust
    // The authorization server's nonce is not the resource server's, so the
    // refresh never shares a cache with the PDS client.
    let resp = send_dpop(
        http,
        &session.dpop_key,
        "POST",
        &authz.token_endpoint,
        None,
        &NonceCache::default(),
        |r| r.form(&form),
    )
```

- [ ] **Step 6: Verify every caller was updated**

Run: `rg -n "send_dpop\(" src/ tests/`
Expected: exactly the definition plus three call sites (`pds.rs` ×2, `session.rs` ×1), each with a nonce argument.

- [ ] **Step 7: Build, lint, test**

Run: `cargo clippy --features web --all-targets -- -D warnings`
Expected: clean.

Run: `cargo test --features web --test unit_actions_pds`
Expected: PASS — the existing tests (including `dpop_nonce_challenge_is_retried_once`, which starts from an empty cache and is unchanged) plus the three new ones.

Run: `cargo test --features web --test unit_actions_session --test unit_actions_runner --test web_actions`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/web/actions/dpop_http.rs src/web/actions/pds.rs src/web/actions/session.rs tests/unit_actions_pds.rs
git commit -m 'feat(333): cache the DPoP nonce per PdsClient so steady state is one round trip'
```

---

### Task 8: #318 design-hook findings

**Files:**
- Modify: `web/src/routes/(protected)/accounts/[handle]/+page.svelte` (signal bars L159/L166 markup, L393-402 CSS; radii L262, L423, L438)
- Modify: `web/src/lib/components/ConfirmSheet.svelte:131-148` (backdrop, token fallbacks)
- Modify: `DESIGN.md` (front-matter `typography:` and `rounded:`)
- Modify: `.impeccable/design.json` (`typographyMeta`)
- Generated: `.impeccable/config.json` (by `hook-admin.mjs` — never hand-edited)

**Interfaces:** none. This is CSS, docs, and one hook waiver. Nothing visible moves except the two radius snaps (10/6 px → 8 px) and the backdrop tint (#000 → #0c0a09 at 40 %).

No vitest coverage applies; the checks are `npm --prefix web run check`, `npm --prefix web run build`, the svelte MCP validator on both `.svelte` files, and the impeccable detector reporting zero findings on both files.

- [ ] **Step 1: Baseline the detector**

Run: `npx impeccable detect "web/src/routes/(protected)/accounts/[handle]/+page.svelte" web/src/lib/components/ConfirmSheet.svelte`
Expected: findings listed (at least `side-tab` on ConfirmSheet). Note the count — Step 8 must bring it to zero.

- [ ] **Step 2: Signal bars — `scaleX` instead of `width`**

In `accounts/[handle]/+page.svelte`, change both bar elements (Quote ratio and Reply ratio):

```svelte
							<div class="signal-bar" style="transform: scaleX({scoreBar(b.quote_ratio) / 100})"></div>
```

```svelte
							<div class="signal-bar" style="transform: scaleX({scoreBar(b.reply_ratio) / 100})"></div>
```

and the CSS rule to:

```css
	.signal-bar {
		height: 100%;
		width: 100%;
		background: linear-gradient(90deg, var(--copper), var(--amber-500));
		border-radius: 2px;
		/* Scale, not width: same motion, no layout on each frame (#318). */
		transform-origin: left;
		transition: transform 0.5s ease;
	}
```

- [ ] **Step 3: Radius snaps**

Same file: `border-radius: 10px;` at L262 and L423 → `border-radius: 8px;`; `border-radius: 6px;` at L438 → `border-radius: 8px;`. The `2px` radii on the 4 px bars (L393, L400) stay.

Run: `rg -n "border-radius: (10px|6px)" "web/src/routes/(protected)/accounts/[handle]/+page.svelte"`
Expected: no output.

- [ ] **Step 4: ConfirmSheet tokens**

In `web/src/lib/components/ConfirmSheet.svelte` `<style>`:

```css
	.backdrop { position: fixed; inset: 0; background: rgb(var(--charcoal-950-rgb) / 0.4); display: flex; align-items: flex-end; justify-content: center; z-index: 50; }
```

```css
	.sheet { width: 100%; max-width: 26rem; background: var(--charcoal-900); color: var(--cream-50); border-radius: 16px 16px 0 0; padding: 1.25rem 1.25rem 1.5rem; display: flex; flex-direction: column; gap: 0.75rem; }
```

```css
	.confirm { background: var(--cream-50); color: var(--charcoal-900); border: 0; }
```

(`--charcoal-100` is not a token — `tokens.css` stops at `--charcoal-300` — so the `#f5f5f4` fallback was the only thing painting that colour. `--cream-50` is the existing off-white the account headline already uses.)

Run: `rg -n "#f5f5f4|#1c1917|rgb\(0 0 0" web/src/lib/components/ConfirmSheet.svelte`
Expected: no output.

- [ ] **Step 5: DESIGN.md ramp**

In `DESIGN.md` front-matter, after the `eyebrow:` block (before `rounded:`), add:

```yaml
  subtitle:
    fontFamily: "Outfit, system-ui, sans-serif"
    fontSize: "1.125rem"
    fontWeight: 400
    lineHeight: 1.3
  body-sm:
    fontFamily: "Outfit, system-ui, sans-serif"
    fontSize: "0.9375rem"
    fontWeight: 300
    lineHeight: 1.6
  small:
    fontFamily: "Outfit, system-ui, sans-serif"
    fontSize: "0.875rem"
    fontWeight: 400
    lineHeight: 1.5
  caption:
    fontFamily: "Outfit, system-ui, sans-serif"
    fontSize: "0.75rem"
    fontWeight: 400
    lineHeight: 1.4
  stat:
    fontFamily: "Libre Baskerville, Georgia, serif"
    fontSize: "1.875rem"
    fontWeight: 400
    lineHeight: 1.1
```

and change the `rounded:` block to:

```yaml
rounded:
  xs: "2px"
  sm: "8px"
  md: "12px"
  lg: "16px"
  xl: "20px"
  2xl: "24px"
```

- [ ] **Step 6: design.json typography metadata**

In `.impeccable/design.json`, extend `typographyMeta` (keep the existing six entries; add these five, valid JSON — mind the trailing comma on `eyebrow`):

```json
      "subtitle": { "displayName": "Subtitle", "purpose": "Sheet and dialog headings. Outfit 1.125rem." },
      "body-sm": { "displayName": "Body small", "purpose": "Secondary running text and empty-state copy. 0.9375rem." },
      "small": { "displayName": "Small", "purpose": "Table cells, meta lines, signal labels. 0.875rem." },
      "caption": { "displayName": "Caption", "purpose": "Notes, DIDs, inline errors under controls. 0.75rem." },
      "stat": { "displayName": "Stat", "purpose": "The account handle on the detail page. Libre Baskerville 1.875rem." }
```

Run: `python3 -c "import json; json.load(open('.impeccable/design.json')); print('ok')"`
Expected: `ok`.

- [ ] **Step 7: The one waiver**

Run (from the repo root; this is a plain node script, not a slash command):

```bash
node ~/.claude/plugins/cache/impeccable/impeccable/4.0.4/skills/impeccable/scripts/hook-admin.mjs ignore-value side-tab "*" --file "web/src/lib/components/ConfirmSheet.svelte" --shared --reason "Consent-terms rule, confirmed intentional 2026-09-04 (#318)"
```

Expected: it reports the waiver written to `.impeccable/config.json`.

Run: `git diff --stat .impeccable/config.json`
Expected: one file changed, a `detector.ignoreValues` entry for `side-tab` scoped to ConfirmSheet and nothing else. **Do not add any other ignore.**

- [ ] **Step 8: Verify the detector is clean**

Run: `npx impeccable detect "web/src/routes/(protected)/accounts/[handle]/+page.svelte" web/src/lib/components/ConfirmSheet.svelte`
Expected: zero findings on both files. If a font-size or radius still fires, the DESIGN.md front-matter did not parse — check YAML indentation (two spaces under `typography:`).

- [ ] **Step 9: Validate and build**

Run the svelte MCP validator on both `.svelte` files.

Run: `npm --prefix web run check` — no new errors (the five pre-existing on `accounts/[handle]` remain). Run: `npm --prefix web run build` — succeeds.

- [ ] **Step 10: Commit**

```bash
git add "web/src/routes/(protected)/accounts/[handle]/+page.svelte" web/src/lib/components/ConfirmSheet.svelte DESIGN.md .impeccable/design.json .impeccable/config.json
git commit -m 'fix(318): scaleX signal bars, token fallbacks, DESIGN.md type ramp, one side-tab waiver'
```

---

### Task 9: CHANGELOG

**Files:**
- Modify: `CHANGELOG.md` (`## [Unreleased]` — add a `### Changed` section between the existing `### Fixed` and `### Added`)

**Interfaces:** none.

- [ ] **Step 1: Add the entries**

Insert, after the last bullet of the `### Fixed` block under `## [Unreleased]` and before `### Added`:

```markdown
### Changed
- Muting or blocking one account now finishes where you are (#332). The
  button reads `Muting…`, a toast at the bottom of the page follows the
  batch, and it settles to `Muted @handle` with Undo and a link to the
  record — no more detour to the action page and back. Undo works the same
  way. A failure keeps the toast up with Retry; a lost connection offers
  Reconnect; after 60 s of silence it points you at the record instead of
  guessing.
- The action page finishes properly (#332): a `Done · 14 muted, 0 failed`
  banner (or `Finished with problems`) with Undo all / Retry failed and a
  `← Back to Watch accounts` link that returns to where the batch was
  started. It polls every second while running instead of every three.
- Actions are faster on the wire (#333): the runner remembers the DPoP nonce
  a PDS hands back, so the steady state is one round trip per call instead
  of two. It also logs how long the session load, the reconcile read, each
  PDS call, and the whole batch took, so the next round of speed work is
  measured rather than guessed.
- Design-hook findings on the account page and the confirm sheet (#318):
  signal bars animate with `transform` instead of `width`, the sheet uses
  palette tokens with no hex fallbacks, two radii snapped to 8 px, and the
  type sizes the site already ships (0.75 / 0.875 / 0.9375 / 1.125 /
  1.875 rem) are now in DESIGN.md's ramp. The consent sentence's side rule
  is kept on purpose and waived for that one file.
```

- [ ] **Step 2: Commit**

```bash
git add CHANGELOG.md
git commit -m 'docs: CHANGELOG entries for #332, #333, #318'
```

---

## Final gates (run once, after Task 9, before the PR)

```bash
npm --prefix web run test -- --run
npm --prefix web run check
npm --prefix web run build
cargo clippy --features web --all-targets -- -D warnings
cargo clippy --features postgres --all-targets -- -D warnings
cargo clippy --all-targets -- -D warnings
CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "^\s*SKIP:"
DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --all-targets --features postgres
```

Expected: every suite green; the `SKIP:` grep prints nothing; `check` shows only the five pre-existing errors on `accounts/[handle]/+page.svelte`.

Then: push (HTTPS via `gh auth git-credential`, in the background, verify with `git ls-remote`), open the PR `feat/332-single-action-toast` → `staging`, CodeRabbit loop to APPROVED, merge, and verify on staging per spec §7 (one mute, one undo, one bulk Watch mute; the four timing spans in Railway logs). The #333 evidence gate (spec §5.3) decides between closing #333 and opening the `getProfiles` reconcile follow-up.

