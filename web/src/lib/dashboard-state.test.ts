import { describe, expect, it } from 'vitest';
import { dashboardView } from './dashboard-state.js';
import type { ScanStatus } from './types.js';

function status(overrides: Partial<ScanStatus>): ScanStatus {
	return {
		scan_running: false,
		started_at: null,
		progress_message: '',
		last_error: null,
		phase: 'idle',
		progress: null,
		tier_counts: { high: 0, elevated: 0, watch: 0, low: 0, not_assessed: 0, total: 0 },
		...overrides
	};
}

describe('dashboardView', () => {
	it('shows the welcome screen for a brand-new user (never scanned, no data)', () => {
		expect(dashboardView(status({}))).toBe('welcome');
	});

	it('shows all-clear when a scan finished with zero scored accounts', () => {
		// This is the case that used to strand users on a 0/0/0/0 grid.
		expect(dashboardView(status({ started_at: '2026-07-05T12:00:00Z' }))).toBe('all-clear');
	});

	it('shows all-clear (with error copy handled by the page) after a failed empty scan', () => {
		expect(dashboardView(status({ started_at: '2026-07-05T12:00:00Z', last_error: 'boom' }))).toBe(
			'all-clear'
		);
	});

	it('shows results while a first scan runs, even with nothing scored yet', () => {
		expect(dashboardView(status({ scan_running: true, started_at: '2026-07-05T12:00:00Z' }))).toBe(
			'results'
		);
	});

	it('shows results whenever any accounts are scored, scanning or not', () => {
		const counts = { high: 1, elevated: 0, watch: 2, low: 5, not_assessed: 0, total: 8 };
		expect(dashboardView(status({ tier_counts: counts }))).toBe('results');
		expect(
			dashboardView(
				status({ tier_counts: counts, scan_running: true, started_at: '2026-07-05T12:00:00Z' })
			)
		).toBe('results');
		expect(dashboardView(status({ tier_counts: counts, started_at: '2026-07-05T12:00:00Z' }))).toBe(
			'results'
		);
	});

	it('shows results with data even if started_at is missing (server restarted)', () => {
		// started_at lives in server memory only; scored accounts are in the DB.
		const counts = { high: 0, elevated: 0, watch: 0, low: 3, not_assessed: 0, total: 3 };
		expect(dashboardView(status({ tier_counts: counts, started_at: null }))).toBe('results');
	});
});
