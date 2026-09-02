// Pure logic for the bulk tier-action bar on the Accounts list page (#315,
// spec §5.1), kept out of the page component so the gating rules and copy
// are unit-testable without mounting Svelte.

import type { ActionKind, ActionsStatus } from './types.js';

/** The tier a bulk Mute all / Block all bar operates on — `null` when there
 *  is nothing meaningfully "bulk" about the current filter: "All" spans
 *  every tier (too broad to act on in one batch) and "Low" is deliberately
 *  excluded — the least-threat tier is not what tier actions are for. */
export function bulkTierFor(selectedTier: string): string | null {
	return selectedTier !== 'All' && selectedTier !== 'Low' ? selectedTier : null;
}

/** Whether the bulk bar should render at all: a bulk-eligible tier is
 *  selected, actions are enabled server-side, the viewer is not
 *  impersonating someone else's account (admin impersonation is read-only,
 *  per the self-protective invariant), and there is at least one account in
 *  the tier to act on. */
export function showBulkBar(args: {
	bulkTier: string | null;
	actionsStatus: ActionsStatus | null;
	asUser: string | null;
	total: number;
}): boolean {
	return (
		args.bulkTier !== null &&
		args.actionsStatus?.enabled === true &&
		args.asUser === null &&
		args.total > 0
	);
}

const ERROR_COPY: Record<string, string> = {
	denied: "Bluesky didn't grant permission. Nothing was changed.",
	invalid_scope: 'Bluesky granted different permissions than Charcoal asked for. Nothing was changed.',
	failed: 'Something went wrong while connecting. Nothing was changed.',
	disabled: 'Mute and block actions are not enabled on this server.'
};

/** Copy for a `?actions_error=` code from a failed consent round-trip.
 *  Unrecognized codes fall back to the generic "failed" wording rather than
 *  surfacing nothing. */
export function bulkErrorMessage(code: string): string {
	return ERROR_COPY[code] ?? ERROR_COPY.failed;
}

/** Message for a batch that skipped every target because they were already
 *  in the requested state (`batch_id: null` from createActionBatch). */
export function alreadyDoneMessage(kind: ActionKind, skippedActive: number): string {
	return `${skippedActive} ${kind === 'mute' ? 'already muted' : 'already blocked'} — nothing to do`;
}
