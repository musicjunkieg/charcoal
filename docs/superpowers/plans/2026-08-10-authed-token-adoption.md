# Authed Token Adoption (#250) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the six authed surfaces and two shared components consume `tokens.css` instead of ~292 literal colour values, without changing a single rendered colour.

**Architecture:** Add five semantic colour tokens and ten channel triplets to `tokens.css`, then mechanically replace every literal in the authed app with a `var()` reference. Tier colours move from three duplicated TypeScript maps to CSS classes — the pattern the dashboard already uses. Correctness is proven by a resolved-value diff that must report zero deltas, and that diff script is negative-controlled before any clean run is believed.

**Tech Stack:** SvelteKit 2 / Svelte 5, vanilla CSS custom properties, vitest, Node ESM scripts.

**Spec:** `docs/superpowers/specs/2026-08-10-authed-token-adoption-design.md`

## Global Constraints

- **No colour value may change.** This is a pure refactor. The only acceptable resolved-value delta is zero.
- **Never use `git add -A` / `git add .` / `git commit -am`.** Stage files explicitly by name.
- **Never use heredoc (`<<EOF`)** in shell commands — it breaks in zsh here. Use single-quoted multi-line strings.
- **Build with `npm --prefix web run build`**, never `cd web && npm run build` (the CWD gets stuck).
- **Always invoke the svelte skills** (`svelte-expert`, `mcp__svelte__svelte-autofixer`) before finalising any `.svelte` edit.
- **Baseline `svelte-check` errors: 5**, all pre-existing in `accounts/[handle]`. That number must not rise.
- **Do not touch** `web/src/routes/+page.svelte` (landing) or `web/src/routes/login/+page.svelte`. They already consume tokens correctly and are out of scope.
- **Do not attempt #248 (`prefers-reduced-motion`) or #249 (WCAG contrast).** This change unblocks them and deliberately does neither.
- Commit after every task. Push the branch frequently.
- Branch: `feat/250-tokens-authed-app` (already created, spec already committed at `411aa6c`).

## The Substitution Table

This table is the complete mechanical instruction for Tasks 3–7. Every task references it rather than repeating it.

### Solid hex → token

| Literal | Token |
|---|---|
| `#0c0a09` | `var(--charcoal-950)` |
| `#1c1917` | `var(--charcoal-900)` |
| `#292524` | `var(--charcoal-800)` |
| `#44403c` | `var(--charcoal-700)` |
| `#57534e` | `var(--charcoal-600)` |
| `#78716c` | `var(--charcoal-500)` |
| `#a8a29e` | `var(--charcoal-400)` |
| `#d6d3d1` | `var(--charcoal-300)` |
| `#fffbeb` | `var(--cream-50)` |
| `#fef3c7` | `var(--cream-100)` |
| `#f59e0b` | `var(--amber-500)` |
| `#c9956c` | `var(--copper)` |
| `#e8b48a` | `var(--copper-light)` |
| `#f87171` | `var(--status-error)` |
| `#86efac` | `var(--status-ok)` |
| `#fca5a5` | `var(--tier-high)` |
| `#fdba74` | `var(--tier-elevated)` |
| `#fcd34d` | `var(--tier-watch)` |

Hex matching is case-insensitive.

### rgba → channel token

Preserve the alpha value exactly. `A` below is whatever alpha the source had.

| Literal | Replacement |
|---|---|
| `rgba(201, 149, 108, A)` | `rgb(var(--copper-rgb) / A)` |
| `rgba(168, 162, 158, A)` | `rgb(var(--charcoal-400-rgb) / A)` |
| `rgba(28, 25, 23, A)` | `rgb(var(--charcoal-900-rgb) / A)` |
| `rgba(12, 10, 9, A)` | `rgb(var(--charcoal-950-rgb) / A)` |
| `rgba(245, 158, 11, A)` | `rgb(var(--amber-500-rgb) / A)` |
| `rgba(248, 113, 113, A)` | `rgb(var(--status-error-rgb) / A)` |
| `rgba(134, 239, 172, A)` | `rgb(var(--status-ok-rgb) / A)` |
| `rgba(252, 165, 165, A)` | `rgb(var(--tier-high-rgb) / A)` |
| `rgba(253, 186, 116, A)` | `rgb(var(--tier-elevated-rgb) / A)` |
| `rgba(252, 211, 77, A)` | `rgb(var(--tier-watch-rgb) / A)` |

## The inertness check

Tasks 3–7 each end by proving they changed no colour. **Use exactly this command** — do not filter it with `grep`.

