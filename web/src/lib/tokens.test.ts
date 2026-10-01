import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { globSync } from 'node:fs';

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
			'tier-watch-rgb',
			'tier-low-rgb'
		]) {
			expect(tokens.has(name), `missing --${name}`).toBe(true);
		}
	});
});

// #292/#293/#380: colour was tokenized first (#250) and stayed clean because
// nothing could reintroduce a literal without this file failing. Type, spacing
// and radius have no such guard, which is how they grew back to 193, 385 and 66
// literals after the scales were written down. This is that guard.
describe('no literal type, spacing or radius values', () => {
	const SOURCES = globSync('src/**/*.{svelte,css}').filter(
		(f) => !f.endsWith('styles/tokens.css')
	);

	// `0.9rem` in ConfirmSheet is the one deliberate exception, kept exact
	// rather than snapped to --space-14 (14.4px vs 14px) so that tokenizing
	// changed no pixels. Snapping it is a visual decision, not a refactor.
	const ALLOWED = new Set(['0.9rem']);

	const PROPS = String.raw`(?:padding|margin)(?:-(?:top|right|bottom|left|inline|block))?|(?:row-|column-)?gap`;
	const cases: [string, RegExp][] = [
		['font-size', new RegExp(String.raw`font-size\s*:\s*([^;{}\n]+)`, 'g')],
		['border-radius', new RegExp(String.raw`border-radius\s*:\s*([^;{}\n]+)`, 'g')],
		['spacing', new RegExp(String.raw`(?:${PROPS})\s*:\s*([^;{}\n]+)`, 'g')]
	];

	for (const [label, re] of cases) {
		it(`${label} uses tokens, not literals`, () => {
			const offenders: string[] = [];
			for (const file of SOURCES) {
				const css = readFileSync(file, 'utf8');
				for (const m of css.matchAll(re)) {
					// The sign is part of the atom: `margin-top: -0.5rem` is a literal too,
					// and the token form is calc(var(--space-8) * -1).
					for (const atom of m[1].match(/(?<![\w.])-?\d*\.?\d+(?:rem|em|px)|50%/g) ?? []) {
						if (atom === '0' || ALLOWED.has(atom)) continue;
						// 1px hairlines and 0 are not scale steps; -1px pulls a border
						// back over its neighbour, which is the same hairline.
						if (atom === '1px' || atom === '-1px') continue;
						offenders.push(`${file}: ${m[0].trim()}`);
					}
				}
			}
			expect(offenders).toEqual([]);
		});
	}
});

// A var() for a token the document never loaded is not a fallback, it is a
// dropped declaration: the browser discards the whole property and the element
// renders at the UA default. The landing page and /login carry their own :root
// with the palette only, so tokenizing them silently removed every size and gap
// until this test existed (caught in review on #129).
describe('token availability', () => {
	const USES = /var\(--(?:text|space|radius)-/;
	const IMPORTS = "styles/tokens.css";

	it('every component that uses a scale token loads tokens.css', () => {
		const offenders: string[] = [];
		for (const file of globSync('src/**/*.svelte')) {
			const src = readFileSync(file, 'utf8');
			if (!USES.test(src)) continue;
			// Everything under (protected) inherits the import from its layout.
			if (file.includes('(protected)')) continue;
			if (!src.includes(IMPORTS)) offenders.push(file);
		}
		expect(offenders).toEqual([]);
	});
});
