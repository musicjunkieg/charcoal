# Classifier Eval Harness Implementation Plan (#404, Part A)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `charcoal eval-classifiers` command that runs posts through several classifiers, picks the posts worth hand-labelling, and reports each model's false-alarm rate, miss rate, coverage, cost and latency, with honest error bars computed on a random holdout.

**Architecture:** A new library module `src/cli/eval_classifiers/` (pure, unit-tested pieces: data, verdict cache, cost, metrics, label picker, report, runner, model registry) behind a thin clap subcommand in `src/main.rs`. Models plug in through the existing `ToxicityClassifier` trait. Part A ships the harness with the two models we already have — CoPE-B (RunPod) and Detoxify (local ONNX) — and verifies the new providers' APIs (Task 1). **Part B** (Jev, Clef-flash, Strands Decider adapters) is a separate short plan written from Task 1's verified contracts.

**Tech Stack:** Rust (tokio, async-trait, serde/serde_json, anyhow), the existing `ort` ONNX scorer, `tempfile` for tests. No new dependencies.

**Revision note (2026-10-09):** rewritten after a Codex adversarial review found eight problems in the first draft — an inverted CoPE-B probability conversion, outcome-biased rates, a bypassable spending cap, error-flattered comparisons, a cache blind to content/policy/model changes, a privacy rule that did not survive a fresh clone, no timeout, and an invalid `cargo test` command. Each is fixed below; the spec was amended to match.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-10-08-404-classifier-eval-design.md` (approved 2026-10-09, amended after the Codex review).
- Post level only; account-level evaluation is out of scope.
- **Decision rule:** every model is scored on its **own binary answer** (`ClassifierVerdict::toxic_token`). Never derive a probability from `confidence`: for CoPE-B it is `exp(logprob)` of the emitted token over the whole vocabulary (`gpu/cope-b-runpod/handler.py:122`), so `1 − confidence` is not p(toxic).
- **Headline rates use holdout labels only.** Holdout = a fixed hash-chosen ~⅓ of the pool, labelled regardless of model votes. Disagreement-picked ("diagnostic") labels and the curated seed labels never enter the headline rates.
- Report false-alarm rate and miss rate **separately**, each with a **95 % Wilson interval**; **no blended score**.
- Model-vs-baseline comparisons use a **paired cohort** (holdout posts both models judged). **No winner below 90 % coverage.** Overlapping intervals → **"too close to call — label more"**.
- Errors and timeouts are recorded as **errors, never as clean**. Every call runs under a timeout (default **60 s**).
- A cached verdict is reused only while **content hash, model ID and policy version** all match.
- **A model without a pricing entry is refused.** Default spending cap **$1.00 per run**; reaching it stops cleanly and keeps every paid verdict. Cost estimates cover the whole request (post + the model's `overhead_chars`).
- New adapters are reachable **only** from eval code (`models::build_eval_model`), never from production `build_from_env`.
- **CI never calls a paid API.** All tests use fake classifiers.
- Data: `evals/classifiers/pool.jsonl`, `labels.jsonl`, `pricing.json` committed (PII-scrubbed); `evals/classifiers/verdicts/` never committed, enforced by a committed `evals/classifiers/verdicts/.gitignore`.
- No new crate dependencies (no `toml`, no `rand`, no `sha2`): pricing is JSON; hashing is FNV-1a 64-bit.
- `cargo test` accepts **one** name filter per invocation.
- Charcoal conventions: `anyhow::Result`, `?` not `.unwrap()` outside tests, comments explain *why*, conventional commits, explicit `git add <paths>`.

---

## File Structure

| File | Responsibility |
|---|---|
| `docs/superpowers/specs/404-provider-contracts.md` | Task 1: verified API facts for Jev, Clef-flash, Strands Decider |
| `evals/classifiers/pool.jsonl` | Posts to evaluate (`FixtureRow` lines) |
| `evals/classifiers/labels.jsonl` | Human answers `{id, label, source, note}` |
| `evals/classifiers/pricing.json` | List price per model |
| `evals/classifiers/verdicts/.gitignore` | Keeps verdicts out of git on every clone |
| `evals/classifiers/README.md` | How to run a round |
| `src/cli/eval_classifiers/mod.rs` | Module root; `fnv1a` and `content_hash` |
| `src/cli/eval_classifiers/data.rs` | Load pool and labels |
| `src/cli/eval_classifiers/metrics.rs` | Wilson interval, rates, paired comparison |
| `src/cli/eval_classifiers/cache.rs` | `VerdictRecord`, append-only JSONL cache, reuse rule |
| `src/cli/eval_classifiers/cost.rs` | Pricing (required), estimates, spending cap |
| `src/cli/eval_classifiers/runner.rs` | Run one model over the pool with cache, cap and timeout |
| `src/cli/eval_classifiers/labelling.rs` | Holdout + diagnostic picker, merge |
| `src/cli/eval_classifiers/report.rs` | Plain-text report |
| `src/cli/eval_classifiers/models.rs` | Eval-only model registry |
| `src/toxicity/detoxify_classifier.rs` | Detoxify as a `ToxicityClassifier` |
| `src/main.rs` | `eval-classifiers run|label|report` subcommand |

---

### Task 1: Verify the provider APIs (research; no code)

**Files:**
- Create: `docs/superpowers/specs/404-provider-contracts.md`

**Interfaces:**
- Produces: the contract Part B's adapters are written against.

- [ ] **Step 1: Read the official docs** for TypeSafe Jev, Cloudflare Workers AI Clef-flash (and Clef 27B), and AWS Strands Decider 2B (local server). Vendor documentation only — not blog write-ups.

- [ ] **Step 2: Record, per provider, with the URL each fact came from:**
  1. Endpoint URL and auth header.
  2. Request body shape — exact JSON field names.
  3. **Can the instruction be free text?** Can the whole of `gpu/cope-b-runpod/policy.txt` (length: `wc -c gpu/cope-b-runpod/policy.txt`) be the question, with answers `toxic` / `clean`?
  4. Response shape — where the chosen answer and its probability live, and whether that probability is **normalised over the answer set** (a true p(toxic)) or something else.
  5. Batching: several posts per request; can the policy be sent once per batch?
  6. Token accounting: does the response report input tokens? (Part B must reconcile estimated vs reported cost.)
  7. Price per million input tokens (and output, if any), with the date read.
  8. Rate limits.
  9. Data retention / training-use terms for submitted text.

- [ ] **Step 3: Gate.** A provider that cannot take a free-text policy as its instruction (item 3) is marked **BLOCKED** at the top of the file with the reason. Do not plan an adapter for it; bring it to Bryan.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/404-provider-contracts.md
git commit -m 'docs(404): verified API contracts for Jev, Clef and Strands Decider'
```

---

### Task 2: Eval data, hashing and loaders

**Files:**
- Create: `evals/classifiers/pool.jsonl`, `evals/classifiers/labels.jsonl`, `evals/classifiers/pricing.json`, `evals/classifiers/verdicts/.gitignore`
- Create: `src/cli/eval_classifiers/mod.rs`, `src/cli/eval_classifiers/data.rs`
- Modify: `src/cli/mod.rs` (add `pub mod eval_classifiers;`)

**Interfaces:**
- Consumes: `crate::cli::classify_compare::FixtureRow { id, label, category, content, note }`.
- Produces (in `mod.rs`): `pub fn fnv1a(bytes: &[u8]) -> u64`, `pub fn content_hash(content: &str) -> String` (16 lowercase hex digits)
- Produces (in `data.rs`):
  - `pub enum Truth { Toxic, Clean }` (serde lowercase)
  - `pub enum Source { Seed, Holdout, Diagnostic }` (serde lowercase)
  - `pub struct Answer { pub truth: Truth, pub source: Source }`
  - `pub struct Label { pub id: String, pub label: Truth, pub source: Source, pub note: Option<String> }`
  - `pub fn load_pool(path: &Path) -> Result<Vec<FixtureRow>>`
  - `pub fn load_labels(path: &Path) -> Result<HashMap<String, Answer>>` — missing file → empty map; a label row without `source` is an error

- [ ] **Step 1: Build the data files**

```bash
mkdir -p evals/classifiers/verdicts
cat tests/fixtures/cope_b/smoke.jsonl tests/fixtures/cope_b/ab_sample.jsonl > evals/classifiers/pool.jsonl
python3 -c "
import json
with open('evals/classifiers/labels.jsonl','w') as out:
    for l in open('tests/fixtures/cope_b/smoke.jsonl'):
        r=json.loads(l)
        out.write(json.dumps({'id':r['id'],'label':r['label'],'source':'seed','note':r.get('note') or None})+'\n')
"
printf '*\n!.gitignore\n' > evals/classifiers/verdicts/.gitignore
wc -l evals/classifiers/pool.jsonl evals/classifiers/labels.jsonl
```

Expected: `110 evals/classifiers/pool.jsonl`, `10 evals/classifiers/labels.jsonl`.

Create `evals/classifiers/pricing.json`. The CoPE-B figure is the per-post cost implied by the observed $2.67 / ~20,000-post staging scan; Detoxify runs locally and is explicitly free. Every model the eval runs must appear here.

```json
{
  "cope-b": { "usd_per_post": 0.00013 },
  "detoxify": { "usd_per_post": 0.0 }
}
```

Verify the privacy rule holds without any local config:

```bash
touch evals/classifiers/verdicts/probe.jsonl
git check-ignore -v evals/classifiers/verdicts/probe.jsonl
rm evals/classifiers/verdicts/probe.jsonl
```

Expected: a line naming `evals/classifiers/verdicts/.gitignore` as the matching rule.

- [ ] **Step 2: Write the failing tests** — in `src/cli/eval_classifiers/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_matches_the_reference_vectors() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn content_hash_changes_when_the_post_changes() {
        assert_eq!(content_hash("x").len(), 16);
        assert_eq!(content_hash("x"), content_hash("x"));
        assert_ne!(content_hash("x"), content_hash("x "));
    }
}
```

and in `src/cli/eval_classifiers/data.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn file(lines: &[&str]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        f
    }

    #[test]
    fn loads_pool_rows_and_skips_blank_lines() {
        let f = file(&[r#"{"id":"a","content":"x"}"#, "", r#"{"id":"b","content":"y","label":"toxic"}"#]);
        let rows = load_pool(f.path()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].label, "toxic");
    }

    #[test]
    fn labels_carry_truth_and_source() {
        let f = file(&[
            r#"{"id":"a","label":"toxic","source":"holdout"}"#,
            r#"{"id":"b","label":"clean","source":"seed","note":"ok"}"#,
        ]);
        let labels = load_labels(f.path()).unwrap();
        assert_eq!(labels["a"], Answer { truth: Truth::Toxic, source: Source::Holdout });
        assert_eq!(labels["b"].source, Source::Seed);
    }

    #[test]
    fn bad_label_rows_are_errors() {
        assert!(load_labels(file(&[r#"{"id":"c","label":"maybe","source":"holdout"}"#]).path()).is_err());
        assert!(load_labels(file(&[r#"{"id":"c","label":"toxic"}"#]).path()).is_err(), "source is required");
    }

    #[test]
    fn a_missing_labels_file_is_an_empty_set() {
        assert!(load_labels(Path::new("/nonexistent/labels.jsonl")).unwrap().is_empty());
    }
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test --lib eval_classifiers`
Expected: compile error — module not found.

