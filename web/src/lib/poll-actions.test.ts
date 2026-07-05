import { describe, expect, it } from 'vitest';
import { pollActions } from './poll-actions.js';

describe('pollActions', () => {
	it('refreshes results (not accuracy) while a scan keeps running', () => {
		expect(pollActions(true, true)).toEqual({ refreshResults: true, refreshAccuracy: false });
	});

	it('refreshes results and accuracy on the falling edge (scan just finished)', () => {
		// The fix for the stale-events gap (#108): without this final refresh
		// the dashboard shows pre-scan data until a manual reload.
		expect(pollActions(true, false)).toEqual({ refreshResults: true, refreshAccuracy: true });
	});

	it('does nothing while idle', () => {
		expect(pollActions(false, false)).toEqual({ refreshResults: false, refreshAccuracy: false });
	});

	it('refreshes results when a scan starts between polls', () => {
		expect(pollActions(false, true)).toEqual({ refreshResults: true, refreshAccuracy: false });
	});
});
