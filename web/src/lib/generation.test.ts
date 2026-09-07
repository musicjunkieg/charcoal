import { describe, it, expect } from 'vitest';
import { generation } from './generation';

describe('generation', () => {
	it('the latest ticket is current; earlier ones are not', () => {
		const g = generation();
		const a = g.next();
		expect(g.isCurrent(a)).toBe(true);
		const b = g.next();
		expect(g.isCurrent(a)).toBe(false);
		expect(g.isCurrent(b)).toBe(true);
	});

	it('a refresh that resolves after a newer one started cannot write state', async () => {
		// Reverse completion: load A (pre-disconnect) is slow; load B (post-
		// disconnect) starts later and finishes first. A must be dropped.
		const g = generation();
		let status = 'connected';
		let resolveA!: (s: string) => void;
		const slow = new Promise<string>((r) => (resolveA = r));
		async function load(fetch: Promise<string>) {
			const ticket = g.next();
			const s = await fetch;
			if (!g.isCurrent(ticket)) return 'dropped';
			status = s;
			return 'committed';
		}
		const a = load(slow);
		const b = load(Promise.resolve('disconnected'));
		expect(await b).toBe('committed');
		resolveA('connected');
		expect(await a).toBe('dropped');
		expect(status).toBe('disconnected');
	});
});