- [ ] **Step 4: Implement** `src/cli/eval_classifiers/mod.rs` (above its tests):

```rust
//! `charcoal eval-classifiers` — compare classifiers on labelled posts (#404).
//! See docs/superpowers/specs/2026-10-08-404-classifier-eval-design.md.

pub mod data;

/// FNV-1a 64-bit. A fixed, dependency-free hash: used for content identity
/// in the verdict cache and for the stable holdout/sample selections, so the
/// same post lands in the same bucket on every run and every machine.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Identity of a post's exact text. An edited post gets a new hash, so its
/// cached verdicts are no longer reused.
pub fn content_hash(content: &str) -> String {
    format!("{:016x}", fnv1a(content.as_bytes()))
}
```

`src/cli/eval_classifiers/data.rs` (above its tests):

```rust
//! Loading the eval's posts (the pool) and the human answers (labels).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

pub use crate::cli::classify_compare::FixtureRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Truth {
    Toxic,
    Clean,
}

/// Why a post was labelled. Only `Holdout` labels — chosen independently of
/// any model's answer — may feed the headline rates; the others were picked
/// BECAUSE of model behaviour (or by hand) and would bias them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Seed,
    Holdout,
    Diagnostic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    pub truth: Truth,
    pub source: Source,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub id: String,
    pub label: Truth,
    pub source: Source,
    #[serde(default)]
    pub note: Option<String>,
}

pub fn load_pool(path: &Path) -> Result<Vec<FixtureRow>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| serde_json::from_str(l).with_context(|| format!("{} line {}", path.display(), i + 1)))
        .collect()
}

/// A labels file that does not exist yet is an empty set, not an error.
pub fn load_labels(path: &Path) -> Result<HashMap<String, Answer>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut out = HashMap::new();
    for (i, l) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let label: Label = serde_json::from_str(l).with_context(|| {
            format!("{} line {} (label must be toxic|clean, source seed|holdout|diagnostic)", path.display(), i + 1)
        })?;
        out.insert(label.id, Answer { truth: label.label, source: label.source });
    }
    Ok(out)
}
```

Add `pub mod eval_classifiers;` to `src/cli/mod.rs`.

- [ ] **Step 5: Run to verify they pass**

Run: `cargo test --lib eval_classifiers`
Expected: 6 passed.

- [ ] **Step 6: Commit**

```bash
git add evals/classifiers/pool.jsonl evals/classifiers/labels.jsonl evals/classifiers/pricing.json evals/classifiers/verdicts/.gitignore src/cli/eval_classifiers/mod.rs src/cli/eval_classifiers/data.rs src/cli/mod.rs
git commit -m 'feat(404): eval pool, seed labels, pricing, hashing and loaders'
```

---

### Task 3: Metrics — Wilson intervals, coverage, paired comparison

**Files:**
- Create: `src/cli/eval_classifiers/metrics.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod metrics;`)