```bash
node web/scripts/resolve-colors.mjs \
  "web/src/routes/(protected)/+layout.svelte" \
  "web/src/routes/(protected)/dashboard/+page.svelte" \
  "web/src/routes/(protected)/accounts/+page.svelte" \
  "web/src/routes/(protected)/accounts/[handle]/+page.svelte" \
  "web/src/routes/(protected)/review/+page.svelte" \
  "web/src/routes/(protected)/admin/+page.svelte" \
  web/src/lib/components/ScanProgress.svelte \
  web/src/lib/components/LabelButtons.svelte \
  web/src/lib/website/styles/tiers.css \
  > /tmp/250-check.txt
diff docs/superpowers/plans/250-baseline.txt /tmp/250-check.txt && echo "INERT"
```

Expected: `INERT`, with no diff output.

**`tiers.css` joined the list after Task 5 created it.** A file that owns colour must be scanned, or colours that relocate into it disappear from the check without a same-file replacement — which is exactly what happened on Task 5 and produced a delta that looked like a regression and was not.

**The baseline was re-established once, after Task 5, at TOTAL 327.** This was a deliberate consolidation checkpoint, not a number tuned to make a check pass, and the arithmetic was reconciled line by line first:

- five duplicated `#a8a29e` inline fallbacks collapsed into one shared `.tier-low` rule → **−4**
- three tier hexes relocated from the deleted `TIER_COLORS` maps into `tiers.css` → **0**
- three pill-border colours became statically visible for the first time → **+3**

net **−1**, from 328 to 327.

That third line is worth understanding: the pill borders were previously constructed at runtime by `${TIER_COLORS[tier]}40`, string-concatenating a hex-alpha suffix. No source scan could see them. Making them `rgb(var(--tier-high-rgb) / 0.25)` did not add a colour to the page — it made an existing one visible to static analysis.

A zero-delta invariant cannot express de-duplication.

**Task 6 also consolidates, and re-baselined again at TOTAL 323.** Its Step 1 deletes the dashboard's own `.tier-high` / `.tier-elevated` / `.tier-watch` / `.tier-low` rules in favour of `tiers.css`, which already provides them — so the 327 baseline was double-counting those four colours, once in each file. Removing the duplicates is the point of the step. Verified as exactly four removals and zero additions.

**Only Task 7 is a pure substitution**, so zero deltas is the right expectation there and nowhere else. Any task whose brief tells it to DELETE a rule in favour of a shared one will legitimately reduce the count; check the step, not the assumption.

**Why the whole file and not a `grep` for the file you touched.** The script emits one record per file as `path<TAB>colour1\ncolour2\n…`, so a record spans many lines and only its *first* line contains the path. `grep "admin"` therefore returns 1 line out of ~40 and a diff of that compares almost nothing — it prints `INERT` no matter what changed. The baseline is 332 lines; exactly 1 matches `admin`. An earlier draft of this plan used the grep form in every task, which would have made all five checks near-vacuous. Diff the whole file; it is just as cheap and cannot lie.

### Never substitute

These match a hex-ish pattern but are not colours. Leave them exactly as they are:

- Issue references in comments: `#222`, `#248`, `#249`, `#250`, `#257`, `#286`, `#288`
- The HTML entity `&#10003;` (a checkmark)
- `rgba(0, 0, 0, A)` — pure black, 1 use, has no palette token and is not getting one

---

### Task 1: Resolved-value diff harness

Build the verification tool **first**, so every later task can be checked as it lands.

**Files:**
- Create: `web/scripts/resolve-colors.mjs`
- Create: `docs/superpowers/plans/250-baseline.txt` (generated; committed as evidence)

**Interfaces:**
- Produces: `node web/scripts/resolve-colors.mjs <glob...>` prints one line per file, `<path>\t<sorted, newline-joined multiset of resolved colours>`, then a `TOTAL <n>` line. Later tasks consume this via diff.

- [ ] **Step 1: Write the script**

Create `web/scripts/resolve-colors.mjs`:

```js
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

	// rgb(var(--x-rgb) / a) and rgba(...) literals both land as "hex@alpha".
	const channelRef = /rgba?\(\s*var\(--([\w-]+)\)\s*\/\s*([\d.]+)\s*\)/g;
	let body = text.replace(channelRef, (_, name, alpha) => {
		const raw = tokens.get(name);
		const hex = raw ? channelsToHex(raw) : null;
		found.push(hex ? `${hex}@${alpha}` : `UNRESOLVED(--${name})`);
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
		if (raw && raw.startsWith('#')) found.push(normaliseHex(raw));
		else if (raw === undefined) found.push(`UNRESOLVED(--${name})`);
		// Non-colour tokens (fonts, easings) resolve to a value we ignore.
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
```

- [ ] **Step 2: Capture the baseline**

The spec's counts are a snapshot and the dashboard already drifted 48→52 since the audit. The `before` side must be captured from the tree as it is right now, not from the spec.

