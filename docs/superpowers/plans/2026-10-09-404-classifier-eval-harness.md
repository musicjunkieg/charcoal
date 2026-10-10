# Classifier Eval Harness Implementation Plan (#404, Part A)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `charcoal eval-classifiers` command that runs posts through several classifiers, picks the posts worth hand-labelling, and reports each model's false-alarm rate, miss rate, coverage, cost and latency — with comparisons decided by an exact paired test on an independent holdout.

**Architecture:** A new library module `src/cli/eval_classifiers/` (pure, unit-tested pieces: data, verdict cache, cost, metrics, label picker, report, runner, model registry) behind a thin clap subcommand in `src/main.rs`. Models plug in through the existing `ToxicityClassifier` trait. Part A ships the harness with the two models we already have — CoPE-B (RunPod) and Detoxify (local ONNX) — and verifies the new providers' APIs (Task 1). **Part B** (Jev, Clef-flash, Strands Decider adapters) is a separate short plan written from Task 1's verified contracts.

**Tech Stack:** Rust (tokio, async-trait, serde/serde_json, anyhow), the existing `ort` ONNX scorer and RunPod client with its production `ScanCostMeter`, `tempfile` for tests. No new dependencies.

**Revision notes (2026-10-09):**
- **Round 1** (Codex adversarial review, 8 findings): models scored on their own binary answer (CoPE-B confidence is not p(toxic)); headline rates from an independent holdout; paired comparisons with coverage; cache keyed on content + model + policy; unpriced models refused; per-call timeout; committed `verdicts/.gitignore`; one `cargo test` filter per command.
- **Round 2** (Codex adversarial review, 5 findings): CoPE-B spend bounded by the client's **real** `ScanCostMeter` (worker time) and batched like production, with the eval timeout set above the client's own so jobs are never abandoned; backend identity probed before paid calls and **checked on every verdict**, and report/label read only verdicts current for the pool and identity; coverage enforced **per class** with a **20-paired-labels-per-class** minimum and boundary tests; merge **derives** the source rather than trusting the file; comparisons decided by an **exact paired McNemar test** (α = 0.05), Wilson intervals kept as descriptive context. The effective holdout of the current 110-post pool is **32** posts (4 of the hash-chosen 36 are curated seeds), so the pool must grow before any winner can be named.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-10-08-404-classifier-eval-design.md` (approved 2026-10-09, amended after both Codex reviews).
- Post level only; account-level evaluation is out of scope.
- **Decision rule:** every model is scored on its **own binary answer** (`ClassifierVerdict::toxic_token`). Never derive a probability from `confidence` (for CoPE-B it is `exp(logprob)` of the emitted token over the whole vocabulary — `gpu/cope-b-runpod/handler.py:122`).
- **Headline rates use holdout labels only.** Holdout membership = `fnv1a("holdout:" + id) % 3 == 0`, fixed and independent of models. Diagnostic and seed labels never enter headline rates. The merge step **derives** each label's source from the hash; it never trusts the file.
- Report false-alarm and miss rates **separately**, each with a **95 % Wilson interval** (descriptive); **no blended score**.
- **Comparisons:** per truth class, on the paired cohort (holdout posts of that class both models answered), by an **exact two-sided McNemar test** on the discordant pairs at **α = 0.05**. A verdict needs, per class, **each model's coverage ≥ 90 %** and **≥ 20 paired posts**; otherwise the report says "no verdict — coverage below 90 %" or "not enough holdout labels (need 20 per class) — label more". Non-significant → "too close to call — label more".
- Errors, timeouts and **identity mismatches** are recorded as **errors, never as clean**.
- Before any paid call a backend's `probe_identity()` must pass; every returned verdict's `model_id` and `policy_version` must equal the declared ones.
- A cached verdict is **current** only while its content hash matches the pool's present text and its model/policy match the identity of the last run. Report and label read **only current** verdicts.
- **A model without a pricing entry is refused.** Default cap **$1.00 per run**. For CoPE-B the cap is enforced by the RunPod client's real `ScanCostMeter` (worker-seconds × rate); for priced-per-token models it is enforced by reservation from whole-request estimates (Part B adds provider-reported usage).
- The eval's per-call timeout (default **600 s**) is a last-resort hang guard, set **above** the RunPod client's own warm-up and steady timeouts so the eval never abandons a job the client still owns.
- New adapters are reachable **only** from eval code (`models::build_eval_model`), never from production `build_from_env`.
- **CI never calls a paid API.** All tests use fake classifiers.
- Data: `evals/classifiers/pool.jsonl`, `labels.jsonl`, `pricing.json` committed (PII-scrubbed); `evals/classifiers/verdicts/` never committed, enforced by a committed `evals/classifiers/verdicts/.gitignore`.
- No new crate dependencies: pricing is JSON; hashing is FNV-1a 64-bit.
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
| `evals/classifiers/README.md` | How to run a round, and how to grow the pool |
| `src/cli/eval_classifiers/mod.rs` | Module root; `fnv1a` and `content_hash` |
| `src/cli/eval_classifiers/data.rs` | Load pool and labels |
| `src/cli/eval_classifiers/metrics.rs` | Wilson interval, rates, exact paired comparison |
| `src/cli/eval_classifiers/cache.rs` | `VerdictRecord`, JSONL cache, identity file, `current` filter |
| `src/cli/eval_classifiers/cost.rs` | Pricing (required), estimates, reservation budget |
| `src/cli/eval_classifiers/runner.rs` | Probe, batch, check identity, timeout, cap |
| `src/cli/eval_classifiers/labelling.rs` | Holdout + diagnostic picker; merge that derives the source |
| `src/cli/eval_classifiers/report.rs` | Plain-text report |
| `src/cli/eval_classifiers/models.rs` | Eval-only model registry (CoPE-B with the real cost meter) |
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
  4. Response shape — where the chosen answer lives, and whether any probability is **normalised over the answer set**.
  5. Batching: several posts per request; can the policy be sent once per batch?
  6. **Billing and usage:** does the response report tokens or cost per request? Can a submitted request be cancelled, and is a cancelled/timed-out request still billed? (Part B must bound real spend, not estimates.)
  7. Price per million input tokens (and output, if any), with the date read.
  8. Rate limits.
  9. Data retention / training-use terms for submitted text.
  10. **Identity:** does the response say which model version served it?

- [ ] **Step 3: Gate.** A provider that cannot take a free-text policy as its instruction (item 3), or whose spend cannot be bounded by reported usage or an enforced limit (item 6), is marked **BLOCKED** at the top of the file with the reason. Do not plan an adapter for it; bring it to Bryan.

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
  - `pub fn load_labels(path: &Path) -> Result<HashMap<String, Answer>>` — missing file → empty map; a row without `source` is an error

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

Expected: `110 evals/classifiers/pool.jsonl`, `10 evals/classifiers/labels.jsonl`. (Of the 110, 36 hash into the holdout; 4 of those are seeds, so **32** holdout posts remain to label — too few for a verdict. Growing the pool is a documented step in the README, Task 10.)

Create `evals/classifiers/pricing.json`. The CoPE-B figure is a planning estimate (observed $2.67 / ~20,000-post staging scan); CoPE-B's real spend is bounded by its cost meter (Task 9). Detoxify runs locally and is explicitly free.

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
/// cached verdicts stop being current.
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

### Task 3: Metrics — Wilson intervals, per-class coverage, exact paired test

**Files:**
- Create: `src/cli/eval_classifiers/metrics.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod metrics;`)

**Interfaces:**
- Consumes: `data::Truth`.
- Produces:
  - `pub struct Rate { pub hits: u32, pub n: u32 }` with `fn value(&self) -> Option<f64>`, `fn wilson95(&self) -> Option<(f64, f64)>`
  - `pub struct ModelMetrics { pub model: String, pub false_alarm: Rate, pub miss: Rate, pub labelled: u32, pub judged: u32, pub errors: u32 }` with `fn coverage(&self) -> Option<f64>`
  - `pub fn model_metrics(model: &str, calls: &HashMap<String, Option<bool>>, labels: &HashMap<String, Truth>) -> ModelMetrics` — `Some(true)` toxic, `Some(false)` clean, `None` error; a labelled post with no record counts in `labelled` only
  - `pub const MIN_COVERAGE: f64 = 0.9; pub const MIN_CLASS_N: u32 = 20; pub const ALPHA: f64 = 0.05;`
  - `pub enum Comparison { Better, Worse, TooClose, NotEnoughLabels, LowCoverage }`
  - `pub fn mcnemar_exact_p(b: u32, c: u32) -> f64` — two-sided exact sign test on discordant pairs
  - `pub struct Paired { pub baseline: ModelMetrics, pub candidate: ModelMetrics, pub false_alarm: Comparison, pub miss: Comparison }` — `baseline`/`candidate` are scored on the shared cohort (for display)
  - `pub fn paired(base_name: &str, base: &HashMap<String, Option<bool>>, cand_name: &str, cand: &HashMap<String, Option<bool>>, labels: &HashMap<String, Truth>) -> Paired` — `false_alarm` decided on the Clean class, `miss` on the Toxic class

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::eval_classifiers::data::Truth;

    fn map<T: Clone>(pairs: &[(&str, T)]) -> HashMap<String, T> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    /// `n` toxic labels t0.. and `n` clean labels c0...
    fn labels(n: usize) -> HashMap<String, Truth> {
        (0..n)
            .flat_map(|i| [(format!("t{i}"), Truth::Toxic), (format!("c{i}"), Truth::Clean)])
            .collect()
    }

    /// Answers every label correctly, except the given ids (wrong) and
    /// `errors` (call failed).
    fn calls(labels: &HashMap<String, Truth>, wrong: &[String], errors: &[String]) -> HashMap<String, Option<bool>> {
        labels
            .iter()
            .map(|(id, t)| {
                let right = *t == Truth::Toxic;
                let call = if errors.contains(id) { None } else if wrong.contains(id) { Some(!right) } else { Some(right) };
                (id.clone(), call)
            })
            .collect()
    }

    fn ids(prefix: &str, range: std::ops::Range<usize>) -> Vec<String> {
        range.map(|i| format!("{prefix}{i}")).collect()
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
    }

    #[test]
    fn mcnemar_exact_values() {
        // 0 vs 6 discordant pairs: 2 × (1/2)^6 = 0.03125 (Codex's example).
        assert!((mcnemar_exact_p(0, 6) - 0.031_25).abs() < 1e-12);
        assert!((mcnemar_exact_p(0, 5) - 0.0625).abs() < 1e-12);
        assert_eq!(mcnemar_exact_p(1, 1), 1.0);
        assert_eq!(mcnemar_exact_p(0, 0), 1.0);
    }

    #[test]
    fn a_significantly_better_candidate_is_called_better() {
        let l = labels(25);
        // The baseline misses t0..t7; the candidate misses none: 8 discordant
        // pairs all favouring the candidate, p = 2/256 ≈ 0.0078.
        let base = calls(&l, &ids("t", 0..8), &[]);
        let cand = calls(&l, &[], &[]);
        let p = paired("base", &base, "cand", &cand, &l);
        assert_eq!(p.miss, Comparison::Better);
        assert_eq!(p.false_alarm, Comparison::TooClose, "no discordant clean pairs");
    }

    /// Codex review: per-class coverage, tested exactly at the boundary.
    #[test]
    fn coverage_is_enforced_per_class_at_the_90_percent_boundary() {
        let l = labels(30);
        let base = calls(&l, &[], &[]);
        // 3 errors out of 30 toxic = 90 % coverage: allowed.
        let ok = calls(&l, &[], &ids("t", 0..3));
        assert_ne!(paired("b", &base, "c", &ok, &l).miss, Comparison::LowCoverage);
        // 4 errors out of 30 toxic = 86.7 %: refused, even though clean is perfect.
        let low = calls(&l, &[], &ids("t", 0..4));
        let p = paired("b", &base, "c", &low, &l);
        assert_eq!(p.miss, Comparison::LowCoverage);
        assert_ne!(p.false_alarm, Comparison::LowCoverage, "the clean class is judged on its own");
    }

    /// Codex review: with too few paired labels no winner is named, however
    /// lopsided the counts.
    #[test]
    fn too_few_labels_per_class_gives_no_verdict() {
        let l = labels(10);
        let base = calls(&l, &ids("t", 0..6), &[]);
        let cand = calls(&l, &[], &[]);
        assert_eq!(paired("b", &base, "c", &cand, &l).miss, Comparison::NotEnoughLabels);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::metrics`