**Interfaces:**
- Consumes: `data::Truth`.
- Produces:
  - `pub struct Rate { pub hits: u32, pub n: u32 }` with `fn value(&self) -> Option<f64>`, `fn wilson95(&self) -> Option<(f64, f64)>`
  - `pub struct ModelMetrics { pub model: String, pub false_alarm: Rate, pub miss: Rate, pub labelled: u32, pub judged: u32, pub errors: u32 }` with `fn coverage(&self) -> Option<f64>` (`judged / labelled`)
  - `pub fn model_metrics(model: &str, calls: &HashMap<String, Option<bool>>, labels: &HashMap<String, Truth>) -> ModelMetrics` — `Some(true)` toxic, `Some(false)` clean, `None` error; a labelled post with no record counts in `labelled` but not `judged`
  - `pub enum Comparison { Better, Worse, TooClose, NoData }`
  - `pub fn compare(candidate: &Rate, baseline: &Rate) -> Comparison` — lower is better
  - `pub const MIN_COVERAGE: f64 = 0.9;`
  - `pub struct Paired { pub baseline: ModelMetrics, pub candidate: ModelMetrics, pub false_alarm: Comparison, pub miss: Comparison, pub low_coverage: bool }`
  - `pub fn paired(base_name: &str, base: &HashMap<String, Option<bool>>, cand_name: &str, cand: &HashMap<String, Option<bool>>, labels: &HashMap<String, Truth>) -> Paired` — scores both models on only the labelled posts **both** judged; `low_coverage` when either model's own coverage of `labels` is below `MIN_COVERAGE`, in which case both comparisons are `NoData`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::eval_classifiers::data::Truth;

    fn map<T: Clone>(pairs: &[(&str, T)]) -> HashMap<String, T> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn wilson_matches_known_values() {
        let (lo, hi) = Rate { hits: 0, n: 10 }.wilson95().unwrap();
        assert!(lo.abs() < 1e-9 && (hi - 0.2775).abs() < 1e-3, "{lo} {hi}");
        let (lo, hi) = Rate { hits: 5, n: 10 }.wilson95().unwrap();
        assert!((lo - 0.2366).abs() < 1e-3 && (hi - 0.7634).abs() < 1e-3, "{lo} {hi}");
        assert_eq!(Rate { hits: 0, n: 0 }.wilson95(), None);
    }

    #[test]
    fn rates_split_by_truth_and_errors_are_not_clean() {
        let labels = map(&[("t1", Truth::Toxic), ("t2", Truth::Toxic), ("c1", Truth::Clean), ("c2", Truth::Clean), ("e", Truth::Toxic), ("gone", Truth::Clean)]);
        let calls = map(&[("t1", Some(true)), ("t2", Some(false)), ("c1", Some(true)), ("c2", Some(false)), ("e", None)]);
        let m = model_metrics("m", &calls, &labels);
        assert_eq!((m.miss.hits, m.miss.n), (1, 2), "the errored toxic post is not a miss");
        assert_eq!((m.false_alarm.hits, m.false_alarm.n), (1, 2));
        assert_eq!((m.labelled, m.judged, m.errors), (6, 4, 1));
        assert!((m.coverage().unwrap() - 4.0 / 6.0).abs() < 1e-9);
    }

    #[test]
    fn comparison_needs_non_overlapping_intervals() {
        let good = Rate { hits: 1, n: 40 };
        let bad = Rate { hits: 30, n: 40 };
        assert_eq!(compare(&bad, &good), Comparison::Worse);
        assert_eq!(compare(&good, &bad), Comparison::Better);
        assert_eq!(compare(&Rate { hits: 2, n: 5 }, &Rate { hits: 1, n: 5 }), Comparison::TooClose);
        assert_eq!(compare(&Rate { hits: 0, n: 0 }, &good), Comparison::NoData);
    }

    /// Codex review: a model that errors on the hard toxic posts must not
    /// look better for it. Paired scoring uses only posts both judged.
    #[test]
    fn a_model_that_fails_on_hard_posts_gains_nothing() {
        let labels: HashMap<String, Truth> = (0..40)
            .map(|i| (format!("p{i}"), if i < 20 { Truth::Toxic } else { Truth::Clean }))
            .collect();
        // Baseline misses the 5 hard toxic posts p0..p4; the candidate ERRORS on them.
        let base: HashMap<String, Option<bool>> = (0..40).map(|i| (format!("p{i}"), Some((5..20).contains(&i)))).collect();
        let cand: HashMap<String, Option<bool>> = (0..40)
            .map(|i| (format!("p{i}"), if i < 5 { None } else { Some(i < 20) }))
            .collect();
        let p = paired("base", &base, "cand", &cand, &labels);
        assert_eq!(p.candidate.miss.n, 15, "the 5 posts the candidate dodged are excluded for BOTH");
        assert_eq!(p.baseline.miss.hits, 0, "on the shared cohort the baseline missed nothing either");
        assert!(p.low_coverage, "35/40 = 87.5 % coverage is below the 90 % floor");
        assert_eq!((p.false_alarm, p.miss), (Comparison::NoData, Comparison::NoData));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::metrics`
Expected: compile error — items not found.

- [ ] **Step 3: Implement** `src/cli/eval_classifiers/metrics.rs`:

```rust
//! Error rates with honest uncertainty. With tens of labels a 5-point gap is
//! often noise; the Wilson interval says so instead of crowning a winner.

use std::collections::HashMap;

use super::data::Truth;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rate {
    pub hits: u32,
    pub n: u32,
}

impl Rate {
    pub fn value(&self) -> Option<f64> {
        (self.n > 0).then(|| self.hits as f64 / self.n as f64)
    }

    /// 95 % Wilson score interval — well-behaved at 0 and n, unlike the
    /// normal approximation, which matters with very few labels.
    pub fn wilson95(&self) -> Option<(f64, f64)> {
        let p = self.value()?;
        let n = self.n as f64;
        let z = 1.959_964_f64;
        let z2 = z * z;
        let denom = 1.0 + z2 / n;
        let centre = (p + z2 / (2.0 * n)) / denom;
        let half = z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / denom;
        Some(((centre - half).max(0.0), (centre + half).min(1.0)))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelMetrics {
    pub model: String,
    pub false_alarm: Rate,
    pub miss: Rate,
    /// Labelled posts in scope.
    pub labelled: u32,
    /// Labelled posts the model actually answered.
    pub judged: u32,
    /// Labelled posts whose call failed or timed out — never counted as clean.
    pub errors: u32,
}

impl ModelMetrics {
    pub fn coverage(&self) -> Option<f64> {
        (self.labelled > 0).then(|| self.judged as f64 / self.labelled as f64)
    }
}

pub fn model_metrics(
    model: &str,
    calls: &HashMap<String, Option<bool>>,
    labels: &HashMap<String, Truth>,
) -> ModelMetrics {
    let mut m = ModelMetrics {
        model: model.to_string(),
        false_alarm: Rate::default(),
        miss: Rate::default(),
        labelled: labels.len() as u32,
        judged: 0,
        errors: 0,
    };
    for (id, truth) in labels {
        match calls.get(id) {
            None => {}
            Some(None) => m.errors += 1,
            Some(Some(called_toxic)) => {
                m.judged += 1;
                let rate = match truth {
                    Truth::Toxic => &mut m.miss,
                    Truth::Clean => &mut m.false_alarm,
                };
                rate.n += 1;
                let wrong = match truth {
                    Truth::Toxic => !called_toxic,
                    Truth::Clean => *called_toxic,
                };
                if wrong {
                    rate.hits += 1;
                }
            }
        }
    }
    m
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    Better,
    Worse,
    TooClose,
    NoData,
}

/// Lower is better for both rates. A verdict only when the intervals do not
/// overlap; otherwise the honest answer is "label more".
pub fn compare(candidate: &Rate, baseline: &Rate) -> Comparison {
    match (candidate.wilson95(), baseline.wilson95()) {
        (Some((c_lo, c_hi)), Some((b_lo, b_hi))) => {
            if c_hi < b_lo {
                Comparison::Better
            } else if c_lo > b_hi {
                Comparison::Worse
            } else {
                Comparison::TooClose
            }
        }
        _ => Comparison::NoData,
    }
}

/// Below this share of labelled posts answered, a model's rates describe a
/// different set of posts than its rival's and no winner is named.
pub const MIN_COVERAGE: f64 = 0.9;

#[derive(Debug, Clone, PartialEq)]
pub struct Paired {
    pub baseline: ModelMetrics,
    pub candidate: ModelMetrics,
    pub false_alarm: Comparison,
    pub miss: Comparison,
    pub low_coverage: bool,
}

/// Score two models on the SAME posts: the labelled posts both answered. A
/// model that errors on the hard cases cannot look better for skipping them
/// (Codex review).
pub fn paired(
    base_name: &str,
    base: &HashMap<String, Option<bool>>,
    cand_name: &str,
    cand: &HashMap<String, Option<bool>>,
    labels: &HashMap<String, Truth>,
) -> Paired {
    let low_coverage = [base, cand].iter().any(|calls| {
        model_metrics("", calls, labels).coverage().is_none_or(|c| c < MIN_COVERAGE)
    });
    let shared: HashMap<String, Truth> = labels
        .iter()
        .filter(|(id, _)| {
            matches!(base.get(*id), Some(Some(_))) && matches!(cand.get(*id), Some(Some(_)))
        })
        .map(|(id, t)| (id.clone(), *t))
        .collect();
    let baseline = model_metrics(base_name, base, &shared);
    let candidate = model_metrics(cand_name, cand, &shared);
    let (false_alarm, miss) = if low_coverage {
        (Comparison::NoData, Comparison::NoData)
    } else {
        (
            compare(&candidate.false_alarm, &baseline.false_alarm),
            compare(&candidate.miss, &baseline.miss),
        )
    };
    Paired { baseline, candidate, false_alarm, miss, low_coverage }
}
```

`Option::is_none_or` needs Rust 1.82+; the project builds with Rust 1.94 (Dockerfile). Add `pub mod metrics;` to `mod.rs`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --lib eval_classifiers::metrics`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/cli/eval_classifiers/metrics.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): Wilson metrics with coverage and paired-cohort comparison'
```

---

### Task 4: Verdict cache with content and model identity

**Files:**
- Create: `src/cli/eval_classifiers/cache.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod cache;`)

**Interfaces:**
- Consumes: `ClassifierVerdict`, `super::content_hash`.
- Produces:
  - `pub struct VerdictRecord { pub id: String, pub model: String, pub content_hash: String, pub model_id: String, pub policy_version: String, pub called_toxic: Option<bool>, pub error: Option<String>, pub latency_ms: u32, pub input_chars: usize, pub est_usd: f64 }`
  - `pub struct Identity<'a> { pub model_id: &'a str, pub policy_version: &'a str }`
  - `impl VerdictRecord { pub fn from_verdict(id: &str, model: &str, content: &str, v: &ClassifierVerdict, est_usd: f64) -> Self; pub fn from_error(id: &str, model: &str, content: &str, who: Identity<'_>, detail: &str, est_usd: f64) -> Self; pub fn reusable(&self, content: &str, who: Identity<'_>) -> bool }`
  - `pub fn append(dir: &Path, rec: &VerdictRecord) -> Result<()>` — `dir/<model>.jsonl`
  - `pub fn load(dir: &Path, model: &str) -> Result<HashMap<String, VerdictRecord>>` — last record per id wins

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::toxicity::classifier::ClassifierVerdict;

    fn verdict(toxic: bool, conf: f32) -> ClassifierVerdict {
        ClassifierVerdict { toxic_token: toxic, confidence: conf, latency_ms: 12, model_id: "m-1".into(), policy_version: "p-1".into() }
    }
    const WHO: Identity<'static> = Identity { model_id: "m-1", policy_version: "p-1" };

    /// Codex review, critical: CoPE-B reports confidence as exp(logprob) of the
    /// emitted token over the WHOLE vocabulary. A "clean" answer with 0.1 must
    /// stay clean — it is not "0.9 toxic". This is the repository's own RunPod
    /// fixture shape (src/toxicity/runpod_cope_b.rs, `toxic:false, confidence:0.1`).
    #[test]
    fn the_models_own_answer_is_kept_whatever_its_confidence() {
        let r = VerdictRecord::from_verdict("a", "cope-b", "x", &verdict(false, 0.1), 0.0);
        assert_eq!(r.called_toxic, Some(false));
        let r = VerdictRecord::from_verdict("b", "cope-b", "x", &verdict(true, 0.2), 0.0);
        assert_eq!(r.called_toxic, Some(true));
        let e = VerdictRecord::from_error("c", "cope-b", "x", WHO, "timed out", 0.0);
        assert_eq!(e.called_toxic, None, "an error, never clean");
    }

    #[test]
    fn reuse_requires_same_content_model_and_policy() {
        let r = VerdictRecord::from_verdict("a", "cope-b", "post text", &verdict(true, 0.9), 0.0);
        assert!(r.reusable("post text", WHO));
        assert!(!r.reusable("post text, edited", WHO), "edited post");
        assert!(!r.reusable("post text", Identity { model_id: "m-2", policy_version: "p-1" }), "redeployed model");
        assert!(!r.reusable("post text", Identity { model_id: "m-1", policy_version: "p-2" }), "new policy");
        let e = VerdictRecord::from_error("a", "cope-b", "post text", WHO, "boom", 0.0);
        assert!(!e.reusable("post text", WHO), "errors are always retried");
    }

    #[test]
    fn cache_round_trips_and_the_last_record_wins() {
        let dir = tempfile::tempdir().unwrap();
        append(dir.path(), &VerdictRecord::from_error("a", "m", "x", WHO, "timeout", 0.0)).unwrap();
        append(dir.path(), &VerdictRecord::from_verdict("a", "m", "x", &verdict(true, 0.9), 0.01)).unwrap();
        append(dir.path(), &VerdictRecord::from_verdict("b", "m", "y", &verdict(false, 0.7), 0.01)).unwrap();
        let got = load(dir.path(), "m").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got["a"].called_toxic, Some(true));
        assert!(load(dir.path(), "other").unwrap().is_empty());
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::cache`
Expected: compile error.

- [ ] **Step 3: Implement** `src/cli/eval_classifiers/cache.rs`:

```rust
//! Every verdict ever paid for, one JSONL file per model. A verdict is reused
//! only while the post text, the deployed model and its policy are unchanged
//! (Codex review): otherwise "re-ask cheaply when a model changes" would
//! silently answer with the old model.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::content_hash;
use crate::toxicity::classifier::ClassifierVerdict;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerdictRecord {
    pub id: String,
    /// The eval's name for the model (file name), e.g. "cope-b".
    pub model: String,
    pub content_hash: String,
    /// What the backend reported serving — not just the eval's name for it.
    pub model_id: String,
    pub policy_version: String,
    /// The model's OWN binary answer. `None` = the call failed or timed out.
    /// Never derived from `confidence` (see the plan's decision rule).
    pub called_toxic: Option<bool>,
    pub error: Option<String>,
    pub latency_ms: u32,
    pub input_chars: usize,
    /// Estimated spend for this call, from pricing.json.
    pub est_usd: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct Identity<'a> {
    pub model_id: &'a str,
    pub policy_version: &'a str,
}

impl VerdictRecord {
    pub fn from_verdict(id: &str, model: &str, content: &str, v: &ClassifierVerdict, est_usd: f64) -> Self {
        Self {
            id: id.to_string(),
            model: model.to_string(),
            content_hash: content_hash(content),
            model_id: v.model_id.clone(),
            policy_version: v.policy_version.clone(),
            called_toxic: Some(v.toxic_token),
            error: None,
            latency_ms: v.latency_ms,
            input_chars: content.chars().count(),
            est_usd,
        }
    }

    pub fn from_error(id: &str, model: &str, content: &str, who: Identity<'_>, detail: &str, est_usd: f64) -> Self {
        Self {
            id: id.to_string(),
            model: model.to_string(),
            content_hash: content_hash(content),
            model_id: who.model_id.to_string(),
            policy_version: who.policy_version.to_string(),
            called_toxic: None,
            error: Some(detail.to_string()),
            latency_ms: 0,
            input_chars: content.chars().count(),
            est_usd,
        }
    }

    /// A successful verdict for exactly this text, from exactly this model
    /// and policy. Anything else is judged again.
    pub fn reusable(&self, content: &str, who: Identity<'_>) -> bool {
        self.called_toxic.is_some()
            && self.content_hash == content_hash(content)
            && self.model_id == who.model_id
            && self.policy_version == who.policy_version
    }
}

fn path_for(dir: &Path, model: &str) -> PathBuf {
    dir.join(format!("{model}.jsonl"))
}

pub fn append(dir: &Path, rec: &VerdictRecord) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = path_for(dir, &rec.model);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(f, "{}", serde_json::to_string(rec)?)?;
    Ok(())
}

pub fn load(dir: &Path, model: &str) -> Result<HashMap<String, VerdictRecord>> {
    let path = path_for(dir, model);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut out = HashMap::new();
    for (i, l) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let rec: VerdictRecord =
            serde_json::from_str(l).with_context(|| format!("{} line {}", path.display(), i + 1))?;
        out.insert(rec.id.clone(), rec);
    }
    Ok(out)
}
```

Add `pub mod cache;` to `mod.rs`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --lib eval_classifiers::cache`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add src/cli/eval_classifiers/cache.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): verdict cache keyed by content, model and policy identity'
```

---

### Task 5: Pricing (required) and the spending cap

**Files:**
- Create: `src/cli/eval_classifiers/cost.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod cost;`)

**Interfaces:**
- Produces:
  - `pub struct Price { pub usd_per_post: Option<f64>, pub usd_per_million_input_tokens: Option<f64>, pub overhead_chars: usize }` (serde; `overhead_chars` defaults to 0 — Part B sets it to the policy's length)
  - `pub fn load_pricing(path: &Path) -> Result<HashMap<String, Price>>` — an entry with neither price field is an error
  - `pub fn price_for<'a>(prices: &'a HashMap<String, Price>, model: &str) -> Result<&'a Price>` — a model with no entry is refused
  - `pub fn estimate_usd(price: &Price, input_chars: usize) -> f64` — per-post, else `(input_chars + overhead_chars) / 4` tokens × price
  - `pub struct Budget` with `pub fn new(cap_usd: f64) -> Self`, `pub fn try_spend(&mut self, usd: f64) -> bool`, `pub fn spent(&self) -> f64`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn per_tok(price: f64, overhead: usize) -> Price {
        Price { usd_per_post: None, usd_per_million_input_tokens: Some(price), overhead_chars: overhead }
    }

    #[test]
    fn estimates_cover_the_whole_request() {
        let per_post = Price { usd_per_post: Some(0.001), usd_per_million_input_tokens: None, overhead_chars: 0 };
        assert!((estimate_usd(&per_post, 4_000) - 0.001).abs() < 1e-12);
        // 4,000 chars of post + 12,000 chars of policy = 16,000 chars ≈ 4,000 tokens.
        assert!((estimate_usd(&per_tok(0.042, 12_000), 4_000) - 0.000_168).abs() < 1e-12);
    }

    #[test]
    fn the_budget_refuses_a_call_that_would_cross_the_cap() {
        let mut b = Budget::new(1.0);
        assert!(b.try_spend(0.6));
        assert!(!b.try_spend(0.5), "0.6 + 0.5 > 1.0: refused, nothing spent");
        assert!((b.spent() - 0.6).abs() < 1e-12);
        assert!(b.try_spend(0.4));
    }

    /// Codex review: an unpriced model must be refused, never run as free.
    #[test]
    fn unpriced_models_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pricing.json");
        std::fs::write(&p, r#"{"cope-b":{"usd_per_post":0.00013},"detoxify":{"usd_per_post":0.0}}"#).unwrap();
        let prices = load_pricing(&p).unwrap();
        assert!(price_for(&prices, "cope-b").is_ok());
        assert!(price_for(&prices, "detoxify").is_ok(), "explicitly free is fine");
        let err = format!("{:#}", price_for(&prices, "jev").unwrap_err());
        assert!(err.contains("jev") && err.contains("pricing.json"), "{err}");
        std::fs::write(&p, r#"{"x":{}}"#).unwrap();
        assert!(load_pricing(&p).is_err(), "an entry with no price is a mistake, not free");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::cost`
Expected: compile error.

- [ ] **Step 3: Implement** `src/cli/eval_classifiers/cost.rs`:

```rust
//! Cost estimates and the per-run spending cap — the lesson of the $193
//! incident (#385): a run must stop before it can surprise anyone, and a
//! model nobody priced must not run as if it were free (Codex review).

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Price {
    #[serde(default)]
    pub usd_per_post: Option<f64>,
    #[serde(default)]
    pub usd_per_million_input_tokens: Option<f64>,
    /// Characters every request carries besides the post — e.g. the policy a
    /// decision model is given as its instruction. Without it the estimate
    /// would miss the dominant part of the bill.
    #[serde(default)]
    pub overhead_chars: usize,
}

pub fn load_pricing(path: &Path) -> Result<HashMap<String, Price>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let prices: HashMap<String, Price> =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    for (model, p) in &prices {
        if p.usd_per_post.is_none() && p.usd_per_million_input_tokens.is_none() {
            bail!("{}: {model:?} has no price (use usd_per_post: 0.0 for a free model)", path.display());
        }
    }
    Ok(prices)
}

pub fn price_for<'a>(prices: &'a HashMap<String, Price>, model: &str) -> Result<&'a Price> {
    prices
        .get(model)
        .with_context(|| format!("{model:?} has no entry in pricing.json — refusing to run an unpriced model"))
}