```bash
node web/scripts/resolve-colors.mjs \
  "web/src/routes/(protected)/+layout.svelte" \
  "web/src/routes/(protected)/dashboard/+page.svelte" \
  "web/src/routes/(protected)/accounts/+page.svelte" \
  "web/src/routes/(protected)/accounts/[handle]/+page.svelte" \
  "web/src/routes/(protected)/review/+page.svelte" \
  "web/src/routes/(protected)/admin/+page.svelte" \
  web/src/lib/components/ScanProgress.svelte \
  web/src/lib/components/LabelButtons.svelte \
  > docs/superpowers/plans/250-baseline.txt
```

Expected: a `TOTAL` line in the ballpark of 290–300. Record the exact number — later tasks compare against it.

- [ ] **Step 3: Confirm the baseline has no UNRESOLVED entries**

```bash
grep -c "UNRESOLVED" docs/superpowers/plans/250-baseline.txt
```

Expected: `0`. Any hit means a `var()` in the current tree already points at a token that does not exist — a live bug. Stop and report it rather than proceeding.

- [ ] **Step 4: NEGATIVE CONTROL — prove the script can fail**

A clean diff is worthless if a dirty one was impossible. Break a token on purpose:

```bash
sed -i '' 's/--copper: #c9956c;/--copper: #ff0000;/' web/src/lib/website/styles/tokens.css
node web/scripts/resolve-colors.mjs "web/src/routes/(protected)/admin/+page.svelte" > /tmp/250-nc.txt
diff <(grep "admin" docs/superpowers/plans/250-baseline.txt) <(grep "admin" /tmp/250-nc.txt) && echo "CONTROL FAILED — script is blind" || echo "CONTROL PASSED — script detects the change"
```

Expected: `CONTROL PASSED`. The admin route already has 14 `var()` uses, so corrupting `--copper` must move its resolved set.

- [ ] **Step 5: Revert the corruption and confirm the baseline is restored**

```bash
git checkout web/src/lib/website/styles/tokens.css
node web/scripts/resolve-colors.mjs "web/src/routes/(protected)/admin/+page.svelte" > /tmp/250-restored.txt
diff <(grep "admin" docs/superpowers/plans/250-baseline.txt) <(grep "admin" /tmp/250-restored.txt) && echo "RESTORED"
```

Expected: `RESTORED`.

- [ ] **Step 6: Commit**

```bash
git add web/scripts/resolve-colors.mjs docs/superpowers/plans/250-baseline.txt
git commit -m 'test(250): add the resolved-value diff harness, negative-controlled

CSS swallows a bad var() silently - var(--copper-typo) renders as nothing
rather than erroring - so a grep proving the literal is gone cannot prove it
was replaced correctly. This resolves every reference back to a literal.

Negative-controlled before use: corrupting --copper to #ff0000 moves the
admin route resolved set, and reverting restores it.

Refs #250

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>'
```

---

### Task 2: Extend `tokens.css`, with a test binding hex to channels

**Files:**
- Modify: `web/src/lib/website/styles/tokens.css`
- Create: `web/src/lib/tokens.test.ts`

**Interfaces:**
- Produces: tokens `--copper-light`, `--tier-high`, `--tier-elevated`, `--tier-watch`, `--tier-low`, and channel triplets `--copper-rgb`, `--charcoal-400-rgb`, `--charcoal-900-rgb`, `--charcoal-950-rgb`, `--amber-500-rgb`, `--status-error-rgb`, `--status-ok-rgb`, `--tier-high-rgb`, `--tier-elevated-rgb`, `--tier-watch-rgb`. Tasks 3–7 consume all of these.

- [ ] **Step 1: Write the failing test**

Create `web/src/lib/tokens.test.ts`:

```ts
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
```

- [ ] **Step 2: Run it and watch it fail**

```bash
npm --prefix web run test -- tokens.test.ts
```

Expected: FAIL on the second test — `missing --copper-light`. The first test passes vacuously right now because no `-rgb` token exists yet; that is fine and expected.

- [ ] **Step 3: Add the tokens**

In `web/src/lib/website/styles/tokens.css`, after the existing `--copper-glow` line, add:

```css
	/* Copper's hover state. The audit called #e8b48a off-palette; it is not.
	   Five of its six uses are :hover on a copper element — it is an unnamed
	   interaction state, so naming it is the whole fix. */
	--copper-light: #e8b48a;
```

After the `--status-ok` line, add:

