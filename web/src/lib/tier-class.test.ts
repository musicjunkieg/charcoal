import { describe, it, expect } from 'vitest';
import { tierClass } from './tier-class';

describe('tierClass', () => {
	it('maps the four scored tiers to their own class', () => {
		expect(tierClass('High')).toBe('tier-high');
		expect(tierClass('Elevated')).toBe('tier-elevated');
		expect(tierClass('Watch')).toBe('tier-watch');
		expect(tierClass('Low')).toBe('tier-low');
	});

	// The code this replaces ended in `?? '#a8a29e'`, which is --tier-low.
	// Anything unrecognised MUST keep landing there or the refactor is not
	// inert for abstained accounts (#283, #245).
	it('falls back to tier-low for unscored and unknown values', () => {
		expect(tierClass('NotAssessed')).toBe('tier-low');
		expect(tierClass('Not assessed')).toBe('tier-low');
		expect(tierClass('InsufficientData')).toBe('tier-low');
		expect(tierClass(null)).toBe('tier-low');
		expect(tierClass(undefined)).toBe('tier-low');
		expect(tierClass('')).toBe('tier-low');
	});

	it('never emits a class containing whitespace', () => {
		for (const input of ['Not assessed', 'a b c', ' High ']) {
			expect(tierClass(input)).not.toMatch(/\s/);
		}
	});
});
