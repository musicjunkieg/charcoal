import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const CSS = readFileSync('src/lib/website/styles/tokens.css', 'utf8');

function tokenMap(): Map<string, string> {
	const map = new Map<string, string>();
	for (const m of CSS.matchAll(/--([\w-]+)\s*:\s*([^;]+);/g)) {
		map.set(m[1].trim(), m[2].trim());
	}
	return map;
}

describe('tokens.css', () => {
	// --copper and --copper-rgb are INDEPENDENT declarations. Nothing in CSS
	// binds them, so nothing but this test stops them drifting apart and
	// shipping a wrong colour on every translucent surface.
	it('every --x-rgb triplet matches the hex of its --x token', () => {
		const tokens = tokenMap();
		const mismatches: string[] = [];

		for (const [name, value] of tokens) {
			if (!name.endsWith('-rgb')) continue;
			const base = name.slice(0, -'-rgb'.length);
			const hex = tokens.get(base);
			expect(hex, `--${name} has no matching --${base}`).toBeDefined();

			const channels = value.split(/[\s,]+/).map(Number);
			const fromHex = [1, 3, 5].map((i) => parseInt(hex!.slice(i, i + 2), 16));
			if (channels.join() !== fromHex.join()) {
				mismatches.push(`--${name}: ${value} != ${hex} (${fromHex.join(' ')})`);
			}
		}

		expect(mismatches).toEqual([]);
	});

	it('defines every token the authed app relies on', () => {
		const tokens = tokenMap();
		for (const name of [
			'copper-light',
			'tier-high',
			'tier-elevated',
			'tier-watch',
			'tier-low',
			'copper-rgb',
			'charcoal-400-rgb',
			'charcoal-900-rgb',
			'charcoal-950-rgb',
			'amber-500-rgb',
			'status-error-rgb',
			'status-ok-rgb',
			'tier-high-rgb',
			'tier-elevated-rgb',
			'tier-watch-rgb'
		]) {
			expect(tokens.has(name), `missing --${name}`).toBe(true);
		}
	});
});