Expected: compile error — items not found.

- [ ] **Step 3: Implement** `src/cli/eval_classifiers/metrics.rs`:

```rust
//! Error rates and model comparisons. Rates carry 95 % Wilson intervals as
//! description; a comparison is DECIDED by an exact paired test on the posts
//! where the two models disagree, with minimum label and coverage gates set
//! in advance (Codex review, round 2).

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

    /// 95 % Wilson score interval — well-behaved at 0 and n.
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
    /// Labelled posts whose call failed, timed out or mismatched identity —
    /// never counted as clean.
    pub errors: u32,
}

impl ModelMetrics {
    pub fn coverage(&self) -> Option<f64> {
        (self.labelled > 0).then(|| self.judged as f64 / self.labelled as f64)
    }
}

fn is_wrong(called_toxic: bool, truth: Truth) -> bool {
    match truth {
        Truth::Toxic => !called_toxic,
        Truth::Clean => called_toxic,
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
                if is_wrong(*called_toxic, *truth) {
                    rate.hits += 1;
                }
            }
        }
    }
    m
}

/// Gates, fixed before any data is seen.
pub const MIN_COVERAGE: f64 = 0.9;
pub const MIN_CLASS_N: u32 = 20;
pub const ALPHA: f64 = 0.05;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    Better,
    Worse,
    TooClose,
    NotEnoughLabels,
    LowCoverage,
}

/// Exact two-sided McNemar (sign) test: under "no difference" each of the
/// `b + c` discordant pairs is a fair coin. Exact rather than chi-squared
/// because the counts here are small. Fine for n ≤ 1000 in f64.
pub fn mcnemar_exact_p(b: u32, c: u32) -> f64 {
    let n = b + c;
    if n == 0 {
        return 1.0;
    }
    let k = b.min(c);
    let mut coef = 1.0_f64; // C(n, 0)
    let mut tail = 0.0_f64;
    for i in 0..=k {
        if i > 0 {
            coef = coef * f64::from(n - i + 1) / f64::from(i);
        }
        tail += coef;
    }
    (2.0 * tail / 2.0_f64.powi(n as i32)).min(1.0)
}

/// One truth class: coverage gate, label gate, then the exact paired test.
fn compare_class(
    base: &HashMap<String, Option<bool>>,
    cand: &HashMap<String, Option<bool>>,
    labels: &HashMap<String, Truth>,
    class: Truth,
) -> Comparison {
    let in_class: Vec<&String> = labels.iter().filter(|(_, t)| **t == class).map(|(id, _)| id).collect();
    if in_class.is_empty() {
        return Comparison::NotEnoughLabels;
    }
    let coverage = |calls: &HashMap<String, Option<bool>>| {
        let judged = in_class.iter().filter(|id| matches!(calls.get(**id), Some(Some(_)))).count();
        judged as f64 / in_class.len() as f64
    };
    // Per class: a model that fails on the hard toxic posts cannot hide it
    // behind a perfect clean class (Codex review).
    if coverage(base) < MIN_COVERAGE || coverage(cand) < MIN_COVERAGE {
        return Comparison::LowCoverage;
    }
    let (mut shared, mut b, mut c) = (0u32, 0u32, 0u32);
    for id in &in_class {
        if let (Some(Some(bc)), Some(Some(cc))) = (base.get(*id), cand.get(*id)) {
            shared += 1;
            match (is_wrong(*bc, class), is_wrong(*cc, class)) {
                (false, true) => b += 1, // candidate wrong only
                (true, false) => c += 1, // baseline wrong only
                _ => {}
            }
        }
    }
    if shared < MIN_CLASS_N {
        return Comparison::NotEnoughLabels;
    }
    if mcnemar_exact_p(b, c) >= ALPHA {
        Comparison::TooClose
    } else if b < c {
        Comparison::Better
    } else {
        Comparison::Worse
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Paired {
    pub baseline: ModelMetrics,
    pub candidate: ModelMetrics,
    pub false_alarm: Comparison,
    pub miss: Comparison,
}

pub fn paired(
    base_name: &str,
    base: &HashMap<String, Option<bool>>,
    cand_name: &str,
    cand: &HashMap<String, Option<bool>>,
    labels: &HashMap<String, Truth>,
) -> Paired {
    let shared: HashMap<String, Truth> = labels
        .iter()
        .filter(|(id, _)| matches!(base.get(*id), Some(Some(_))) && matches!(cand.get(*id), Some(Some(_))))
        .map(|(id, t)| (id.clone(), *t))
        .collect();
    Paired {
        baseline: model_metrics(base_name, base, &shared),
        candidate: model_metrics(cand_name, cand, &shared),
        false_alarm: compare_class(base, cand, labels, Truth::Clean),
        miss: compare_class(base, cand, labels, Truth::Toxic),
    }
}
```

