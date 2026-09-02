import { describe, it, expect } from 'vitest';
import { buttonState, type ActiveRow } from './action-selection';

const mute: ActiveRow = { id: 7, kind: 'mute', status: 'applied' };
const block: ActiveRow = { id: 8, kind: 'block', status: 'skipped_already_done' };

describe('buttonState', () => {
	it('offers the action when nothing is active', () => {
		expect(buttonState([], 'mute')).toEqual({ state: 'available', label: 'Mute', actionId: null });
		expect(buttonState([], 'block')).toEqual({ state: 'available', label: 'Block', actionId: null });
	});

	it('shows the done state with the row to undo', () => {
		expect(buttonState([mute], 'mute')).toEqual({ state: 'done', label: 'Muted ✓', actionId: 7 });
		expect(buttonState([mute, block], 'block')).toEqual({ state: 'done', label: 'Blocked ✓', actionId: 8 });
	});

	it('treats an already-done skip the same as applied', () => {
		expect(buttonState([block], 'block').state).toBe('done');
	});

	it('ignores rows of the other kind', () => {
		expect(buttonState([block], 'mute').state).toBe('available');
	});
});
