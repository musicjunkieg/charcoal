import { describe, it, expect } from 'vitest';
import { buildSheetRows } from './sheet-rows';
import type { Account, ActiveActionRef } from './types';

function account(overrides: Partial<Account> = {}): Account {
	return {
		rank: 1,
		did: 'did:plc:a',
		handle: 'a.test',
		toxicity_score: 0.5,
		topic_overlap: 0.2,
		threat_score: 40,
		threat_tier: 'High',
		posts_analyzed: 10,
		top_toxic_posts: [],
		scored_at: '2026-09-01T12:00:00Z',
		behavioral_signals: null,
		...overrides
	};
}

describe('buildSheetRows', () => {
	it('marks done only for a matching kind — an active block does not mark a mute row done', () => {
		const accounts = [account({ did: 'did:plc:a', handle: 'a.test' })];
		const active: ActiveActionRef[] = [{ did: 'did:plc:a', kind: 'block' }];
		const rows = buildSheetRows(accounts, active, 'mute');
		expect(rows).toHaveLength(1);
		expect(rows[0].done).toBe(false);
	});

	it('marks done true when an active row matches the kind', () => {
		const accounts = [account({ did: 'did:plc:a', handle: 'a.test' })];
		const active: ActiveActionRef[] = [{ did: 'did:plc:a', kind: 'mute' }];
		const rows = buildSheetRows(accounts, active, 'mute');
		expect(rows[0].done).toBe(true);
	});

	it('drops accounts whose did is empty or null', () => {
		const accounts = [
			account({ did: '', handle: 'noone.test' }),
			account({ did: null as unknown as string, handle: 'also-noone.test' }),
			account({ did: 'did:plc:b', handle: 'b.test' })
		];
		const rows = buildSheetRows(accounts, [], 'mute');
		expect(rows).toHaveLength(1);
		expect(rows[0].did).toBe('did:plc:b');
	});

	it('preserves input order and carries handle + threat_tier as tier', () => {
		const accounts = [
			account({ did: 'did:plc:b', handle: 'b.test', threat_tier: 'Elevated' }),
			account({ did: 'did:plc:a', handle: 'a.test', threat_tier: 'High' })
		];
		const rows = buildSheetRows(accounts, [], 'mute');
		expect(rows.map((r) => r.did)).toEqual(['did:plc:b', 'did:plc:a']);
		expect(rows[0].handle).toBe('b.test');
		expect(rows[0].tier).toBe('Elevated');
		expect(rows[1].tier).toBe('High');
	});
});
