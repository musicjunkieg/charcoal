# Design: Abstain from toxicity scoring on unassessable-language text (#222)

**Date:** 2026-07-20
**Issue:** #222 (follow-up to #220)
**Branch:** `feat/language-abstention` (to be created; must not commit to `staging`)
**Follow-up:** #230 — same treatment for amplification events (explicitly out of scope here)

## Problem

Charcoal's toxicity models are English-only. Detoxify (`unbiased-toxic-roberta`,
the ONNX Stage-1 clean-pass gate) and CoPE-B (`cope-b-a4b`, the Stage-2
classifier) are both validated for US English only. The CoPE-B model card states:
"Currently optimized for US English policy interpretation. Performance may degrade
for other languages and locales."

When these models are handed non-English text, they do not return "unknown" — they
return a number the scoring pipeline reads as **benign**. Measured behaviour
(`examples/lang_gate_probe.rs`, 2026-07-20):

| group | mean ONNX score | max | cleared gate (<0.10) |
|---|---|---|---|
| en_hostile (positive control) | 0.9980 | 0.9990 | 0/5 |
| en_benign (negative control) | 0.0005 | 0.0007 | 5/5 |
| th_hostile (Thai) | 0.0004 | 0.0004 | 5/5 |
| ja_hostile (Japanese) | 0.0004 | 0.0004 | 5/5 |
| ru_hostile (Cyrillic) | 0.0003 | 0.0004 | 5/5 |
| xx_gibberish (non-Latin noise) | 0.0004 | 0.0006 | 5/5 |
| pt_hostile (Portuguese) | 0.0406 | 0.1911 | 4/5 |
| de_hostile (German) | 0.2526 | 0.9963 | 3/5 |

Two distinct failure regimes:

1. **Non-Latin script — zero signal.** Hostile Thai/Japanese/Cyrillic score
   identically to random character noise (0.0004). The model emits a floor
   constant for tokens outside its vocabulary. Hostile and benign are
   indistinguishable to four decimal places. "ไปตายซะ" (*go die*) scores lower
   than "Happy birthday!".
2. **Latin-script non-English — partial, unreliable signal.** Portuguese hostile
   scores ~11x its benign baseline but still clears the gate 4/5 times. German
   is noisier (one sample 0.9963, three cleared). There is *some* signal but not
   enough to trust; genuine threats are missed.

### Consequence chain (the actual bug)

```
score < 0.10  ->  is_toxic: false        (ensemble.rs:128)
              ->  Stage 2 never called
              ->  contributes 0 to binary toxicity rate
              ->  tox = 0
              ->  threat_score = tox * 70 * (...) = 0
              ->  ThreatTier::Low
```

Every non-English account currently exits the pipeline as confidently benign, and
nothing records that it was never actually assessed. A harasser posting in Japanese
is undetectable **by construction**. This is a correctness defect, independent of
how common the population is.

### Rationale for scope: all non-English, not just non-Latin

The models are validated for English only. Scoring Latin-script non-English
(Portuguese, German, Spanish) is the same category error as scoring Thai — we are
doing multilingual detection with a model not built for it, and the `pt`/`de` data
confirms it fails. Therefore we abstain on **all non-English**, not merely the
script we can trivially detect. The one case we cannot detect (a Latin-script
non-English post misdeclared as `en`) is an accepted, documented limit — see
Non-Goals.

## Non-Goals

- **Amplification-event scoring.** Quotes/reposts of the protected user run through
  a separate code path. Same bug, tracked as #230. Not touched here.
- **Latin-script non-English posts misdeclared as `en`.** Neither `langs` nor a
  script check can catch these. We do not add a language-identification dependency
  (`whatlang`/`lingua`) to chase them — those are themselves unreliable on short
  social posts, introducing a new false-abstain failure path. This residual gap is
  a documented limitation of the feature, not a detection problem we solve.