/// ~4 characters per token: a rough English average, good enough for a cap.
/// Part B adapters record provider-reported usage and reconcile against this.
pub fn estimate_usd(price: &Price, input_chars: usize) -> f64 {
    if let Some(per_post) = price.usd_per_post {
        return per_post;
    }
    let tokens = (input_chars + price.overhead_chars) as f64 / 4.0;
    price.usd_per_million_input_tokens.unwrap_or(0.0) * tokens / 1_000_000.0
}

#[derive(Debug, Clone)]
pub struct Budget {
    cap_usd: f64,
    spent_usd: f64,
}

impl Budget {
    pub fn new(cap_usd: f64) -> Self {
        Self { cap_usd, spent_usd: 0.0 }
    }

    /// Reserve `usd` before a call. `false` means the call must not be made.
    pub fn try_spend(&mut self, usd: f64) -> bool {
        if self.spent_usd + usd > self.cap_usd {
            return false;
        }
        self.spent_usd += usd;
        true
    }

    pub fn spent(&self) -> f64 {
        self.spent_usd
    }
}
```

Add `pub mod cost;` to `mod.rs`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --lib eval_classifiers::cost`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add src/cli/eval_classifiers/cost.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): required pricing, whole-request estimates and the spending cap'
```

---

### Task 6: The runner — cache, cap and timeout

**Files:**
- Create: `src/cli/eval_classifiers/runner.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod runner;`)

**Interfaces:**
- Consumes: `data::FixtureRow`, `cache::{append, load, Identity, VerdictRecord}`, `cost::{estimate_usd, Budget, Price}`, `ToxicityClassifier` (`model_id()`, `policy_version()`, `classify()`).
- Produces:
  - `pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);`
  - `pub struct RunStats { pub model: String, pub judged: usize, pub cached: usize, pub errors: usize, pub stopped_at_cap: bool }`
  - `pub async fn run_model(model: &str, classifier: &dyn ToxicityClassifier, pool: &[FixtureRow], dir: &Path, price: &Price, budget: &mut Budget, timeout: Duration) -> Result<RunStats>`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::eval_classifiers::cost::Price;
    use crate::toxicity::classifier::{ClassifierVerdict, ToxicityClassifier};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fake {
        calls: AtomicUsize,
        fail_on: Option<&'static str>,
        hang_on: Option<&'static str>,
        policy: &'static str,
    }

    impl Fake {
        fn new() -> Self {
            Self { calls: AtomicUsize::new(0), fail_on: None, hang_on: None, policy: "p-1" }
        }
    }

    #[async_trait::async_trait]
    impl ToxicityClassifier for Fake {
        async fn classify(&self, content: &str) -> anyhow::Result<ClassifierVerdict> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.hang_on.is_some_and(|h| content.contains(h)) {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            if self.fail_on.is_some_and(|f| content.contains(f)) {
                anyhow::bail!("upstream 503");
            }
            Ok(ClassifierVerdict { toxic_token: content.contains("bad"), confidence: 0.9, latency_ms: 5, model_id: "fake-1".into(), policy_version: self.policy.into() })
        }
        fn name(&self) -> &'static str { "fake" }
        fn model_id(&self) -> &'static str { "fake-1" }
        fn policy_version(&self) -> &'static str { self.policy }
        fn threshold(&self) -> f32 { 0.5 }
    }

    fn row(id: &str, content: &str) -> FixtureRow {
        serde_json::from_value(serde_json::json!({"id": id, "content": content})).unwrap()
    }

    const T: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn judges_each_post_once_and_reuses_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad"), row("b", "fine")];
        let model = Fake::new();
        let mut budget = Budget::new(1.0);
        let s = run_model("m", &model, &pool, dir.path(), &Price::default(), &mut budget, T).await.unwrap();
        assert_eq!((s.judged, s.cached), (2, 0));
        let s = run_model("m", &model, &pool, dir.path(), &Price::default(), &mut budget, T).await.unwrap();
        assert_eq!((s.judged, s.cached), (0, 2), "nothing re-paid");
        assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    }

    /// Codex review: a changed post or a new policy is judged again.
    #[tokio::test]
    async fn edited_posts_and_new_policies_are_rejudged() {
        let dir = tempfile::tempdir().unwrap();
        let mut budget = Budget::new(1.0);
        run_model("m", &Fake::new(), &[row("a", "bad")], dir.path(), &Price::default(), &mut budget, T).await.unwrap();
        let edited = Fake::new();
        run_model("m", &edited, &[row("a", "bad, edited")], dir.path(), &Price::default(), &mut budget, T).await.unwrap();
        assert_eq!(edited.calls.load(Ordering::SeqCst), 1);
        let repoliced = Fake { policy: "p-2", ..Fake::new() };
        run_model("m", &repoliced, &[row("a", "bad, edited")], dir.path(), &Price::default(), &mut budget, T).await.unwrap();
        assert_eq!(repoliced.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_error_is_recorded_as_an_error_and_retried_next_run() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad 503")];
        let mut budget = Budget::new(1.0);
        let failing = Fake { fail_on: Some("503"), ..Fake::new() };
        let s = run_model("m", &failing, &pool, dir.path(), &Price::default(), &mut budget, T).await.unwrap();
        assert_eq!(s.errors, 1);
        assert_eq!(load(dir.path(), "m").unwrap()["a"].called_toxic, None, "an error, never clean");
        let ok = Fake::new();
        run_model("m", &ok, &pool, dir.path(), &Price::default(), &mut budget, T).await.unwrap();
        assert_eq!(ok.calls.load(Ordering::SeqCst), 1, "the error was retried");
        assert_eq!(load(dir.path(), "m").unwrap()["a"].called_toxic, Some(true));
    }

    /// Codex review: a hung model must not stall the run.
    #[tokio::test]
    async fn a_hung_call_times_out_as_an_error_and_the_run_continues() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "hang"), row("b", "bad")];
        let model = Fake { hang_on: Some("hang"), ..Fake::new() };
        let mut budget = Budget::new(1.0);
        let started = std::time::Instant::now();
        let s = run_model("m", &model, &pool, dir.path(), &Price::default(), &mut budget, Duration::from_millis(50)).await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(5), "did not wait out the 30 s hang");
        assert_eq!((s.errors, s.judged), (1, 1));
        let recs = load(dir.path(), "m").unwrap();
        assert!(recs["a"].error.as_deref().unwrap().contains("timed out"));
        assert_eq!(recs["b"].called_toxic, Some(true));
    }

    #[tokio::test]
    async fn the_cap_stops_the_run_and_keeps_what_was_paid_for() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "x"), row("b", "y"), row("c", "z")];
        let price = Price { usd_per_post: Some(0.4), usd_per_million_input_tokens: None, overhead_chars: 0 };
        let mut budget = Budget::new(1.0);
        let s = run_model("m", &Fake::new(), &pool, dir.path(), &price, &mut budget, T).await.unwrap();
        assert!(s.stopped_at_cap);
        assert_eq!(s.judged, 2, "$0.80 spent; the third $0.40 call would cross $1.00");
        let recs = load(dir.path(), "m").unwrap();
        assert_eq!(recs.len(), 2, "both paid verdicts kept");
        assert!((recs["a"].est_usd - 0.4).abs() < 1e-12);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::runner`
Expected: compile error.

- [ ] **Step 3: Implement** `src/cli/eval_classifiers/runner.rs`:

```rust
//! Run one model over the pool: reuse only verdicts whose content, model and
//! policy still match, record every outcome (errors included), bound every
//! call with a timeout, and stop cleanly at the spending cap.

use anyhow::Result;
use std::path::Path;
use std::time::Duration;

use super::cache::{append, load, Identity, VerdictRecord};
use super::cost::{estimate_usd, Budget, Price};
use super::data::FixtureRow;
use crate::toxicity::classifier::ToxicityClassifier;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStats {
    pub model: String,
    pub judged: usize,
    pub cached: usize,
    pub errors: usize,
    pub stopped_at_cap: bool,
}

pub async fn run_model(
    model: &str,
    classifier: &dyn ToxicityClassifier,
    pool: &[FixtureRow],
    dir: &Path,
    price: &Price,
    budget: &mut Budget,
    timeout: Duration,
) -> Result<RunStats> {
    let cached = load(dir, model)?;
    let who = Identity {
        model_id: classifier.model_id(),
        policy_version: classifier.policy_version(),
    };
    let mut stats = RunStats {
        model: model.to_string(),
        judged: 0,
        cached: 0,
        errors: 0,
        stopped_at_cap: false,
    };
    for row in pool {
        if cached.get(&row.id).is_some_and(|r| r.reusable(&row.content, who)) {
            stats.cached += 1;
            continue;
        }
        let est = estimate_usd(price, row.content.chars().count());
        if !budget.try_spend(est) {
            stats.stopped_at_cap = true;
            break;
        }
        let rec = match tokio::time::timeout(timeout, classifier.classify(&row.content)).await {
            Ok(Ok(v)) => {
                stats.judged += 1;
                VerdictRecord::from_verdict(&row.id, model, &row.content, &v, est)
            }
            Ok(Err(e)) => {
                stats.errors += 1;
                VerdictRecord::from_error(&row.id, model, &row.content, who, &format!("{e:#}"), est)
            }
            Err(_) => {
                stats.errors += 1;
                let detail = format!("timed out after {} ms", timeout.as_millis());
                VerdictRecord::from_error(&row.id, model, &row.content, who, &detail, est)
            }
        };
        // Written per post, so a crash or a cap mid-run keeps every paid verdict.
        append(dir, &rec)?;
    }
    Ok(stats)
}
```

Add `pub mod runner;` to `mod.rs`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --lib eval_classifiers::runner`
Expected: 5 passed, in well under 5 seconds.

- [ ] **Step 5: Commit**

```bash
git add src/cli/eval_classifiers/runner.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): eval runner with identity-checked cache, timeout and spending cap'
```

---

### Task 7: The label picker — holdout and diagnostics

**Files:**
- Create: `src/cli/eval_classifiers/labelling.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod labelling;`)

**Interfaces:**
- Consumes: `data::{Answer, FixtureRow, Label, Source, Truth, load_labels}`, `cache::VerdictRecord`, `super::fnv1a`.
- Produces:
  - `pub fn in_holdout(id: &str) -> bool` — `fnv1a("holdout:" + id) % 3 == 0`
  - `pub fn sampled(id: &str) -> bool` — `fnv1a(id) % 10 == 0`
  - `pub fn pick_for_labelling(pool: &[FixtureRow], per_model: &[HashMap<String, VerdictRecord>], labels: &HashMap<String, Answer>) -> Vec<(FixtureRow, Source)>` — unlabelled holdout posts always (as `Holdout`, regardless of votes); other unlabelled posts as `Diagnostic` when ≥ 2 models answered and they disagree, or they agree and `sampled(id)`
  - `pub fn write_to_label(path: &Path, rows: &[(FixtureRow, Source)]) -> Result<()>` — JSONL `{id, content, source, label: "", note: ""}`; **no model votes**
  - `pub fn merge_labels(to_label: &Path, labels: &Path) -> Result<usize>` — appends answered rows (`toxic`/`clean`) with their `source`, skips already-labelled ids, errors on any other non-empty label

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::eval_classifiers::cache::VerdictRecord;

    fn row(id: &str) -> FixtureRow {
        serde_json::from_value(serde_json::json!({"id": id, "content": format!("post {id}")})).unwrap()
    }
    fn rec(id: &str, call: Option<bool>) -> VerdictRecord {
        VerdictRecord { id: id.into(), model: "m".into(), content_hash: String::new(), model_id: "m".into(), policy_version: "p".into(), called_toxic: call, error: call.is_none().then(|| "e".into()), latency_ms: 1, input_chars: 1, est_usd: 0.0 }
    }
    fn by_id(recs: Vec<VerdictRecord>) -> HashMap<String, VerdictRecord> {
        recs.into_iter().map(|r| (r.id.clone(), r)).collect()
    }

    #[test]
    fn holdout_and_sample_fractions_are_stable() {
        let holdout = (0..3000).filter(|i| in_holdout(&format!("post-{i}"))).count();
        assert!((900..=1100).contains(&holdout), "{holdout} of 3000 ≈ 1/3");
        let sample = (0..2000).filter(|i| sampled(&format!("post-{i}"))).count();
        assert!((140..=260).contains(&sample), "{sample} of 2000 ≈ 1/10");
        assert_eq!(in_holdout("post-7"), in_holdout("post-7"));
    }

    #[test]
    fn holdout_posts_are_picked_whatever_the_models_say() {
        let h = (0..100).map(|i| format!("h{i}")).find(|id| in_holdout(id)).unwrap();
        let picked = pick_for_labelling(&[row(&h)], &[], &HashMap::new());
        assert_eq!(picked.len(), 1, "no model verdicts needed for the holdout");
        assert_eq!(picked[0].1, Source::Holdout);
    }

    #[test]
    fn diagnostics_are_disagreements_plus_the_agreed_sample() {
        let ids: Vec<String> = (0..400).map(|i| format!("d{i}")).filter(|id| !in_holdout(id)).collect();
        let dis = ids[0].clone();
        let labelled = ids[1].clone();
        let agree = ids.iter().skip(2).find(|id| !sampled(id)).unwrap().clone();
        let agree_sampled = ids.iter().skip(2).find(|id| sampled(id)).unwrap().clone();
        let pool: Vec<FixtureRow> = [&dis, &agree, &agree_sampled, &labelled].iter().map(|id| row(id)).collect();
        let m1 = by_id(vec![rec(&dis, Some(true)), rec(&agree, Some(true)), rec(&agree_sampled, Some(true)), rec(&labelled, Some(true))]);
        let m2 = by_id(vec![rec(&dis, Some(false)), rec(&agree, Some(true)), rec(&agree_sampled, Some(true)), rec(&labelled, Some(false))]);
        let labels: HashMap<String, Answer> = [(labelled.clone(), Answer { truth: Truth::Toxic, source: Source::Diagnostic })].into();
        let picked: Vec<(String, Source)> = pick_for_labelling(&pool, &[m1, m2], &labels).into_iter().map(|(r, s)| (r.id, s)).collect();
        assert!(picked.contains(&(dis.clone(), Source::Diagnostic)));
        assert!(picked.contains(&(agree_sampled.clone(), Source::Diagnostic)));
        assert!(!picked.iter().any(|(id, _)| *id == agree), "agreed and not sampled");
        assert!(!picked.iter().any(|(id, _)| *id == labelled), "already answered");
    }

    #[test]
    fn to_label_rows_are_blind_and_merge_keeps_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let labels = dir.path().join("labels.jsonl");
        write_to_label(&to_label, &[(row("a"), Source::Holdout), (row("b"), Source::Diagnostic)]).unwrap();
        let text = std::fs::read_to_string(&to_label).unwrap();
        assert!(!text.contains("called_toxic") && !text.contains("model"), "no votes shown: {text}");
        std::fs::write(&to_label, text.replacen(r#""label":"""#, r#""label":"toxic""#, 1)).unwrap();
        assert_eq!(merge_labels(&to_label, &labels).unwrap(), 1);
        assert_eq!(load_labels(&labels).unwrap()["a"].source, Source::Holdout);
        assert_eq!(merge_labels(&to_label, &labels).unwrap(), 0, "already merged ids are skipped");
        std::fs::write(&to_label, r#"{"id":"c","content":"x","source":"holdout","label":"maybe","note":""}"#).unwrap();
        assert!(merge_labels(&to_label, &labels).is_err());
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::labelling`
Expected: compile error.

- [ ] **Step 3: Implement** `src/cli/eval_classifiers/labelling.rs`:

```rust
//! Choosing what a human labels next, in two streams kept strictly apart:
//!
//! - **Holdout** — a fixed, hash-chosen third of the pool, labelled whatever
//!   the models say. Only these labels feed the headline rates: posts picked
//!   BECAUSE models disagreed are outcome-dependent and would bias the rates
//!   and invalidate the intervals (Codex review).
//! - **Diagnostic** — disagreements, plus a stable ~10 % of agreed posts to
//!   catch blind spots every model shares. Useful for understanding models;
//!   never mixed into the headline numbers.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use super::cache::VerdictRecord;
use super::data::{load_labels, Answer, FixtureRow, Label, Source, Truth};
use super::fnv1a;

/// Salted so holdout membership is independent of the diagnostic sample.
pub fn in_holdout(id: &str) -> bool {
    fnv1a(format!("holdout:{id}").as_bytes()) % 3 == 0
}

pub fn sampled(id: &str) -> bool {
    fnv1a(id.as_bytes()) % 10 == 0
}

pub fn pick_for_labelling(
    pool: &[FixtureRow],
    per_model: &[HashMap<String, VerdictRecord>],
    labels: &HashMap<String, Answer>,
) -> Vec<(FixtureRow, Source)> {
    let mut out = Vec::new();
    for row in pool.iter().filter(|r| !labels.contains_key(&r.id)) {
        if in_holdout(&row.id) {
            out.push((row.clone(), Source::Holdout));
            continue;
        }
        let calls: Vec<bool> = per_model
            .iter()
            .filter_map(|m| m.get(&row.id).and_then(|r| r.called_toxic))
            .collect();
        if calls.len() < 2 {
            continue;
        }
        let disagree = calls.iter().any(|c| *c != calls[0]);
        if disagree || sampled(&row.id) {
            out.push((row.clone(), Source::Diagnostic));
        }
    }
    out
}

#[derive(Serialize)]
struct ToLabel<'a> {
    id: &'a str,
    content: &'a str,
    source: Source,
    label: &'a str,
    note: &'a str,
}

/// Blind on purpose: no model votes, so they cannot anchor the answer.
pub fn write_to_label(path: &Path, rows: &[(FixtureRow, Source)]) -> Result<()> {
    let mut f = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    for (r, source) in rows {
        let line = ToLabel { id: &r.id, content: &r.content, source: *source, label: "", note: "" };
        writeln!(f, "{}", serde_json::to_string(&line)?)?;
    }
    Ok(())
}

pub fn merge_labels(to_label: &Path, labels_path: &Path) -> Result<usize> {
    let existing = load_labels(labels_path)?;
    let text = std::fs::read_to_string(to_label).with_context(|| format!("reading {}", to_label.display()))?;
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(labels_path)
        .with_context(|| format!("opening {}", labels_path.display()))?;
    let mut merged = 0;
    for (i, l) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let row: serde_json::Value = serde_json::from_str(l)?;
        let id = row["id"].as_str().context("row without id")?;
        let answer = row["label"].as_str().unwrap_or("").trim();
        if answer.is_empty() || existing.contains_key(id) {
            continue;
        }
        let truth = match answer {
            "toxic" => Truth::Toxic,
            "clean" => Truth::Clean,
            other => bail!("{} line {}: label {other:?} must be toxic or clean", to_label.display(), i + 1),
        };
        let source: Source = serde_json::from_value(row["source"].clone())
            .with_context(|| format!("{} line {}: missing or bad source", to_label.display(), i + 1))?;
        let note = row["note"].as_str().filter(|n| !n.is_empty()).map(str::to_string);
        let label = Label { id: id.to_string(), label: truth, source, note };
        writeln!(out, "{}", serde_json::to_string(&label)?)?;
        merged += 1;
    }
    Ok(merged)
}
```

