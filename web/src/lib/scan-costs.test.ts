import { describe, expect, it } from 'vitest';
import { formatCents } from './scan-costs';

describe('formatCents', () => {
	it('renders dollars with two decimals', () => {
		expect(formatCents(0)).toBe('$0.00');
		expect(formatCents(7)).toBe('$0.07');
		expect(formatCents(250)).toBe('$2.50');
	});
	it('renders a missing full-scan figure as a dash', () => {
		expect(formatCents(null)).toBe('—');
	});
});
