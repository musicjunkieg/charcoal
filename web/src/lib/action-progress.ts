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

// Also imported by web/src/routes/(protected)/actions/[id]/+page.svelte,
// which polls the same batch endpoint at this cadence — the single-action
// toast poll and the batch page poll are deliberately kept in lockstep, not
// coincidentally equal. Retune both together, or split them on purpose.
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
