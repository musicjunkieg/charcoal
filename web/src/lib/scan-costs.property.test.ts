import { describe, expect, it } from 'vitest';
import fc from 'fast-check';
import { formatCents } from './scan-costs';

describe('formatCents properties (#420)', () => {
	// Invariant: any whole, non-negative number of cents renders as "$" then
	// whole dollars, a dot, and exactly two cent digits, and reading that
	// string back gives the same number of cents (nothing lost or rounded).
	it('renders whole cents as $d.cc that round-trips', () => {
		fc.assert(
			fc.property(fc.integer({ min: 0, max: 1_000_000_000 }), (cents) => {
				const out = formatCents(cents);
				expect(out).toMatch(/^\$\d+\.\d{2}$/);
				const [dollars, frac] = out.slice(1).split('.');
				expect(Number(dollars) * 100 + Number(frac)).toBe(cents);
			})
		);
	});
});
