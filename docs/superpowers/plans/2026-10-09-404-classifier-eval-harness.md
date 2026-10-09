# Classifier Eval Harness Implementation Plan (#404, Part A)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `charcoal eval-classifiers` command that runs labelled and unlabelled posts through several classifiers, picks the posts worth hand-labelling, and reports each model's false-alarm rate, miss rate, cost and latency with honest error bars.

**Architecture:** A new library module `src/cli/eval_classifiers/` (pure, unit-tested pieces: data, verdict cache, cost cap, metrics, label picker, report, runner) behind a thin clap subcommand in `src/main.rs`. Models plug in through the existing `ToxicityClassifier` trait; the eval reuses `classify_compare::FixtureRow` and the eval-only `build_backend_named`. Part A ships the harness with the two models we already have — CoPE-B (RunPod) and Detoxify (local ONNX) — and verifies the new providers' APIs (Task 1). **Part B** (Jev, Clef-flash, Strands Decider adapters) is a separate short plan written from Task 1's verified contracts, so no adapter is coded against a guessed API.

**Tech Stack:** Rust (tokio, async-trait, serde/serde_json, anyhow), existing `ort` ONNX scorer, `tempfile` for tests. No new dependencies.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-10-08-404-classifier-eval-design.md` (approved 2026-10-09).
- Post level only: one verdict per post; account-level evaluation is out of scope.
- A post is **toxic** when the model's probability of toxic is **≥ 0.5**, identically for every model. `p_toxic = confidence` if `toxic_token`, else `1 − confidence`.
- Report false-alarm rate and miss rate **separately**, each with a **95 % Wilson interval**; **no blended score**.
- When two models' intervals overlap on a metric, the report says **"too close to call — label more"**.
- Errors and timeouts are recorded as **errors, never as clean**.
- Default spending cap **$1.00 per run**; reaching it stops cleanly and keeps every paid verdict.
- New adapters are reachable **only** from eval commands (`build_backend_named` / the eval model registry), never from production `build_from_env`.
- **CI never calls a paid API.** All tests use fake classifiers; live models are exercised only by hand.
- Data: `evals/classifiers/pool.jsonl` and `labels.jsonl` committed (PII-scrubbed); `evals/classifiers/verdicts/` local only via `.gitignore.local`.
- No new crate dependencies (no `toml`, no `rand`): pricing is JSON; sampling is hash-based.
- Charcoal conventions: `anyhow::Result`, `?` not `.unwrap()` outside tests, comments explain *why*, conventional commits, explicit `git add <paths>`.

---

## File Structure

| File | Responsibility |
|---|---|
| `docs/superpowers/specs/404-provider-contracts.md` | Task 1: verified API facts for Jev, Clef-flash, Strands Decider |
| `evals/classifiers/pool.jsonl` | Posts to evaluate (`FixtureRow` lines) |
| `evals/classifiers/labels.jsonl` | Human answers: `{id, label, note}` |
| `evals/classifiers/pricing.json` | List prices per model |
| `evals/classifiers/README.md` | How to run a round |
| `src/cli/eval_classifiers/mod.rs` | Module root, re-exports |
| `src/cli/eval_classifiers/data.rs` | Load pool and labels |
| `src/cli/eval_classifiers/cache.rs` | `VerdictRecord`, append-only JSONL cache, last-record-wins load |
| `src/cli/eval_classifiers/cost.rs` | Pricing, per-call estimate, spending cap |
| `src/cli/eval_classifiers/metrics.rs` | Wilson interval, per-model rates, comparisons |
| `src/cli/eval_classifiers/labelling.rs` | Disagreement + 10 % agreed-sample picker, merge |
| `src/cli/eval_classifiers/report.rs` | Plain-text report |
| `src/cli/eval_classifiers/runner.rs` | Run models over the pool with cache and cap |
| `src/cli/eval_classifiers/models.rs` | Eval model registry (name → classifier) |
| `src/toxicity/detoxify_classifier.rs` | Detoxify as a `ToxicityClassifier` |
| `src/main.rs` | `eval-classifiers run|label|report` subcommand |

---

### Task 1: Verify the provider APIs (research; no code)

**Files:**
- Create: `docs/superpowers/specs/404-provider-contracts.md`

**Interfaces:**
- Produces: the contract Part B's adapters are written against.

- [ ] **Step 1: Read the official docs** for TypeSafe Jev, Cloudflare Workers AI Clef-flash (and Clef 27B), and AWS Strands Decider 2B (local server). Use vendor documentation pages only — not blog write-ups.

- [ ] **Step 2: Record, per provider, with the URL each fact came from:**
  1. Endpoint URL and auth header.
  2. Request body shape — exact JSON field names.
  3. **Can the instruction be free text?** Can we send the whole of `gpu/cope-b-runpod/policy.txt` (check its length with `wc -c gpu/cope-b-runpod/policy.txt`) as the question, with answers `toxic` / `clean`?
  4. Response shape — where the chosen answer and its probability live.
  5. Batching: several posts per request? Can the policy be sent once per batch?
  6. Token accounting: does the response report input tokens?
  7. Price per million input tokens (and output, if any), with the date read.
  8. Rate limits.
  9. Data retention / training-use terms for submitted text.

- [ ] **Step 3: Gate.** If a provider cannot take a free-text policy as its instruction (item 3), mark it **BLOCKED** at the top of the file with the reason. Do not plan an adapter for it; bring it to Bryan.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/404-provider-contracts.md
git commit -m 'docs(404): verified API contracts for Jev, Clef and Strands Decider'
```

---

### Task 2: Eval data — pool, labels, pricing, loaders

**Files:**
- Create: `evals/classifiers/pool.jsonl`, `evals/classifiers/labels.jsonl`, `evals/classifiers/pricing.json`
- Create: `src/cli/eval_classifiers/mod.rs`, `src/cli/eval_classifiers/data.rs`
- Modify: `src/cli/mod.rs` (add `pub mod eval_classifiers;`)
- Modify: `.gitignore.local` (add `/evals/classifiers/verdicts/`)

**Interfaces:**
- Consumes: `crate::cli::classify_compare::FixtureRow { id, label, category, content, note }`.
- Produces:
  - `pub fn load_pool(path: &Path) -> Result<Vec<FixtureRow>>`
  - `pub struct Label { pub id: String, pub label: Truth, pub note: Option<String> }`
  - `pub enum Truth { Toxic, Clean }` (serde lowercase)
  - `pub fn load_labels(path: &Path) -> Result<HashMap<String, Truth>>`

- [ ] **Step 1: Build the data files**

```bash
mkdir -p evals/classifiers
cat tests/fixtures/cope_b/smoke.jsonl tests/fixtures/cope_b/ab_sample.jsonl > evals/classifiers/pool.jsonl
python3 -c "
import json
with open('evals/classifiers/labels.jsonl','w') as out:
    for l in open('tests/fixtures/cope_b/smoke.jsonl'):
        r=json.loads(l)
        out.write(json.dumps({'id':r['id'],'label':r['label'],'note':r.get('note') or None})+'\n')
"
wc -l evals/classifiers/pool.jsonl evals/classifiers/labels.jsonl
```