Add `pub mod labelling;` to `mod.rs`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --lib eval_classifiers::labelling`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/cli/eval_classifiers/labelling.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): label picker with an independent holdout and separate diagnostics'
```

---

### Task 8: The report — and proof it catches bad models

**Files:**
- Create: `src/cli/eval_classifiers/report.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod report;`)

**Interfaces:**
- Consumes: `metrics::{model_metrics, paired, Comparison, ModelMetrics, Paired, Rate}`, `cache::VerdictRecord`, `cost::{estimate_usd, Price}`, `data::{Answer, Source, Truth}`.
- Produces:
  - `pub struct ModelReport { pub name: String, pub calls: HashMap<String, Option<bool>>, pub holdout: ModelMetrics, pub diagnostic: ModelMetrics, pub usd_per_1k: Option<f64>, pub p50_ms: Option<u32>, pub p95_ms: Option<u32> }`
  - `pub fn labels_of(answers: &HashMap<String, Answer>, source: Source) -> HashMap<String, Truth>`
  - `pub fn build(models: &[(String, HashMap<String, VerdictRecord>)], answers: &HashMap<String, Answer>, prices: &HashMap<String, Price>) -> Vec<ModelReport>`
  - `pub fn vs_baseline(baseline: &ModelReport, candidate: &ModelReport, answers: &HashMap<String, Answer>) -> Paired` — paired, holdout labels only
  - `pub fn render(reports: &[ModelReport], answers: &HashMap<String, Answer>, baseline: &str) -> String`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::eval_classifiers::cache::VerdictRecord;
    use crate::cli::eval_classifiers::metrics::Comparison;

    /// 40 toxic + 40 clean posts, all HOLDOUT.
    fn holdout() -> HashMap<String, Answer> {
        (0..80)
            .map(|i| (format!("p{i}"), Answer { truth: if i < 40 { Truth::Toxic } else { Truth::Clean }, source: Source::Holdout }))
            .collect()
    }

    fn model(name: &str, decide: impl Fn(usize) -> Option<bool>) -> (String, HashMap<String, VerdictRecord>) {
        let recs = (0..80)
            .map(|i| {
                let id = format!("p{i}");
                let call = decide(i);
                let r = VerdictRecord { id: id.clone(), model: name.into(), content_hash: String::new(), model_id: name.into(), policy_version: "p".into(), called_toxic: call, error: call.is_none().then(|| "timed out".into()), latency_ms: 10 + i as u32, input_chars: 400, est_usd: 0.0 };
                (id, r)
            })
            .collect();
        (name.to_string(), recs)
    }

    /// Good baseline: misses p0,p1; false alarms on p40,p41.
    fn good(i: usize) -> Option<bool> {
        Some((2..40).contains(&i) || (40..42).contains(&i))
    }

    #[test]
    fn deliberately_bad_models_are_caught() {
        let reports = build(
            &[model("cope-b", good), model("always-clean", |_| Some(false)), model("coin-flip", |i| Some(i % 2 == 0))],
            &holdout(),
            &HashMap::new(),
        );
        let p = vs_baseline(&reports[0], &reports[1], &holdout());
        assert_eq!(p.miss, Comparison::Worse, "always-clean misses every toxic post");
        let p = vs_baseline(&reports[0], &reports[2], &holdout());
        assert!(p.false_alarm == Comparison::Worse || p.miss == Comparison::Worse, "coin-flip must lose on a rate");
        let text = render(&reports, &holdout(), "cope-b");
        assert!(text.contains("always-clean: false alarms") && text.contains("misses worse"), "{text}");
    }

    #[test]
    fn close_models_are_too_close_to_call() {
        let a = model("cope-b", |i| Some(i < 39 || i == 79));
        let b = model("other", |i| Some(i < 38 || i == 79));
        let reports = build(&[a, b], &holdout(), &HashMap::new());
        let p = vs_baseline(&reports[0], &reports[1], &holdout());
        assert_eq!((p.false_alarm, p.miss), (Comparison::TooClose, Comparison::TooClose));
        assert!(render(&reports, &holdout(), "cope-b").contains("too close to call — label more"));
    }

    /// Codex review: errors on the hard posts must not buy a better score.
    #[test]
    fn a_model_erroring_on_hard_posts_gets_no_verdict() {
        let dodger = model("dodger", |i| if i < 10 { None } else { Some(i < 40) });
        let reports = build(&[model("cope-b", good), dodger], &holdout(), &HashMap::new());
        let p = vs_baseline(&reports[0], &reports[1], &holdout());
        assert!(p.low_coverage, "70/80 = 87.5 % coverage");
        assert!(render(&reports, &holdout(), "cope-b").contains("coverage below 90 %"));
    }

    /// Codex review: only holdout labels feed the headline rates.
    #[test]
    fn diagnostic_and_seed_labels_do_not_move_the_headline() {
        let mut answers = holdout();
        for i in 80..100 {
            answers.insert(format!("p{i}"), Answer { truth: Truth::Toxic, source: Source::Diagnostic });
        }
        for i in 100..105 {
            answers.insert(format!("p{i}"), Answer { truth: Truth::Clean, source: Source::Seed });
        }
        let (name, mut recs) = model("cope-b", good);
        for i in 80..105 {
            let id = format!("p{i}");
            let mut r = recs["p0"].clone();
            r.id = id.clone();
            r.called_toxic = Some(i >= 100);
            recs.insert(id, r);
        }
        let r = &build(&[(name, recs)], &answers, &HashMap::new())[0];
        assert_eq!((r.holdout.miss.hits, r.holdout.miss.n), (2, 40), "headline = holdout only");
        assert_eq!((r.diagnostic.miss.hits, r.diagnostic.miss.n), (20, 20));
    }

    #[test]
    fn cost_and_latency_columns() {
        let prices: HashMap<String, Price> = [("jev".to_string(), Price { usd_per_post: None, usd_per_million_input_tokens: Some(0.042), overhead_chars: 0 })].into();
        let r = &build(&[model("jev", |_| Some(true))], &holdout(), &prices)[0];
        // 400 chars ≈ 100 tokens; 1,000 posts ≈ 100k tokens ≈ $0.0042.
        assert!((r.usd_per_1k.unwrap() - 0.0042).abs() < 1e-9);
        // Latencies are 10..=89 ms: nearest-rank p50 = 49, p95 = 85.
        assert_eq!((r.p50_ms, r.p95_ms), (Some(49), Some(85)));
        let unpriced = &build(&[model("x", |_| Some(true))], &holdout(), &HashMap::new())[0];
        assert_eq!(unpriced.usd_per_1k, None);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::report`
Expected: compile error.

- [ ] **Step 3: Implement** `src/cli/eval_classifiers/report.rs`:

```rust
//! The report. Headline rates come from HOLDOUT labels only, comparisons are
//! paired on the posts both models answered, and every rate shows its 95 %
//! interval — never a blended score (spec).

use std::collections::HashMap;
use std::fmt::Write;

use super::cache::VerdictRecord;
use super::cost::{estimate_usd, Price};
use super::data::{Answer, Source, Truth};
use super::metrics::{model_metrics, paired, Comparison, ModelMetrics, Paired, Rate};

#[derive(Debug, Clone)]
pub struct ModelReport {
    pub name: String,
    pub calls: HashMap<String, Option<bool>>,
    pub holdout: ModelMetrics,
    pub diagnostic: ModelMetrics,
    pub usd_per_1k: Option<f64>,
    pub p50_ms: Option<u32>,
    pub p95_ms: Option<u32>,
}

pub fn labels_of(answers: &HashMap<String, Answer>, source: Source) -> HashMap<String, Truth> {
    answers
        .iter()
        .filter(|(_, a)| a.source == source)
        .map(|(id, a)| (id.clone(), a.truth))
        .collect()
}

/// Nearest-rank percentile.
fn percentile(sorted: &[u32], pct: usize) -> Option<u32> {
    if sorted.is_empty() {
        return None;
    }
    let idx = (sorted.len() * pct).div_ceil(100).saturating_sub(1);
    sorted.get(idx).copied()
}

pub fn build(
    models: &[(String, HashMap<String, VerdictRecord>)],
    answers: &HashMap<String, Answer>,
    prices: &HashMap<String, Price>,
) -> Vec<ModelReport> {
    let holdout = labels_of(answers, Source::Holdout);
    let diagnostic = labels_of(answers, Source::Diagnostic);
    models
        .iter()
        .map(|(name, recs)| {
            let calls: HashMap<String, Option<bool>> =
                recs.iter().map(|(id, r)| (id.clone(), r.called_toxic)).collect();
            let ok: Vec<&VerdictRecord> = recs.values().filter(|r| r.called_toxic.is_some()).collect();
            let mut lat: Vec<u32> = ok.iter().map(|r| r.latency_ms).collect();
            lat.sort_unstable();
            let usd_per_1k = prices.get(name).filter(|_| !ok.is_empty()).map(|p| {
                let mean_chars = ok.iter().map(|r| r.input_chars).sum::<usize>() / ok.len();
                estimate_usd(p, mean_chars) * 1000.0
            });
            ModelReport {
                name: name.clone(),
                holdout: model_metrics(name, &calls, &holdout),
                diagnostic: model_metrics(name, &calls, &diagnostic),
                calls,
                usd_per_1k,
                p50_ms: percentile(&lat, 50),
                p95_ms: percentile(&lat, 95),
            }
        })
        .collect()
}

pub fn vs_baseline(baseline: &ModelReport, candidate: &ModelReport, answers: &HashMap<String, Answer>) -> Paired {
    paired(
        &baseline.name,
        &baseline.calls,
        &candidate.name,
        &candidate.calls,
        &labels_of(answers, Source::Holdout),
    )
}

fn rate_cell(r: &Rate) -> String {
    match (r.value(), r.wilson95()) {
        (Some(v), Some((lo, hi))) => {
            format!("{:>5.1}% [{:.0}–{:.0}] ({}/{})", v * 100.0, lo * 100.0, hi * 100.0, r.hits, r.n)
        }
        _ => "no labels".to_string(),
    }
}

fn word(c: Comparison) -> &'static str {
    match c {
        Comparison::Better => "better",
        Comparison::Worse => "worse",
        Comparison::TooClose => "too close to call — label more",
        Comparison::NoData => "no data",
    }
}

