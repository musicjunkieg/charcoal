# Amplification-Pair Language Abstention (#230) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the English-only NLI cross-encoder from producing noise context scores on non-English text, which currently inflates threat scores by up to 1.5x.

**Architecture:** A pure `pair_is_assessable(original, response) -> bool` predicate in `scoring::language`, invoked *inside* `NliScorer::score_pair`, which grows `Ok(None)` to mean "abstained" while `Err` keeps meaning "inference failed". Gating inside the scorer rather than at each call site makes the invariant structural — this bug exists because call sites were missed twice. The separate toxicity print site takes the per-post predicate directly.

**Tech Stack:** Rust, `ort` 2.0.0-rc.11 (ONNX), `tokio`, `tracing`. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-07-25-amplification-language-abstention-design.md`

## Global Constraints

- **No schema change.** No new columns, no migration, no schema v11.
- **No new dependencies.** The gate uses the existing `assess_language` and its Unicode-script heuristic.
- **Language signal is script-only** at these seams: always call `assess_language(text, &[])` with empty `langs`.
- **Pair rule:** both texts must be assessable. **Toxicity rule:** amplifier text alone.
- **Abstention is not an error.** It never propagates, never sets `degraded`, never writes an audit-log entry.
- **Events still persist with full text evidence.** Only scoring abstains.
- `NotAssessed` is an account-level tier and must **not** be applied to events.
- Error direction being fixed is **inflation** (`context_multiplier = 1.0 + ctx*0.5`, bounded below at 1.0), not suppression.

## Working Agreements

- **Run tests in the FOREGROUND and wait for them.** Never launch `cargo test` in the background and yield — a stray test process races the next run against shared temp paths in `tests/unit_nli.rs`.
- **Never run two `cargo test` processes at once.**
- **Stage files explicitly by name.** Never `git add -A`, `git add .`, or `git commit -am`.
- **Never use heredocs** (`<<EOF`) — they break in zsh on this machine. Use single-quoted multi-line strings.
- Branch is `feat/amplification-language-abstention`, already created from `staging`. Do not create a worktree.
- The pre-commit hook runs fmt + clippy + tests and will reject a bad commit. That is expected; fix and retry.

## Verification Status (updated 2026-07-26)

**All model-gated tests can run, and do.** Verified in both directions: with the models reachable, `finalize_amplifier_always_runs_nli` executed the ONNX model with no SKIP notice; the same test pointed at an empty `CHARCOAL_MODEL_DIR` printed its SKIP and asserted nothing. The population can fail; it didn't. Task 3's real-model regression test has genuine coverage.

A read-only sandbox grant for `~/Library/Application Support/charcoal/models` also landed, but hazard 2 below makes it redundant — `./models` was the answer all along.

Two hazards to keep in mind:

**1. `grep "^SKIP"` on a plain `cargo test` run is a no-op.** libtest captures stderr from *passing* tests and discards it, so a SKIP notice emitted by a test that returns early never reaches the terminal. A grep for it comes back empty whether or not anything skipped — the exact false-confidence failure the check was written to prevent. **Always pass `-- --show-output`.**

**2. Test binaries do not load `.env`, so they look in the wrong models directory.** All three models (toxicity, `all-MiniLM-L6-v2/`, `nli-deberta-v3-xsmall/`) already live in **`./models` inside the project**, readable and writable, and `.env:25` sets `CHARCOAL_MODEL_DIR=./models`. But `dotenvy` runs only in `main.rs:184` — test harnesses never see it and fall back to `default_model_dir()`, the platform data dir, which holds only the NLI model.

**Run the suite as `CHARCOAL_MODEL_DIR=./models cargo test --features web` and every model-gated test executes: 40 suites, zero SKIP lines.** That is the expected result; any SKIP output is a list of things not verified. Without the env var, seven tests skip on the missing toxicity/embedding models — a harness artifact, not a code problem.

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `src/scoring/language.rs` | modify | Add `pair_is_assessable` beside `assess_language` |
| `src/toxicity/download.rs` | modify | Add `resolve_model_dir` / `resolve_model_dir_from` |
| `src/scoring/nli.rs` | modify | Gate inside `score_pair`; honor `CHARCOAL_MODEL_DIR` |
| `src/pipeline/amplification.rs` | modify | Tox-site gate, `tox_suffix` helper, event NLI call site |
| `src/scoring/profile.rs` | modify | Mode A and Mode B NLI call sites |
| `tests/unit_language.rs` | modify | `pair_is_assessable` truth table |
| `tests/unit_nli.rs` | modify | `resolve_model_dir_from` cases |
| `tests/regression_language_gate.rs` | modify | Real-model abstain + English control |
| `CHANGELOG.md` | modify | Unreleased entry |

---

### Task 1: `pair_is_assessable` predicate

**Files:**
- Modify: `src/scoring/language.rs` (append after `assess_language`, which ends at line 88)
- Test: `tests/unit_language.rs`

**Interfaces:**
- Consumes: `assess_language(text: &str, langs: &[String]) -> Assessability` and `Assessability::{Assessable, Unassessable}`, both already public in `src/scoring/language.rs`.
- Produces: `pub fn pair_is_assessable(original: &str, response: &str) -> bool` in `crate::scoring::language`. Task 3 calls this from inside `NliScorer::score_pair`.

- [ ] **Step 1: Write the failing tests**

Append to `tests/unit_language.rs`. The file already has `use charcoal::scoring::language::{assess_language, Assessability};` at line 1 — change that line to add the new import:

```rust
use charcoal::scoring::language::{assess_language, pair_is_assessable, Assessability};
```

Then append these tests at the end of the file:

```rust
// --- #230: pair-level assessability ---

