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
		drifted: false,
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
