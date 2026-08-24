# Taking the token system into the authed app (#250)

**Status:** approved 2026-08-10
**Issue:** #250 (P1, parent #244). Unblocks #248, #249.
**Branch:** `feat/250-tokens-authed-app`

## Problem

`web/src/lib/website/styles/tokens.css` defines the Charcoal palette. The
marketing pages consume it. The product does not.

This is **bypass, not drift**. The hard-coded values in the authed app are
overwhelmingly palette-*correct* — `#c9956c` really is `--copper`, `#78716c`
really is `--charcoal-500`. Nothing looks broken. That is precisely what makes
it P1: a token change propagates to the landing page and silently skips the
entire product. The failure surfaces only when someone changes a colour and
half the app ignores them.

### Measured scope

The audit in #250 counted 161 literal hex values. That undercounts, because it
only grepped hex. Actual state of the six authed surfaces plus two shared
components, measured 2026-08-10:

| File | hex | rgba | `var(--)` |
|---|---:|---:|---:|
| `dashboard/+page.svelte` | 52 | — | 2 |
| `accounts/[handle]/+page.svelte` | 30 | — | 0 |
| `admin/+page.svelte` | 28 | — | 14 |
| `accounts/+page.svelte` | 24 | — | 0 |
| `review/+page.svelte` | 20 | — | 0 |
| `(protected)/+layout.svelte` | 13 | — | 16 |
| `ScanProgress.svelte` | 9 | — | 15 |
| `LabelButtons.svelte` | 7 | — | 6 |
| **rgba across all of the above** | — | **117** | — |

Roughly **175 real hex** (the raw count of 183 includes eight false positives:
issue references such as `#257`, `#288`, `#222`, and the HTML entity
`&#10003;`) plus **117 `rgba()`** literals — about **292 literals**, not 161.

The rgba half is the same palette at varying alpha, and is invisible to a
hex-only grep:

| triplet | count | is |
|---|---:|---|
| `201, 149, 108` | 41 | `--copper` |
| `168, 162, 158` | 30 | `--charcoal-400` |
| `28, 25, 23` | 12 | `--charcoal-900` |
| `245, 158, 11` | 11 | `--amber-500` |
| `248, 113, 113` | 6 | `--status-error` |
| `12, 10, 9` | 6 | `--charcoal-950` |
| `134, 239, 172` | 4 | `--status-ok` |
| `253, 186, 116` | 2 | tier elevated |
| `252, 211, 77` | 2 | tier watch |
| `252, 165, 165` | 2 | tier high |
| `0, 0, 0` | 1 | a shadow; stays literal |

### What #288 already fixed