Add `pub mod metrics;` to `mod.rs`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --lib eval_classifiers::metrics`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```bash
git add src/cli/eval_classifiers/metrics.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): exact paired McNemar comparison with per-class coverage and label gates'
```

---

### Task 4: Verdict cache, identity file and the "current" filter

**Files:**
- Create: `src/cli/eval_classifiers/cache.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod cache;`)

**Interfaces:**
- Consumes: `ClassifierVerdict`, `super::content_hash`, `data::FixtureRow`.
- Produces:
  - `pub struct VerdictRecord { pub id: String, pub model: String, pub content_hash: String, pub model_id: String, pub policy_version: String, pub called_toxic: Option<bool>, pub error: Option<String>, pub latency_ms: u32, pub input_chars: usize, pub est_usd: f64 }`
  - `pub struct Identity<'a> { pub model_id: &'a str, pub policy_version: &'a str }`
  - `pub struct IdentityRecord { pub model_id: String, pub policy_version: String }` (serde)
  - `impl VerdictRecord { pub fn from_verdict(id, model, content, &ClassifierVerdict, est_usd) -> Self; pub fn from_error(id, model, content, Identity, detail, est_usd) -> Self; pub fn reusable(&self, content: &str, who: Identity<'_>) -> bool }`
  - `pub fn append(dir: &Path, rec: &VerdictRecord) -> Result<()>`; `pub fn load(dir: &Path, model: &str) -> Result<HashMap<String, VerdictRecord>>` (last record per id wins)
  - `pub fn save_identity(dir: &Path, model: &str, who: Identity<'_>) -> Result<()>`; `pub fn load_identity(dir: &Path, model: &str) -> Result<Option<IdentityRecord>>`
  - `pub fn current(recs: HashMap<String, VerdictRecord>, pool: &[FixtureRow], who: Option<&IdentityRecord>) -> HashMap<String, VerdictRecord>` — keeps only records for posts in the pool whose content hash matches the pool's text and whose model/policy match `who` (no identity recorded → nothing is current)

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
    fn row(id: &str, content: &str) -> FixtureRow {
        serde_json::from_value(serde_json::json!({"id": id, "content": content})).unwrap()
    }

    /// Codex review, critical: CoPE-B reports confidence as exp(logprob) of the
    /// emitted token over the WHOLE vocabulary. A "clean" answer with 0.1 must
    /// stay clean — the repository's own RunPod fixture shape.
    #[test]
    fn the_models_own_answer_is_kept_whatever_its_confidence() {
        assert_eq!(VerdictRecord::from_verdict("a", "cope-b", "x", &verdict(false, 0.1), 0.0).called_toxic, Some(false));
        assert_eq!(VerdictRecord::from_verdict("b", "cope-b", "x", &verdict(true, 0.2), 0.0).called_toxic, Some(true));
        assert_eq!(VerdictRecord::from_error("c", "cope-b", "x", WHO, "timed out", 0.0).called_toxic, None);
    }

    #[test]
    fn reuse_requires_same_content_model_and_policy() {
        let r = VerdictRecord::from_verdict("a", "cope-b", "post text", &verdict(true, 0.9), 0.0);
        assert!(r.reusable("post text", WHO));
        assert!(!r.reusable("post text, edited", WHO));
        assert!(!r.reusable("post text", Identity { model_id: "m-2", policy_version: "p-1" }));
        assert!(!r.reusable("post text", Identity { model_id: "m-1", policy_version: "p-2" }));
        assert!(!VerdictRecord::from_error("a", "cope-b", "post text", WHO, "boom", 0.0).reusable("post text", WHO));
    }

    #[test]
    fn cache_and_identity_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        append(dir.path(), &VerdictRecord::from_error("a", "m", "x", WHO, "timeout", 0.0)).unwrap();
        append(dir.path(), &VerdictRecord::from_verdict("a", "m", "x", &verdict(true, 0.9), 0.01)).unwrap();
        let got = load(dir.path(), "m").unwrap();
        assert_eq!(got["a"].called_toxic, Some(true), "the last record wins");
        assert!(load(dir.path(), "other").unwrap().is_empty());
        assert_eq!(load_identity(dir.path(), "m").unwrap(), None);
        save_identity(dir.path(), "m", WHO).unwrap();
        assert_eq!(load_identity(dir.path(), "m").unwrap().unwrap().model_id, "m-1");
    }

    /// Codex review, round 2: report and label must not score stale or
    /// foreign verdicts.
    #[test]
    fn only_current_verdicts_survive() {
        let pool = vec![row("kept", "same"), row("edited", "new text"), row("err", "e")];
        let mut recs = HashMap::new();
        recs.insert("kept".into(), VerdictRecord::from_verdict("kept", "m", "same", &verdict(true, 0.9), 0.0));
        recs.insert("edited".into(), VerdictRecord::from_verdict("edited", "m", "old text", &verdict(true, 0.9), 0.0));
        recs.insert("gone".into(), VerdictRecord::from_verdict("gone", "m", "x", &verdict(true, 0.9), 0.0));
        recs.insert("err".into(), VerdictRecord::from_error("err", "m", "e", WHO, "timed out", 0.0));
        let who = IdentityRecord { model_id: "m-1".into(), policy_version: "p-1".into() };
        let cur = current(recs.clone(), &pool, Some(&who));
        let mut keys: Vec<&str> = cur.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["err", "kept"], "errors still count toward coverage");
        let newer = IdentityRecord { model_id: "m-1".into(), policy_version: "p-2".into() };
        assert!(current(recs.clone(), &pool, Some(&newer)).is_empty(), "redeployed: nothing current");
        assert!(current(recs, &pool, None).is_empty(), "no identity recorded: nothing current");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::cache`
Expected: compile error.

- [ ] **Step 3: Implement** `src/cli/eval_classifiers/cache.rs`:

```rust
//! Every verdict ever paid for, one JSONL file per model, plus the identity
//! (model id + policy version) of the last run. A verdict is reused, reported
//! or labelled from only while it is CURRENT: same post text, same model,
//! same policy (Codex review, rounds 1 and 2).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::content_hash;
use super::data::FixtureRow;
use crate::toxicity::classifier::ClassifierVerdict;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerdictRecord {
    pub id: String,
    /// The eval's name for the model (file name), e.g. "cope-b".
    pub model: String,
    pub content_hash: String,
    /// What the backend reported serving.
    pub model_id: String,
    pub policy_version: String,
    /// The model's OWN binary answer. `None` = failed, timed out, or the
    /// backend reported an unexpected identity. Never derived from confidence.
    pub called_toxic: Option<bool>,
    pub error: Option<String>,
    pub latency_ms: u32,
    pub input_chars: usize,
    /// Estimated spend for this post, from pricing.json.
    pub est_usd: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct Identity<'a> {
    pub model_id: &'a str,
    pub policy_version: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityRecord {
    pub model_id: String,
    pub policy_version: String,
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

    fn matches(&self, content: &str, model_id: &str, policy_version: &str) -> bool {
        self.content_hash == content_hash(content)
            && self.model_id == model_id
            && self.policy_version == policy_version
    }

    /// A successful, current verdict: nothing to re-judge.
    pub fn reusable(&self, content: &str, who: Identity<'_>) -> bool {
        self.called_toxic.is_some() && self.matches(content, who.model_id, who.policy_version)
    }
}

fn path_for(dir: &Path, model: &str) -> PathBuf {
    dir.join(format!("{model}.jsonl"))
}

fn identity_path(dir: &Path, model: &str) -> PathBuf {
    dir.join(format!("{model}.identity.json"))
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

/// The identity the last run verified, so report and label — which build no
/// backend — know which verdicts are current.
pub fn save_identity(dir: &Path, model: &str, who: Identity<'_>) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let rec = IdentityRecord { model_id: who.model_id.to_string(), policy_version: who.policy_version.to_string() };
    let path = identity_path(dir, model);
    std::fs::write(&path, serde_json::to_string(&rec)?).with_context(|| format!("writing {}", path.display()))
}

pub fn load_identity(dir: &Path, model: &str) -> Result<Option<IdentityRecord>> {
    let path = identity_path(dir, model);
    match std::fs::read_to_string(&path) {
        Ok(t) => Ok(Some(serde_json::from_str(&t).with_context(|| format!("parsing {}", path.display()))?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The ONE gate every consumer goes through: only verdicts for posts still
/// in the pool, with the pool's present text, from the identity last
/// verified. Errors that pass are kept — they count against coverage.
pub fn current(
    recs: HashMap<String, VerdictRecord>,
    pool: &[FixtureRow],
    who: Option<&IdentityRecord>,
) -> HashMap<String, VerdictRecord> {
    let Some(who) = who else {
        return HashMap::new();
    };
    let text: HashMap<&str, &str> = pool.iter().map(|r| (r.id.as_str(), r.content.as_str())).collect();
    recs.into_iter()
        .filter(|(id, r)| {
            text.get(id.as_str())
                .is_some_and(|content| r.matches(content, &who.model_id, &who.policy_version))
        })
        .collect()
}
```

Add `pub mod cache;` to `mod.rs`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --lib eval_classifiers::cache`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/cli/eval_classifiers/cache.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): verdict cache with identity file and a single current-verdict gate'
```

---

### Task 5: Pricing (required) and the reservation budget

**Files:**
- Create: `src/cli/eval_classifiers/cost.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod cost;`)

**Interfaces:**
- Produces:
  - `pub struct Price { pub usd_per_post: Option<f64>, pub usd_per_million_input_tokens: Option<f64>, pub overhead_chars: usize }` (serde)
  - `pub fn load_pricing(path: &Path) -> Result<HashMap<String, Price>>` — an entry with neither price field is an error
  - `pub fn price_for<'a>(prices: &'a HashMap<String, Price>, model: &str) -> Result<&'a Price>` — a model with no entry is refused
  - `pub fn estimate_usd(price: &Price, input_chars: usize) -> f64`
  - `pub struct Budget` with `pub fn new(cap_usd: f64) -> Self`, `pub fn try_spend(&mut self, usd: f64) -> bool`, `pub fn spent(&self) -> f64`

This budget is the cap for **priced-per-token** models (Part B, plus provider-reported usage). CoPE-B's cap is enforced separately by its real cost meter (Task 9); the budget still reserves CoPE-B's estimate so the printed "estimated spend" stays meaningful.

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
        assert!(!b.try_spend(0.5));
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
//! Cost estimates and the reservation budget. A model nobody priced must not
//! run as if it were free (Codex review). For CoPE-B, real spend is bounded
//! by the RunPod client's own cost meter (models.rs); this budget bounds
//! priced-per-token models and keeps the printed estimate honest.

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
    /// decision model is given as its instruction.
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

/// ~4 characters per token: a rough English average.
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
git commit -m 'feat(404): required pricing, whole-request estimates and a reservation budget'
```

---

### Task 6: The runner — probe, batch, identity check, timeout, cap

**Files:**
- Create: `src/cli/eval_classifiers/runner.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod runner;`)

**Interfaces:**
- Consumes: `data::FixtureRow`, `cache::{append, load, save_identity, Identity, VerdictRecord}`, `cost::{estimate_usd, Budget, Price}`, `ToxicityClassifier` (`probe_identity`, `classify_batch`, `max_batch_size`, `model_id`, `policy_version`), `ItemOutcome`, `crate::toxicity::cost_meter::CostCeilingExceeded`.
- Produces:
  - `pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);`
  - `pub struct RunStats { pub model: String, pub judged: usize, pub cached: usize, pub errors: usize, pub stopped_at_cap: bool }`
  - `pub async fn run_model(model: &str, classifier: &dyn ToxicityClassifier, pool: &[FixtureRow], dir: &Path, price: &Price, budget: &mut Budget, timeout: Duration) -> Result<RunStats>` — errors (before any paid call) if `probe_identity` fails

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::eval_classifiers::cost::Price;
    use crate::toxicity::classifier::{ClassifierVerdict, ToxicityClassifier};
    use crate::toxicity::cost_meter::CostCeilingExceeded;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fake {
        calls: AtomicUsize,
        fail_on: Option<&'static str>,
        hang_on: Option<&'static str>,
        capped_on: Option<&'static str>,
        policy: &'static str,
        reported_model: &'static str,
        probe_fails: bool,
    }

    impl Fake {
        fn new() -> Self {
            Self { calls: AtomicUsize::new(0), fail_on: None, hang_on: None, capped_on: None, policy: "p-1", reported_model: "fake-1", probe_fails: false }
        }
    }

    #[async_trait::async_trait]
    impl ToxicityClassifier for Fake {
        async fn classify(&self, content: &str) -> anyhow::Result<ClassifierVerdict> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.hang_on.is_some_and(|h| content.contains(h)) {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            if self.capped_on.is_some_and(|c| content.contains(c)) {
                return Err(CostCeilingExceeded { est_cents: 100, ceiling_cents: 100 }.into());
            }
            if self.fail_on.is_some_and(|f| content.contains(f)) {
                anyhow::bail!("upstream 503");
            }
            Ok(ClassifierVerdict { toxic_token: content.contains("bad"), confidence: 0.9, latency_ms: 5, model_id: self.reported_model.into(), policy_version: self.policy.into() })
        }
        async fn probe_identity(&self) -> anyhow::Result<()> {
            if self.probe_fails {
                anyhow::bail!("endpoint serves policy-old, deployment declares p-1");
            }
            Ok(())
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
        assert!(crate::cli::eval_classifiers::cache::load_identity(dir.path(), "m").unwrap().is_some());
    }

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

    /// Codex review, round 2: a backend that fails its identity probe is not
    /// paid for at all.
    #[tokio::test]
    async fn a_failed_identity_probe_stops_before_any_call() {
        let dir = tempfile::tempdir().unwrap();
        let model = Fake { probe_fails: true, ..Fake::new() };
        let mut budget = Budget::new(1.0);
        let err = run_model("m", &model, &[row("a", "bad")], dir.path(), &Price::default(), &mut budget, T).await.unwrap_err();
        assert!(format!("{err:#}").contains("identity"), "{err:#}");
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    }

    /// Codex review, round 2: a verdict from an unexpected model is an error.
    #[tokio::test]
    async fn a_verdict_from_a_foreign_model_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let model = Fake { reported_model: "someone-else", ..Fake::new() };
        let mut budget = Budget::new(1.0);
        let s = run_model("m", &model, &[row("a", "bad")], dir.path(), &Price::default(), &mut budget, T).await.unwrap();
        assert_eq!((s.judged, s.errors), (0, 1));
        let rec = &load(dir.path(), "m").unwrap()["a"];
        assert_eq!(rec.called_toxic, None);
        assert!(rec.error.as_deref().unwrap().contains("identity mismatch"));
    }

    #[tokio::test]
    async fn an_error_is_recorded_as_an_error_and_retried_next_run() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad 503")];
        let mut budget = Budget::new(1.0);
        let s = run_model("m", &Fake { fail_on: Some("503"), ..Fake::new() }, &pool, dir.path(), &Price::default(), &mut budget, T).await.unwrap();
        assert_eq!(s.errors, 1);
        assert_eq!(load(dir.path(), "m").unwrap()["a"].called_toxic, None, "an error, never clean");
        let ok = Fake::new();
        run_model("m", &ok, &pool, dir.path(), &Price::default(), &mut budget, T).await.unwrap();
        assert_eq!(ok.calls.load(Ordering::SeqCst), 1, "the error was retried");
    }

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
        assert!(load(dir.path(), "m").unwrap()["a"].error.as_deref().unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn the_reservation_cap_stops_the_run_and_keeps_what_was_paid_for() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "x"), row("b", "y"), row("c", "z")];
        let price = Price { usd_per_post: Some(0.4), usd_per_million_input_tokens: None, overhead_chars: 0 };
        let mut budget = Budget::new(1.0);
        let s = run_model("m", &Fake::new(), &pool, dir.path(), &price, &mut budget, T).await.unwrap();
        assert!(s.stopped_at_cap);
        assert_eq!(s.judged, 2);
        assert_eq!(load(dir.path(), "m").unwrap().len(), 2, "both paid verdicts kept");
    }

    /// Codex review, round 2: the backend's REAL cost meter tripping stops the
    /// run too — the cap is not only an estimate.
    #[tokio::test]
    async fn the_backends_real_cost_ceiling_stops_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad"), row("b", "capped"), row("c", "bad")];
        let model = Fake { capped_on: Some("capped"), ..Fake::new() };
        let mut budget = Budget::new(1.0);
        let s = run_model("m", &model, &pool, dir.path(), &Price::default(), &mut budget, T).await.unwrap();
        assert!(s.stopped_at_cap);
        assert_eq!(s.judged, 1);
        assert_eq!(model.calls.load(Ordering::SeqCst), 2, "c was never sent");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::runner`
Expected: compile error.

- [ ] **Step 3: Implement** `src/cli/eval_classifiers/runner.rs`:

```rust
//! Run one model over the pool:
//! 1. probe the backend's identity before any paid call;
//! 2. reuse only current verdicts;
//! 3. send the rest in the backend's own batch size (as production does);
//! 4. reject verdicts from an unexpected model or policy;
//! 5. bound each batch with a last-resort timeout, set ABOVE the backend's own
//!    so a job the backend still owns is never abandoned;
//! 6. stop at the reservation cap or when the backend's real cost meter trips.

use anyhow::{Context, Result};
use std::path::Path;
use std::time::Duration;

use super::cache::{append, load, save_identity, Identity, VerdictRecord};
use super::cost::{estimate_usd, Budget, Price};
use super::data::FixtureRow;
use crate::toxicity::classifier::{ItemOutcome, ToxicityClassifier};
use crate::toxicity::cost_meter::CostCeilingExceeded;

/// Above the RunPod client's own warm-up (180–300 s) and steady timeouts:
/// this guards against a hang, it must not race the backend's own timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

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
    classifier
        .probe_identity()
        .await
        .with_context(|| format!("{model}: identity probe failed — not running it"))?;
    let who = Identity {
        model_id: classifier.model_id(),
        policy_version: classifier.policy_version(),
    };
    save_identity(dir, model, who)?;

    let cached = load(dir, model)?;
    let mut stats = RunStats {
        model: model.to_string(),
        judged: 0,
        cached: 0,
        errors: 0,
        stopped_at_cap: false,
    };
    let todo: Vec<&FixtureRow> = pool
        .iter()
        .filter(|row| {
            let hit = cached.get(&row.id).is_some_and(|r| r.reusable(&row.content, who));
            if hit {
                stats.cached += 1;
            }
            !hit
        })
        .collect();

    for chunk in todo.chunks(classifier.max_batch_size().max(1)) {
        let ests: Vec<f64> = chunk.iter().map(|r| estimate_usd(price, r.content.chars().count())).collect();
        if !budget.try_spend(ests.iter().sum()) {
            stats.stopped_at_cap = true;
            break;
        }
        let contents: Vec<String> = chunk.iter().map(|r| r.content.clone()).collect();
        let mut records = Vec::with_capacity(chunk.len());
        match tokio::time::timeout(timeout, classifier.classify_batch(&contents)).await {
            Ok(Ok(outcomes)) if outcomes.len() == chunk.len() => {
                for ((row, est), outcome) in chunk.iter().zip(&ests).zip(outcomes) {
                    records.push(match outcome {
                        ItemOutcome::Verdict(v) if v.model_id == who.model_id && v.policy_version == who.policy_version => {
                            VerdictRecord::from_verdict(&row.id, model, &row.content, &v, *est)
                        }
                        ItemOutcome::Verdict(v) => {
                            let detail = format!(
                                "identity mismatch: backend reported {}/{}, deployment declares {}/{}",
                                v.model_id, v.policy_version, who.model_id, who.policy_version
                            );
                            VerdictRecord::from_error(&row.id, model, &row.content, who, &detail, *est)
                        }
                        ItemOutcome::Error(detail) => {
                            VerdictRecord::from_error(&row.id, model, &row.content, who, &detail, *est)
                        }
                    });
                }
            }
            Ok(Ok(outcomes)) => {
                let detail = format!("backend returned {} outcomes for {} posts", outcomes.len(), chunk.len());
                for (row, est) in chunk.iter().zip(&ests) {
                    records.push(VerdictRecord::from_error(&row.id, model, &row.content, who, &detail, *est));
                }
            }
            // The backend's REAL cost meter tripped: stop, record nothing for
            // this batch (it was refused, not judged).
            Ok(Err(e)) if e.downcast_ref::<CostCeilingExceeded>().is_some() => {
                stats.stopped_at_cap = true;
                break;
            }
            Ok(Err(e)) => {
                let detail = format!("{e:#}");
                for (row, est) in chunk.iter().zip(&ests) {
                    records.push(VerdictRecord::from_error(&row.id, model, &row.content, who, &detail, *est));
                }
            }
            Err(_) => {
                let detail = format!("timed out after {} s", timeout.as_secs_f64());
                for (row, est) in chunk.iter().zip(&ests) {
                    records.push(VerdictRecord::from_error(&row.id, model, &row.content, who, &detail, *est));
                }
            }
        }
        for rec in &records {
            if rec.called_toxic.is_some() {
                stats.judged += 1;
            } else {
                stats.errors += 1;
            }
            // Written as each batch returns, so a crash or a cap keeps every paid verdict.
            append(dir, rec)?;
        }
    }
    Ok(stats)
}
```

Add `pub mod runner;` to `mod.rs`. Note: `classify_batch`'s default implementation loops `classify` and short-circuits on the first error, so for a batch size of 1 (the fakes, Detoxify) each post is its own batch — which is what the tests above rely on.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --lib eval_classifiers::runner`
Expected: 8 passed, in well under 5 seconds.

- [ ] **Step 5: Commit**

```bash
git add src/cli/eval_classifiers/runner.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): runner probes identity, batches, rejects foreign verdicts and honours real cost ceilings'
```

---

### Task 7: The label picker and a merge that derives the source

**Files:**
- Create: `src/cli/eval_classifiers/labelling.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod labelling;`)

**Interfaces:**
- Consumes: `data::{Answer, FixtureRow, Label, Source, Truth, load_labels}`, `cache::VerdictRecord`, `super::fnv1a`.
- Produces:
  - `pub fn in_holdout(id: &str) -> bool` — `fnv1a("holdout:" + id) % 3 == 0`
  - `pub fn sampled(id: &str) -> bool` — `fnv1a(id) % 10 == 0`
  - `pub fn source_for(id: &str) -> Source` — `Holdout` if `in_holdout(id)`, else `Diagnostic`
  - `pub fn pick_for_labelling(pool: &[FixtureRow], per_model: &[HashMap<String, VerdictRecord>], labels: &HashMap<String, Answer>) -> Vec<(FixtureRow, Source)>` — callers pass **current** verdicts only
  - `pub fn write_to_label(path: &Path, rows: &[(FixtureRow, Source)]) -> Result<()>` — JSONL `{id, content, source, label: "", note: ""}`; no model votes
  - `pub fn merge_labels(to_label: &Path, labels: &Path, pool: &[FixtureRow]) -> Result<usize>` — **ignores** the file's `source` and uses `source_for(id)`; errors on an id not in the pool, on content that differs from the pool, or on a label other than `toxic`/`clean`

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
    fn holdout_id() -> String {
        (0..100).map(|i| format!("h{i}")).find(|id| in_holdout(id)).unwrap()
    }
    fn other_id() -> String {
        (0..100).map(|i| format!("o{i}")).find(|id| !in_holdout(id)).unwrap()
    }

    #[test]
    fn holdout_and_sample_fractions_are_stable() {
        let holdout = (0..3000).filter(|i| in_holdout(&format!("post-{i}"))).count();
        assert!((900..=1100).contains(&holdout), "{holdout} of 3000 ≈ 1/3");
        let sample = (0..2000).filter(|i| sampled(&format!("post-{i}"))).count();
        assert!((140..=260).contains(&sample), "{sample} of 2000 ≈ 1/10");
    }

    #[test]
    fn holdout_posts_are_picked_whatever_the_models_say() {
        let picked = pick_for_labelling(&[row(&holdout_id())], &[], &HashMap::new());
        assert_eq!(picked.len(), 1);
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
        assert!(!picked.iter().any(|(id, _)| *id == agree));
        assert!(!picked.iter().any(|(id, _)| *id == labelled));
    }

    /// Codex review, round 2: the source is derived, never trusted — a
    /// diagnostic post relabelled "holdout" in the file stays diagnostic.
    #[test]
    fn merge_derives_the_source_and_validates_against_the_pool() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let labels = dir.path().join("labels.jsonl");
        let (h, o) = (holdout_id(), other_id());
        let pool = vec![row(&h), row(&o)];
        write_to_label(&to_label, &[(row(&h), Source::Holdout), (row(&o), Source::Diagnostic)]).unwrap();
        let text = std::fs::read_to_string(&to_label).unwrap();
        assert!(!text.contains("called_toxic") && !text.contains("model"), "no votes shown: {text}");
        // Answer both — and "promote" the diagnostic one to holdout by hand.
        let edited = text.replace(r#""label":"""#, r#""label":"toxic""#).replace(r#""source":"diagnostic""#, r#""source":"holdout""#);
        std::fs::write(&to_label, edited).unwrap();
        assert_eq!(merge_labels(&to_label, &labels, &pool).unwrap(), 2);
        let got = load_labels(&labels).unwrap();
        assert_eq!(got[&h].source, Source::Holdout);
        assert_eq!(got[&o].source, Source::Diagnostic, "the hand edit is ignored");
        assert_eq!(merge_labels(&to_label, &labels, &pool).unwrap(), 0, "already merged ids are skipped");

        std::fs::write(&to_label, r#"{"id":"nope","content":"x","source":"holdout","label":"toxic","note":""}"#).unwrap();
        assert!(merge_labels(&to_label, &labels, &pool).is_err(), "unknown id");
        std::fs::write(&to_label, format!(r#"{{"id":"{h}x","content":"post {h}x","source":"holdout","label":"maybe","note":""}}"#)).unwrap();
        assert!(merge_labels(&to_label, &labels, &[row(&format!("{h}x"))]).is_err(), "bad label");
        let pool2 = vec![row(&format!("{o}y"))];
        std::fs::write(&to_label, format!(r#"{{"id":"{o}y","content":"different text","source":"diagnostic","label":"clean","note":""}}"#)).unwrap();
        assert!(merge_labels(&to_label, &labels, &pool2).is_err(), "content differs from the pool");
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
//!   the models say. Only these labels feed the headline rates.
//! - **Diagnostic** — disagreements, plus a stable ~10 % of agreed posts.
//!   Useful for understanding models; never mixed into the headline numbers.
//!
//! The stream a label belongs to is DERIVED from the post id at merge time,
//! never read from the (human-edited) file (Codex review, round 2).

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

pub fn source_for(id: &str) -> Source {
    if in_holdout(id) {
        Source::Holdout
    } else {
        Source::Diagnostic
    }
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

/// Blind on purpose: no model votes, so they cannot anchor the answer. The
/// `source` is shown for the labeller's information only.
pub fn write_to_label(path: &Path, rows: &[(FixtureRow, Source)]) -> Result<()> {
    let mut f = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    for (r, source) in rows {
        let line = ToLabel { id: &r.id, content: &r.content, source: *source, label: "", note: "" };
        writeln!(f, "{}", serde_json::to_string(&line)?)?;
    }
    Ok(())
}

pub fn merge_labels(to_label: &Path, labels_path: &Path, pool: &[FixtureRow]) -> Result<usize> {
    let existing = load_labels(labels_path)?;
    let pool_text: HashMap<&str, &str> = pool.iter().map(|r| (r.id.as_str(), r.content.as_str())).collect();
    let text = std::fs::read_to_string(to_label).with_context(|| format!("reading {}", to_label.display()))?;
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(labels_path)
        .with_context(|| format!("opening {}", labels_path.display()))?;
    let mut merged = 0;
    for (i, l) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let at = || format!("{} line {}", to_label.display(), i + 1);
        let row: serde_json::Value = serde_json::from_str(l).with_context(at)?;
        let id = row["id"].as_str().with_context(|| format!("{}: row without id", at()))?;
        let answer = row["label"].as_str().unwrap_or("").trim();
        if answer.is_empty() || existing.contains_key(id) {
            continue;
        }
        let Some(pool_content) = pool_text.get(id) else {
            bail!("{}: id {id:?} is not in the pool", at());
        };
        if let Some(content) = row["content"].as_str() {
            if content != *pool_content {
                bail!("{}: content for {id:?} differs from the pool — re-run `label`", at());
            }
        }
        let truth = match answer {
            "toxic" => Truth::Toxic,
            "clean" => Truth::Clean,
            other => bail!("{}: label {other:?} must be toxic or clean", at()),
        };
        let note = row["note"].as_str().filter(|n| !n.is_empty()).map(str::to_string);
        // Derived, not read: a hand-edited "source" cannot promote a
        // disagreement-picked post into the headline rates.
        let label = Label { id: id.to_string(), label: truth, source: source_for(id), note };
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
git commit -m 'feat(404): label picker with independent holdout; merge derives the source and validates the pool'
```

---

### Task 8: The report — and proof it catches bad models

**Files:**
- Create: `src/cli/eval_classifiers/report.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod report;`)

**Interfaces:**
- Consumes: `metrics::{model_metrics, paired, Comparison, ModelMetrics, Paired, Rate, MIN_CLASS_N}`, `cache::VerdictRecord`, `cost::{estimate_usd, Price}`, `data::{Answer, Source, Truth}`.
- Produces:
  - `pub struct ModelReport { pub name: String, pub calls: HashMap<String, Option<bool>>, pub holdout: ModelMetrics, pub diagnostic: ModelMetrics, pub usd_per_1k: Option<f64>, pub p50_ms: Option<u32>, pub p95_ms: Option<u32> }`
  - `pub fn labels_of(answers: &HashMap<String, Answer>, source: Source) -> HashMap<String, Truth>`
  - `pub fn build(models: &[(String, HashMap<String, VerdictRecord>)], answers: &HashMap<String, Answer>, prices: &HashMap<String, Price>) -> Vec<ModelReport>` — callers pass **current** verdicts only
  - `pub fn vs_baseline(baseline: &ModelReport, candidate: &ModelReport, answers: &HashMap<String, Answer>) -> Paired`
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
        assert_eq!(vs_baseline(&reports[0], &reports[1], &holdout()).miss, Comparison::Worse, "always-clean misses everything");
        let p = vs_baseline(&reports[0], &reports[2], &holdout());
        assert_eq!((p.false_alarm, p.miss), (Comparison::Worse, Comparison::Worse), "coin-flip loses on both");
        let text = render(&reports, &holdout(), "cope-b");
        assert!(text.contains("always-clean: false alarms") && text.contains("misses worse"), "{text}");
    }

    #[test]
    fn close_models_are_too_close_to_call() {
        let reports = build(&[model("cope-b", |i| Some(i < 39 || i == 79)), model("other", |i| Some(i < 38 || i == 79))], &holdout(), &HashMap::new());
        let p = vs_baseline(&reports[0], &reports[1], &holdout());
        assert_eq!((p.false_alarm, p.miss), (Comparison::TooClose, Comparison::TooClose));
        assert!(render(&reports, &holdout(), "cope-b").contains("too close to call — label more"));
    }

    #[test]
    fn a_model_erroring_on_hard_posts_gets_no_verdict_on_that_class() {
        let dodger = model("dodger", |i| if i < 10 { None } else { Some(i < 40) });
        let reports = build(&[model("cope-b", good), dodger], &holdout(), &HashMap::new());
        let p = vs_baseline(&reports[0], &reports[1], &holdout());
        assert_eq!(p.miss, Comparison::LowCoverage, "30/40 toxic answered");
        assert!(render(&reports, &holdout(), "cope-b").contains("misses no verdict — coverage below 90 %"));
    }

    #[test]
    fn too_few_holdout_labels_say_so() {
        let few: HashMap<String, Answer> = holdout().into_iter().filter(|(id, _)| id[1..].parse::<usize>().unwrap() % 4 == 0).collect();
        let reports = build(&[model("cope-b", good), model("always-clean", |_| Some(false))], &few, &HashMap::new());
        assert_eq!(vs_baseline(&reports[0], &reports[1], &few).miss, Comparison::NotEnoughLabels, "10 toxic < 20");
        assert!(render(&reports, &few, "cope-b").contains("not enough holdout labels"));
    }

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
        assert_eq!(build(&[model("x", |_| Some(true))], &holdout(), &HashMap::new())[0].usd_per_1k, None);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib eval_classifiers::report`
Expected: compile error.

- [ ] **Step 3: Implement** `src/cli/eval_classifiers/report.rs`:

```rust
//! The report. Headline rates come from HOLDOUT labels only; comparisons are
//! decided per class by the exact paired test in metrics.rs; every rate shows
//! its 95 % Wilson interval as description — never a blended score.

use std::collections::HashMap;
use std::fmt::Write;

use super::cache::VerdictRecord;
use super::cost::{estimate_usd, Price};
use super::data::{Answer, Source, Truth};
use super::metrics::{model_metrics, paired, Comparison, ModelMetrics, Paired, Rate, MIN_CLASS_N};

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
    paired(&baseline.name, &baseline.calls, &candidate.name, &candidate.calls, &labels_of(answers, Source::Holdout))
}

fn rate_cell(r: &Rate) -> String {
    match (r.value(), r.wilson95()) {
        (Some(v), Some((lo, hi))) => {
            format!("{:>5.1}% [{:.0}–{:.0}] ({}/{})", v * 100.0, lo * 100.0, hi * 100.0, r.hits, r.n)
        }
        _ => "no labels".to_string(),
    }
}

fn word(c: Comparison) -> String {
    match c {
        Comparison::Better => "better".to_string(),
        Comparison::Worse => "worse".to_string(),
        Comparison::TooClose => "too close to call — label more".to_string(),
        Comparison::NotEnoughLabels => {
            format!("not enough holdout labels (need {MIN_CLASS_N} per class) — label more")
        }
        Comparison::LowCoverage => "no verdict — coverage below 90 %".to_string(),
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
            let _ = writeln!(s, "\nCompared with {baseline} (exact paired test, holdout, α = 0.05):");
            for r in reports.iter().filter(|r| r.name != baseline) {
                let p = vs_baseline(base, r, answers);
                let _ = writeln!(s, "  {}: false alarms {}; misses {}", r.name, word(p.false_alarm), word(p.miss));
            }
        }
        None => {
            let _ = writeln!(s, "\n(baseline {baseline:?} has no current verdicts — run it first)");
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
Expected: 6 passed. If `deliberately_bad_models_are_caught` fails, print `render(...)` and check the discordant counts; do **not** loosen the assertion — fix the fake.

- [ ] **Step 5: Commit**

```bash
git add src/cli/eval_classifiers/report.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): holdout-only report with exact paired verdicts; proven to catch bad models'
```

---

### Task 9: Detoxify as a classifier, and the eval model registry

**Files:**
- Create: `src/toxicity/detoxify_classifier.rs`
- Modify: `src/toxicity/mod.rs` (add `pub mod detoxify_classifier;`)
- Create: `src/cli/eval_classifiers/models.rs`
- Modify: `src/cli/eval_classifiers/mod.rs` (add `pub mod models;`)

**Interfaces:**
- Consumes: `OnnxToxicityScorer::load(&Path)`, `ToxicityScorer::score_text`, `download::resolve_model_dir()`, `RunPodCopeBClient::new(endpoint, key)` + `.with_meter(Arc<ScanCostMeter>)`, `ScanCostMeter::new(ceiling_cents, rate_cents_per_hour)`, `cost_meter::DEFAULT_RATE_CENTS_PER_HOUR`.
- Produces:
  - `pub struct DetoxifyClassifier<S: ToxicityScorer> { scorer: S }` with `pub fn new(scorer: S) -> Self` and a `ToxicityClassifier` impl: `toxic_token = toxicity >= 0.5`
  - `pub const KNOWN_MODELS: &[&str] = &["cope-b", "detoxify"];`
  - `pub fn cap_cents(cap_usd: f64) -> u32` — at least 1 (0 would disable the meter)
  - `pub fn build_eval_model(name: &str, cap_usd: f64) -> Result<Arc<dyn ToxicityClassifier>>` — CoPE-B is built **with the real cost meter** set to the cap

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
    use super::*;

    #[test]
    fn unknown_models_are_named_in_the_error() {
        let err = match build_eval_model("nope", 1.0) {
            Ok(_) => panic!("unknown model accepted"),
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains("cope-b") && err.contains("detoxify"), "{err}");
    }

    /// A zero ceiling DISABLES the meter (cost_meter.rs), so the cap must
    /// never round down to it.
    #[test]
    fn the_cap_never_becomes_a_disabled_meter() {
        assert_eq!(cap_cents(1.0), 100);
        assert_eq!(cap_cents(0.004), 1);
        assert_eq!(cap_cents(0.0), 1);
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
//! from production `build_from_env`. Part B adds jev, clef-flash, clef and
//! strands-decider here.

use anyhow::{bail, Context, Result};
use std::sync::Arc;

use crate::toxicity::classifier::ToxicityClassifier;
use crate::toxicity::cost_meter::{ScanCostMeter, DEFAULT_RATE_CENTS_PER_HOUR};
use crate::toxicity::detoxify_classifier::DetoxifyClassifier;
use crate::toxicity::onnx::OnnxToxicityScorer;
use crate::toxicity::runpod_cope_b::RunPodCopeBClient;

pub const KNOWN_MODELS: &[&str] = &["cope-b", "detoxify"];

/// The run's cap as the meter's ceiling. Never 0: a zero ceiling disables
/// the backstop entirely.
pub fn cap_cents(cap_usd: f64) -> u32 {
    ((cap_usd * 100.0).ceil() as u32).max(1)
}

pub fn build_eval_model(name: &str, cap_usd: f64) -> Result<Arc<dyn ToxicityClassifier>> {
    match name {
        // Not via `build_backend_named`, which deliberately builds the client
        // with a DISABLED meter. Here the production cost meter — real GPU
        // worker time × rate — is set to the run's cap, so CoPE-B's spend is
        // bounded by what RunPod actually ran, not by an estimate (Codex
        // review, round 2).
        "cope-b" => {
            let endpoint = std::env::var("RUNPOD_ENDPOINT_URL")
                .context("RUNPOD_ENDPOINT_URL must be set for cope-b")?;
            let api_key = std::env::var("RUNPOD_API_KEY").context("RUNPOD_API_KEY must be set for cope-b")?;
            let meter = Arc::new(ScanCostMeter::new(cap_cents(cap_usd), DEFAULT_RATE_CENTS_PER_HOUR));
            Ok(Arc::new(RunPodCopeBClient::new(endpoint, api_key)?.with_meter(meter)))
        }
        "detoxify" => {
            let dir = crate::toxicity::download::resolve_model_dir();
            Ok(Arc::new(DetoxifyClassifier::new(OnnxToxicityScorer::load(&dir)?)))
        }
        other => bail!("unknown eval model {other:?} (known: {})", KNOWN_MODELS.join(", ")),
    }
}
```

Add `pub mod detoxify_classifier;` to `src/toxicity/mod.rs` and `pub mod models;` to `src/cli/eval_classifiers/mod.rs`. Confirm `OnnxToxicityScorer::load` expects the **base** model dir (`src/toxicity/download.rs:128`); if it expects a subdirectory, pass that. Confirm `RunPodCopeBClient`, `with_meter` and `ScanCostMeter` are `pub` at those paths (`src/toxicity/runpod_cope_b.rs:89,330`, `src/toxicity/cost_meter.rs:129`).

- [ ] **Step 4: Run to verify they pass** — one filter per command:

Run: `cargo test --lib detoxify_classifier`
Expected: 1 passed.
Run: `cargo test --lib eval_classifiers::models`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add src/toxicity/detoxify_classifier.rs src/toxicity/mod.rs src/cli/eval_classifiers/models.rs src/cli/eval_classifiers/mod.rs
git commit -m 'feat(404): Detoxify eval classifier; CoPE-B built with the real cost meter at the run cap'
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
- Produces: `charcoal eval-classifiers run --models cope-b,detoxify [--cap-usd 1.0] [--timeout-secs 600]`, `charcoal eval-classifiers label [--merge]`, `charcoal eval-classifiers report [--baseline cope-b]`; all take `--dir` (default `evals/classifiers`). Label and report consume **only** `cache::current(...)` verdicts.

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
    let current = |m: &str| {
        cache::current(cache::load(&verdicts, m).unwrap(), &pool, cache::load_identity(&verdicts, m).unwrap().as_ref())
    };
    let per_model = vec![current("good"), current("lazy")];
    let labels_path = dir.path().join("labels.jsonl");
    let picked = labelling::pick_for_labelling(&pool, &per_model, &data::load_labels(&labels_path).unwrap());
    assert_eq!(picked.len(), 2, "the holdout post, and the disagreement");

    let to_label = dir.path().join("to-label.jsonl");
    labelling::write_to_label(&to_label, &picked).unwrap();
    let answered = std::fs::read_to_string(&to_label).unwrap().replace("\"label\":\"\"", "\"label\":\"toxic\"");
    std::fs::write(&to_label, answered).unwrap();
    assert_eq!(labelling::merge_labels(&to_label, &labels_path, &pool).unwrap(), 2);

    let answers = data::load_labels(&labels_path).unwrap();
    let models = vec![("good".to_string(), per_model[0].clone()), ("lazy".to_string(), per_model[1].clone())];
    let text = report::render(&report::build(&models, &answers, &HashMap::new()), &answers, "good");
    assert!(text.contains("1 holdout (scored), 1 diagnostic"), "{text}");
    assert!(text.contains("lazy") && text.contains("(1/1)"), "{text}");
    assert!(text.contains("not enough holdout labels"), "one label is far below the 20-per-class gate: {text}");
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
    /// Run models over the pool, reusing verdicts that are still current.
    Run {
        /// Comma-separated model names.
        #[arg(long, value_delimiter = ',', default_value = "cope-b,detoxify")]
        models: Vec<String>,
        /// Stop before spending more than this many US dollars this run.
        #[arg(long, default_value_t = 1.0)]
        cap_usd: f64,
        /// Last-resort per-batch timeout; must exceed the backends' own.
        #[arg(long, default_value_t = 600)]
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
            // The ONE gate for label and report: current verdicts only.
            let current = |m: &str| -> anyhow::Result<_> {
                Ok(cache::current(cache::load(&verdicts, m)?, &pool, cache::load_identity(&verdicts, m)?.as_ref()))
            };
            match action {
                EvalAction::Run { models: names, cap_usd, timeout_secs } => {
                    let prices = cost::load_pricing(&dir.join("pricing.json"))?;
                    let mut budget = cost::Budget::new(cap_usd);
                    let timeout = std::time::Duration::from_secs(timeout_secs);
                    for name in &names {
                        // Refuse an unpriced model before building (or paying for) anything.
                        let price = cost::price_for(&prices, name)?;
                        let classifier = match models::build_eval_model(name, cap_usd - budget.spent()) {
                            Ok(c) => c,
                            Err(e) => {
                                // A missing key skips that model, not the run.
                                eprintln!("skipping {name}: {e:#}");
                                continue;
                            }
                        };
                        match runner::run_model(name, classifier.as_ref(), &pool, &verdicts, price, &mut budget, timeout).await {
                            Ok(s) => {
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
                            // An identity-probe failure skips that model, not the run.
                            Err(e) => eprintln!("skipping {name}: {e:#}"),
                        }
                    }
                    println!("estimated spend this run: ${:.4} (cap ${cap_usd:.2})", budget.spent());
                }
                EvalAction::Label { merge } => {
                    let to_label = dir.join("to-label.jsonl");
                    if merge {
                        let n = labelling::merge_labels(&to_label, &labels_path, &pool)?;
                        println!("merged {n} labels into {}", labels_path.display());
                    } else {
                        let per_model: Vec<_> = models::KNOWN_MODELS
                            .iter()
                            .map(|m| current(*m))
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
                    for m in models::KNOWN_MODELS {
                        let recs = current(*m)?;
                        if !recs.is_empty() {
                            loaded.push((m.to_string(), recs));
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
# 1. Judge every post with each model (cached; $1 cap; CoPE-B metered for real)
cargo run --features web -- eval-classifiers run --models cope-b,detoxify
# 2. Write the posts worth labelling to evals/classifiers/to-label.jsonl
cargo run --features web -- eval-classifiers label
#    ...set each "label" to toxic or clean (optional "note")...
# 3. Fold your answers into labels.jsonl
cargo run --features web -- eval-classifiers label --merge
# 4. Compare
cargo run --features web -- eval-classifiers report
```

## Grow the pool before expecting a verdict

A comparison needs **20 holdout labels in each class** (toxic and clean). The
starting pool of 110 posts yields only 32 holdout posts, so the report will
say "not enough holdout labels" until the pool grows. Add PII-scrubbed posts
with `tools/find-fixtures` (see its README) and append them to `pool.jsonl` —
roughly 600 posts gives a holdout of ~200. If toxic posts are rare in what
you harvest, use `find-fixtures`' list mode on known-hostile accounts so the
toxic class can reach 20.

## How to read the report

- **Holdout** posts (a fixed third, chosen by hash — you cannot move a post
  into or out of it) are the only ones scored. **Diagnostic** posts — where
  models disagree — appear in a separate section; they are not rates.
- **better / worse**: an exact paired test (McNemar) on the posts where the
  two models disagreed, at the 5 % level. The Wilson intervals are context.
- **too close to call — label more**: the paired test is not significant.
- **not enough holdout labels**: fewer than 20 paired labels in that class.
- **no verdict — coverage below 90 %**: a model answered too few of that
  class to compare fairly.
- Every model must be in `pricing.json`. CoPE-B's spend is capped by the
  RunPod client's real cost meter at the run's cap.
- CoPE-B needs `RUNPOD_ENDPOINT_URL` and `RUNPOD_API_KEY`; Detoxify needs the
  local models (`charcoal download-model`). A model missing its keys, or
  failing its identity check, is skipped.
- Verdicts live in `verdicts/` and are never committed (its `.gitignore`).
````

- [ ] **Step 5: Add to `CHANGELOG.md`** under `## [Unreleased]` → `### Added`:

```markdown
- #404 — `charcoal eval-classifiers`: compares classifiers (today CoPE-B and
  Detoxify; Jev, Clef and Strands Decider follow in Part B) on the same posts.
  Headline false-alarm and miss rates come from a fixed hash-chosen holdout;
  "better" or "worse" is decided per class by an exact paired test on the
  posts where the models disagreed, and only with at least 20 paired labels
  and 90 % coverage in that class — otherwise the report says why there is
  no verdict yet. Disagreement-picked labels are reported separately, never as
  rates, and a label's stream is derived from the post id, not the file.
  Every model is scored on its own yes/no answer. Backends must pass an
  identity check, and verdicts from an unexpected model or policy are errors;
  only verdicts current for the pool's text and the model's identity are
  reported. Unpriced models are refused; CoPE-B's spend is bounded by the
  RunPod client's real cost meter at the run's $1 default cap.
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

Expected: clippy clean; all tests pass; the report prints `Labels: 0 holdout (scored), 0 diagnostic, 10 seed (not scored)` and the "no current verdicts — run it first" baseline line.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs tests/cli_eval_classifiers.rs evals/classifiers/README.md CHANGELOG.md
git commit -m 'feat(404): charcoal eval-classifiers run|label|report'
```

---

## After Part A

1. **Grow the pool** (README, "Grow the pool before expecting a verdict"): harvest PII-scrubbed posts with `tools/find-fixtures` until the holdout can reach 20 labels per class — on the order of 600 posts.
2. **First live round** (by hand, with Bryan's go-ahead to spend up to $1): `run --models cope-b,detoxify`, then `label`, Bryan labels, `label --merge`, `report`.
3. **Part B plan**, written from `docs/superpowers/specs/404-provider-contracts.md`. Each unblocked provider gets a `ToxicityClassifier` adapter with wiremock tests from the verified request/response shapes, registered in `models::build_eval_model` and priced in `pricing.json` with `overhead_chars` = the instruction's length. Part B must also:
   - send the full text of `gpu/cope-b-runpod/policy.txt` as the instruction, with answers `toxic` / `clean`;
   - bound real spend from **provider-reported usage** (or an enforced provider-side limit), not estimates, and refuse a provider where neither exists;
   - report the serving model version so the identity check is real;
   - expose a probability only where the provider's is normalised over the answer set.
