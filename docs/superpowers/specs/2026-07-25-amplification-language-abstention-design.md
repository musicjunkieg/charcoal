# Design: Abstain from NLI context scoring on unassessable-language pairs (#230)

Status: approved (brainstorm 2026-07-25)
Issue: #230 — follow-up to #222
Branch: `feat/amplification-language-abstention` (from `staging`)
Predecessor spec: `docs/superpowers/specs/2026-07-20-language-abstention-design.md`

## Problem

#222 stopped the English-only toxicity models from silently scoring non-English
*accounts* as benign. It did not touch the NLI contextual scorer, which runs on
a different set of seams and is equally English-only:
`nli-deberta-v3-xsmall` is MNLI-trained, and all five hypothesis templates in
`HYPOTHESES` (`src/scoring/nli.rs:74`) are English sentences. Feeding it a
non-English premise produces a cross-lingual entailment judgment the model was
never trained to make.

### The four affected sites

| Site | Model | Persisted to | Feeds threat score |
|---|---|---|---|
| `pipeline/amplification.rs:117` — `score_with_context` | two-stage ONNX/CoPE | nothing | no |
| `pipeline/amplification.rs:143` — `nli.score_pair` | DeBERTa NLI | `amplification_events.context_score` | no |
| `scoring/profile.rs:592` — Mode A, direct pairs | DeBERTa NLI | `account_scores.context_score` | **yes** |
| `scoring/profile.rs:686` — Mode B, inferred pairs | DeBERTa NLI | `account_scores.context_score` | **yes** |

Two corrections to the #230 kickoff note, both established by reading the code:

1. **`quote_toxicity` is display-only.** It is computed at `:117`, rendered into
   the progress line at `:217`, and discarded. `NewAmplificationEvent`
   (`src/db/models.rs:196`) has no such field and no schema column exists. The
   kickoff note's premise — that an English model scores non-English quote text
   and the result silently counts as benign — does not hold on the toxicity
   path. What it produces is a misleading `[tox: 0.00]` in terminal output.
2. **There are three NLI seams, not two.** `profile.rs:686` (Mode B, follower
   inferred pairs) was not in the kickoff note. It writes the same
   `account_scores.context_score` and feeds the same multiplier.

### Consequence chain (the actual bug)

```
non-English text reaches score_pair
  → DeBERTa returns a cross-lingual entailment score (noise)
  → avg_context_score folds it into context_score
  → compute_threat_score_contextual applies 1.0 + context_score * 0.5
  → threat score inflated by up to 1.5x
```

The multiplier is bounded below at 1.0 (`src/scoring/threat.rs:73`), and a
zero-toxicity account stays at zero regardless of context. So the error is
**inflation only** — noise can push an account toward Watch/Elevated but can
never suppress a real threat. This is a bounded false-positive bug, the mirror
image of #222's false-negative.

### Exposed population

Narrower than #222's, because #222 already covers part of the ground:

- An amplifier whose own posts are unassessable is already `NotAssessed` with a
  NULL score, so context is moot for them.
- In Mode B, `partition_assessable` runs at `profile.rs:133`, before
  `all_post_texts` is built at `:149` — so the follower's side of an inferred
  pair is already language-filtered.

What remains ungated on every path is the **protected user's own post text**,
which is never language-filtered, plus the amplifier text in Mode A pairs. The
live cases are therefore: an account with assessable English posts who quotes
the protected user in another language, and any non-English protected user on
the hosted instance (charcoal.watch is open to all Bluesky users).

## Non-Goals

- **No schema change.** No new columns, no migration, no schema v11.
- **No `ThreatTier::NotAssessed` for events.** An amplification event is one
  post, not a population — there is no coverage ratio to gate on, so #222's
  account-level tier concept does not map.
- **No new UI or report surfacing.** The event-level `context_score` is already
  never read back; a NULL there is indistinguishable from "NLI disabled" and
  that is acceptable.
- **No change to event recording.** Events still persist with full text
  evidence. Only *scoring* abstains.
- **No change to pile-on or behavioral signals.** Those are distinct-DID counts
  over a 24-hour window, text-independent and unaffected.
- **No `langs` plumbing.** See "Language signal" below for why script-only is
  the accepted trade.

## Design

### Component 1 — `pair_is_assessable` (new pure function)

