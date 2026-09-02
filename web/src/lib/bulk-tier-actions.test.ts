import { describe, it, expect } from 'vitest';
import { bulkTierFor, showBulkBar, bulkErrorMessage, alreadyDoneMessage } from './bulk-tier-actions';
import type { ActionsStatus } from './types';

const ENABLED: ActionsStatus = { enabled: true, connected: true };
const DISABLED: ActionsStatus = { enabled: false, connected: false };

describe('bulkTierFor', () => {
	it('excludes All — too broad to act on at once', () => {
		expect(bulkTierFor('All')).toBeNull();
	});

	it('excludes Low — least-threat accounts are not what tier actions are for', () => {
		expect(bulkTierFor('Low')).toBeNull();
	});

	it('passes through the other tiers unchanged', () => {
		expect(bulkTierFor('High')).toBe('High');
		expect(bulkTierFor('Elevated')).toBe('Elevated');
		expect(bulkTierFor('Watch')).toBe('Watch');
	});
});

describe('showBulkBar', () => {
	const base = { bulkTier: 'High', actionsStatus: ENABLED, asUser: null, total: 12 };

	it('shows when every condition holds', () => {
		expect(showBulkBar(base)).toBe(true);
	});

	it('hides with no bulk-eligible tier selected', () => {
		expect(showBulkBar({ ...base, bulkTier: null })).toBe(false);
	});

	it('hides when actions are not enabled server-side', () => {
		expect(showBulkBar({ ...base, actionsStatus: DISABLED })).toBe(false);
	});

	it('hides while the status has not loaded yet', () => {
		expect(showBulkBar({ ...base, actionsStatus: null })).toBe(false);
	});

	it('hides while impersonating another user', () => {
		expect(showBulkBar({ ...base, asUser: 'did:plc:someoneelse' })).toBe(false);
	});

	it('hides when the tier has no accounts', () => {
		expect(showBulkBar({ ...base, total: 0 })).toBe(false);
	});
});

describe('bulkErrorMessage', () => {
	it('maps known consent-failure codes to their copy', () => {
		expect(bulkErrorMessage('denied')).toBe("Bluesky didn't grant permission. Nothing was changed.");
		expect(bulkErrorMessage('invalid_scope')).toBe(
			'Bluesky granted different permissions than Charcoal asked for. Nothing was changed.'
		);
		expect(bulkErrorMessage('failed')).toBe('Something went wrong while connecting. Nothing was changed.');
		expect(bulkErrorMessage('disabled')).toBe('Mute and block actions are not enabled on this server.');
	});

	it('falls back to the generic failure copy for an unrecognized code', () => {
		expect(bulkErrorMessage('some_unknown_code')).toBe(
			'Something went wrong while connecting. Nothing was changed.'
		);
	});
});

describe('alreadyDoneMessage', () => {
	it('reports muted accounts', () => {
		expect(alreadyDoneMessage('mute', 3)).toBe('3 already muted — nothing to do');
	});

	it('reports blocked accounts', () => {
		expect(alreadyDoneMessage('block', 1)).toBe('1 already blocked — nothing to do');
	});
});
