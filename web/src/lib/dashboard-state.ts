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
	if (status.scan_running || status.tier_counts.total > 0 || status.tier_counts.not_assessed > 0) {
		return 'results';
	}
	return status.started_at ? 'all-clear' : 'welcome';
}

// Whether the user is waiting for a free scan slot rather than being scanned
// (#257). The phase is the authority — the server sends the `queue` block only
// while queued, but a client that reads it without checking the phase would
// happily present a running scan as a wait.
export function isQueued(status: ScanStatus): boolean {
	return status.phase === 'queued';
}

// Human phrasing for a queue position (#257). `enqueued_at` is accepted so the
// whole queue block can be passed through, but only position and ETA are said
// out loud.
//
// The estimate is deliberately omitted rather than defaulted when eta_seconds
// is null (no completed scan to take a median from). Someone waiting on
// information about their own safety is owed an honest "we don't know yet"
// instead of a number we invented.
export function queueMessage(q: {
	position: number;
	eta_seconds: number | null;
	enqueued_at?: string;
}): string {
	const place = q.position <= 1 ? "You're next in line" : `You're ${ordinal(q.position)} in line`;
	if (q.eta_seconds == null) return place;
	// Round up to a minute: a 20-second estimate is still a wait, and "about 0
	// minutes" reads as "any moment now", which it is not.
	const mins = Math.max(1, Math.ceil(q.eta_seconds / 60));
	return `${place} — about ${mins} minute${mins === 1 ? '' : 's'}`;
}

// 1 -> "1st", 12 -> "12th", 21 -> "21st". The teens are the special case: they
// all take "th" regardless of their last digit.
function ordinal(n: number): string {
	const s = ['th', 'st', 'nd', 'rd'];
	const v = n % 100;
	return n + (s[(v - 20) % 10] || s[v] || s[0]);
}