#[test]
fn pair_both_english_is_assessable() {
    assert!(pair_is_assessable(
        "fat people deserve healthcare too",
        "lol imagine being that big"
    ));
}

#[test]
fn pair_nonlatin_response_is_unassessable() {
    // The response side alone is enough to poison the entailment judgment.
    assert!(!pair_is_assessable(
        "fat people deserve healthcare too",
        "お前は本当に馬鹿だ、死ね"
    ));
}

#[test]
fn pair_nonlatin_original_is_unassessable() {
    // The protected user's own post is never language-filtered upstream, so
    // this side must be checked too (a non-English protected user on the
    // hosted instance would otherwise get noise on every pair).
    assert!(!pair_is_assessable(
        "แกมันโง่ชิบหาย ไปตายซะ",
        "that is a terrible take"
    ));
}

#[test]
fn pair_both_nonlatin_is_unassessable() {
    assert!(!pair_is_assessable(
        "Ты чёртов идиот, иди убей себя",
        "お前は本当に馬鹿だ、死ね"
    ));
}

#[test]
fn pair_inherits_assess_language_verdicts_no_new_policy() {
    // Emoji count as neither script, and short non-Latin runs sit below
    // MIN_NONLATIN_CHARS — both are Assessable per assess_language. The pair
    // predicate must not invent stricter rules of its own.
    assert_eq!(assess_language("🎉🎉🎉", &[]), Assessability::Assessable);
    assert!(pair_is_assessable("🎉🎉🎉", "congrats on the news"));

    assert_eq!(assess_language("ok です", &[]), Assessability::Assessable);
    assert!(pair_is_assessable("ok です", "glad to hear it"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test unit_language 2>&1 | tail -20`

Expected: FAIL to compile — `error[E0432]: unresolved import charcoal::scoring::language::pair_is_assessable`.

- [ ] **Step 3: Write the implementation**

Append to `src/scoring/language.rs`, immediately after `assess_language` (which ends at line 88) and before the `use crate::bluesky::posts::{...}` line at line 90:

```rust
/// Whether an NLI text pair can be scored by our English-only cross-encoder.
///
/// Both sides matter (#230): `NliScorer::score_pair` builds a combined premise
/// ("Original: {a} Response: {b}") and tests it against English hypothesis
/// templates, so either side being non-English makes the entailment judgment
/// noise rather than a weak signal.
///
/// Returns `bool` rather than [`Assessability`] because the pair case has
/// exactly two outcomes and no caller needs to know which side failed — it
/// abstains either way.
///
/// Called with empty `langs` at both NLI seams: the account-side pairs are
/// re-derived from stored event text, which carries no `langs`, so the Unicode
/// script heuristic is the only signal available on both sides consistently.
pub fn pair_is_assessable(original: &str, response: &str) -> bool {
    assess_language(original, &[]) == Assessability::Assessable
        && assess_language(response, &[]) == Assessability::Assessable
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test unit_language 2>&1 | tail -20`

Expected: PASS, `test result: ok.` with 5 more tests than before (18 → 23).

- [ ] **Step 5: Format and commit**

```bash
cargo fmt
git add src/scoring/language.rs tests/unit_language.rs
git commit -m 'feat(230): pair_is_assessable predicate for NLI text pairs

Both sides of an NLI pair must be assessable: score_pair builds a combined
premise and tests it against English hypothesis templates, so either side
being non-English makes the entailment judgment noise.

Returns bool rather than Assessability — the pair case has two outcomes and
no caller needs to know which side failed.

Refs #230'
```

---

### Task 2: `resolve_model_dir` helper

**Files:**
- Modify: `src/toxicity/download.rs` (append after `default_model_dir`, which ends at line 42)
- Modify: `src/scoring/nli.rs:316-322`
- Test: `tests/unit_nli.rs`

**Interfaces:**
- Consumes: `default_model_dir() -> PathBuf` (already public, `src/toxicity/download.rs:37`).
- Produces:
  - `pub fn resolve_model_dir_from(override_dir: Option<String>) -> PathBuf` — pure, testable.
  - `pub fn resolve_model_dir() -> PathBuf` — thin wrapper reading `CHARCOAL_MODEL_DIR`.
  Task 3's regression test calls `resolve_model_dir()`.

**Why the split:** `std::env::set_var` is racy across Rust's parallel test threads and is `unsafe` in newer editions. Taking the override as a parameter keeps the logic testable without touching process environment.

- [ ] **Step 1: Write the failing tests**

Append to `tests/unit_nli.rs`:

```rust
// --- #230: model dir resolution ---

#[test]
fn resolve_model_dir_uses_explicit_override() {
    let resolved = charcoal::toxicity::download::resolve_model_dir_from(Some(
        "/tmp/charcoal-models-override".to_string(),
    ));
    assert_eq!(
        resolved,
        std::path::PathBuf::from("/tmp/charcoal-models-override")
    );
}

#[test]
fn resolve_model_dir_falls_back_when_unset() {
    let resolved = charcoal::toxicity::download::resolve_model_dir_from(None);
    assert_eq!(resolved, charcoal::toxicity::download::default_model_dir());
}

#[test]
fn resolve_model_dir_treats_blank_override_as_unset() {
    // An exported-but-empty CHARCOAL_MODEL_DIR must not resolve to "" and send
    // every model lookup to the filesystem root.
    for blank in ["", "   "] {
        assert_eq!(
            charcoal::toxicity::download::resolve_model_dir_from(Some(blank.to_string())),
            charcoal::toxicity::download::default_model_dir(),
            "blank override {blank:?} should fall back"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test unit_nli resolve_model_dir 2>&1 | tail -20`

Expected: FAIL to compile — `error[E0425]: cannot find function resolve_model_dir_from`.

- [ ] **Step 3: Write the implementation**

Append to `src/toxicity/download.rs`, immediately after `default_model_dir` (ends line 42):

```rust
/// Resolve the model directory from an explicit override, falling back to the
/// platform default. A blank override is treated as unset — an exported-but-
/// empty `CHARCOAL_MODEL_DIR` must not send every lookup to the filesystem root.
///
/// Takes the override as a parameter rather than reading the environment so it
/// stays pure and testable: `std::env::set_var` races Rust's parallel test
/// threads.
pub fn resolve_model_dir_from(override_dir: Option<String>) -> PathBuf {
    override_dir
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_model_dir)
}

/// [`resolve_model_dir_from`] reading `CHARCOAL_MODEL_DIR`.
pub fn resolve_model_dir() -> PathBuf {
    resolve_model_dir_from(std::env::var("CHARCOAL_MODEL_DIR").ok())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test unit_nli resolve_model_dir 2>&1 | tail -20`

Expected: PASS, `test result: ok. 3 passed`.

- [ ] **Step 5: Use it in the existing model-gated test**

In `src/scoring/nli.rs`, change the import at line 306 from:

```rust
    use crate::toxicity::download::{default_model_dir, nli_files_present};
```

to:

```rust
    use crate::toxicity::download::{nli_files_present, resolve_model_dir};
```

and change line 318 from:

```rust
        let base = default_model_dir();
```

to:

```rust
        let base = resolve_model_dir();
```

This makes the #213 batching test honor `CHARCOAL_MODEL_DIR` the same way `tests/unit_scan_phases.rs:1695` already does, instead of staying the odd one out.

- [ ] **Step 6: Run the touched test to verify it still behaves**

Run: `cargo test --lib scoring::nli::tests::batched -- --nocapture 2>&1 | tail -10`

Expected: PASS, and **no** `SKIP: NLI model not present at ...` line — the grant has landed, so the model is reachable and the test should genuinely run. A SKIP here means the grant regressed.

- [ ] **Step 7: Format and commit**

```bash
cargo fmt
git add src/toxicity/download.rs src/scoring/nli.rs tests/unit_nli.rs
git commit -m 'refactor(230): resolve_model_dir honors CHARCOAL_MODEL_DIR

src/scoring/nli.rs called default_model_dir() directly, ignoring the env var
that tests/unit_scan_phases.rs already honors. Adds a pure
resolve_model_dir_from(Option<String>) plus an env-reading wrapper, and uses
it in the #213 batching test so it stops being the odd one out.

Blank override treated as unset so an exported-but-empty CHARCOAL_MODEL_DIR
does not resolve every lookup to the filesystem root.

Refs #230'
```

---

### Task 3: Gate inside `score_pair` and update all three call sites

**Files:**
- Modify: `src/scoring/nli.rs:256-300`
- Modify: `src/pipeline/amplification.rs:14` (import), `:141-186` (call site)
- Modify: `src/scoring/profile.rs:12` (import), `:592-633` (Mode A), `:686-727` (Mode B)
- Test: `tests/regression_language_gate.rs`

**Interfaces:**
- Consumes: `crate::scoring::language::pair_is_assessable` (Task 1), `charcoal::toxicity::download::resolve_model_dir` (Task 2).
- Produces: `NliScorer::score_pair(&self, original_text: &str, response_text: &str) -> Result<Option<(f64, HypothesisScores)>>`. Contract: `Ok(Some(..))` scored, `Ok(None)` abstained, `Err` inference failed. No other task depends on this.

**This task is atomic by necessity** — changing the signature breaks all three call sites, so they must move together to compile.

- [ ] **Step 1: Write the failing regression test**

Append to `tests/regression_language_gate.rs`:

```rust
// --- #230: the gate inside score_pair, against the real model ---

/// Requires the NLI model to be readable at `CHARCOAL_MODEL_DIR` (or the
/// platform default). If it is not, this SKIPS and asserts nothing — a green
/// run is then not evidence the gate works. Under the Safehouse sandbox the
/// models live outside the project directory and need an explicit read
/// grant; as of 2026-07-26 that grant is in place, so this test should
/// genuinely run. Check with `-- --show-output`: libtest swallows the SKIP
/// notice on a plain run.
#[tokio::test]
async fn score_pair_abstains_on_nonlatin_but_still_scores_english() {
    let base = charcoal::toxicity::download::resolve_model_dir();
    if !charcoal::toxicity::download::nli_files_present(&base) {
        eprintln!(
            "SKIP: NLI model not reachable at {} — THIS TEST ASSERTED NOTHING. \
             Run `charcoal download-model`, and ensure the path is readable \
             from the sandbox.",
            base.display()
        );
        return;
    }
    let scorer = charcoal::scoring::nli::NliScorer::load(&base).expect("load NLI model");

    // Non-Latin response → abstain.
    let abstained = scorer
        .score_pair("fat people deserve healthcare too", "お前は本当に馬鹿だ、死ね")
        .await
        .expect("abstention is not an error");
    assert!(
        abstained.is_none(),
        "non-Latin response must abstain, got {abstained:?}"
    );

    // Non-Latin original → abstain. The protected user's own text is never
    // language-filtered upstream, so this side must be gated too.
    let abstained_orig = scorer
        .score_pair("แกมันโง่ชิบหาย ไปตายซะ", "that is a terrible take")
        .await
        .expect("abstention is not an error");
    assert!(
        abstained_orig.is_none(),
        "non-Latin original must abstain, got {abstained_orig:?}"
    );

    // CONTROL — an all-English pair MUST still score. Without this assertion a
    // gate that abstained on everything would pass silently, which is exactly
    // the failure mode that motivated writing it down.
    let scored = scorer
        .score_pair(
            "fat people deserve healthcare too",
            "lol imagine being that big",
        )
        .await
        .expect("English pair must not error");
    let (score, _hypotheses) = scored.expect("English pair must score, not abstain");
    assert!(
        (0.0..=1.0).contains(&score),
        "hostility score out of range: {score}"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test regression_language_gate 2>&1 | tail -20`

Expected: FAIL to compile — `expected Result<(f64, HypothesisScores)>` has no method `is_none`, i.e. `score_pair` does not yet return an `Option`.

- [ ] **Step 3: Add the guard inside `score_pair`**

In `src/scoring/nli.rs`, add to the imports near line 26:

```rust
use crate::scoring::language::pair_is_assessable;
```

Change the signature at lines 256-260 from:

```rust
    pub async fn score_pair(
        &self,
        original_text: &str,
        response_text: &str,
    ) -> Result<(f64, HypothesisScores)> {
        let premise = format!("Original: {} Response: {}", original_text, response_text);
```

to:

```rust
    pub async fn score_pair(
        &self,
        original_text: &str,
        response_text: &str,
    ) -> Result<Option<(f64, HypothesisScores)>> {
        // #230: the HYPOTHESES below are English sentences and this cross-encoder
        // is MNLI-trained, so a non-English side yields a cross-lingual
        // entailment judgment that is noise, not weak signal. Left ungated it
        // inflates threat scores by up to 1.5x via context_multiplier.
        //
        // The gate lives HERE rather than at each call site because two call
        // sites were missed before — inside the scorer, every current and future
        // caller is gated by construction.
        if !pair_is_assessable(original_text, response_text) {
            return Ok(None);
        }

        let premise = format!("Original: {} Response: {}", original_text, response_text);
```

Change the return at line 299 from:

```rust
        Ok((hostility, hypothesis_scores))
```

to:

```rust
        Ok(Some((hostility, hypothesis_scores)))
```

- [ ] **Step 4: Update call site 1 — the event seam**

In `src/pipeline/amplification.rs`, change line 14 from:

```rust
use tracing::{info, warn};
```

to:

```rust
use tracing::{debug, info, warn};
```

Then in the `context_score` match starting at line 141, change the `Ok(...)` arm header at line 144 from:

```rust
                    Ok((score, hypothesis_scores)) => {
```

to:

```rust
                    Ok(Some((score, hypothesis_scores))) => {
```

and insert a new arm immediately before the `Err(e) =>` arm at line 179:

```rust
                    Ok(None) => {
                        debug!(
                            handle = event.amplifier_handle,
                            "Skipped NLI for event pair: unassessable language"
                        );
                        None
                    }
```

Leave the `Err(e)` arm exactly as it is. Do not write an audit-log entry for the abstained case — an abstention is the absence of a judgment, and recording it as one would corrupt the accuracy metrics computed over the NLI audit JSONL.

- [ ] **Step 5: Update call sites 2 and 3 — Mode A and Mode B**

In `src/scoring/profile.rs`, change line 12 from:

```rust
use tracing::{info, warn};
```

to:

```rust
use tracing::{debug, info, warn};
```

**Mode A**, at line 592: change

```rust
                    match nli.score_pair(original, response).await {
                        Ok((score, hypothesis_scores)) => {
```

to

```rust
                    match nli.score_pair(original, response).await {
                        Ok(Some((score, hypothesis_scores))) => {
```

and insert a new arm immediately before the `Err(e) =>` arm at line 630:

```rust
                        Ok(None) => {
                            debug!(
                                target_did = target_did,
                                pair_type = "direct",
                                "Skipped NLI pair: unassessable language"
                            );
                        }
```

**Mode B**, at line 686: change

```rust
                            match nli.score_pair(original, target_text).await {
                                Ok((score, hypothesis_scores)) => {
```

to

```rust
                            match nli.score_pair(original, target_text).await {
                                Ok(Some((score, hypothesis_scores))) => {
```

and insert a new arm immediately before the `Err(e) =>` arm at line 724:

```rust
                                Ok(None) => {
                                    debug!(
                                        target_did = target_did,
                                        pair_type = "inferred",
                                        "Skipped NLI pair: unassessable language"
                                    );
                                }
```

In both modes the abstained pair is simply never pushed to `pair_scores`. `avg_context_score` then averages over the survivors, and returns `None` when all pairs abstained — which yields `context_multiplier = 1.0`. No further change is needed for either case.

- [ ] **Step 6: Verify it compiles and the whole suite passes**

Run, in the foreground, waiting for completion:

`cargo test --features web 2>&1 | tail -30`

Expected: compiles clean, `test result: ok.` across all suites.

Then confirm the new regression test actually ran rather than skipping:

`cargo test --features web --test regression_language_gate -- --show-output 2>&1 | grep -iE "^\s*SKIP"`

Expected: **no output.** The grant has landed, so the NLI model is reachable and this test must genuinely execute. Any SKIP here means the real-model coverage is absent.

- [ ] **Step 7: Check clippy**

Run: `cargo clippy --features web --all-targets 2>&1 | tail -20`

Expected: no warnings.

- [ ] **Step 8: Format and commit**

```bash
cargo fmt
git add src/scoring/nli.rs src/pipeline/amplification.rs src/scoring/profile.rs tests/regression_language_gate.rs
git commit -m 'fix(230): abstain from NLI scoring on unassessable-language pairs

score_pair now returns Result<Option<(f64, HypothesisScores)>>: Ok(None)
means abstained, Err keeps meaning inference failed. The gate lives inside
the scorer so every caller is covered by construction — this bug existed
because call sites were missed, including profile.rs:686 (Mode B inferred
pairs), which the issue kickoff note did not list.

Left ungated, an English-only MNLI cross-encoder fed non-English text
returns noise that context_multiplier (1.0 + ctx*0.5) turns into up to a
1.5x threat-score inflation. Bounded false positive — mirror image of #222.

Abstained pairs are not pushed to pair_scores, so avg_context_score averages
survivors only and returns None when all abstain (multiplier 1.0). No audit
entry is written for an abstention.

Refs #230'
```

---

### Task 4: Toxicity site gate and progress rendering

**Files:**
- Modify: `src/pipeline/amplification.rs:95-138` (gate), `:215-221` (render), plus a new inline test module at end of file
- Test: inline `#[cfg(test)] mod tests` in `src/pipeline/amplification.rs`

**Interfaces:**
- Consumes: `crate::scoring::language::{assess_language, Assessability}`.
- Produces: `fn tox_suffix(quote_toxicity: Option<f64>, assessable: bool) -> String` — module-private, tested inline. Nothing outside this file depends on it.

**Context:** `quote_toxicity` here is display-only — computed, printed, discarded. `NewAmplificationEvent` has no such field and no column exists. So this task is not fixing a scoring bug; it is stopping a misleading `[tox: 0.00]` from being printed for text the model cannot read, and avoiding a pointless Zentropi/RunPod round-trip per non-English quote.

The helper is tested inline rather than from `tests/` because it is module-private, matching the existing in-crate test module in `src/scoring/nli.rs:303`.

- [ ] **Step 1: Write the failing tests**

Append to the very end of `src/pipeline/amplification.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::tox_suffix;

    #[test]
    fn scored_text_renders_the_score() {
        assert_eq!(tox_suffix(Some(0.42), true), " [tox: 0.42]");
    }

    #[test]
    fn unscored_assessable_text_renders_nothing() {
        // No `--analyze`: there was no scorer, so there is nothing to say.
        assert_eq!(tox_suffix(None, true), "");
    }

    #[test]
    fn unassessable_text_renders_the_language_marker() {
        // Distinguishable from both "[tox: 0.00]" (a real benign score) and ""
        // (not scored at all) — the reader can tell we looked and declined.
        assert_eq!(tox_suffix(None, false), " [tox: n/a — language]");
    }

    #[test]
    fn a_real_score_wins_over_the_assessable_flag() {
        // Defensive: if a score somehow exists, show it rather than claiming
        // we abstained.
        assert_eq!(tox_suffix(Some(0.0), false), " [tox: 0.00]");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib pipeline::amplification 2>&1 | tail -20`

Expected: FAIL to compile — `error[E0432]: unresolved import super::tox_suffix`.

- [ ] **Step 3: Add the helper**

Insert into `src/pipeline/amplification.rs`, immediately before the `pub async fn run(` declaration at line 46:

```rust
/// Render the toxicity suffix for an amplification-event progress line.
///
/// Three states, deliberately distinguishable in output (#230):
/// - scored                  → `" [tox: 0.42]"`
/// - not scored, assessable  → `""` (no `--analyze`; nothing to report)
/// - not scored, unassessable → `" [tox: n/a — language]"`
///
/// The third case previously printed `[tox: 0.00]`, which read as "we checked
/// and it was benign" when the truth was "an English-only model was handed text
/// it cannot read".
fn tox_suffix(quote_toxicity: Option<f64>, assessable: bool) -> String {
    match (quote_toxicity, assessable) {
        (Some(t), _) => format!(" [tox: {:.2}]", t),
        (None, true) => String::new(),
        (None, false) => " [tox: n/a — language]".to_string(),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib pipeline::amplification 2>&1 | tail -20`

Expected: PASS, `test result: ok. 4 passed`.

- [ ] **Step 5: Wire the gate into the event loop**

In `src/pipeline/amplification.rs`, add to the imports near line 22:

```rust
use crate::scoring::language::{assess_language, Assessability};
```

At lines 96-97, change:

```rust
        let mut amplifier_text: Option<String> = None;
        let mut quote_toxicity: Option<f64> = None;
```

to:

```rust
        let mut amplifier_text: Option<String> = None;
        let mut quote_toxicity: Option<f64> = None;
        // Defaults to true so events with no fetched text — reposts, likes, and
        // fetch failures — render exactly as they did before. The language
        // marker is reserved for text we actually looked at and declined to
        // score.
        let mut amplifier_assessable = true;
```

Then replace the `Ok(Some(text)) => { ... }` arm at lines 114-127 with:

```rust
                Ok(Some(text)) => {
                    // #230: never hand non-English text to the English-only
                    // toxicity models. Skipping the call rather than scoring and
                    // discarding also avoids a Zentropi/RunPod round-trip per
                    // non-English quote.
                    //
                    // Gated on the amplifier text ALONE, not the pair: this
                    // scores one post with the other as context, so it takes the
                    // per-post predicate. The NLI seams take the pair predicate.
                    amplifier_assessable =
                        assess_language(&text, &[]) == Assessability::Assessable;
                    if amplifier_assessable {
                        // Score only when a real scorer is present (i.e. `--analyze`).
                        if let Some(scorer) = scorer {
                            match scorer.score_with_context(&text, original_post_text).await {
                                Ok(result) => {
                                    quote_toxicity = Some(result.toxicity);
                                }
                                Err(e) => {
                                    warn!(error = %e, "Failed to score amplifier text");
                                }
                            }
                        }
                    } else {
                        debug!(
                            uri = event.amplifier_post_uri,
                            "Skipped toxicity scoring: unassessable language"
                        );
                    }
                    amplifier_text = Some(text);
                }
```

The text is still assigned to `amplifier_text` on every path, so the event persists with full evidence either way.

- [ ] **Step 6: Use the helper at the render site**

At lines 215-221, change:

```rust
        if let Some(ref text) = amplifier_text {
            let preview = crate::output::truncate_chars(text, 120);
            let tox_str = quote_toxicity
                .map(|t| format!(" [tox: {:.2}]", t))
                .unwrap_or_default();
            crate::progress!("    \"{}\"{}", preview, tox_str);
        }
```

to:

```rust
        if let Some(ref text) = amplifier_text {
            let preview = crate::output::truncate_chars(text, 120);
            let tox_str = tox_suffix(quote_toxicity, amplifier_assessable);
            crate::progress!("    \"{}\"{}", preview, tox_str);
        }
```

- [ ] **Step 7: Verify the full suite and clippy**

Run, in the foreground, one at a time:

`cargo test --features web 2>&1 | tail -30`

Expected: `test result: ok.` across all suites.

`cargo clippy --features web --all-targets 2>&1 | tail -20`

Expected: no warnings.

- [ ] **Step 8: Format and commit**

```bash
cargo fmt
git add src/pipeline/amplification.rs
git commit -m 'fix(230): abstain from toxicity scoring on unassessable amplifier text

quote_toxicity here is display-only — computed, printed, discarded; there is
no column for it. So this is not a scoring fix: it stops a misleading
"[tox: 0.00]" from being printed for text an English-only model cannot read,
and skips a pointless Zentropi/RunPod round-trip per non-English quote.

New tox_suffix helper renders three distinguishable states: scored, not
scored (no --analyze), and abstained on language. Gated on the amplifier
text alone — per-post predicate for per-post scoring, where the NLI seams
take the pair predicate.

Refs #230'
```

---

### Task 5: Changelog and final verification

**Files:**
- Modify: `CHANGELOG.md`

**Interfaces:**
- Consumes: everything from Tasks 1-4. Produces nothing consumed downstream.

- [ ] **Step 1: Add the changelog entry**

In `CHANGELOG.md`, under `## [Unreleased]`, add a `### Fixed` section immediately above the existing `### Changed` section at line 9 (create the heading if it is not already there):

```markdown
### Fixed
- Abstain from NLI context scoring on unassessable-language pairs (#230) —
  the English-only MNLI cross-encoder returned noise on non-English text,
  which `context_multiplier` turned into up to a 1.5x threat-score inflation.
  The gate now lives inside `score_pair`, so all three NLI seams are covered,
  including the Mode B inferred-pair path. Amplifier toxicity scoring abstains
  on the same basis and the progress line reports `[tox: n/a — language]`
  instead of a misleading `[tox: 0.00]`.
```

- [ ] **Step 2: Run the full suite one final time**

Run, in the foreground: `cargo test --features web 2>&1 | tail -30`

Expected: `test result: ok.` across all suites.

- [ ] **Step 3: Confirm which model-gated tests skipped**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -iE "^\s*SKIP" | sort -u`

Both parts matter. `CHARCOAL_MODEL_DIR=./models` points the harness at the models that are actually on disk (test binaries do not load `.env`). `--show-output` is required because libtest discards stderr from passing tests, so without it the grep returns empty regardless — reading as "nothing skipped" when tests may have skipped en masse.

Expected: **no output at all.** Every model-gated test runs. **Record that in the PR description.** Any SKIP line is a list of things not verified; if `score_pair_abstains_on_nonlatin_but_still_scores_english` appears, the real-model coverage does not exist and the PR must say so rather than claiming the gate is verified end to end.

- [ ] **Step 4: Clippy across all feature combinations**

Run each in the foreground, one at a time:

```bash
cargo clippy --all-targets 2>&1 | tail -10
cargo clippy --features web --all-targets 2>&1 | tail -10
cargo clippy --features postgres --all-targets 2>&1 | tail -10
```

Expected: no warnings from any of the three.

- [ ] **Step 5: Commit and push**

```bash
cargo fmt
git add CHANGELOG.md
git commit -m 'docs(230): changelog for amplification-pair language abstention

Refs #230'
git push origin feat/amplification-language-abstention
```

- [ ] **Step 6: Report status**

Report to Bryan:
- Which tasks completed and their commit hashes.
- The exact list of SKIPped model-gated tests from Step 3.
- Whether the sandbox grant landed. If not, state plainly that the real-model regression test asserted nothing and the gate is verified only by the pure predicate tests plus compilation.

---

## Self-Review

**Spec coverage:**

| Spec section | Task |
|---|---|
| Component 1 — `pair_is_assessable` | Task 1 |
| Component 2 — gate inside `score_pair` | Task 3 |
| Component 3 — script-only signal | Tasks 1, 3, 4 (all call `assess_language(text, &[])`) |
| Component 4 — toxicity site + progress string | Task 4 |
| Component 5 — `CHARCOAL_MODEL_DIR` | Task 2 |
| Data flow — abstain paths | Tasks 3, 4 |
| Error handling — abstention is not an error | Task 3 Steps 3-5 (no audit entry, `debug!` not `warn!`) |
| Testing — `unit_language.rs` truth table | Task 1 Step 1 |
| Testing — `regression_language_gate.rs` + control | Task 3 Step 1 |
| Testing — verification hazard documented | "Verification Status", Task 3 Step 6, Task 5 Step 3 |
| Non-goals — no schema, no event tier, no surfacing | Nothing in any task touches schema, `ThreatTier`, or web/UI |
| Rollout — branch and PR target | Working Agreements, Task 5 Step 5 |

**Type consistency:** `pair_is_assessable(&str, &str) -> bool` is defined in Task 1 and called in Task 3 Step 3. `resolve_model_dir() -> PathBuf` is defined in Task 2 and called in Task 3 Step 1. `tox_suffix(Option<f64>, bool) -> String` is defined and consumed within Task 4. `score_pair` returns `Result<Option<(f64, HypothesisScores)>>` in Task 3 Step 3 and every call site in Steps 4-5 destructures `Ok(Some(..))` / `Ok(None)` / `Err`. Consistent.

**Placeholder scan:** no TBD/TODO, every code step carries complete code, every command carries expected output.
