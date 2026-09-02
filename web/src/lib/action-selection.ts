// Pure button-state logic for the per-account mute/block buttons (#315), kept
// out of the component so it can be unit-tested without a DOM.

import type { ActionKind, ActionRowStatus } from './types.js';

export interface ActiveRow {
	id: number;
	kind: ActionKind;
	status: ActionRowStatus;
}

export interface ButtonState {
	/** `available` — offer the action; `done` — show the tick. */
	state: 'available' | 'done';
	label: string;
	/** The row to undo, when Charcoal is the one that applied it. `null` for
	 *  a `skipped_already_done` row: that mute or block is the user's own and
	 *  Charcoal never removes it (#261), so no Undo is offered. */
	actionId: number | null;
}

const LABELS: Record<ActionKind, { available: string; done: string }> = {
	mute: { available: 'Mute', done: 'Muted ✓' },
	block: { available: 'Block', done: 'Blocked ✓' }
};

export function buttonState(active: ActiveRow[], kind: ActionKind): ButtonState {
	const row = active.find(
		(r) => r.kind === kind && (r.status === 'applied' || r.status === 'skipped_already_done')
	);
	if (row) {
		return {
			state: 'done',
			label: LABELS[kind].done,
			actionId: row.status === 'applied' ? row.id : null
		};
	}
	return { state: 'available', label: LABELS[kind].available, actionId: null };
}