pub fn render(reports: &[ModelReport], answers: &HashMap<String, Answer>, baseline: &str) -> String {
    let mut s = String::new();
    let count = |src: Source| answers.values().filter(|a| a.source == src).count();
    let _ = writeln!(
        s,
        "Labels: {} holdout (scored), {} diagnostic, {} seed (not scored)\n",
        count(Source::Holdout),
        count(Source::Diagnostic),
        count(Source::Seed)
    );
    let _ = writeln!(s, "HOLDOUT — the headline numbers");
    let _ = writeln!(
        s,
        "{:<14} {:<28} {:<28} {:>9} {:>6} {:>10} {:>12}",
        "model", "false alarms (clean→toxic)", "misses (toxic→clean)", "coverage", "errors", "$/1k posts", "p50/p95 ms"
    );
    for r in reports {
        let m = &r.holdout;
        let cov = m.coverage().map_or("-".to_string(), |c| format!("{:.0}%", c * 100.0));
        let cost = r.usd_per_1k.map_or("unpriced".to_string(), |c| format!("{c:.4}"));
        let lat = match (r.p50_ms, r.p95_ms) {
            (Some(a), Some(b)) => format!("{a}/{b}"),
            _ => "-".to_string(),
        };
        let _ = writeln!(
            s,
            "{:<14} {:<28} {:<28} {:>9} {:>6} {:>10} {:>12}",
            r.name, rate_cell(&m.false_alarm), rate_cell(&m.miss), cov, m.errors, cost, lat
        );
    }
    match reports.iter().find(|r| r.name == baseline) {
        Some(base) => {
            let _ = writeln!(s, "\nCompared with {baseline} (paired: holdout posts both answered):");
            for r in reports.iter().filter(|r| r.name != baseline) {
                let p = vs_baseline(base, r, answers);
                if p.low_coverage {
                    let _ = writeln!(s, "  {}: no verdict — coverage below 90 %", r.name);
                } else {
                    let _ = writeln!(s, "  {}: false alarms {}; misses {}", r.name, word(p.false_alarm), word(p.miss));
                }
            }
        }
        None => {
            let _ = writeln!(s, "\n(baseline {baseline:?} has no verdicts — run it first)");
        }
    }
    let _ = writeln!(s, "\nWHERE MODELS DIFFER — diagnostic labels, picked by disagreement: NOT population rates");
    for r in reports {
        let m = &r.diagnostic;
        let _ = writeln!(s, "  {:<14} wrong on {}/{} diagnostic posts", r.name, m.false_alarm.hits + m.miss.hits, m.judged);
    }
    s
}
```

Add `pub mod report;` to `mod.rs`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --lib eval_classifiers::report`
Expected: 5 passed. If `deliberately_bad_models_are_caught` fails on the coin-flip arm, print `render(...)` and check its rates against the good model's intervals; do **not** loosen the assertion — fix the fake.

- [ ] **Step 5: Commit**

```bash
git add src/cli/eval_classifiers/report.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): holdout-only, paired eval report; proven to catch bad models'
```

---

### Task 9: Detoxify as a classifier, and the eval model registry

**Files:**
- Create: `src/toxicity/detoxify_classifier.rs`
- Modify: `src/toxicity/mod.rs` (add `pub mod detoxify_classifier;`)
- Create: `src/cli/eval_classifiers/models.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod models;`)

**Interfaces:**
- Consumes: `OnnxToxicityScorer::load(&Path)`, `ToxicityScorer::score_text`, `download::resolve_model_dir()`, `classifier::build_backend_named`.
- Produces:
  - `pub struct DetoxifyClassifier<S: ToxicityScorer> { scorer: S }` with `pub fn new(scorer: S) -> Self` and a `ToxicityClassifier` impl: `toxic_token = toxicity >= 0.5`
  - `pub const KNOWN_MODELS: &[&str] = &["cope-b", "detoxify"];`
  - `pub fn build_eval_model(name: &str) -> Result<Arc<dyn ToxicityClassifier>>`

- [ ] **Step 1: Write the failing tests** — in `src/toxicity/detoxify_classifier.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::toxicity::traits::{ToxicityAttributes, ToxicityResult};

    struct Fixed(f64);
    #[async_trait::async_trait]
    impl ToxicityScorer for Fixed {
        async fn score_text(&self, _text: &str) -> anyhow::Result<ToxicityResult> {
            Ok(ToxicityResult { toxicity: self.0, attributes: ToxicityAttributes::default() })
        }
    }

    #[tokio::test]
    async fn the_answer_is_detoxifys_own_half_way_cut() {
        assert!(DetoxifyClassifier::new(Fixed(0.8)).classify("x").await.unwrap().toxic_token);
        assert!(!DetoxifyClassifier::new(Fixed(0.3)).classify("x").await.unwrap().toxic_token);
    }
}
```

Read `src/toxicity/traits.rs` around line 62 first: if `ToxicityScorer` has other required methods, implement them on `Fixed` with constants.

In `src/cli/eval_classifiers/models.rs`:

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn unknown_models_are_named_in_the_error() {
        let err = match super::build_eval_model("nope") {
            Ok(_) => panic!("unknown model accepted"),
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains("cope-b") && err.contains("detoxify"), "{err}");
    }
}
```

- [ ] **Step 2: Run to verify they fail** — one filter per command:

Run: `cargo test --lib detoxify_classifier`
Expected: compile error.
Run: `cargo test --lib eval_classifiers::models`
Expected: compile error.

- [ ] **Step 3: Implement** `src/toxicity/detoxify_classifier.rs`:

```rust
//! Detoxify (the local ONNX toxicity model) behind the classifier trait, so
//! the eval can use it as the free floor every paid model must beat.

use anyhow::Result;
use async_trait::async_trait;
use std::time::Instant;

use super::classifier::{ClassifierVerdict, ToxicityClassifier};
use super::traits::ToxicityScorer;

pub struct DetoxifyClassifier<S: ToxicityScorer> {
    scorer: S,
}

impl<S: ToxicityScorer> DetoxifyClassifier<S> {
    pub fn new(scorer: S) -> Self {
        Self { scorer }
    }
}

#[async_trait]
impl<S: ToxicityScorer> ToxicityClassifier for DetoxifyClassifier<S> {
    async fn classify(&self, content: &str) -> Result<ClassifierVerdict> {
        let started = Instant::now();
        let t = self.scorer.score_text(content).await?.toxicity as f32;
        let toxic = t >= 0.5;
        Ok(ClassifierVerdict {
            toxic_token: toxic,
            confidence: if toxic { t } else { 1.0 - t },
            latency_ms: started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32,
            model_id: self.model_id().to_string(),
            policy_version: self.policy_version().to_string(),
        })
    }
    fn name(&self) -> &'static str {
        "detoxify"
    }
    fn model_id(&self) -> &'static str {
        "unbiased-toxic-roberta"
    }
    fn policy_version(&self) -> &'static str {
        "none"
    }
    fn threshold(&self) -> f32 {
        0.5
    }
}
```

`src/cli/eval_classifiers/models.rs`:

```rust
//! The eval's model registry. Eval-only by design: nothing here is reachable
//! from production `build_from_env` (spec: safety). Part B adds jev,
//! clef-flash, clef and strands-decider here.

use anyhow::{bail, Result};
use std::sync::Arc;

use crate::toxicity::classifier::{build_backend_named, ToxicityClassifier};
use crate::toxicity::detoxify_classifier::DetoxifyClassifier;
use crate::toxicity::onnx::OnnxToxicityScorer;

pub const KNOWN_MODELS: &[&str] = &["cope-b", "detoxify"];

pub fn build_eval_model(name: &str) -> Result<Arc<dyn ToxicityClassifier>> {
    match name {
        "cope-b" => build_backend_named("runpod"),
        "detoxify" => {
            let dir = crate::toxicity::download::resolve_model_dir();
            Ok(Arc::new(DetoxifyClassifier::new(OnnxToxicityScorer::load(&dir)?)))
        }
        other => bail!("unknown eval model {other:?} (known: {})", KNOWN_MODELS.join(", ")),
    }
}
```

Add `pub mod detoxify_classifier;` to `src/toxicity/mod.rs` and `pub mod models;` to `src/cli/eval_classifiers/mod.rs`. Confirm `OnnxToxicityScorer::load` expects the **base** model dir (the toxicity model sits at the base — `src/toxicity/download.rs:128`); if it expects a subdirectory, pass that.

- [ ] **Step 4: Run to verify they pass** — one filter per command:

Run: `cargo test --lib detoxify_classifier`
Expected: 1 passed.
Run: `cargo test --lib eval_classifiers::models`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add src/toxicity/detoxify_classifier.rs src/toxicity/mod.rs src/cli/eval_classifiers/models.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): Detoxify as an eval classifier and the eval-only model registry'
```

---

### Task 10: CLI wiring, README and CHANGELOG

**Files:**
- Modify: `src/main.rs` (new `EvalClassifiers` subcommand next to `ClassifyGate`; handler next to `Commands::ClassifyGate`)
- Create: `evals/classifiers/README.md`
- Modify: `CHANGELOG.md` (`### Added` under `[Unreleased]`)
- Test: `tests/cli_eval_classifiers.rs`

**Interfaces:**
- Consumes: every earlier task.
- Produces: `charcoal eval-classifiers run --models cope-b,detoxify [--cap-usd 1.0] [--timeout-secs 60]`, `charcoal eval-classifiers label [--merge]`, `charcoal eval-classifiers report [--baseline cope-b]`; all take `--dir` (default `evals/classifiers`).

- [ ] **Step 1: Write the end-to-end test** `tests/cli_eval_classifiers.rs`:

