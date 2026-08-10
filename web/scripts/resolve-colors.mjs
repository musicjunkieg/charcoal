#!/usr/bin/env node
// Resolves every colour reference in a set of files back to a literal, so a
// token refactor can be proven inert. Reads token definitions from
// tokens.css, then expands var(--x) and rgb(var(--x-rgb) / a) in each target.
//
// Exists because CSS swallows a bad var() silently: `var(--copper-typo)`
// renders as nothing at all rather than erroring. A grep can tell you the
// literal is gone; only resolution can tell you it was replaced correctly.
import { readFileSync } from 'node:fs';
import { argv } from 'node:process';

const TOKENS_PATH = 'web/src/lib/website/styles/tokens.css';

/** Parse `--name: value;` pairs out of a CSS file. */
function parseTokens(css) {
	const tokens = new Map();
	for (const m of css.matchAll(/--([\w-]+)\s*:\s*([^;]+);/g)) {
		tokens.set(m[1].trim(), m[2].trim());
	}
	return tokens;
}

/** #abc -> #aabbcc, and lowercase everything for stable comparison. */
function normaliseHex(hex) {
	let h = hex.toLowerCase();
	if (h.length === 4) h = '#' + h[1] + h[1] + h[2] + h[2] + h[3] + h[3];
	return h;
}

/** "201 149 108" -> "#c9956c" so channel and solid forms compare equal. */
function channelsToHex(channels) {
	const parts = channels.trim().split(/[\s,]+/).map(Number);
	if (parts.length !== 3 || parts.some((n) => !Number.isFinite(n))) return null;
	return '#' + parts.map((n) => n.toString(16).padStart(2, '0')).join('');
}

function resolve(text, tokens) {
	const found = [];

	// Strip comments before scanning. Hex-shaped issue references living only
	// in comments (`// #257`, `/* #250 */`, `<!-- #257 -->`) are not colours,
	// and left in place they'd read as spurious literals. Guard the `//` case
	// on a preceding ':' so it doesn't eat `https://` URLs.
	const stripped = text
		.replace(/<!--[\s\S]*?-->/g, '')
		.replace(/\/\*[\s\S]*?\*\//g, '')
		.replace(/(?<!:)\/\/.*$/gm, '');

	// --x is not in tokens.css, but IS set somewhere else in this same file
	// (a markup `style="--x: …"` attribute, or a CSS rule's own `--x:`
	// declaration) — legitimate local indirection, not a missing token.
	// Distinct from UNRESOLVED so it stays visible rather than silently
	// swallowed: an unknown var() is exactly the failure mode this harness
	// exists to catch.
	function isLocallyDefined(name) {
		return new RegExp(`--${name}\\s*:`).test(stripped);
	}

	// rgb(var(--x-rgb) / a) and rgba(...) literals both land as "hex@alpha".
	const channelRef = /rgba?\(\s*var\(--([\w-]+)\)\s*\/\s*([\d.]+)\s*\)/g;
	let body = stripped.replace(channelRef, (_, name, alpha) => {
		const raw = tokens.get(name);
		if (raw !== undefined) {
			const hex = channelsToHex(raw);
			found.push(hex ? `${hex}@${alpha}` : `UNRESOLVED(--${name})`);
		} else if (isLocallyDefined(name)) {
			found.push(`LOCAL(--${name})`);
		} else {
			found.push(`UNRESOLVED(--${name})`);
		}
		return '';
	});

	const rgbaLiteral = /rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*(?:,\s*([\d.]+)\s*)?\)/g;
	body = body.replace(rgbaLiteral, (_, r, g, b, a) => {
		const hex = channelsToHex(`${r} ${g} ${b}`);
		found.push(`${hex}@${a ?? '1'}`);
		return '';
	});

	// Solid var(--x). Recorded as the hex it resolves to.
	const solidRef = /var\(--([\w-]+)\)/g;
	body = body.replace(solidRef, (_, name) => {
		const raw = tokens.get(name);
		if (raw !== undefined) {
			if (raw.startsWith('#')) found.push(normaliseHex(raw));
			// Non-colour tokens (fonts, easings) resolve to a value we ignore.
		} else if (isLocallyDefined(name)) {
			found.push(`LOCAL(--${name})`);
		} else {
			found.push(`UNRESOLVED(--${name})`);
		}
		return '';
	});

	// Remaining bare hex literals. Skip issue refs and HTML entities, which
	// are not colours: a colour is 3, 6 or 8 hex digits at a word boundary
	// and never preceded by '&'.
	const bareHex = /(?<!&)#([0-9a-fA-F]{3}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})\b/g;
	for (const m of body.matchAll(bareHex)) found.push(normaliseHex(m[0]));

	return found;
}

const tokens = parseTokens(readFileSync(TOKENS_PATH, 'utf8'));
let total = 0;
for (const path of argv.slice(2)) {
	const colours = resolve(readFileSync(path, 'utf8'), tokens);
	total += colours.length;
	console.log(`${path}\t${[...colours].sort().join('\n')}`);
}
console.log(`TOTAL ${total}`);
