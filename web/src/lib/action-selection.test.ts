import { describe, it, expect } from 'vitest';
import { buttonState, type ActiveRow } from './action-selection';

const mute: ActiveRow = { id: 7, kind: 'mute', status: 'applied' };
const block: ActiveRow = { id: 8, kind: 'block', status: 'skipped_already_done' };
const blockApplied: ActiveRow = { id: 9, kind: 'block', status: 'applied' };

describe('buttonState', () => {
	it('offers the action when nothing is active', () => {
		expect(buttonState([], 'mute')).toEqual({ state: 'available', label: 'Mute', actionId: null });
		expect(buttonState([], 'block')).toEqual({ state: 'available', label: 'Block', actionId: null });
	});

	it('shows the done state with the row to undo', () => {
		expect(buttonState([mute], 'mute')).toEqual({ state: 'done', label: 'Muted ✓', actionId: 7 });
		expect(buttonState([mute, blockApplied], 'block')).toEqual({
			state: 'done',
			label: 'Blocked ✓',
			actionId: 9
		});
	});

	// In force either way — but a `skipped_already_done` row is the person's
	// own block, so there is no row for Charcoal to undo (#261).
	it('shows an already-done skip as done with no row to undo', () => {
		expect(buttonState([block], 'block')).toEqual({
			state: 'done',
			label: 'Blocked ✓',
			actionId: null
		});
	});

	it('ignores rows of the other kind', () => {
		expect(buttonState([block], 'mute').state).toBe('available');
	});
});