Expected: `110 evals/classifiers/pool.jsonl`, `10 evals/classifiers/labels.jsonl`.

Create `evals/classifiers/pricing.json`. Prices are **estimates to be replaced by Task 1's verified figures**; the CoPE-B figure is the per-post cost implied by the observed $2.67 / ~20,000-post staging scan:

```json
{
  "cope-b": { "usd_per_post": 0.00013 },
  "detoxify": { "usd_per_post": 0.0 }
}
```

Append to `.gitignore.local`:

```
/evals/classifiers/verdicts/
```

- [ ] **Step 2: Write the failing tests** in `src/cli/eval_classifiers/data.rs`

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
    fn labels_map_id_to_truth_and_reject_unknown_values() {
        let f = file(&[r#"{"id":"a","label":"toxic"}"#, r#"{"id":"b","label":"clean","note":"ok"}"#]);
        let labels = load_labels(f.path()).unwrap();
        assert_eq!(labels["a"], Truth::Toxic);
        assert_eq!(labels["b"], Truth::Clean);
        let bad = file(&[r#"{"id":"c","label":"maybe"}"#]);
        assert!(load_labels(bad.path()).is_err(), "only toxic/clean are labels");
    }

    #[test]
    fn a_missing_labels_file_is_an_empty_set() {
        let labels = load_labels(Path::new("/nonexistent/labels.jsonl")).unwrap();
        assert!(labels.is_empty());
    }
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::data`
Expected: compile error — `load_pool` / `load_labels` not found.

- [ ] **Step 4: Implement** `src/cli/eval_classifiers/data.rs` (above the tests):

```rust
//! Loading the eval's posts (the pool) and the human answers (labels).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

pub use crate::cli::classify_compare::FixtureRow;

/// A human answer. Only two values: the eval scores binary verdicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Truth {
    Toxic,
    Clean,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub id: String,
    pub label: Truth,
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

/// A labels file that does not exist yet is an empty set, not an error: the
/// first round of an eval starts with no labels beyond the seed.
pub fn load_labels(path: &Path) -> Result<HashMap<String, Truth>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut out = HashMap::new();
    for (i, l) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let label: Label = serde_json::from_str(l)
            .with_context(|| format!("{} line {} (label must be toxic or clean)", path.display(), i + 1))?;
        out.insert(label.id, label.label);
    }
    Ok(out)
}
```

`src/cli/eval_classifiers/mod.rs`:

```rust
//! `charcoal eval-classifiers` — compare classifiers on labelled posts (#404).
//! See docs/superpowers/specs/2026-10-08-404-classifier-eval-design.md.

pub mod data;
```

Add `pub mod eval_classifiers;` to `src/cli/mod.rs`.

- [ ] **Step 5: Run to verify they pass**

Run: `cargo test --lib eval_classifiers::data`
Expected: 3 passed.

- [ ] **Step 6: Commit**

```bash
git add evals/classifiers/pool.jsonl evals/classifiers/labels.jsonl evals/classifiers/pricing.json src/cli/eval_classifiers/mod.rs src/cli/eval_classifiers/data.rs src/cli/mod.rs
git commit -m 'feat(404): eval pool, seed labels, pricing and loaders'
```

(`.gitignore.local` is untracked by design; it is edited, not committed.)

---

### Task 3: Metrics — Wilson intervals and per-model rates

**Files:**
- Create: `src/cli/eval_classifiers/metrics.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod metrics;`)

**Interfaces:**
- Consumes: `data::Truth`.
- Produces:
  - `pub struct Rate { pub hits: u32, pub n: u32 }` with `fn value(&self) -> Option<f64>` and `fn wilson95(&self) -> Option<(f64, f64)>`
  - `pub struct ModelMetrics { pub model: String, pub false_alarm: Rate, pub miss: Rate, pub errors: u32, pub judged: u32 }`
  - `pub fn model_metrics(model: &str, verdicts: &HashMap<String, Option<bool>>, labels: &HashMap<String, Truth>) -> ModelMetrics` — `Some(true)` = called toxic, `Some(false)` = called clean, `None` = error
  - `pub enum Comparison { Better, Worse, TooClose, NoData }`
  - `pub fn compare(candidate: &Rate, baseline: &Rate) -> Comparison` — lower is better for both rates

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::eval_classifiers::data::Truth;

    #[test]
    fn wilson_matches_known_values() {
        // 0 of 10: [0, 0.278]; 5 of 10: [0.237, 0.763] (standard table values).
        let (lo, hi) = Rate { hits: 0, n: 10 }.wilson95().unwrap();
        assert!(lo.abs() < 1e-9 && (hi - 0.2775).abs() < 1e-3, "{lo} {hi}");
        let (lo, hi) = Rate { hits: 5, n: 10 }.wilson95().unwrap();
        assert!((lo - 0.2366).abs() < 1e-3 && (hi - 0.7634).abs() < 1e-3, "{lo} {hi}");
        assert_eq!(Rate { hits: 0, n: 0 }.wilson95(), None);
    }

    #[test]
    fn rates_split_by_truth_and_errors_are_not_clean() {
        let labels: HashMap<String, Truth> = [
            ("t1", Truth::Toxic), ("t2", Truth::Toxic), ("c1", Truth::Clean), ("c2", Truth::Clean), ("e", Truth::Toxic),
        ].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let verdicts: HashMap<String, Option<bool>> = [
            ("t1", Some(true)), ("t2", Some(false)), ("c1", Some(true)), ("c2", Some(false)), ("e", None),
        ].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let m = model_metrics("m", &verdicts, &labels);
        assert_eq!((m.miss.hits, m.miss.n), (1, 2), "t2 missed; the errored toxic post is not a miss");
        assert_eq!((m.false_alarm.hits, m.false_alarm.n), (1, 2));
        assert_eq!(m.errors, 1);
        assert_eq!(m.judged, 4);
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
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::metrics`
Expected: compile error — module items not found.

- [ ] **Step 3: Implement** `src/cli/eval_classifiers/metrics.rs`:

```rust
//! Error rates with honest uncertainty. With tens of labels a 5-point gap is
//! often noise; the Wilson interval says so instead of crowning a winner.

use std::collections::HashMap;

use super::data::Truth;

/// `hits` out of `n`. For a miss rate a hit is a missed toxic post; for a
/// false-alarm rate it is a clean post called toxic.
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
    /// Labelled posts whose call failed or timed out — never counted as clean.
    pub errors: u32,
    /// Labelled posts the model actually judged.
    pub judged: u32,
}

pub fn model_metrics(
    model: &str,
    verdicts: &HashMap<String, Option<bool>>,
    labels: &HashMap<String, Truth>,
) -> ModelMetrics {
    let mut m = ModelMetrics {
        model: model.to_string(),
        false_alarm: Rate::default(),
        miss: Rate::default(),
        errors: 0,
        judged: 0,
    };
    for (id, truth) in labels {
        match verdicts.get(id) {
            None => {}
            Some(None) => m.errors += 1,
            Some(Some(called_toxic)) => {
                m.judged += 1;
                match truth {
                    Truth::Toxic => {
                        m.miss.n += 1;
                        if !called_toxic {
                            m.miss.hits += 1;
                        }
                    }
                    Truth::Clean => {
                        m.false_alarm.n += 1;
                        if *called_toxic {
                            m.false_alarm.hits += 1;
                        }
                    }
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
```

Add `pub mod metrics;` to `mod.rs`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --lib eval_classifiers::metrics`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add src/cli/eval_classifiers/metrics.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): eval metrics with Wilson intervals and interval-overlap comparison'
```

---

### Task 4: Verdict cache

**Files:**
- Create: `src/cli/eval_classifiers/cache.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod cache;`)

**Interfaces:**
- Consumes: `crate::toxicity::classifier::ClassifierVerdict`.
- Produces:
  - `pub struct VerdictRecord { pub id: String, pub model: String, pub p_toxic: Option<f32>, pub error: Option<String>, pub latency_ms: u32, pub input_chars: usize }`
  - `impl VerdictRecord { pub fn from_verdict(id, model, &ClassifierVerdict, input_chars) -> Self; pub fn from_error(id, model, detail, input_chars) -> Self; pub fn called_toxic(&self) -> Option<bool> }`
  - `pub const TOXIC_AT: f32 = 0.5;`
  - `pub fn append(dir: &Path, rec: &VerdictRecord) -> Result<()>` — writes `dir/<model>.jsonl`
  - `pub fn load(dir: &Path, model: &str) -> Result<HashMap<String, VerdictRecord>>` — last record per id wins

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::toxicity::classifier::ClassifierVerdict;

    fn verdict(toxic: bool, conf: f32) -> ClassifierVerdict {
        ClassifierVerdict { toxic_token: toxic, confidence: conf, latency_ms: 12, model_id: "m".into(), policy_version: "p".into() }
    }

    #[test]
    fn p_toxic_is_uniform_across_token_conventions() {
        let a = VerdictRecord::from_verdict("a", "m", &verdict(true, 0.8), 10);
        let b = VerdictRecord::from_verdict("b", "m", &verdict(false, 0.8), 10);
        assert!((a.p_toxic.unwrap() - 0.8).abs() < 1e-6);
        assert!((b.p_toxic.unwrap() - 0.2).abs() < 1e-6, "a confident 'clean' is a low p_toxic");
        assert_eq!(a.called_toxic(), Some(true));
        assert_eq!(b.called_toxic(), Some(false));
        assert_eq!(VerdictRecord::from_error("c", "m", "timeout", 10).called_toxic(), None);
    }

    #[test]
    fn cache_round_trips_and_the_last_record_wins() {
        let dir = tempfile::tempdir().unwrap();
        append(dir.path(), &VerdictRecord::from_error("a", "m", "timeout", 5)).unwrap();
        append(dir.path(), &VerdictRecord::from_verdict("a", "m", &verdict(true, 0.9), 5)).unwrap();
        append(dir.path(), &VerdictRecord::from_verdict("b", "m", &verdict(false, 0.7), 5)).unwrap();
        let got = load(dir.path(), "m").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got["a"].called_toxic(), Some(true), "a retried error is replaced by the success");
        assert!(load(dir.path(), "other").unwrap().is_empty(), "no file yet is an empty cache");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::cache`
Expected: compile error.

- [ ] **Step 3: Implement** `src/cli/eval_classifiers/cache.rs`:

```rust
//! Every verdict ever paid for, one JSONL file per model. Re-runs and newly
//! added models never re-pay for a post already judged. Errors are recorded
//! too, but the last record per post wins, so a retried error is replaced.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::toxicity::classifier::ClassifierVerdict;

/// The one threshold every model is judged at (spec: fairness rule 3).
pub const TOXIC_AT: f32 = 0.5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerdictRecord {
    pub id: String,
    pub model: String,
    /// Probability the post is toxic; `None` when the call failed.
    pub p_toxic: Option<f32>,
    pub error: Option<String>,
    pub latency_ms: u32,
    /// Characters sent — the basis of the token-cost estimate.
    pub input_chars: usize,
}

impl VerdictRecord {
    pub fn from_verdict(id: &str, model: &str, v: &ClassifierVerdict, input_chars: usize) -> Self {
        // Backends report confidence in the token they emitted; convert to a
        // single "probability toxic" so every model is cut at the same place.
        let p = if v.toxic_token { v.confidence } else { 1.0 - v.confidence };
        Self {
            id: id.to_string(),
            model: model.to_string(),
            p_toxic: Some(p.clamp(0.0, 1.0)),
            error: None,
            latency_ms: v.latency_ms,
            input_chars,
        }
    }

    pub fn from_error(id: &str, model: &str, detail: &str, input_chars: usize) -> Self {
        Self {
            id: id.to_string(),
            model: model.to_string(),
            p_toxic: None,
            error: Some(detail.to_string()),
            latency_ms: 0,
            input_chars,
        }
    }

    /// `Some(true)` toxic, `Some(false)` clean, `None` an error.
    pub fn called_toxic(&self) -> Option<bool> {
        self.p_toxic.map(|p| p >= TOXIC_AT)
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
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add src/cli/eval_classifiers/cache.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): append-only per-model verdict cache'
```

---

### Task 5: Pricing and the spending cap

**Files:**
- Create: `src/cli/eval_classifiers/cost.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod cost;`)

**Interfaces:**
- Produces:
  - `pub struct Price { pub usd_per_post: Option<f64>, pub usd_per_million_input_tokens: Option<f64> }` (serde)
  - `pub fn load_pricing(path: &Path) -> Result<HashMap<String, Price>>`
  - `pub fn estimate_usd(price: &Price, input_chars: usize) -> f64` — tokens ≈ chars / 4
  - `pub struct Budget { cap_usd: f64, spent_usd: f64 }` with `pub fn new(cap_usd: f64) -> Self`, `pub fn try_spend(&mut self, usd: f64) -> bool`, `pub fn spent(&self) -> f64`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_per_post_and_per_token() {
        let per_post = Price { usd_per_post: Some(0.001), usd_per_million_input_tokens: None };
        assert!((estimate_usd(&per_post, 4_000) - 0.001).abs() < 1e-12);
        let per_tok = Price { usd_per_post: None, usd_per_million_input_tokens: Some(0.042) };
        // 4,000 chars ≈ 1,000 tokens ≈ $0.000042.
        assert!((estimate_usd(&per_tok, 4_000) - 0.000_042).abs() < 1e-12);
    }

    #[test]
    fn the_budget_refuses_a_call_that_would_cross_the_cap() {
        let mut b = Budget::new(1.0);
        assert!(b.try_spend(0.6));
        assert!(!b.try_spend(0.5), "0.6 + 0.5 > 1.0: refused, nothing spent");
        assert!((b.spent() - 0.6).abs() < 1e-12);
        assert!(b.try_spend(0.4));
    }

    #[test]
    fn pricing_file_parses() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pricing.json");
        std::fs::write(&p, r#"{"cope-b":{"usd_per_post":0.00013},"jev":{"usd_per_million_input_tokens":0.042}}"#).unwrap();
        let prices = load_pricing(&p).unwrap();
        assert_eq!(prices["cope-b"].usd_per_post, Some(0.00013));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::cost`
Expected: compile error.

- [ ] **Step 3: Implement** `src/cli/eval_classifiers/cost.rs`:

```rust
//! Cost estimates and the per-run spending cap — the lesson of the $193
//! incident (#385): a run must stop before it can surprise anyone.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// List price for one model. Exactly one field is expected; a model with
/// neither (or missing from the file) is treated as free and the report says
/// so rather than inventing a number.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Price {
    #[serde(default)]
    pub usd_per_post: Option<f64>,
    #[serde(default)]
    pub usd_per_million_input_tokens: Option<f64>,
}

pub fn load_pricing(path: &Path) -> Result<HashMap<String, Price>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// ~4 characters per token: a rough English average, good enough for a cap
/// and a ballpark — Part B adapters that report real token counts replace it.
pub fn estimate_usd(price: &Price, input_chars: usize) -> f64 {
    if let Some(per_post) = price.usd_per_post {
        return per_post;
    }
    let tokens = input_chars as f64 / 4.0;
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
git commit -m 'feat(404): eval pricing and per-run spending cap'
```

---

### Task 6: The runner

**Files:**
- Create: `src/cli/eval_classifiers/runner.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod runner;`)

**Interfaces:**
- Consumes: `data::FixtureRow`, `cache::{append, load, VerdictRecord}`, `cost::{Budget, Price, estimate_usd}`, `ToxicityClassifier`.
- Produces:
  - `pub struct RunStats { pub model: String, pub judged: usize, pub cached: usize, pub errors: usize, pub stopped_at_cap: bool }`
  - `pub async fn run_model(model: &str, classifier: &dyn ToxicityClassifier, pool: &[FixtureRow], dir: &Path, price: &Price, budget: &mut Budget) -> Result<RunStats>`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::eval_classifiers::cost::Price;
    use crate::toxicity::classifier::{ClassifierVerdict, ToxicityClassifier};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting { calls: AtomicUsize, fail_on: Option<&'static str> }

    #[async_trait::async_trait]
    impl ToxicityClassifier for Counting {
        async fn classify(&self, content: &str) -> anyhow::Result<ClassifierVerdict> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_on.is_some_and(|f| content.contains(f)) {
                anyhow::bail!("timeout");
            }
            Ok(ClassifierVerdict { toxic_token: content.contains("bad"), confidence: 0.9, latency_ms: 5, model_id: "c".into(), policy_version: "p".into() })
        }
        fn name(&self) -> &'static str { "counting" }
        fn model_id(&self) -> &'static str { "counting" }
        fn policy_version(&self) -> &'static str { "p" }
        fn threshold(&self) -> f32 { 0.5 }
    }

    fn row(id: &str, content: &str) -> FixtureRow {
        serde_json::from_value(serde_json::json!({"id": id, "content": content})).unwrap()
    }

    #[tokio::test]
    async fn judges_each_post_once_and_reuses_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad"), row("b", "fine")];
        let model = Counting { calls: AtomicUsize::new(0), fail_on: None };
        let mut budget = Budget::new(1.0);
        let s = run_model("m", &model, &pool, dir.path(), &Price::default(), &mut budget).await.unwrap();
        assert_eq!((s.judged, s.cached), (2, 0));
        let s = run_model("m", &model, &pool, dir.path(), &Price::default(), &mut budget).await.unwrap();
        assert_eq!((s.judged, s.cached), (0, 2), "nothing re-paid");
        assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn an_error_is_recorded_as_an_error_and_retried_next_run() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad timeout")];
        let failing = Counting { calls: AtomicUsize::new(0), fail_on: Some("timeout") };
        let mut budget = Budget::new(1.0);
        let s = run_model("m", &failing, &pool, dir.path(), &Price::default(), &mut budget).await.unwrap();
        assert_eq!(s.errors, 1);
        assert_eq!(load(dir.path(), "m").unwrap()["a"].called_toxic(), None, "an error, never clean");
        let ok = Counting { calls: AtomicUsize::new(0), fail_on: None };
        run_model("m", &ok, &pool, dir.path(), &Price::default(), &mut budget).await.unwrap();
        assert_eq!(ok.calls.load(Ordering::SeqCst), 1, "the error was retried");
        assert_eq!(load(dir.path(), "m").unwrap()["a"].called_toxic(), Some(true));
    }

    #[tokio::test]
    async fn the_cap_stops_the_run_and_keeps_what_was_paid_for() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "x"), row("b", "y"), row("c", "z")];
        let model = Counting { calls: AtomicUsize::new(0), fail_on: None };
        let price = Price { usd_per_post: Some(0.4), usd_per_million_input_tokens: None };
        let mut budget = Budget::new(1.0);
        let s = run_model("m", &model, &pool, dir.path(), &price, &mut budget).await.unwrap();
        assert!(s.stopped_at_cap);
        assert_eq!(s.judged, 2, "$0.80 spent; the third $0.40 call would cross $1.00");
        assert_eq!(load(dir.path(), "m").unwrap().len(), 2, "both paid verdicts kept");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::runner`
Expected: compile error.

- [ ] **Step 3: Implement** `src/cli/eval_classifiers/runner.rs`:

```rust
//! Run one model over the pool: skip posts already judged, record every
//! outcome (errors included), and stop cleanly at the spending cap.