In `src/scoring/language.rs`, beside the existing `assess_language`:

```rust
/// Whether an NLI text pair can be scored by our English-only cross-encoder.
///
/// Both sides matter: `score_pair` builds a combined premise
/// ("Original: {a} Response: {b}") and tests it against English hypothesis
/// templates, so either side being non-English makes the entailment judgment
/// unreliable.
pub fn pair_is_assessable(original: &str, response: &str) -> bool {
    assess_language(original, &[]) == Assessability::Assessable
        && assess_language(response, &[]) == Assessability::Assessable
}
```

Returns `bool`, not `Assessability`: the pair case has exactly two outcomes and
no caller needs to know which side failed — it abstains either way.
`assess_language` keeps returning the enum, unchanged from #222.

### Component 2 — the gate lives inside `score_pair`

`NliScorer::score_pair` changes signature:

```rust
// before
pub async fn score_pair(&self, original_text: &str, response_text: &str)
    -> Result<(f64, HypothesisScores)>

// after
pub async fn score_pair(&self, original_text: &str, response_text: &str)
    -> Result<Option<(f64, HypothesisScores)>>
```

with the guard as the first statement, before any model work:

```rust
if !pair_is_assessable(original_text, response_text) {
    return Ok(None);
}
```

Contract:

| Return | Meaning | Caller behavior |
|---|---|---|
| `Ok(Some((score, scores)))` | scored normally | push to `pair_scores`, write audit entry — unchanged |
| `Ok(None)` | abstained, text unassessable | skip pair, `debug!`, no audit entry |
| `Err(e)` | inference failed | `warn!` — unchanged |

**Why inside the scorer rather than at each call site.** #230 exists because a
seam was missed — and the investigation for this spec found a *fourth* one
(`profile.rs:686`) that the issue's own kickoff note had also missed. A gate
that callers must remember to invoke has now demonstrably failed twice. Putting
it inside `score_pair` makes the invariant structural: every current and future
caller is gated by construction. The cost is that `NliScorer` becomes
language-policy-aware, mixing two concerns. That is accepted. The distinct
`Ok(None)` variant is what keeps the cost bounded — abstention stays
distinguishable from failure in logs, which a plain `Result<...>` collapse would
have destroyed.

### Component 3 — language signal is script-only

Both NLI seams call `assess_language(text, &[])` with empty `langs`, which falls
back to the Unicode-script heuristic (`is_nonlatin_dominant`).

The alternative was persisting `langs` on `amplification_events` (schema v11,
both backends, models, the UNNEST batch insert path, all selects) so the account
seam could apply #222's full decision table. Rejected as disproportionate: it
roughly doubles the implementation and adds two columns nothing else reads, to
close a bounded ≤1.5x false-positive on a tiny population.

**Accepted limitation, documented:** a Latin-script non-English quote declared
`pt` or `de` still receives a noise context multiplier. #222 already accepts a
comparable documented limit (a `pt` post misdeclared as `en`). Using `langs` at
the event seam only — where it is nearly free, since `fetch_post_text` already
parses the record — was also rejected: it would let the two seams reach
different verdicts on the same pair.

### Component 4 — the toxicity site

At `pipeline/amplification.rs:113-126`, gate on the **amplifier text alone**:

```rust
if assess_language(&text, &[]) == Assessability::Assessable {
    if let Some(scorer) = scorer {
        // existing score_with_context call
    }
}
amplifier_text = Some(text);   // evidence preserved regardless
```

Skip the `score_with_context` call entirely rather than calling and discarding —
that also avoids a Zentropi/RunPod round-trip per non-English quote.

