// Presentation logic for the action log (#315, spec §5.3–§5.4). Pure so the
// copy is pinned by vitest and the pages stay thin.

import type { ActionBatchSummary, ActionRowView } from './types.js';

/** Marker the runner writes to `batch.error` when the write session is gone.
 *  The batch stays `queued` so a reconnect resumes it without a retry. */
export const NOT_CONNECTED = 'not_connected';

function n(count: number, word: string): string {
	return `${count} ${word}${count === 1 ? '' : 's'}`;
}

/** Waiting for the person to reconnect — nothing will move until they do. */
export function isParked(b: ActionBatchSummary): boolean {
	return b.status === 'queued' && b.error === NOT_CONNECTED;
}

/** Worth polling: the runner is (or is about to be) working on it. */
export function isRunning(b: ActionBatchSummary): boolean {
	return (b.status === 'queued' || b.status === 'running') && !isParked(b);
}

/** Rows accounted for so far. While a batch is running/queued, `requested`
 *  is the number asked for but the row counts are the live picture — use
 *  those once the runner has started writing them, falling back to
 *  `requested` only for a batch that hasn't been touched yet (no counts). */
function totalCount(b: ActionBatchSummary): number {
	const c = b.counts;
	const sum =
		(c.pending ?? 0) + (c.applied ?? 0) + (c.skipped_already_done ?? 0) + (c.failed ?? 0) + (c.undone ?? 0);
	return sum > 0 ? sum : b.requested;
}

export function batchHeadline(b: ActionBatchSummary): string {
	if (isParked(b)) {
		return 'Not connected — reconnect to continue';
	}
	const c = b.counts;
	const applied = c.applied ?? 0;
	const skipped = c.skipped_already_done ?? 0;
	const failed = c.failed ?? 0;
	if (b.kind === 'undo') {
		if (isRunning(b)) return `Undoing ${n(totalCount(b), 'action')}…`;
		const head = `Undid ${n(applied + skipped, 'action')}`;
		return failed ? `${head} · ${failed} failed` : head;
	}
	const past = b.kind === 'mute' ? 'Muted' : 'Blocked';
	const present = b.kind === 'mute' ? 'Muting' : 'Blocking';
	if (isRunning(b)) return `${present} ${n(totalCount(b), 'account')}…`;
	let head = `${past} ${n(applied + skipped, 'account')}`;
	if (skipped) head += ` (${skipped} already ${b.kind === 'mute' ? 'muted' : 'blocked'})`;
	return failed ? `${head} · ${failed} failed` : head;
}

/** The failure reason, when the row failed. Empty otherwise. */
export function failureNote(r: ActionRowView): string {
	if (r.status === 'failed' && r.error) return r.error;
	return '';
}

/** The tier-drift note, when the account's tier has since moved. Empty
 *  otherwise. Kept independent of `failureNote` so a row that is both
 *  failed and drifted can still show the drift copy in a cell of its own
 *  (the Tier-then column) without repeating the failure text. */
export function driftNote(r: ActionRowView): string {
	if (r.drifted && r.current_tier) return `since dropped to ${r.current_tier}`;
	return '';
}

/** Combined note for a single-note context: failure takes priority over
 *  drift when a row is both. Callers that render failure and drift in
 *  separate cells (e.g. the batch detail table) should use
 *  `failureNote`/`driftNote` directly instead. */
export function rowNote(r: ActionRowView): string {
	return failureNote(r) || driftNote(r);
}

export function canRetry(b: ActionBatchSummary): boolean {
	if (isRunning(b) || isParked(b)) return false;
	// A batch that gave up before the write step (a PDS 5xx on the reconcile
	// read, a transient token refresh failure) is stored `failed` with every
	// row still `pending`. Those rows are the work that never happened, so
	// Retry re-queues them alongside genuinely failed ones.
	const stalled = b.status === 'failed' || b.status === 'partial' ? (b.counts.pending ?? 0) : 0;
	return (b.counts.failed ?? 0) + stalled > 0;
}

/** Undo is offered only for rows Charcoal itself applied. A
 *  `skipped_already_done` row is the user's own mute or block — shown as in
 *  force, never undone (#261). */
export function canUndo(b: ActionBatchSummary): boolean {
	if (b.kind === 'undo' || isRunning(b) || isParked(b)) return false;
	return (b.counts.applied ?? 0) > 0;
}