```rust
use charcoal::cli::eval_classifiers::{
    cache,
    cost::{Budget, Price},
    data, labelling, report, runner,
};
use charcoal::toxicity::classifier::{ClassifierVerdict, ToxicityClassifier};
use std::collections::HashMap;

struct Says(bool);
#[async_trait::async_trait]
impl ToxicityClassifier for Says {
    async fn classify(&self, content: &str) -> anyhow::Result<ClassifierVerdict> {
        let toxic = self.0 && content.contains("bad");
        Ok(ClassifierVerdict { toxic_token: toxic, confidence: 0.9, latency_ms: 3, model_id: "f".into(), policy_version: "p".into() })
    }
    fn name(&self) -> &'static str { "fake" }
    fn model_id(&self) -> &'static str { "f" }
    fn policy_version(&self) -> &'static str { "p" }
    fn threshold(&self) -> f32 { 0.5 }
}

#[tokio::test]
async fn a_full_round_runs_picks_merges_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    // One holdout id and one non-holdout, non-sampled id, so the round is deterministic.
    let h = (0..200).map(|i| format!("h{i}")).find(|id| labelling::in_holdout(id)).unwrap();
    let d = (0..200)
        .map(|i| format!("d{i}"))
        .find(|id| !labelling::in_holdout(id) && !labelling::sampled(id))
        .unwrap();
    let pool_path = dir.path().join("pool.jsonl");
    std::fs::write(
        &pool_path,
        format!("{{\"id\":\"{h}\",\"content\":\"bad\"}}\n{{\"id\":\"{d}\",\"content\":\"bad too\"}}\n"),
    )
    .unwrap();
    let pool = data::load_pool(&pool_path).unwrap();
    let verdicts = dir.path().join("verdicts");
    let mut budget = Budget::new(1.0);
    for (name, m) in [("good", Says(true)), ("lazy", Says(false))] {
        runner::run_model(name, &m, &pool, &verdicts, &Price::default(), &mut budget, runner::DEFAULT_TIMEOUT)
            .await
            .unwrap();
    }
    let per_model = vec![cache::load(&verdicts, "good").unwrap(), cache::load(&verdicts, "lazy").unwrap()];
    let labels_path = dir.path().join("labels.jsonl");
    let picked = labelling::pick_for_labelling(&pool, &per_model, &data::load_labels(&labels_path).unwrap());
    assert_eq!(picked.len(), 2, "the holdout post, and the disagreement");

    let to_label = dir.path().join("to-label.jsonl");
    labelling::write_to_label(&to_label, &picked).unwrap();
    let answered = std::fs::read_to_string(&to_label).unwrap().replace("\"label\":\"\"", "\"label\":\"toxic\"");
    std::fs::write(&to_label, answered).unwrap();
    assert_eq!(labelling::merge_labels(&to_label, &labels_path).unwrap(), 2);

    let answers = data::load_labels(&labels_path).unwrap();
    let models = vec![("good".to_string(), per_model[0].clone()), ("lazy".to_string(), per_model[1].clone())];
    let text = report::render(&report::build(&models, &answers, &HashMap::new()), &answers, "good");
    assert!(text.contains("1 holdout (scored), 1 diagnostic"), "{text}");
    assert!(text.contains("lazy") && text.contains("(1/1)"), "{text}");
}
```

- [ ] **Step 2: Run it**

Run: `cargo test --test cli_eval_classifiers`
Expected: PASS if Tasks 2–8 are complete (it guards the wiring contract). If it fails, fix the library — not the test.

- [ ] **Step 3: Add the subcommand** to `enum Commands` in `src/main.rs`:

```rust
    /// Compare classifiers on labelled posts (#404): run, pick posts to
    /// label, and report false alarms, misses, coverage, cost and latency.
    EvalClassifiers {
        #[command(subcommand)]
        action: EvalAction,
        /// Data directory (pool.jsonl, labels.jsonl, pricing.json, verdicts/).
        #[arg(long, default_value = "evals/classifiers", global = true)]
        dir: std::path::PathBuf,
    },
```

Below `enum Commands`:

```rust
#[derive(Subcommand)]
enum EvalAction {
    /// Run models over the pool, reusing cached verdicts that still match.
    Run {
        /// Comma-separated model names.
        #[arg(long, value_delimiter = ',', default_value = "cope-b,detoxify")]
        models: Vec<String>,
        /// Stop before spending more than this many US dollars this run.
        #[arg(long, default_value_t = 1.0)]
        cap_usd: f64,
        /// Per-call timeout; a call past it is recorded as an error.
        #[arg(long, default_value_t = 60)]
        timeout_secs: u64,
    },
    /// Write to-label.jsonl, or with --merge fold your answers into labels.jsonl.
    Label {
        #[arg(long)]
        merge: bool,
    },
    /// Print the comparison report.
    Report {
        #[arg(long, default_value = "cope-b")]
        baseline: String,
    },
}
```

Handler next to `Commands::ClassifyGate`:

```rust
        Commands::EvalClassifiers { action, dir } => {
            use charcoal::cli::eval_classifiers::{cache, cost, data, labelling, models, report, runner};
            let verdicts = dir.join("verdicts");
            let pool = data::load_pool(&dir.join("pool.jsonl"))?;
            let labels_path = dir.join("labels.jsonl");
            let known: Vec<String> = models::KNOWN_MODELS.iter().map(|s| s.to_string()).collect();
            match action {
                EvalAction::Run { models: names, cap_usd, timeout_secs } => {
                    let prices = cost::load_pricing(&dir.join("pricing.json"))?;
                    let mut budget = cost::Budget::new(cap_usd);
                    let timeout = std::time::Duration::from_secs(timeout_secs);
                    for name in &names {
                        // Refuse an unpriced model before building (or paying for) anything.
                        let price = cost::price_for(&prices, name)?;
                        let classifier = match models::build_eval_model(name) {
                            Ok(c) => c,
                            Err(e) => {
                                // A missing key skips that model, not the run.
                                eprintln!("skipping {name}: {e:#}");
                                continue;
                            }
                        };
                        let s = runner::run_model(name, classifier.as_ref(), &pool, &verdicts, price, &mut budget, timeout).await?;
                        println!(
                            "{name}: judged {}, cached {}, errors {}{}",
                            s.judged,
                            s.cached,
                            s.errors,
                            if s.stopped_at_cap { " — STOPPED AT SPENDING CAP" } else { "" }
                        );
                        if s.stopped_at_cap {
                            break;
                        }
                    }
                    println!("estimated spend this run: ${:.4} (cap ${cap_usd:.2})", budget.spent());
                }
                EvalAction::Label { merge } => {
                    let to_label = dir.join("to-label.jsonl");
                    if merge {
                        let n = labelling::merge_labels(&to_label, &labels_path)?;
                        println!("merged {n} labels into {}", labels_path.display());
                    } else {
                        let per_model: Vec<_> = known
                            .iter()
                            .map(|m| cache::load(&verdicts, m))
                            .collect::<anyhow::Result<_>>()?;
                        let answers = data::load_labels(&labels_path)?;
                        let picked = labelling::pick_for_labelling(&pool, &per_model, &answers);
                        let holdout = picked.iter().filter(|(_, s)| *s == data::Source::Holdout).count();
                        labelling::write_to_label(&to_label, &picked)?;
                        println!(
                            "{} posts to label ({holdout} holdout, {} diagnostic) in {} — set label to toxic or clean, then run with --merge",
                            picked.len(),
                            picked.len() - holdout,
                            to_label.display()
                        );
                    }
                }
                EvalAction::Report { baseline } => {
                    let answers = data::load_labels(&labels_path)?;
                    let prices = cost::load_pricing(&dir.join("pricing.json"))?;
                    let mut loaded = Vec::new();
                    for m in &known {
                        let recs = cache::load(&verdicts, m)?;
                        if !recs.is_empty() {
                            loaded.push((m.clone(), recs));
                        }
                    }
                    print!("{}", report::render(&report::build(&loaded, &answers, &prices), &answers, &baseline));
                }
            }
        }
```

- [ ] **Step 4: Write** `evals/classifiers/README.md`:

````markdown
# Classifier eval (#404)

Compares classifiers on the same posts. Design:
`docs/superpowers/specs/2026-10-08-404-classifier-eval-design.md`.

One round:

```bash
# 1. Judge every post with each model (cached; $1 cap; 60 s per-call timeout)
cargo run --features web -- eval-classifiers run --models cope-b,detoxify
# 2. Write the posts worth labelling to evals/classifiers/to-label.jsonl
cargo run --features web -- eval-classifiers label
#    ...set each "label" to toxic or clean (optional "note"); leave "source" alone...
# 3. Fold your answers into labels.jsonl
cargo run --features web -- eval-classifiers label --merge
# 4. Compare
cargo run --features web -- eval-classifiers report
```

How to read it:

- **Holdout** posts (a fixed third of the pool, picked by hash) are the only
  ones scored. **Diagnostic** posts — where models disagree — appear in a
  separate "where models differ" section; they are not population rates.
- **too close to call — label more**: the two models' 95 % intervals overlap.
- **no verdict — coverage below 90 %**: a model failed on too many posts to
  compare fairly.
- Every model must have an entry in `pricing.json`; an unpriced model is refused.
- CoPE-B needs `RUNPOD_ENDPOINT_URL` and `RUNPOD_API_KEY`; Detoxify needs the
  local models (`charcoal download-model`). A model missing its keys is skipped.
- Verdicts live in `verdicts/` and are never committed (its `.gitignore`).
````

- [ ] **Step 5: Add to `CHANGELOG.md`** under `## [Unreleased]` → `### Added`:

```markdown
- #404 — `charcoal eval-classifiers`: compares classifiers (today CoPE-B and
  Detoxify; Jev, Clef and Strands Decider follow in Part B) on the same posts.
  Headline false-alarm and miss rates come from a fixed random holdout, each
  with a 95 % interval, and models are compared only on posts both answered —
  "too close to call — label more" when the intervals overlap, no verdict at
  all below 90 % coverage. Posts where models disagree are labelled too, but
  reported separately as diagnostics, never as rates. Every model is scored on
  its own yes/no answer (CoPE-B's confidence is not a probability of toxic).
  Verdicts are cached and reused only while the post, model and policy are
  unchanged; every call has a timeout; unpriced models are refused and a $1
  per-run cap stops a run cleanly. Tests prove the report catches a
  deliberately bad always-clean and coin-flip model, and a model that hides
  its misses behind errors.
```

- [ ] **Step 6: Verify** — one filter per command:

```bash
cargo fmt
cargo clippy --features web --all-targets
cargo test --lib eval_classifiers
cargo test --lib detoxify_classifier
cargo test --test cli_eval_classifiers
cargo run --features web -- eval-classifiers report
```

Expected: clippy clean; all tests pass; the report prints `Labels: 0 holdout (scored), 0 diagnostic, 10 seed (not scored)` and the "run it first" baseline line.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs tests/cli_eval_classifiers.rs evals/classifiers/README.md CHANGELOG.md
git commit -m 'feat(404): charcoal eval-classifiers run|label|report'
```

---

## After Part A

1. **First live round (by hand, with Bryan's go-ahead to spend up to $1):** `run --models cope-b,detoxify`, then `label`, Bryan labels, `label --merge`, `report`. With 36 holdout posts (of 110) the first report will mostly say "too close to call" — expected; rounds add labels.
2. **Part B plan**, written from `docs/superpowers/specs/404-provider-contracts.md`. Each unblocked provider gets a `ToxicityClassifier` adapter with wiremock tests from the verified request/response shapes, registered in `models::build_eval_model` and priced in `pricing.json` with `overhead_chars` = the instruction's length. Part B must also:
   - send the full text of `gpu/cope-b-runpod/policy.txt` as the instruction, with answers `toxic` / `clean` (fairness rule 2);
   - record provider-reported token usage per call and report estimated vs actual spend, refusing a provider whose usage cannot be read (Codex review);
   - expose a probability only where the provider's is normalised over the answer set.
