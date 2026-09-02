import { describe, it, expect } from 'vitest';
import { batchHeadline, rowNote, isRunning, isParked, canRetry, canUndo } from './action-status';
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
		expect(rowNote(row({ current_tier: 'Watch', drifted: true }))).toBe('since dropped to Watch');
		expect(rowNote(row({}))).toBe('');
	});
	it('surfaces a failure reason', () => {
		expect(rowNote(row({ status: 'failed', error: 'PDS returned 500' }))).toBe('PDS returned 500');
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
	});
	it('canUndo for finished forward batches with active rows', () => {
		expect(canUndo(summary({ counts: { applied: 1 } }))).toBe(true);
		expect(canUndo(summary({ kind: 'undo', counts: { applied: 1 } }))).toBe(false);
		expect(canUndo(summary({ counts: { failed: 1 } }))).toBe(false);
	});
});