Two of the three blockers the audit named are gone. `tokens.css` is no longer
imported by nothing — `admin`, `dashboard`, and `ScanProgress` import it. And
`--status-error` / `--status-ok` exist, promoted from values already shipped
six times each as bare hex (closed #255). The pattern is proven end to end;
this spec applies it to the rest.

## Design

### 1. New tokens

Five named colours, every one **promoted from a value already shipped**, not
invented — the same discipline #288 used for the status pair:

```css
--copper-light:  #e8b48a;   /* copper's hover state, which never had a name */
--tier-high:     #fca5a5;
--tier-elevated: #fdba74;
--tier-watch:    #fcd34d;
--tier-low:      #a8a29e;   /* today the `?? '#a8a29e'` inline fallback */
```

`#e8b48a` was flagged "off-palette" by the audit. It is not junk: five of its
six uses are `:hover` on a copper element. It is an unnamed interaction state.

The sixth use — `ScanProgress .progress-title`, a non-hover context — is
**preserved exactly as `var(--copper-light)`**. Whether that was intentional or
copy-paste is a separate question; this change is not the place to answer it,
and normalising it silently would be a visual change smuggled into a refactor.

Plus channel triplets for the ten colours that appear at alpha:

```css
--copper-rgb:        201 149 108;
--charcoal-400-rgb:  168 162 158;
--charcoal-900-rgb:   28  25  23;
--charcoal-950-rgb:   12  10   9;
--amber-500-rgb:     245 158  11;
--status-error-rgb:  248 113 113;
--status-ok-rgb:     134 239 172;
--tier-high-rgb:     252 165 165;
--tier-elevated-rgb: 253 186 116;
--tier-watch-rgb:    252 211  77;
```

Consumed as `rgb(var(--copper-rgb) / 0.2)`.

**The cost, stated plainly:** each of these colours now exists twice, as hex
and as channels, and the two must agree. Relative colour syntax
(`rgb(from var(--copper) r g b / 0.2)`) avoids the duplication but is harder to
read for a reader learning CSS, which matters more here than the saved line.

The duplication is therefore **enforced, not trusted**: a test parses
`tokens.css` and asserts every `--x-rgb` equals the channel decomposition of
its `--x` hex. Drift fails the suite instead of shipping as a wrong colour.

### 2. Tier colours become CSS classes

`TIER_COLORS` is currently a TypeScript map duplicated in three routes
(`accounts`, `accounts/[handle]`, `review`) and duplicated *again* as CSS
classes in `dashboard`. All three maps are deleted; markup selects by class:

```svelte
<span class="tier tier-{account.threat_tier.toLowerCase()}">
```

This is also the only approach that fixes `accounts/+page.svelte:100`:

```svelte
style={`color: ${TIER_COLORS[tier]}; border-color: ${TIER_COLORS[tier]}40`}
```

That `40` is a hex-alpha suffix appended by string concatenation. Any approach
that keeps the colour in TypeScript produces the literal garbage
`var(--tier-high)40`. As a class it becomes
`border-color: rgb(var(--tier-high-rgb) / 0.25)`.

### 3. The redeclared `:root` blocks

Three files redeclare `:root`: `(protected)/+layout`, landing, and login.
(ScanProgress does not — the earlier "four" count was a comment mentioning
`:root`, not a declaration.) Only `(protected)/+layout` is in scope; the two
marketing files are out of scope.

`(protected)/+layout` diverges from `tokens.css` in exactly one value:

```
tokens.css              --copper-glow: rgba(201, 149, 108, 0.3)
(protected)/+layout     --copper-glow: rgba(201, 149, 108, 0.25)
```

**This divergence is inert.** `--copper-glow` is read by nothing in the authed
app — its only readers are `routes/+page.svelte:1159` and
`login/+page.svelte:402,409`, both of which declare their own local `0.3` that
shadows `tokens.css` anyway. Deleting the authed block therefore cannot change
what renders, and the resolved-value diff in §4 must confirm it empirically.

`(protected)/+layout` also omits `--amber-600` and both `--ease-*` tokens.
Neither is referenced anywhere in the authed app today, so importing the full
`tokens.css` adds availability, not appearance.

### 4. Verification

A script (`web/scripts/resolve-colors.mjs`) resolves every `var(--x)` and
`rgb(var(--x-rgb) / a)` back to a literal, then emits a per-file multiset of
computed colours. Run before and after; diff.

**Success is zero deltas.** Not "looks the same" — zero.

**The script earns trust before the run counts.** Before accepting a clean
diff, corrupt one token (`--copper` → `#ff0000`) and confirm the script reports
a delta in every file that uses copper, with a total matching copper's known
occurrence count (31 solid uses; the 41 translucent ones resolve through
`--copper-rgb` and are exercised by corrupting *that* token separately, since
the two are independent declarations). Then revert.

A green result is meaningless if a red one was unreachable — the failure mode
that made an orphan-node audit report zero orphans earlier the same day,
because it was silently reading an empty database.

The negative control is a **required, recorded step**, not a nicety.

### 5. Testing

- `tokens.css` hex/channel consistency test (new, §1) — must fail RED against a
  deliberately mismatched triplet before it counts.
- `npm --prefix web run build` succeeds.
- `svelte-check` shows no *new* errors. Baseline is 5 pre-existing errors in
  `accounts/[handle]`; that count must not rise.
- `svelte-autofixer` returns zero issues on every touched `.svelte` file.
- Existing 36 vitest logic tests still pass.
- Rust suite is untouched by this change but must stay green:
  `CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output`
  with zero `SKIP:` lines.

## Scope boundary

**In:** the six authed surfaces, `ScanProgress`, `LabelButtons`, `tokens.css`,
one new test, one new script.

**Out:** the marketing pages (landing and login already consume tokens
correctly). Any change to a colour *value*. #248 (`prefers-reduced-motion`) and
#249 (WCAG AA contrast) — this change unblocks both by putting the tokens
within reach, and deliberately does not attempt either.

**Explicitly not done:** normalising the one non-hover `--copper-light` use in
ScanProgress (§1).

## Risks

| Risk | Mitigation |
|---|---|
| A typo'd token name resolves to nothing; CSS swallows it silently | Resolved-value diff (§4), negative-controlled |
| Hex and channel representations drift apart later | Consistency test (§1), runs in CI |
| Tier class rename misses a `threat_tier` value | `--tier-low` covers the former `?? '#a8a29e'` fallback; unknown values fall through to the base `.tier` class |
| A `:root` deletion changes appearance | Proven inert by grep (§3), reconfirmed by the diff |
