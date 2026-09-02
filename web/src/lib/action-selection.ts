// Pure button-state logic for the per-account mute/block buttons (#315), kept
// out of the component so it can be unit-tested without a DOM.

import type { ActionKind, ActionRowStatus } from './types.js';

export interface ActiveRow {
	id: number;
	kind: ActionKind;
	status: ActionRowStatus;
}

export interface ButtonState {
	/** `available` — offer the action; `done` — show the tick and an Undo. */
	state: 'available' | 'done';
	label: string;
	/** The active row to undo when `state === 'done'`. */
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
	if (row) return { state: 'done', label: LABELS[kind].done, actionId: row.id };
	return { state: 'available', label: LABELS[kind].available, actionId: null };
}