The asymmetry with the NLI sites is deliberate and principled: the tox site
scores one post with the other as context, so it takes the per-post predicate
(#222's granularity); the NLI sites score a pair, so they take the pair
predicate. Same underlying `assess_language`, applied at the granularity each
model actually consumes.

Progress output renders `[tox: n/a — language]` in place of `[tox: 0.00]`. This
stays distinguishable from a non-`--analyze` scan, which prints no suffix at
all.

### Component 5 — `CHARCOAL_MODEL_DIR` consistency fix

`src/scoring/nli.rs:318` calls `default_model_dir()` directly and ignores
`CHARCOAL_MODEL_DIR`, unlike `tests/unit_scan_phases.rs:1695` which resolves the
env var first. Align it with the established pattern:

```rust
let base = std::env::var("CHARCOAL_MODEL_DIR")
    .map(std::path::PathBuf::from)
    .unwrap_or_else(|_| default_model_dir());
```

Folded in here because without it that test keeps skipping regardless of how the
model is made reachable.

## Data flow (end to end)

```
Constellation event
  → fetch_post_text (amplifier text)
  → assess_language(amp_text, &[])
      Unassessable → skip tox call, print "[tox: n/a — language]"
      Assessable   → score_with_context (unchanged)
  → nli.score_pair(orig, amp)
      Ok(None)     → context_score = None, debug!, no audit entry
      Ok(Some(..)) → context_score = Some(score) (unchanged)
  → event row persists with full text evidence either way

finalize_account
  → Mode A (direct_pairs) / Mode B (inferred pairs)
  → nli.score_pair per pair
      Ok(None)     → pair skipped, not pushed to pair_scores
  → avg_context_score(&pair_scores)
      all abstained → None → context_multiplier = 1.0
      some survived → average over survivors only
  → compute_threat_score_contextual
```

Partial coverage averaging over survivors mirrors `partition_assessable`'s
semantics from #222. Total abstention yielding `None` is the existing
`avg_context_score(&[])` behavior — no new code needed for either case.

## Error handling

- Abstention is not an error. It never propagates, never degrades a scan, never
  sets the `degraded` flag.
- `Err` from `score_pair` keeps its current meaning and current `warn!`
  handling; the new `Ok(None)` arm logs at `debug!` so abstention does not
  drown normal scan output.
- No audit-log entry is written for an abstained pair. An abstention is the
  absence of a judgment, and recording it as one would corrupt the accuracy
  metrics computed over the NLI audit JSONL.

## Testing

TDD: tests before implementation.

**Testability constraint.** `nli_scorer` is typed `Option<&NliScorer>` — a
concrete struct, not a trait — so there is no injection point for testing the
call sites without the real ONNX model.

**Verification hazard (must be fixed first).** Model-gated tests currently
*silently pass* under the Safehouse sandbox: models live in
`~/Library/Application Support/charcoal/models`, denied at the kernel level, so
`nli_files_present` reports absent and the test returns having asserted nothing.
Confirmed by running `scoring::nli::tests::batched_...`, which printed
`SKIP: NLI model not present` and reported `ok`. This already affects
`tests/unit_scan_phases.rs:1858` ("finalize amplifier-NLI case") — the exact
path this change modifies — plus the follower-NLI case at `:1753`.

Resolution: a **read-only sandbox grant** for the models directory, preferred
over copying models into the project because it fixes every model-gated test at
once, duplicates no disk, cannot drift after `charcoal download-model`, and
needs no `CHARCOAL_MODEL_DIR` plumbing in hooks. Until the grant lands, the
real-model regression test below cannot be trusted, and that must not be
mistaken for a pass.

### `tests/unit_language.rs` — pure, always runs

- both texts English → `true`
- non-Latin original, English response → `false`
- English original, non-Latin response → `false`
- both non-Latin → `false`
- emoji-only and short non-Latin strings inherit `assess_language`'s existing
  verdicts (confirms no new policy is introduced at the pair level)

### `tests/regression_language_gate.rs` — extends the #222 file

When the NLI model is reachable:

- `score_pair` on a non-Latin pair returns `Ok(None)`
- `score_pair` on an English control pair returns `Ok(Some(_))`

The control is not optional. Without it a no-op gate passes silently — the
failure this spec's own investigation just demonstrated. Skips print a visible
notice when the model is unreachable.

### Regression coverage

`tests/unit_scan_phases.rs` finalize cases must be re-run and observed to
actually execute (not skip) once the sandbox grant is in place, since they cover
Mode A and Mode B end to end.

## Rollout

Blast radius: 4 source files (`scoring/language.rs`, `scoring/nli.rs`,
`pipeline/amplification.rs`, `scoring/profile.rs`) and 2 test files. No schema,
no migration, no web/UI, no Postgres changes.

`feat/amplification-language-abstention` → PR to `staging` → validate on the
staging environment → promote to `main` with the next production PR.