```css
	/* Threat tiers. Promoted from values shipped across three duplicated
	   TIER_COLORS maps and again as dashboard CSS classes. --tier-low is the
	   colour the inline `?? '#a8a29e'` fallback was already producing. */
	--tier-high: #fca5a5;
	--tier-elevated: #fdba74;
	--tier-watch: #fcd34d;
	--tier-low: #a8a29e;

	/* Channel triplets for the ten colours that appear at alpha. Consumed as
	   rgb(var(--copper-rgb) / 0.2). These are INDEPENDENT of the hex tokens
	   above — src/lib/tokens.test.ts is what keeps the two in agreement. */
	--copper-rgb: 201 149 108;
	--charcoal-400-rgb: 168 162 158;
	--charcoal-900-rgb: 28 25 23;
	--charcoal-950-rgb: 12 10 9;
	--amber-500-rgb: 245 158 11;
	--status-error-rgb: 248 113 113;
	--status-ok-rgb: 134 239 172;
	--tier-high-rgb: 252 165 165;
	--tier-elevated-rgb: 253 186 116;
	--tier-watch-rgb: 252 211 77;
```

- [ ] **Step 4: Run the test and watch it pass**

```bash
npm --prefix web run test -- tokens.test.ts
```

Expected: PASS, 2 tests.

- [ ] **Step 5: NEGATIVE CONTROL — prove the consistency test bites**

```bash
sed -i '' 's/--copper-rgb: 201 149 108;/--copper-rgb: 200 149 108;/' web/src/lib/website/styles/tokens.css
npm --prefix web run test -- tokens.test.ts
```

Expected: FAIL with `--copper-rgb: 200 149 108 != #c9956c (201 149 108)`. A one-digit drift is exactly the failure this test exists for; if it passes, the test is decorative.

- [ ] **Step 6: Revert and re-confirm green**

```bash
sed -i '' 's/--copper-rgb: 200 149 108;/--copper-rgb: 201 149 108;/' web/src/lib/website/styles/tokens.css
npm --prefix web run test -- tokens.test.ts
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add web/src/lib/website/styles/tokens.css web/src/lib/tokens.test.ts
git commit -m 'feat(250): name the five colours the authed app was retyping

--copper-light, --tier-high/elevated/watch/low, plus channel triplets for
the ten colours that appear at alpha. Every value is promoted from one
already shipped, not invented.

--copper and --copper-rgb are independent declarations - nothing in CSS
binds them - so tokens.test.ts asserts every triplet matches its hex.
Verified RED against a one-digit drift.

Refs #250, #255

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>'
```

---

### Task 3: `(protected)/+layout.svelte` — delete the redeclared `:root`

The layout is first because every authed route renders inside it. Once it imports `tokens.css`, its children inherit the tokens.

**Files:**
- Modify: `web/src/routes/(protected)/+layout.svelte` (13 hex, plus the `:root` block at ~line 105–120)

- [ ] **Step 1: Delete the `:root` block and import tokens**

Remove the entire `:root { ... }` block from the `<style>` section. In the `<script>` block, add as the first import:

```js
	// Side-effectful CSS import — it defines the :root custom properties the
	// styles below reference, for this layout and every route inside it.
	import '$lib/website/styles/tokens.css';
```

The deleted block declared `--copper-glow: rgba(201, 149, 108, 0.25)` where `tokens.css` says `0.3`. That divergence is inert: nothing in the authed app reads `--copper-glow`. Step 4 confirms this empirically rather than trusting it.

- [ ] **Step 2: Apply the substitution table**

Replace all 13 hex literals and every `rgba()` in this file per the Substitution Table above.

- [ ] **Step 3: Run the autofixer**

Pass the full file content to `mcp__svelte__svelte-autofixer`. Expected: zero issues. Fix anything it reports and re-run until clean.

- [ ] **Step 4: Verify inert**

Run **The inertness check** from the top of this plan, verbatim.

Expected: `INERT`, no diff output. If `--copper-glow` shows a delta, the assumption that nothing reads it was wrong — stop and report.

- [ ] **Step 5: Build**

```bash
npm --prefix web run build
```

Expected: succeeds.

- [ ] **Step 6: Commit**

```bash
git add "web/src/routes/(protected)/+layout.svelte"
git commit -m 'refactor(250): the authed layout imports tokens instead of redeclaring them

Deletes the :root block that shadowed tokens.css for every authed route.
Its one divergent value, --copper-glow at 0.25 vs 0.3, is read by nothing
in the authed app - the resolved-value diff confirms the deletion is inert.

Refs #250

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>'
```

---

### Task 4: Shared components — `ScanProgress` and `LabelButtons`

**Files:**
- Modify: `web/src/lib/components/ScanProgress.svelte` (9 hex; already imports tokens)
- Modify: `web/src/lib/components/LabelButtons.svelte` (7 hex; no import yet)

- [ ] **Step 1: Add the missing import to `LabelButtons.svelte`**

