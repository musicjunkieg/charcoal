import { describe, it, expect } from 'vitest';
import {
	batchHeadline,
	rowNote,
	driftNote,
	isRunning,
	isParked,
	canRetry,
	canUndo,
	bannerSummary,
	returnPath
} from './action-status';
import type { ActionBatchSummary, ActionRowView } from './types';

function summary(over: Partial<ActionBatchSummary>): ActionBatchSummary {
	return {
		id: 1,
		kind: 'mute',
		source: 'tier:High',
		requested: 3,
		status: 'done',
		error: null,
		created_at: '2026-09-01T12:00:00Z',
		started_at: null,
		finished_at: null,
		counts: {},
		drifted: false,
		...over
	};
}

function row(over: Partial<ActionRowView>): ActionRowView {
	return {
		id: 1,
		batch_id: 1,
		target_did: 'did:plc:x',
		handle: 'a.test',
		kind: 'mute',
		status: 'applied',
		record_uri: null,
		undo_of: null,
		error: null,
		score_at_action: 41.5,
		tier_at_action: 'High',
		current_tier: 'High',
		drifted: false,
		applied_at: null,
		undone_at: null,
		...over
	};
}

describe('batchHeadline', () => {
	it('describes a finished forward batch by counts', () => {
		expect(batchHeadline(summary({ counts: { applied: 2, skipped_already_done: 1 } }))).toBe(
			'Muted 3 accounts (1 already muted)'
		);
		expect(batchHeadline(summary({ kind: 'block', requested: 1, counts: { applied: 1 } }))).toBe(
			'Blocked 1 account'
		);
	});
	it('describes a partial batch with the failure count', () => {
		expect(batchHeadline(summary({ status: 'partial', counts: { applied: 1, failed: 2 } }))).toBe(
			'Muted 1 account · 2 failed'
		);
	});
	it('describes queued and running batches', () => {
		expect(batchHeadline(summary({ status: 'queued', counts: { pending: 3 } }))).toBe('Muting 3 accounts…');
		expect(batchHeadline(summary({ status: 'running', counts: { pending: 1, applied: 2 } }))).toBe('Muting 3 accounts…');
	});
	it('describes undo batches', () => {
		expect(batchHeadline(summary({ kind: 'undo', requested: 2, counts: { applied: 2 } }))).toBe('Undid 2 actions');
		expect(batchHeadline(summary({ kind: 'undo', status: 'running', counts: { pending: 2 } }))).toBe('Undoing 2 actions…');
	});
	// The runner parks a batch as `queued` + error `not_connected` (Task 8);
	// it must read as waiting for reconnect, not as "Muting…".
	it('shows a parked batch as waiting for reconnect', () => {
		expect(batchHeadline(summary({ status: 'queued', error: 'not_connected', counts: { pending: 3 } }))).toBe(
			'Not connected — reconnect to continue'
		);
	});
});

describe('rowNote', () => {
	it('mentions drift with the current tier', () => {
		expect(rowNote(row({ current_tier: 'Watch', drifted: true }))).toBe('now Watch');
		expect(rowNote(row({}))).toBe('');
	});
	it('surfaces a failure reason', () => {
		expect(rowNote(row({ status: 'failed', error: 'PDS returned 500' }))).toBe('PDS returned 500');
	});
});

describe('driftNote', () => {
	// A row can be both failed AND drifted (the account's tier moved after the
	// action failed). The Tier-then cell needs the drift copy specifically —
	// rowNote() prioritises the failure message, which would otherwise show
	// the same failure text twice (once under Status, again under Tier then)
	// and hide the drift note entirely.
	it('returns the drift copy even when the row also failed', () => {
		expect(
			driftNote(row({ status: 'failed', error: 'PDS returned 500', drifted: true, current_tier: 'Watch' }))
		).toBe('now Watch');
	});
	it('is empty when not drifted', () => {
		expect(driftNote(row({ status: 'failed', error: 'PDS returned 500' }))).toBe('');
	});
});

describe('flags', () => {
	it('isRunning for queued and running, except a parked batch', () => {
		expect(isRunning(summary({ status: 'queued' }))).toBe(true);
		expect(isRunning(summary({ status: 'running' }))).toBe(true);
		expect(isRunning(summary({ status: 'done' }))).toBe(false);
		expect(isRunning(summary({ status: 'queued', error: 'not_connected' }))).toBe(false);
		expect(isParked(summary({ status: 'queued', error: 'not_connected' }))).toBe(true);
		expect(isParked(summary({ status: 'queued' }))).toBe(false);
	});
	it('canRetry only for a finished batch with failed rows', () => {
		expect(canRetry(summary({ status: 'partial', counts: { failed: 1 } }))).toBe(true);
		expect(canRetry(summary({ status: 'queued', error: 'not_connected', counts: { pending: 2 } }))).toBe(false);
		expect(canRetry(summary({ status: 'running', counts: { failed: 1 } }))).toBe(false);
		expect(canRetry(summary({ status: 'done', counts: { applied: 2 } }))).toBe(false);
	});
	// A reconcile-step failure stores the batch `failed` with every row still
	// `pending`. Retry re-queues exactly those, so the button has to appear.
	it('canRetry for a failed batch whose rows never ran', () => {
		expect(canRetry(summary({ status: 'failed', counts: { pending: 2 } }))).toBe(true);
	});
	it('canUndo only for rows Charcoal applied', () => {
		expect(canUndo(summary({ counts: { applied: 1 } }))).toBe(true);
		expect(canUndo(summary({ kind: 'undo', counts: { applied: 1 } }))).toBe(false);
		expect(canUndo(summary({ counts: { failed: 1 } }))).toBe(false);
		// The user's own mute or block: in force, but never Charcoal's to
		// remove (#261).
		expect(canUndo(summary({ counts: { skipped_already_done: 3 } }))).toBe(false);
		expect(canUndo(summary({ counts: { applied: 1, skipped_already_done: 3 } }))).toBe(true);
	});
});

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

	it('undo batch that never wrote is a problem with zero done, not zero failed', () => {
		// A batch that dies before the write step leaves every row `pending`
		// and the batch `failed` — `pending` must not count as undone, or the
		// banner would say "Undone" while the toast for the same batch says
		// "Couldn't unmute" (I1).
		const b = summary({ kind: 'undo', status: 'failed', counts: { pending: 2 } });
		const rows = [row({ kind: 'mute', status: 'pending' }), row({ kind: 'block', status: 'pending' })];
		expect(bannerSummary(b, rows)).toEqual({
			title: 'Finished with problems',
			detail: '0 unmuted, 0 unblocked, 0 failed',
			tone: 'error'
		});
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