use anyhow::Result;
use std::path::Path;

use super::cache::{append, load, VerdictRecord};
use super::cost::{estimate_usd, Budget, Price};
use super::data::FixtureRow;
use crate::toxicity::classifier::ToxicityClassifier;

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
) -> Result<RunStats> {
    let cached = load(dir, model)?;
    let mut stats = RunStats {
        model: model.to_string(),
        judged: 0,
        cached: 0,
        errors: 0,
        stopped_at_cap: false,
    };
    for row in pool {
        // Only a successful verdict counts as done; an earlier error is retried.
        if cached.get(&row.id).is_some_and(|r| r.p_toxic.is_some()) {
            stats.cached += 1;
            continue;
        }
        let chars = row.content.chars().count();
        if !budget.try_spend(estimate_usd(price, chars)) {
            stats.stopped_at_cap = true;
            break;
        }
        let rec = match classifier.classify(&row.content).await {
            Ok(v) => {
                stats.judged += 1;
                VerdictRecord::from_verdict(&row.id, model, &v, chars)
            }
            Err(e) => {
                stats.errors += 1;
                VerdictRecord::from_error(&row.id, model, &format!("{e:#}"), chars)
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
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add src/cli/eval_classifiers/runner.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): eval runner with cache reuse, error recording and spending cap'
```

---

### Task 7: The label picker and merge

**Files:**
- Create: `src/cli/eval_classifiers/labelling.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod labelling;`)

**Interfaces:**
- Consumes: `data::{FixtureRow, Truth, Label}`, `cache::VerdictRecord`.
- Produces:
  - `pub fn pick_for_labelling(pool: &[FixtureRow], per_model: &[HashMap<String, VerdictRecord>], labels: &HashMap<String, Truth>) -> Vec<FixtureRow>` — unlabelled posts where ≥ 2 models gave verdicts and they disagree, plus agreed posts whose `sampled(id)` is true; output rows carry **no** model votes and an empty `label`
  - `pub fn sampled(id: &str) -> bool` — FNV-1a 64-bit of the id, `% 10 == 0`
  - `pub fn write_to_label(path: &Path, rows: &[FixtureRow]) -> Result<()>` — JSONL `{id, content, label: "", note: ""}`
  - `pub fn merge_labels(to_label: &Path, labels: &Path) -> Result<usize>` — appends rows whose `label` is `toxic`/`clean` to `labels.jsonl`, skips ids already labelled, returns the count merged; a row with any other non-empty label is an error

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::eval_classifiers::cache::VerdictRecord;

    fn row(id: &str) -> FixtureRow {
        serde_json::from_value(serde_json::json!({"id": id, "content": format!("post {id}")})).unwrap()
    }
    fn rec(id: &str, model: &str, p: Option<f32>) -> VerdictRecord {
        VerdictRecord { id: id.into(), model: model.into(), p_toxic: p, error: p.is_none().then(|| "e".into()), latency_ms: 1, input_chars: 1 }
    }
    fn by_id(recs: Vec<VerdictRecord>) -> HashMap<String, VerdictRecord> {
        recs.into_iter().map(|r| (r.id.clone(), r)).collect()
    }

    #[test]
    fn picks_disagreements_skips_labelled_and_errors() {
        let pool = vec![row("dis"), row("agree"), row("labelled"), row("err")];
        let m1 = by_id(vec![rec("dis", "m1", Some(0.9)), rec("agree", "m1", Some(0.9)), rec("labelled", "m1", Some(0.9)), rec("err", "m1", Some(0.9))]);
        let m2 = by_id(vec![rec("dis", "m2", Some(0.1)), rec("agree", "m2", Some(0.8)), rec("labelled", "m2", Some(0.1)), rec("err", "m2", None)]);
        let labels: HashMap<String, Truth> = [("labelled".to_string(), Truth::Toxic)].into();
        let picked: Vec<String> = pick_for_labelling(&pool, &[m1, m2], &labels).into_iter().map(|r| r.id).collect();
        assert!(picked.contains(&"dis".to_string()));
        assert!(!picked.contains(&"labelled".to_string()), "already answered");
        assert!(!picked.contains(&"err".to_string()), "only one model judged it");
        assert_eq!(picked.contains(&"agree".to_string()), sampled("agree"), "agreed posts only via the sample");
    }

    #[test]
    fn the_agreed_sample_is_about_ten_percent_and_stable() {
        let hits = (0..2000).filter(|i| sampled(&format!("post-{i}"))).count();
        assert!((140..=260).contains(&hits), "{hits} of 2000");
        assert_eq!(sampled("post-7"), sampled("post-7"));
    }

    #[test]
    fn to_label_rows_are_blind_and_merge_appends_only_answered_rows() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let labels = dir.path().join("labels.jsonl");
        write_to_label(&to_label, &[row("a"), row("b")]).unwrap();
        let text = std::fs::read_to_string(&to_label).unwrap();
        assert!(!text.contains("p_toxic") && !text.contains("model"), "no votes shown: {text}");
        // Bryan answers one of the two.
        std::fs::write(&to_label, text.replacen(r#""label":"""#, r#""label":"toxic""#, 1)).unwrap();
        assert_eq!(merge_labels(&to_label, &labels).unwrap(), 1);
        assert_eq!(merge_labels(&to_label, &labels).unwrap(), 0, "already merged ids are skipped");
        std::fs::write(&to_label, r#"{"id":"c","content":"x","label":"maybe","note":""}"#).unwrap();
        assert!(merge_labels(&to_label, &labels).is_err(), "only toxic/clean are answers");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::labelling`
Expected: compile error.

- [ ] **Step 3: Implement** `src/cli/eval_classifiers/labelling.rs`:

```rust
//! Choosing what a human labels next. Disagreements are where a label
//! changes the answer; a stable ~10 % sample of agreed posts catches blind
//! spots every model shares, which disagreement-only labelling would miss.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use super::cache::VerdictRecord;
use super::data::{load_labels, FixtureRow, Label, Truth};

/// FNV-1a 64-bit — a fixed hash, so the same posts are sampled every run
/// (no `rand` dependency, and re-runs never reshuffle the work).
pub fn sampled(id: &str) -> bool {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h % 10 == 0
}

pub fn pick_for_labelling(
    pool: &[FixtureRow],
    per_model: &[HashMap<String, VerdictRecord>],
    labels: &HashMap<String, Truth>,
) -> Vec<FixtureRow> {
    pool.iter()
        .filter(|row| !labels.contains_key(&row.id))
        .filter(|row| {
            let calls: Vec<bool> = per_model
                .iter()
                .filter_map(|m| m.get(&row.id).and_then(|r| r.called_toxic()))
                .collect();
            if calls.len() < 2 {
                return false;
            }
            let disagree = calls.iter().any(|c| *c != calls[0]);
            disagree || sampled(&row.id)
        })
        .cloned()
        .collect()
}

#[derive(Serialize)]
struct ToLabel<'a> {
    id: &'a str,
    content: &'a str,
    label: &'a str,
    note: &'a str,
}

/// Blind on purpose: no model votes, so they cannot anchor the answer.
pub fn write_to_label(path: &Path, rows: &[FixtureRow]) -> Result<()> {
    let mut f = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    for r in rows {
        let line = ToLabel { id: &r.id, content: &r.content, label: "", note: "" };
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
        let note = row["note"].as_str().filter(|n| !n.is_empty()).map(str::to_string);
        let label = Label { id: id.to_string(), label: truth, note };
        writeln!(out, "{}", serde_json::to_string(&label)?)?;
        merged += 1;
    }
    Ok(merged)
}
```

Add `pub mod labelling;` to `mod.rs`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --lib eval_classifiers::labelling`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add src/cli/eval_classifiers/labelling.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): blind disagreement picker with a stable agreed-post sample'
```

---

### Task 8: The report — and proof it catches bad models

**Files:**
- Create: `src/cli/eval_classifiers/report.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod report;`)

**Interfaces:**
- Consumes: `metrics::{model_metrics, compare, Comparison, ModelMetrics}`, `cache::VerdictRecord`, `cost::{Price, estimate_usd}`, `data::Truth`.
- Produces:
  - `pub struct ModelReport { pub metrics: ModelMetrics, pub usd_per_1k: Option<f64>, pub p50_ms: Option<u32>, pub p95_ms: Option<u32> }`
  - `pub fn build(models: &[(String, HashMap<String, VerdictRecord>)], labels: &HashMap<String, Truth>, prices: &HashMap<String, Price>) -> Vec<ModelReport>`
  - `pub fn verdicts_vs(baseline: &ModelReport, candidate: &ModelReport) -> (Comparison, Comparison)` — `(false_alarm, miss)`
  - `pub fn render(reports: &[ModelReport], baseline: &str) -> String`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::eval_classifiers::cache::VerdictRecord;
    use crate::cli::eval_classifiers::labelling::sampled;

    /// 40 toxic + 40 clean labelled posts.
    fn labels() -> HashMap<String, Truth> {
        (0..80).map(|i| (format!("p{i}"), if i < 40 { Truth::Toxic } else { Truth::Clean })).collect()
    }

    fn model(name: &str, decide: impl Fn(usize) -> f32) -> (String, HashMap<String, VerdictRecord>) {
        let recs = (0..80)
            .map(|i| {
                let id = format!("p{i}");
                let r = VerdictRecord { id: id.clone(), model: name.into(), p_toxic: Some(decide(i)), error: None, latency_ms: 10 + i as u32, input_chars: 400 };
                (id, r)
            })
            .collect();
        (name.to_string(), recs)
    }

    #[test]
    fn deliberately_bad_models_are_caught() {
        // A good baseline: 2 misses, 2 false alarms.
        let good = model("cope-b", |i| if (i < 40 && i >= 2) || (40..42).contains(&i) { 0.9 } else { 0.1 });
        let always_clean = model("always-clean", |_| 0.0);
        let coin_flip = model("coin-flip", |i| if sampled(&format!("flip{i}")) || i % 2 == 0 { 0.9 } else { 0.1 });
        let reports = build(&[good, always_clean, coin_flip], &labels(), &HashMap::new());
        let base = &reports[0];
        let (_, miss) = verdicts_vs(base, &reports[1]);
        assert_eq!(miss, Comparison::Worse, "always-clean misses every toxic post");
        let (fa, miss) = verdicts_vs(base, &reports[2]);
        assert!(fa == Comparison::Worse || miss == Comparison::Worse, "coin-flip must lose on at least one rate");
        let text = render(&reports, "cope-b");
        assert!(text.contains("always-clean") && text.contains("worse"), "{text}");
    }

    #[test]
    fn close_models_are_reported_as_too_close_to_call() {
        let a = model("cope-b", |i| if i < 39 || i == 79 { 0.9 } else { 0.1 });
        let b = model("other", |i| if i < 38 || i == 79 { 0.9 } else { 0.1 });
        let reports = build(&[a, b], &labels(), &HashMap::new());
        assert_eq!(verdicts_vs(&reports[0], &reports[1]), (Comparison::TooClose, Comparison::TooClose));
        assert!(render(&reports, "cope-b").contains("too close to call — label more"));
    }

    #[test]
    fn cost_and_latency_columns() {
        let a = model("jev", |_| 0.9);
        let prices: HashMap<String, Price> = [("jev".to_string(), Price { usd_per_post: None, usd_per_million_input_tokens: Some(0.042) })].into();
        let r = &build(&[a], &labels(), &prices)[0];
        // 400 chars ≈ 100 tokens; 1,000 posts ≈ 100k tokens ≈ $0.0042.
        assert!((r.usd_per_1k.unwrap() - 0.0042).abs() < 1e-9);
        // Latencies are 10..=89 ms: nearest-rank p50 = 49, p95 = 85.
        assert_eq!(r.p50_ms, Some(49));
        assert_eq!(r.p95_ms, Some(85));
        let unpriced = &build(&[model("x", |_| 0.9)], &labels(), &HashMap::new())[0];
        assert_eq!(unpriced.usd_per_1k, None, "no price listed: say so, don't invent one");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::report`
Expected: compile error.

- [ ] **Step 3: Implement** `src/cli/eval_classifiers/report.rs`:

```rust
//! The per-model table and the verdict lines. Two rates, side by side, each
//! with its interval — never a blended score (spec decision).

use std::collections::HashMap;
use std::fmt::Write;

use super::cache::VerdictRecord;
use super::cost::{estimate_usd, Price};
use super::data::Truth;
use super::metrics::{compare, model_metrics, Comparison, ModelMetrics, Rate};

#[derive(Debug, Clone)]
pub struct ModelReport {
    pub metrics: ModelMetrics,
    pub usd_per_1k: Option<f64>,
    pub p50_ms: Option<u32>,
    pub p95_ms: Option<u32>,
}

fn percentile(sorted: &[u32], pct: usize) -> Option<u32> {
    if sorted.is_empty() {
        return None;
    }
    let idx = (sorted.len() * pct).div_ceil(100).saturating_sub(1);
    sorted.get(idx).copied()
}

pub fn build(
    models: &[(String, HashMap<String, VerdictRecord>)],
    labels: &HashMap<String, Truth>,
    prices: &HashMap<String, Price>,
) -> Vec<ModelReport> {
    models
        .iter()
        .map(|(name, recs)| {
            let calls: HashMap<String, Option<bool>> =
                recs.iter().map(|(id, r)| (id.clone(), r.called_toxic())).collect();
            let metrics = model_metrics(name, &calls, labels);
            let ok: Vec<&VerdictRecord> = recs.values().filter(|r| r.p_toxic.is_some()).collect();
            let mut lat: Vec<u32> = ok.iter().map(|r| r.latency_ms).collect();
            lat.sort_unstable();
            let usd_per_1k = prices.get(name).filter(|_| !ok.is_empty()).map(|p| {
                let mean_chars = ok.iter().map(|r| r.input_chars).sum::<usize>() / ok.len();
                estimate_usd(p, mean_chars) * 1000.0
            });
            ModelReport { metrics, usd_per_1k, p50_ms: percentile(&lat, 50), p95_ms: percentile(&lat, 95) }
        })
        .collect()
}

pub fn verdicts_vs(baseline: &ModelReport, candidate: &ModelReport) -> (Comparison, Comparison) {
    (
        compare(&candidate.metrics.false_alarm, &baseline.metrics.false_alarm),
        compare(&candidate.metrics.miss, &baseline.metrics.miss),
    )
}

fn rate_cell(r: &Rate) -> String {
    match (r.value(), r.wilson95()) {
        (Some(v), Some((lo, hi))) => format!("{:>5.1}% [{:.0}–{:.0}] ({}/{})", v * 100.0, lo * 100.0, hi * 100.0, r.hits, r.n),
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

pub fn render(reports: &[ModelReport], baseline: &str) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{:<14} {:<28} {:<28} {:>6} {:>10} {:>12}", "model", "false alarms (clean→toxic)", "misses (toxic→clean)", "errors", "$/1k posts", "p50/p95 ms");
    for r in reports {
        let m = &r.metrics;
        let cost = r.usd_per_1k.map_or("unpriced".to_string(), |c| format!("{c:.4}"));
        let lat = match (r.p50_ms, r.p95_ms) {
            (Some(a), Some(b)) => format!("{a}/{b}"),
            _ => "-".to_string(),
        };
        let _ = writeln!(s, "{:<14} {:<28} {:<28} {:>6} {:>10} {:>12}", m.model, rate_cell(&m.false_alarm), rate_cell(&m.miss), m.errors, cost, lat);
    }
    if let Some(base) = reports.iter().find(|r| r.metrics.model == baseline) {
        let _ = writeln!(s, "\nCompared with {baseline}:");
        for r in reports.iter().filter(|r| r.metrics.model != baseline) {
            let (fa, miss) = verdicts_vs(base, r);
            let _ = writeln!(s, "  {}: false alarms {}; misses {}", r.metrics.model, word(fa), word(miss));
        }
    } else {
        let _ = writeln!(s, "\n(baseline {baseline:?} has no verdicts — run it first)");
    }
    s
}
```

Add `pub mod report;` to `mod.rs`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --lib eval_classifiers::report`
Expected: 3 passed. If `deliberately_bad_models_are_caught` fails on the coin-flip arm, print `render(...)` and check the coin-flip's two rates: with 40/40 labels a ~50 % rate must fall outside the good model's ~5 % interval. Do **not** loosen the assertion; fix the fake.

- [ ] **Step 5: Commit**

```bash
git add src/cli/eval_classifiers/report.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): eval report with intervals; proven to catch always-clean and coin-flip models'
```

---

### Task 9: Detoxify as a classifier, and the eval model registry

**Files:**
- Create: `src/toxicity/detoxify_classifier.rs`
- Modify: `src/toxicity/mod.rs` (add `pub mod detoxify_classifier;`)
- Create: `src/cli/eval_classifiers/models.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod models;`)

**Interfaces:**
- Consumes: `crate::toxicity::onnx::OnnxToxicityScorer::load(&Path)`, `crate::toxicity::traits::ToxicityScorer::score_text`, `crate::toxicity::download::resolve_model_dir()`, `crate::toxicity::classifier::build_backend_named`.
- Produces:
  - `pub struct DetoxifyClassifier<S: ToxicityScorer> { scorer: S }` with `pub fn new(scorer: S) -> Self` and a `ToxicityClassifier` impl (`toxic_token = t >= 0.5`, `confidence = t` if toxic else `1 − t`)
  - `pub fn build_eval_model(name: &str) -> Result<Arc<dyn ToxicityClassifier>>` — `"cope-b"` → `build_backend_named("runpod")`, `"detoxify"` → Detoxify over the local ONNX model; anything else → error naming the known models

- [ ] **Step 1: Write the failing tests** (in `src/toxicity/detoxify_classifier.rs`)

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
    async fn maps_a_score_to_a_verdict_with_the_same_probability() {
        let v = DetoxifyClassifier::new(Fixed(0.8)).classify("x").await.unwrap();
        assert!(v.toxic_token && (v.confidence - 0.8).abs() < 1e-6);
        let v = DetoxifyClassifier::new(Fixed(0.3)).classify("x").await.unwrap();
        assert!(!v.toxic_token && (v.confidence - 0.7).abs() < 1e-6, "p_toxic stays 0.3");
    }
}
```

If `ToxicityScorer` has required methods beyond `score_text`, implement them in `Fixed` by delegating to `score_text` or returning a constant — check `src/toxicity/traits.rs:62` before writing.

And in `src/cli/eval_classifiers/models.rs`:

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

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib detoxify_classifier eval_classifiers::models`
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
            // Confidence in the emitted token, the convention every backend
            // uses; the eval converts it back to p_toxic = t.
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

Add `pub mod detoxify_classifier;` to `src/toxicity/mod.rs` and `pub mod models;` to `src/cli/eval_classifiers/mod.rs`. Confirm `OnnxToxicityScorer::load` expects the **base** model dir (the toxicity model sits at the base; see `src/toxicity/download.rs:128`); if it expects a subdirectory, pass that instead.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --lib detoxify_classifier eval_classifiers::models`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add src/toxicity/detoxify_classifier.rs src/toxicity/mod.rs src/cli/eval_classifiers/models.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): Detoxify as an eval classifier and the eval-only model registry'
```

---

### Task 10: CLI wiring, README and CHANGELOG

**Files:**
- Modify: `src/main.rs` (new `EvalClassifiers` subcommand next to `ClassifyGate`, ~line 172; handler next to `Commands::ClassifyGate`, ~line 1102)
- Create: `evals/classifiers/README.md`
- Modify: `CHANGELOG.md` (`### Added` under `[Unreleased]`)
- Test: `tests/cli_eval_classifiers.rs`

**Interfaces:**
- Consumes: every earlier task.
- Produces: `charcoal eval-classifiers run --models cope-b,detoxify [--cap-usd 1.0] [--dir evals/classifiers]`, `charcoal eval-classifiers label [--merge]`, `charcoal eval-classifiers report [--baseline cope-b]`.

- [ ] **Step 1: Write the failing test** `tests/cli_eval_classifiers.rs` — an end-to-end run with fakes, through the library entry points the CLI calls:

```rust
use charcoal::cli::eval_classifiers::{cache, cost::{Budget, Price}, data, labelling, report, runner};
use charcoal::toxicity::classifier::{ClassifierVerdict, ToxicityClassifier};
use std::collections::HashMap;

struct Says(bool);
#[async_trait::async_trait]
impl ToxicityClassifier for Says {
    async fn classify(&self, content: &str) -> anyhow::Result<ClassifierVerdict> {
        let toxic = if self.0 { content.contains("bad") } else { false };
        Ok(ClassifierVerdict { toxic_token: toxic, confidence: 0.9, latency_ms: 3, model_id: "f".into(), policy_version: "p".into() })
    }
    fn name(&self) -> &'static str { "fake" }
    fn model_id(&self) -> &'static str { "fake" }
    fn policy_version(&self) -> &'static str { "p" }
    fn threshold(&self) -> f32 { 0.5 }
}

#[tokio::test]
async fn a_full_round_runs_picks_merges_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let pool_path = dir.path().join("pool.jsonl");
    std::fs::write(&pool_path, "{\"id\":\"a\",\"content\":\"bad\"}\n{\"id\":\"b\",\"content\":\"fine\"}\n").unwrap();
    let pool = data::load_pool(&pool_path).unwrap();
    let verdicts = dir.path().join("verdicts");
    let mut budget = Budget::new(1.0);
    runner::run_model("good", &Says(true), &pool, &verdicts, &Price::default(), &mut budget).await.unwrap();
    runner::run_model("lazy", &Says(false), &pool, &verdicts, &Price::default(), &mut budget).await.unwrap();

    let per_model = vec![cache::load(&verdicts, "good").unwrap(), cache::load(&verdicts, "lazy").unwrap()];
    let labels_path = dir.path().join("labels.jsonl");
    let picked = labelling::pick_for_labelling(&pool, &per_model, &data::load_labels(&labels_path).unwrap());
    assert_eq!(picked.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["a"], "they disagree only on a");

    let to_label = dir.path().join("to-label.jsonl");
    labelling::write_to_label(&to_label, &picked).unwrap();
    let answered = std::fs::read_to_string(&to_label).unwrap().replace("\"label\":\"\"", "\"label\":\"toxic\"");
    std::fs::write(&to_label, answered).unwrap();
    assert_eq!(labelling::merge_labels(&to_label, &labels_path).unwrap(), 1);

    let labels = data::load_labels(&labels_path).unwrap();
    let models = vec![("good".to_string(), per_model[0].clone()), ("lazy".to_string(), per_model[1].clone())];
    let text = report::render(&report::build(&models, &labels, &HashMap::new()), "good");
    assert!(text.contains("lazy") && text.contains("1/1"), "{text}");
}
```

- [ ] **Step 2: Run to verify it fails or passes for the right reason**

Run: `cargo test --test cli_eval_classifiers`
Expected: PASS if Tasks 2–8 are complete (this test guards the wiring contract); if it fails, fix the library — not the test.

- [ ] **Step 3: Add the subcommand** to `enum Commands` in `src/main.rs`:

```rust
    /// Compare classifiers on labelled posts (#404): run, pick posts to
    /// label, and report false alarms, misses, cost and latency.
    EvalClassifiers {
        #[command(subcommand)]
        action: EvalAction,
        /// Data directory (pool.jsonl, labels.jsonl, pricing.json, verdicts/).
        #[arg(long, default_value = "evals/classifiers", global = true)]
        dir: std::path::PathBuf,
    },
```

And below `enum Commands`:

```rust
#[derive(Subcommand)]
enum EvalAction {
    /// Run models over the pool, reusing cached verdicts.
    Run {
        /// Comma-separated model names (see `charcoal eval-classifiers report`).
        #[arg(long, value_delimiter = ',', default_value = "cope-b,detoxify")]
        models: Vec<String>,
        /// Stop before spending more than this many US dollars this run.
        #[arg(long, default_value_t = 1.0)]
        cap_usd: f64,
    },
    /// Write to-label.jsonl, or with --merge fold your answers into labels.jsonl.
    Label {
        #[arg(long)]
        merge: bool,
    },
    /// Print the comparison table.
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
                EvalAction::Run { models: names, cap_usd } => {
                    let prices = cost::load_pricing(&dir.join("pricing.json"))?;
                    let mut budget = cost::Budget::new(cap_usd);
                    for name in &names {
                        let classifier = match models::build_eval_model(name) {
                            Ok(c) => c,
                            Err(e) => {
                                // A missing key skips that model, not the run.
                                eprintln!("skipping {name}: {e:#}");
                                continue;
                            }
                        };
                        let price = prices.get(name).cloned().unwrap_or_default();
                        let s = runner::run_model(name, classifier.as_ref(), &pool, &verdicts, &price, &mut budget).await?;
                        println!("{name}: judged {}, cached {}, errors {}{}", s.judged, s.cached, s.errors,
                            if s.stopped_at_cap { " — STOPPED AT SPENDING CAP" } else { "" });
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
                        let per_model: Vec<_> = known.iter().map(|m| cache::load(&verdicts, m)).collect::<anyhow::Result<_>>()?;
                        let labels = data::load_labels(&labels_path)?;
                        let picked = labelling::pick_for_labelling(&pool, &per_model, &labels);
                        labelling::write_to_label(&to_label, &picked)?;
                        println!("{} posts to label in {} — fill in toxic or clean, then run with --merge", picked.len(), to_label.display());
                    }
                }
                EvalAction::Report { baseline } => {
                    let labels = data::load_labels(&labels_path)?;
                    let prices = cost::load_pricing(&dir.join("pricing.json"))?;
                    let mut loaded = Vec::new();
                    for m in &known {
                        let recs = cache::load(&verdicts, m)?;
                        if !recs.is_empty() {
                            loaded.push((m.clone(), recs));
                        }
                    }
                    println!("{} labelled posts\n", labels.len());
                    print!("{}", report::render(&report::build(&loaded, &labels, &prices), &baseline));
                }
            }
        }
```

- [ ] **Step 4: Write** `evals/classifiers/README.md`:

````markdown
# Classifier eval (#404)

Compares classifiers on the same posts. See the design in
`docs/superpowers/specs/2026-10-08-404-classifier-eval-design.md`.

One round:

```bash
# 1. Judge every post with each model (cached; $1 cap by default)
cargo run --features web -- eval-classifiers run --models cope-b,detoxify
# 2. Write the posts worth labelling to evals/classifiers/to-label.jsonl
cargo run --features web -- eval-classifiers label
#    ...open to-label.jsonl, set "label" to toxic or clean, optional "note"...
# 3. Fold your answers into labels.jsonl
cargo run --features web -- eval-classifiers label --merge
# 4. Compare
cargo run --features web -- eval-classifiers report
```

- CoPE-B needs `RUNPOD_ENDPOINT_URL` and `RUNPOD_API_KEY`; Detoxify needs the
  local models (`charcoal download-model`). A model whose keys are missing is
  skipped, not fatal.
- Verdicts live in `verdicts/` and are never committed.
- "too close to call — label more" means the two models' 95 % intervals
  overlap: label another round before deciding.
````

- [ ] **Step 5: Add to `CHANGELOG.md`** under `## [Unreleased]` → `### Added`:

```markdown
- #404 — `charcoal eval-classifiers`: compares classifiers (today CoPE-B and
  Detoxify; Jev, Clef and Strands Decider follow in Part B) on the same posts.
  It caches every verdict so re-runs never re-pay, picks only the posts where
  models disagree (plus a stable 10 % of agreed ones) for hand labelling, and
  reports false alarms and misses separately with 95 % intervals — saying
  "too close to call — label more" rather than crowning a lucky winner — along
  with cost per 1,000 posts and latency. A $1 per-run spending cap stops a run
  cleanly; errors are recorded as errors, never as clean. The test suite proves
  the report catches a deliberately bad always-clean and coin-flip model.
```

- [ ] **Step 6: Verify**

```bash
cargo fmt
cargo clippy --features web --all-targets
cargo test --lib eval_classifiers
cargo test --test cli_eval_classifiers
cargo run --features web -- eval-classifiers report
```

Expected: clippy clean; all eval tests pass; the report prints the "10 labelled posts" line and "(baseline "cope-b" has no verdicts — run it first)".

- [ ] **Step 7: Commit**

```bash
git add src/main.rs tests/cli_eval_classifiers.rs evals/classifiers/README.md CHANGELOG.md
git commit -m 'feat(404): charcoal eval-classifiers run|label|report'
```

---

## After Part A

1. **First live round (by hand, with Bryan's go-ahead to spend up to $1):** `eval-classifiers run --models cope-b,detoxify`, then `label`, then Bryan labels, then `label --merge`, then `report`.
2. **Part B plan:** written from `docs/superpowers/specs/404-provider-contracts.md` (Task 1). It adds a `ToxicityClassifier` adapter per unblocked provider (Jev, Clef-flash, Strands Decider), each with wiremock tests built from the verified request and response shapes, registered in `models::build_eval_model` and priced in `pricing.json`. Fairness rule 2 applies there: each decision model receives the full text of `gpu/cope-b-runpod/policy.txt` as its instruction, with answers `toxic` / `clean`, so a better score means a better model rather than an easier question.
