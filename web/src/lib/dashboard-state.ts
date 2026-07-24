// Which top-level view the dashboard should render, extracted from the
// page so the gating is unit-testable. Three states:
//
//   welcome   — brand-new user: never scanned, nothing scored.
//   all-clear — a scan has run (started_at set) and finished with zero
//               scored AND zero not_assessed accounts; shown instead of a
//               dead-end 0/0/0/0 grid. The page decides between "all clear"
//               and error copy via status.last_error.
//   results   — anything else: a scan is running (partial results fill in)
//               or scored accounts exist.
//
// Note: started_at lives in server memory only, so after a server restart a
// user with scored accounts has started_at === null — tier counts come from
// the DB and take precedence.

import type { ScanStatus } from './types.js';

export type DashboardView = 'welcome' | 'all-clear' | 'results';

export function dashboardView(status: ScanStatus): DashboardView {
	// tier_counts.total excludes not_assessed (NULL-scored) accounts — a scan
	// whose entire population came back not_assessed still has real results
	// to show, not "nothing to worry about" (#222).
	if (
		status.scan_running ||
		status.tier_counts.total > 0 ||
		status.tier_counts.not_assessed > 0
	) {
		return 'results';
	}
	return status.started_at ? 'all-clear' : 'welcome';
}
