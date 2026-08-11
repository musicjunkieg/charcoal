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

/**
 * Strip `//`-to-end-of-line comments, but only a `//` that occurs outside any
 * quoted string on that line. A lookbehind on the single preceding character
 * (checking for ':') is not enough: a protocol-relative URL
 * (`//cdn.example.com/x`), or any second `//` later on the same line (e.g.
 * `href="https://example.com/foo//bar"`), starts a false "comment" under
 * that scheme and silently deletes the rest of the line — including any
 * colour literal after it. Tracking quote state instead means a `//` inside
 * `"…"` / `'…'` / `` `…` `` is never treated as a comment, on the first
 * occurrence or the fifth, while a genuine bare `// comment` still is.
 * Resets quote state at each newline — deliberately: none of the eight
 * target files carry a `//` inside a template literal that spans a line
 * break, and handling that would add complexity for a case that isn't here.
 */
function stripLineComments(text) {
	return text
		.split('\n')
		.map((line) => {
			let quote = null;
			for (let i = 0; i < line.length; i++) {
				const ch = line[i];
				if (quote) {
					if (ch === '\\') i++; // skip escaped char, including an escaped quote
					else if (ch === quote) quote = null;
					continue;
				}
				if (ch === '"' || ch === "'" || ch === '`') {
					quote = ch;
					continue;
				}
				if (ch === '/' && line[i + 1] === '/') return line.slice(0, i);
			}
			return line;
		})
		.join('\n');
}

function resolve(text, tokens) {
	const found = [];

	// Strip comments before scanning. Hex-shaped issue references living only
	// in comments (`// #257`, `/* #250 */`, `<!-- #257 -->`) are not colours,
	// and left in place they'd read as spurious literals.
	const stripped = stripLineComments(
		text.replace(/<!--[\s\S]*?-->/g, '').replace(/\/\*[\s\S]*?\*\//g, '')
	);

	// --x is not in tokens.css, but IS set somewhere else in this same file
	// (a markup `style="--x: …"` attribute, or a CSS rule's own `--x:`
	// declaration) — legitimate local indirection, not a missing token.
	// Distinct from UNRESOLVED so it stays visible rather than silently
	// swallowed: an unknown var() is exactly the failure mode this harness
	// exists to catch.
	function isLocallyDefined(name) {
		return new RegExp(`--${name}\\s*:`).test(stripped);
	}

	// A custom-property DECLARATION is a definition, not a render:
	// `--charcoal-900: #1c1917;` paints nothing by itself — only a var(--x)
	// USAGE resolves to an actual displayed colour, and that's already
	// handled below by channelRef/solidRef. Blank the value half of any
	// `--name: value;` pair before the literal scans run, so a declared
	// literal isn't double-counted against its own usages (or, if unused,
	// counted as a phantom "rendered" colour that never painted anything).
	// Scoped to `--`-prefixed properties only: ordinary CSS declarations
	// (`color: #123;`) and JS object keys (`color: '#123'`) don't match the
	// leading `--`, so they still count exactly as before. Operates on a
	// derived copy, not `stripped` itself — isLocallyDefined above still
	// needs the untouched `--name:` text to detect local indirection.
	const customPropertyDecl = /(?<![\w-])(--[\w-]+\s*:\s*)([^;]+)(;)/g;
	const declStripped = stripped.replace(customPropertyDecl, (_, lhs, _value, semi) => `${lhs}${semi}`);

	// rgb(var(--x-rgb) / a) and rgba(...) literals both land as "hex@alpha".
	const channelRef = /rgba?\(\s*var\(--([\w-]+)\)\s*\/\s*([\d.]+)\s*\)/g;
	let body = declStripped.replace(channelRef, (_, name, alpha) => {
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

	// Named colour keywords. These are real colour values that the hex scan
	// above can't see — a later task that drops or rewrites one without
	// introducing a tracked var() would otherwise show zero diff. Matched
	// case-insensitively; the lookaround excludes '-' as well as word chars
	// so `transparent` doesn't fire inside `--bg-transparent-ish`.
	const namedColour = /(?<![\w-])(transparent|currentColor)(?![\w-])/gi;
	for (const m of body.matchAll(namedColour)) found.push(m[0].toLowerCase());

	return found;
}

const tokens = parseTokens(readFileSync(TOKENS_PATH, 'utf8'));
let total = 0;
let hadUnresolved = false;
for (const path of argv.slice(2)) {
	const colours = resolve(readFileSync(path, 'utf8'), tokens);
	total += colours.length;
	if (colours.some((c) => c.startsWith('UNRESOLVED('))) hadUnresolved = true;
	console.log(`${path}\t${[...colours].sort().join('\n')}`);
}
console.log(`TOTAL ${total}`);

// Structural gate: a caller who forgets to grep for UNRESOLVED should still
// get a hard failure. Full output is already printed above, so the diff
// use-case (piping stdout into `diff`) is unaffected either way. Uses
// exitCode rather than exit() so stdout finishes flushing first, and only
// ever moves the code off its default 0 — a clean run can't end up non-zero.
if (hadUnresolved) process.exitCode = 1;