- **Windowing / truncation of over-long inputs (#225).** Separate concern. Note the
  zalgo-obfuscated English case (`M҉O҉T҉H҉E҉R...` scored 0.1588) surfaced during this
  investigation belongs to #225, not here.
- **Re-tuning tier thresholds.** `NotAssessed` sits outside the ordered scale and
  does not interact with the High/Elevated/Watch/Low cutoffs.

## Design

### Component 1 — `assess_language` (new pure function)

Single-responsibility, dependency-free, exhaustively unit-testable. Lives in a new
`src/scoring/language.rs` (or `src/bluesky/language.rs` — implementer's call based
on where `Post` and script helpers sit most naturally).

```rust
pub enum Assessability {
    Assessable,
    Unassessable,
}

/// Decide whether our English-only models can meaningfully assess `text`.
///
/// `langs` is the post's declared `app.bsky.feed.post.langs` (primary tags only,
/// region subtags stripped: "en-US" -> "en"). Empty when the client omitted it.
pub fn assess_language(text: &str, langs: &[String]) -> Assessability
```

Decision table (first matching row wins):

| `langs` | script of `text` | result | reason |
|---|---|---|---|
| declares a non-`en` primary tag | any | Unassessable | trust the author's client; errors here over-abstain (safe) |
| declares `en` (only) | Latin-dominant | Assessable | the supported case |
| declares `en` (only) | non-Latin-dominant | Unassessable | cross-check; catches the measured 9.1% mis-declaration |
| empty | Latin-dominant | Assessable | script-only fallback (~6% of posts lack `langs`) |
| empty | non-Latin-dominant | Unassessable | script-only fallback |

- "declares `en`" means every primary tag in `langs` is `en`. A multi-lang post
  including any non-`en` tag is Unassessable (0.1% of posts declare multiple langs).
- "Latin-dominant" reuses the script heuristic already validated in the probe:
  count Latin `[a-zA-Z]` vs non-Latin script ranges (Thai, CJK, kana, hangul,
  Cyrillic, Arabic, Hebrew, Devanagari); non-Latin when its count ≥ 5 **and**
  exceeds the Latin count. Emoji/punctuation-only text is treated as Latin-dominant
  (Assessable) — it carries no language and the models handle it in-distribution.

### Component 2 — capture `langs` on `Post`

`Post` (`src/bluesky/posts.rs:44`) gains `pub langs: Vec<String>`. Populated at the
two construction sites (originals ~line 197, replies ~line 311) from the parsed
`app.bsky.feed.post::Record.langs`. The record is already parsed elsewhere in this
file (line 410), so no new fetch or API cost. `ReplyPost` inherits it via its inner
`Post`.

Primary tags are normalised (`"en-US".split('-').next() -> "en"`) at capture time so
downstream consumers see clean tags.

### Component 3 — partition at the scoring seam

In `src/scoring/profile.rs`, before `classify_batch_with_contexts` (~line 136),
partition the account's posts into assessable / unassessable via `assess_language`.

- **Unassessable posts are not classified at all.** They never reach the ONNX gate
  or CoPE-B. This is both correct (we don't trust the verdict) and a cost saving
  (eliminates wasted CoPE-B calls on unreadable text — e.g. the Japanese Linux post
  that scored 0.87 and would have been sent to the GPU).
- Assessable-only counts flow into `compute_reply_weighted_toxicity`
  (`profile.rs:793`). That function is **unchanged** — excluded posts are simply
  absent from `total_replies` / `total_originals`, so they leave the denominator
  automatically.

### Component 4 — coverage gate

After partitioning, before scoring:

```rust
const MIN_ASSESSABLE_POSTS: usize = 5;  // mirrors MIN_FIRST_PERSON_POSTS_FOR_EARLY_EXIT
```

- assessable count ≥ 5  → score normally on the assessable subset.
- assessable count < 5   → construct `ThreatTier::NotAssessed`; do not compute a
  threat score.

Worked examples:

| posts | result |
|---|---|
| 90 en / 10 ja | scored on 90 |
| 40 en / 60 ja | scored on 40 |
| 3 en / 97 ja | NotAssessed |
| 0 en / 50 ja | NotAssessed |

### Component 5 — `ThreatTier::NotAssessed`

Add a variant to `ThreatTier` (`src/db/models.rs:141`), **outside** the ordered
Low→High scale.

- `ThreatTier::from_score` stays score-only and never returns `NotAssessed`. The
  variant is constructed exclusively at the coverage gate. (If `from_score` matches
  exhaustively on an enum with no catch-all, it needs an explicit
  `unreachable!()`/documented arm — implementer confirms.)
- The Rust compiler will flag all ~29 `ThreatTier::` match sites across 6 files
  (`scoring/threat.rs`, `db/models.rs`, `db/queries.rs`, `db/postgres.rs`,
  `scoring/profile.rs`, `discovery/threat_expansion.rs`). Each must make a
  deliberate decision — this compiler-enforced exhaustiveness is the point: it
  prevents any consumer from silently treating a not-assessed account as benign,
  which is the exact failure mode being fixed.
- Ordering/comparison: `NotAssessed` is not part of threat ranking. Sorting and
  "worst tier" logic must exclude it (treat as absent, not as lowest). Reports list
  it as its own bucket: "Not assessed (unsupported language): N".
- Persistence: `NotAssessed` needs a string form (`as_str`) for
  `account_scores.threat_tier`. No schema migration — `threat_tier` is a free-text
  column in both backends.

### Component 5a — persistence read path (critical)

The tier is **not read back from the stored `threat_tier` column** today. Every read
site *recomputes* it from the stored score so threshold changes take effect without
rescanning (`queries.rs:210, 571, 613, 702`, plus the Postgres mirror in
`postgres.rs`):

```rust
let threat_tier = threat_score.map(|s| ThreatTier::from_score(s).to_string());
```

Left as-is, this silently re-creates the bug at the persistence layer: a stored
`NotAssessed` row whose `threat_score` is `0` would be recomputed to `Low` on read.
Two coupled requirements:

1. **Store `NotAssessed` with `threat_score = NULL`, not `0`.** A null score is
   semantically "no score", distinct from a computed zero, and prevents any
   score-derived recomputation from inventing a tier.
2. **Every read site must preserve a stored `NotAssessed` instead of recomputing:**

   ```rust
   let stored_tier: Option<String> = row.get(<tier_col>)?;
   let threat_tier = match stored_tier.as_deref() {
       Some(s) if s == ThreatTier::NotAssessed.as_str() => Some(s.to_string()),
       _ => threat_score.map(|s| ThreatTier::from_score(s).to_string()),
   };
   ```

   This keeps the "recompute real tiers from score" behaviour and carves out the one
   tier that is not score-derived. Applies to all four SQLite sites and the Postgres
   equivalent.

### Component 5b — report/query filters must not hide NotAssessed

Some read queries filter `WHERE threat_score >= ?` (e.g. `queries.rs:199`). With a
NULL score, `NULL >= 0` is unknown, so `NotAssessed` rows are excluded — acceptable
for a "threats above X" query, but the main report and dashboard "all accounts"
surfaces must **not** rely on that filter, or abstained accounts vanish (the
invisibility this design explicitly rejects). Implementer audits each read path:
score-threshold queries may exclude `NotAssessed`; the primary listing must include
it as its own bucket.

### Surfacing

- **Markdown report:** dedicated "Not assessed (unsupported language)" section with
  count and account handles. Not folded into Low.
- **Web dashboard:** a distinct bucket/badge alongside the tier counts, so an
  operator sees "7 accounts not assessed" rather than an inflated Low count.
  (Uses existing tier-rendering seam in the SvelteKit UI; invoke the Svelte skills
  when editing `.svelte`.)

## Data flow (end to end)

```
getAuthorFeed record ──► Post { text, langs, .. }        (Component 2)
                              │
                     assess_language(text, langs)          (Component 1)
                              │
              ┌───────────────┴────────────────┐
        Assessable                        Unassessable
              │                                 │
    classify_batch_with_contexts          (dropped — not classified,
              │                             no ONNX/CoPE-B cost)
    compute_reply_weighted_toxicity
      (assessable-only denominator)
              │
       assessable count ≥ 5 ? ──── no ──► ThreatTier::NotAssessed   (Components 4,5)
              │ yes
       threat score ──► ThreatTier::from_score ──► Low/Watch/Elevated/High
```

## Error handling

- Missing `langs` is normal (6%), not an error — handled by the fallback rows.
- Malformed/unparseable record `langs` → treat as empty and fall back to script.
- `assess_language` is total (never panics, never errors): every `(text, langs)`
  maps to exactly one `Assessability`.
- No behavioural change for English-only accounts: all posts Assessable, coverage
  gate passes, identical scoring path as today.

## Testing

- **Unit — `assess_language`:** one test per decision-table row, plus multi-lang
  declaration, empty text, emoji-only, and `en-US` region-subtag normalisation.
- **Regression fixture:** promote `examples/lang_gate_probe.rs`'s parallel-translation
  samples (with the positive/negative/gibberish controls) into a checked-in test
  asserting the two regimes hold, so a future model swap that changes this is caught.
- **Coverage-gate boundary:** 4 assessable → NotAssessed; 5 assessable → scored.
- **Composition:** mixed-language account scored on its English subset with the
  correct (reduced) denominator; majority-non-English account → NotAssessed.
- **Exhaustiveness:** a test (or the compile itself) confirming `NotAssessed` is
  excluded from tier ordering / "worst tier" selection.
- Full suite green: `cargo test --features web`; clippy clean.

## Rollout

- New branch `feat/language-abstention` off `staging`. Never commit to `staging`
  directly; merge via the feat → staging flow.
- No data migration required for existing scored rows. Existing non-English accounts
  currently sitting at Low will be re-labelled `NotAssessed` on their next scan
  (staleness-driven), not retroactively.
- `examples/lang_gate_probe.rs` is already written (untracked) and carries the
  measurement evidence; it lands with this branch.
```