```js
	import '$lib/website/styles/tokens.css';
```

`ScanProgress.svelte` already has this at line 6 — do not add a second one.

- [ ] **Step 2: Apply the substitution table to both files**

One deliberate carry-over: `ScanProgress` uses `#e8b48a` on `.progress-title`, which is **not** a hover state. Replace it with `var(--copper-light)` like the others. Do **not** "correct" it to `var(--copper)` — whether that use was intentional is a separate question, and normalising it here would smuggle a visual change into a refactor. The spec records this explicitly.

- [ ] **Step 3: Run the autofixer on both files**

Expected: zero issues each.

- [ ] **Step 4: Verify inert**

Run **The inertness check** from the top of this plan, verbatim.

Expected: `INERT`.

Note for this task specifically: `LabelButtons.svelte` contributes six `LOCAL(--tier-color)` / `LOCAL(--tier-bg)` / `LOCAL(--tier-border)` entries to the baseline. Those markers must still be present and unchanged afterwards. If you rename one of those local custom properties, the marker changes and the diff will flag it — that is correct behaviour, not a false alarm.

- [ ] **Step 5: Run the existing component tests**

```bash
npm --prefix web run test
```

Expected: all pass, including the 36 pre-existing logic tests plus the 2 from Task 2.

- [ ] **Step 6: Commit**

```bash
git add web/src/lib/components/ScanProgress.svelte web/src/lib/components/LabelButtons.svelte
git commit -m 'refactor(250): shared components consume tokens

ScanProgress .progress-title keeps --copper-light even though it is not a
hover state, preserving its exact shipped colour. Whether that use was
intentional belongs in its own change, not smuggled into a refactor.

Refs #250

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>'
```

---

### Task 5: Tier unification — `accounts`, `accounts/[handle]`, `review`

The three routes that duplicate `TIER_COLORS`. The dashboard already does this correctly at `dashboard/+page.svelte:404` (`class="legend-tier tier-{acct.threat_tier.toLowerCase()}"`) — this task makes the other three match the codebase's own idiom.

**Files:**
- Create: `web/src/lib/tier-class.ts`
- Create: `web/src/lib/tier-class.test.ts`
- Create: `web/src/lib/website/styles/tiers.css`
- Modify: `web/src/routes/(protected)/accounts/+page.svelte` (24 hex; `TIER_COLORS` at 13–17, uses at 100 and 155)
- Modify: `web/src/routes/(protected)/accounts/[handle]/+page.svelte` (30 hex; `TIER_COLORS` at 13–17, use at 108)
- Modify: `web/src/routes/(protected)/review/+page.svelte` (20 hex; `TIER_COLORS` at 9–13, use at 99)

**Interfaces:**
- Produces: `tierClass(tier: string | null | undefined): string` from `$lib/tier-class`. All three routes consume it.

#### Why a helper rather than inline interpolation

The obvious rewrite is `class="tier-{account.threat_tier.toLowerCase()}"`. It is wrong here, in two ways that both change what renders:

1. The code being replaced ends in `?? '#a8a29e'`. That fallback fires for **any** value not in the map. `threat_tier` can be `NotAssessed` or `InsufficientData` (see #283, #245), and today those render at `#a8a29e`. Naive interpolation emits the unmatched class `tier-notassessed`, which matches no rule, so the element silently inherits a different colour.
2. A value containing a space (`Not assessed`) would interpolate into `class="tier-not assessed"` — two classes, neither intended.

`tierClass()` reproduces the old fallback exactly.

- [ ] **Step 1: Write the failing test**

Create `web/src/lib/tier-class.test.ts`:

```ts
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
```

- [ ] **Step 2: Run it and watch it fail**

```bash
npm --prefix web run test -- tier-class.test.ts
```

Expected: FAIL — cannot resolve `./tier-class`.

- [ ] **Step 3: Write the helper**

Create `web/src/lib/tier-class.ts`:

```ts
/** Threat tiers that have their own colour. Anything else is unscored. */
const SCORED = new Set(['high', 'elevated', 'watch', 'low']);

/**
 * CSS class for a threat tier.
 *
 * Unrecognised values return `tier-low` on purpose: the inline styles this
 * replaced ended in `?? '#a8a29e'`, which is exactly `--tier-low`. Abstained
 * accounts (NotAssessed, InsufficientData) rely on that fallback, so changing
 * it here would recolour them.
 */
export function tierClass(tier: string | null | undefined): string {
	const t = (tier ?? '').trim().toLowerCase();
	return SCORED.has(t) ? `tier-${t}` : 'tier-low';
}
```

- [ ] **Step 4: Run the test and watch it pass**

```bash
npm --prefix web run test -- tier-class.test.ts
```

Expected: PASS, 3 tests.

- [ ] **Step 5: Delete all three `TIER_COLORS` maps**

Remove the `const TIER_COLORS: Record<string, string> = { ... };` declaration from each of the three files, and add to each `<script>`:

```ts
	import { tierClass } from '$lib/tier-class';
```

- [ ] **Step 6: Create the shared tier stylesheet and import it**

**Bryan's ruling, 2026-08-10:** one global stylesheet, not a copy per component. Duplicating these four rules across four files would re-create in CSS exactly the per-file retyping #250 exists to eliminate.

Create `web/src/lib/website/styles/tiers.css`:

```css
/* Threat tier colours.
 *
 * Global rather than component-scoped, and deliberately so: four files need
 * these exact rules, and copying them per component would reproduce the
 * bypass this whole change is removing. Imported alongside tokens.css.
 *
 * Safe to reach only through an interpolated class expression — Svelte stops
 * pruning class selectors entirely once any dynamic class is present on an
 * element, verified against the compiler before this was written.
 */
.tier-high { color: var(--tier-high); }
.tier-elevated { color: var(--tier-elevated); }
.tier-watch { color: var(--tier-watch); }
.tier-low { color: var(--tier-low); }
```

Add to each of the three routes' `<script>` blocks, next to the `tokens.css` import:

```ts
	import '$lib/website/styles/tiers.css';
```

**Specificity note.** A Svelte-scoped `.tier-high` carries the scoping class and so has specificity 0-2-0; this global rule is 0-1-0. That is fine where nothing else colours these elements, but it is a genuine behavioural difference the resolved-value diff cannot see — that diff compares declared colours, not cascade outcomes. Task 8 Step 6 drives the running app for exactly this reason.

- [ ] **Step 7: Rewrite the four call sites**

`accounts/+page.svelte:100` — the pill. This is the site that could not survive any `var()`-based approach while the colour lived in TypeScript, because `` `${TIER_COLORS[tier]}40` `` appends a hex-alpha suffix by string concatenation and would have produced the literal `var(--tier-high)40`.

**The Low and All pills must stay uncoloured.** Today `TIER_COLORS['Low']` and `TIER_COLORS['All']` are both `undefined`, so the template emits `color: undefined; border-color: undefined40` — invalid CSS that the browser discards. Reusing the generic `.tier-*` classes here would hand the active Low pill a colour it does not currently have. A `data-tier` attribute keeps the pill rules separate from the badge rules:

```svelte
				<button
					class="pill"
					class:active={selectedTier === tier}
					data-tier={tier}
					onclick={() => applyTier(tier)}
				>{tier}</button>
```

and in the `<style>` block — three rules only, no `Low`, no `All`:

```css
	.pill.active[data-tier='High'] {
		color: var(--tier-high);
		border-color: rgb(var(--tier-high-rgb) / 0.25);
	}
	.pill.active[data-tier='Elevated'] {
		color: var(--tier-elevated);
		border-color: rgb(var(--tier-elevated-rgb) / 0.25);
	}
	.pill.active[data-tier='Watch'] {
		color: var(--tier-watch);
		border-color: rgb(var(--tier-watch-rgb) / 0.25);
	}
```

Note: hex-alpha `40` is 64/255 ≈ **0.25**. Step 10's diff is what confirms it; if that file shows a delta, adjust the alpha until it does not.

`accounts/+page.svelte:155` — the badge:

```svelte
									<span class="tier-badge {tierClass(account.threat_tier)}">
										{account.threat_tier}
									</span>
```

`accounts/[handle]/+page.svelte:108` — the score value:

```svelte
				<div class="score-value {tierClass(account.threat_tier)}">
```

`review/+page.svelte:99` — replace the `style="color: {TIER_COLORS[...] ?? '#a8a29e'}"` attribute with a class. Merge it into the element's existing `class` attribute rather than adding a second one:

```svelte
									class="... {tierClass(account.threat_tier)}"
```

- [ ] **Step 8: Apply the substitution table to the remaining literals in all three files**

- [ ] **Step 9: Run the autofixer on all three files**

Expected: zero issues each.

- [ ] **Step 10: Verify inert**

Run **The inertness check** from the top of this plan, verbatim.

Expected: `INERT`. The `0.25` alpha assumption from Step 7 is what this catches, along with the abstained-account fallback.

- [ ] **Step 11: Confirm `TIER_COLORS` is fully gone**

```bash
grep -rn "TIER_COLORS" web/src/ && echo "STILL PRESENT — not done" || echo "GONE"
```

Expected: `GONE`.

- [ ] **Step 12: Check `svelte-check` has not regressed**

```bash
npm --prefix web run check
```

Expected: 5 errors, all pre-existing in `accounts/[handle]`. Not 6.

- [ ] **Step 13: Commit**

```bash
git add web/src/lib/tier-class.ts web/src/lib/tier-class.test.ts web/src/lib/website/styles/tiers.css "web/src/routes/(protected)/accounts/+page.svelte" "web/src/routes/(protected)/accounts/[handle]/+page.svelte" "web/src/routes/(protected)/review/+page.svelte"
git commit -m 'refactor(250): tier colour by CSS class, deleting three duplicated maps

TIER_COLORS lived in three routes and again as dashboard CSS. The dashboard
already selected by class; this makes the other three match.

Kills the one site no var()-based approach could survive: the accounts pill
built its border with `${TIER_COLORS[tier]}40`, string-concatenating a
hex-alpha suffix, which would have produced the literal var(--tier-high)40.

Two traps the naive rewrite would have walked into, both caught by writing
the helper test first. The old code ended in `?? #a8a29e`, which fires for
NotAssessed and InsufficientData - interpolating the tier straight into a
class name would have silently recoloured every abstained account, and a
value with a space would have emitted two classes. tierClass() reproduces
the fallback exactly. Separately, the Low and All pills render no colour
today because TIER_COLORS has no entry for them, so the pill rules key off
data-tier and cover only the three scored tiers.

Refs #250

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>'
```

---

### Task 6: `dashboard/+page.svelte`

The largest file: 52 hex literals plus the bulk of the rgba.

**Files:**
- Modify: `web/src/routes/(protected)/dashboard/+page.svelte`

- [ ] **Step 1: Delete the scoped tier rules at lines 699–710 and import the shared sheet**

Per Bryan's ruling in Task 5, the four tier rules live in one global stylesheet. Delete `.tier-high`, `.tier-elevated`, `.tier-watch` and `.tier-low` from this component's `<style>` block entirely, and add to the `<script>` block:

```ts
	import '$lib/website/styles/tiers.css';
```

**Keep `.tier-not-assessed` scoped and in this file**, converting only its literal:

```css
	.tier-not-assessed {
		color: var(--charcoal-500);
		cursor: default;
	}
```

It stays `--charcoal-500` rather than gaining a tier token, and stays out of `tiers.css`, because abstention sits deliberately *outside* the tier scale — that is what #245 is about — and no other file uses it.

**Watch the cascade here.** This file applies tier classes statically (`class="tier-card tier-high"` at line 337) alongside `.tier-card` rules. Moving `.tier-high` from scoped (0-2-0) to global (0-1-0) lowers its specificity, so if any `.tier-card` rule also sets `color`, it may now win where it previously lost. Check the `.tier-card` rules for a `color` declaration before assuming this is inert, and confirm on the running app in Task 8 Step 6 — the resolved-value diff compares declared colours and cannot see a cascade change.

- [ ] **Step 2: Replace the two inline accuracy-stat styles at lines 437 and 441**

```svelte
						<span class="accuracy-num accuracy-over">{accuracy.overscored}</span>
```
```svelte
						<span class="accuracy-num accuracy-under">{accuracy.underscored}</span>
```

and in `<style>`:

```css
	.accuracy-over { color: var(--tier-elevated); }
	.accuracy-under { color: var(--tier-watch); }
```

- [ ] **Step 3: Apply the substitution table to all remaining literals in the file**

- [ ] **Step 4: Run the autofixer**

Expected: zero issues.

- [ ] **Step 5: Verify inert**

Run **The inertness check** from the top of this plan, verbatim.

Expected: `INERT`. Remember this check compares *declared* colours and cannot see the scoped-to-global specificity change described in Step 1 — a green result here does not clear that.

- [ ] **Step 6: Commit**

```bash
git add "web/src/routes/(protected)/dashboard/+page.svelte"
git commit -m 'refactor(250): dashboard consumes tokens

52 hex literals and the bulk of the rgba. .tier-not-assessed deliberately
keeps --charcoal-500 rather than gaining a tier token - abstention sits
outside the tier scale (#245).

Refs #250

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>'
```

---

### Task 7: `admin/+page.svelte`

Partially migrated by #288 — 14 `var()` uses already, 28 literals left.

**Files:**
- Modify: `web/src/routes/(protected)/admin/+page.svelte`

- [ ] **Step 1: Apply the substitution table**

The `tokens.css` import at line 6 already exists — do not add a second. The comment at line 416 references issue numbers, not colours; leave it alone.

- [ ] **Step 2: Run the autofixer**

Expected: zero issues.

- [ ] **Step 3: Verify inert**

Run **The inertness check** from the top of this plan, verbatim.

Expected: `INERT`.

- [ ] **Step 4: Commit**

```bash
git add "web/src/routes/(protected)/admin/+page.svelte"
git commit -m 'refactor(250): admin consumes tokens

Finishes what #288 started on this route.

Refs #250

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>'
```

---

### Task 8: Whole-tree verification and close-out

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Full resolved-value diff across all eight files**

```bash
node web/scripts/resolve-colors.mjs \
  "web/src/routes/(protected)/+layout.svelte" \
  "web/src/routes/(protected)/dashboard/+page.svelte" \
  "web/src/routes/(protected)/accounts/+page.svelte" \
  "web/src/routes/(protected)/accounts/[handle]/+page.svelte" \
  "web/src/routes/(protected)/review/+page.svelte" \
  "web/src/routes/(protected)/admin/+page.svelte" \
  web/src/lib/components/ScanProgress.svelte \
  web/src/lib/components/LabelButtons.svelte \
  > /tmp/250-final.txt
diff docs/superpowers/plans/250-baseline.txt /tmp/250-final.txt && echo "ZERO DELTAS"
```

Expected: `ZERO DELTAS`. Anything else is a defect — investigate before proceeding, do not rationalise it.

- [ ] **Step 2: Confirm no UNRESOLVED tokens anywhere**

```bash
grep -c "UNRESOLVED" /tmp/250-final.txt
```

Expected: `0`. A non-zero count means a typo'd token name that CSS is silently swallowing — the exact failure this whole harness exists to catch.

- [ ] **Step 3: Confirm the literals are actually gone**

```bash
grep -rhoE '#[0-9a-fA-F]{6}\b' "web/src/routes/(protected)/" web/src/lib/components/ | sort -u
```

Expected: empty, or only issue-reference false positives (`#286288` style artefacts should not appear; genuine `#NNN` issue refs are 3-digit and will not match this 6-digit pattern).

```bash
grep -rn "rgba(" "web/src/routes/(protected)/" web/src/lib/components/
```

Expected: only `rgba(0, 0, 0, ...)`, the single shadow with no palette token.

- [ ] **Step 4: Full test suite**

```bash
npm --prefix web run test
npm --prefix web run check
npm --prefix web run build
```

Expected: all vitest tests pass — 36 pre-existing, plus 2 from `tokens.test.ts` (Task 2) and 3 from `tier-class.test.ts` (Task 5), so **41**. `check` reports 5 errors, all pre-existing in `accounts/[handle]`. Build succeeds.

- [ ] **Step 5: Rust suite — untouched but must stay green**

```bash
CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "^\s*SKIP:"
```

Expected: **no output at all**. Note the trailing colon and the absence of `-i` — `grep -i "^\s*SKIP"` false-positives on tests whose *names* begin with "skip".

- [ ] **Step 6: Drive the running app**

Start the app and walk all six authed surfaces. #288's real bug was found this way and no test would have caught it.

Check specifically: tier badges on `/accounts` and `/review`, the active tier pill's border, the score value on an account detail page, the dashboard tier cards and accuracy stats, hover states on links (that is `--copper-light`), and an error state if one can be provoked.

- [ ] **Step 7: Update the CHANGELOG**

Add under `### Changed`. Write it by hand — `chainlink issue close` generates a line from the issue *title*, which for a problem-statement title like this one produces a backwards entry asserting the bug still exists.

```markdown
- The authed app now consumes the design tokens instead of ~292 literal colour
  values. Tier colours moved from three duplicated TypeScript maps to CSS
  classes, matching the pattern the dashboard already used. No colour changed:
  a resolved-value diff reports zero deltas across all eight files. (#250)
```

- [ ] **Step 8: Commit and push**

```bash
git add CHANGELOG.md
git commit -m 'docs(250): changelog for authed token adoption

Refs #250

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>'
git push
```

- [ ] **Step 9: Log the outcome to the decision graph**

The goal node for this work is **475**. `deciduous add` prints the new node's ID; use that number in the `link` call.

```bash
deciduous add outcome "#250 landed: the authed app consumes tokens, zero resolved-value deltas" -c 95 --commit HEAD
# note the printed node id, then:
deciduous link 475 <printed_id> -r "Implementation outcome"
deciduous sync
```

- [ ] **Step 10: Open the PR**

Write the body to a file first — never a heredoc, which breaks in zsh here.

```bash
node -e 'require("fs").writeFileSync("/tmp/250-pr.md", process.argv[1])' "$(git log --format=%b -n 1 HEAD~1)"
gh pr create --base staging \
  --title "#250: take the token system into the authed app" \
  --body-file /tmp/250-pr.md
```

Target `staging`, never `main`. Then paste the resolved-value diff result (`ZERO DELTAS`) into the PR description as the evidence for "no colour changed" — a reviewer cannot verify that claim by reading a 292-line diff.
