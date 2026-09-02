import type { Account, ActionKind, ActiveActionRef, SheetRow } from './types.js';
import { topSignal } from './top-signal.js';

/** Join the tier's accounts with what Charcoal already holds, for the confirm
 *  sheet. Accounts with no DID (unscored stubs) are dropped — there is nothing
 *  to act on. */
export function buildSheetRows(accounts: Account[], active: ActiveActionRef[], kind: ActionKind): SheetRow[] {
	const done = new Set(active.filter((a) => a.kind === kind).map((a) => a.did));
	return accounts
		.filter((a) => !!a.did)
		.map((a) => ({ did: a.did, handle: a.handle, tier: a.threat_tier, signal: topSignal(a), done: done.has(a.did) }));
}
