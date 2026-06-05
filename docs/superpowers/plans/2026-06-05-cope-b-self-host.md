# CoPE-B Self-Host Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Zentropi hosted CoPE-A with self-hosted CoPE-B-A4B on RunPod Serverless A100 80GB + vLLM, behind a Rust `ToxicityClassifier` trait that makes backends swappable at startup. ONNX clean-pass filter stays at Stage 1; CoPE-B replaces only Stage 2.

**Architecture:** Two-stage scoring keeps the same shape — `TwoStageToxicityScorer` composes ONNX (Stage 1) with a `dyn ToxicityClassifier` (Stage 2). New `RunPodCopeBClient` calls vLLM-on-RunPod via `/runsync` HTTP. Existing `ZentropiClient` refactored to implement the trait. Migration is 8 steps from policy authoring → staging gate → prod cutover.

**Tech Stack:** Rust (`reqwest`, `async-trait`, `tokio`), Python 3.12 + vLLM 0.20.2+ for the GPU service, Docker for the container, RunPod Serverless for hosting, Railway for the Rust app (existing).

**Spec:** `docs/superpowers/specs/2026-06-05-cope-b-self-hosted-design.md` (read first — this plan does not re-derive design decisions).

**Tracking issue:** chainlink #185 (Phase 6 epic). Each chunk opens its own subissue.

**Branch:** Work happens on `feat/cope-b-self-host`. Tests are written BEFORE implementation (Bryan's TDD mandate). Commits are atomic and explicit (`git add <files>` — never `-A`/`./*`). Never use heredoc in shell commands; use single-quoted multi-line strings or `--body-file`.

---

## File Structure Map

Files this plan creates or modifies:

**Rust (Charcoal app):**
- Create: `src/toxicity/classifier.rs` — `ToxicityClassifier` trait, `ClassifierVerdict`, `is_toxic` helper, `StubClassifier`
- Create: `src/toxicity/runpod_cope_b.rs` — RunPod HTTP client implementing the trait
- Modify: `src/toxicity/mod.rs` — export new modules
- Modify: `src/toxicity/zentropi.rs` — refactor to implement `ToxicityClassifier` (keep CoPE-A-style calls)
- Modify: `src/toxicity/ensemble.rs` — refactor `TwoStageVerdict` (rename `zentropi_confidence`, add `classifier_model_id` + `classifier_policy_version`), `VerdictSource` variants, `TwoStageToxicityScorer` swap `Option<Arc<ZentropiClient>>` → `Arc<dyn ToxicityClassifier>`
- Modify: `src/scoring/profile.rs` — adapt `TwoStageVerdict` field consumers
- Modify: `src/scoring/audit_log.rs` — NEW (generalized from existing `nli_audit.rs`)
- Modify: `src/scoring/nli_audit.rs` — re-export through `audit_log` for backward compat, then delete after callers move
- Create: `src/cli/classify_compare.rs` — A/B harness CLI command
- Create: `src/cli/classifier_check.rs` — health-check CLI command
- Modify: `src/main.rs` — register new CLI subcommands; refuse to boot when `CHARCOAL_CLASSIFIER` unset or invalid
- Modify: `src/observability/mod.rs` and `src/observability/classifier_metrics.rs` (NEW) — `classifier_*` metrics
- Modify: `Cargo.toml` — add `tower::retry` or `backon` for jittered exponential backoff (decide in Chunk 4)

**Rust tests:**
- Create: `tests/unit_classifier.rs` — trait, prompt assembly, JSON parsing, retry, threshold, `ClassifierVerdict` serde
- Create: `tests/web_classifier.rs` (`--features web`) — end-to-end ensemble flow with `StubClassifier`; boot-fail when classifier unconfigured
- Create: `tests/composition_classifier_v2.rs` — update existing composition flow for `ClassifierToxic`/`ClassifierSafe` variants
- Create: `tests/fixtures/cope_b/known_toxic.jsonl` (Bryan-authored, ≥ 20 entries)
- Create: `tests/fixtures/cope_b/known_clean.jsonl` (Bryan-authored, ≥ 20 entries)
- Create: `tests/fixtures/cope_b/edge_cases.jsonl` (Bryan-authored)
- Modify: `tests/unit_scoring.rs`, `tests/composition.rs` — adapt to `TwoStageVerdict` field renames + variant changes

**GPU service (Python):**
- Create: `gpu/cope-b-runpod/Dockerfile`
- Create: `gpu/cope-b-runpod/handler.py` — RunPod worker entrypoint
- Create: `gpu/cope-b-runpod/prompt.py` — Gemma chat template + POLICY/CONTENT assembly
- Create: `gpu/cope-b-runpod/policy.txt` (Bryan-authored)
- Create: `gpu/cope-b-runpod/runpod.yml` — endpoint config
- Create: `gpu/cope-b-runpod/tests/test_handler.py` — pytest for handler + prompt + response shape
- Create: `gpu/cope-b-runpod/tests/test_prefix_cache.py` — assert second-request latency materially lower
- Create: `gpu/cope-b-runpod/tests/smoke_test.sh` — `vllm serve` + curl with the 10 fixture inputs
- Create: `gpu/cope-b-runpod/README.md` — image build, deploy, redeploy, region notes

**CI / infra:**
- Create: `.github/workflows/build-cope-b-image.yml` — GH Actions build + push to GHCR
- Modify: `Dockerfile` (Charcoal app) — no change required (talks to RunPod over HTTP)
- Modify: `railway.toml` if env-var injection needs adjustment (verify in Chunk 7)

**Docs:**
- This plan, the spec, and the README in `gpu/cope-b-runpod/`. No other doc files until Bryan asks.

---

## Chunk 1: Project setup, chainlink decomposition, and audit-log generalization preflight

The spec calls audit-log generalization "a separate small PR that lands before this work." We treat it as Chunk 1 on this branch — same logical purpose (lands first). If Bryan prefers it on its own PR, the commits in this chunk can be cherry-picked to a separate branch.

**Rotation semantics change (operator-visible).** Existing `src/scoring/nli_audit.rs`
writes to a single `nli-audit.jsonl` and rotates to a dated archive only when the
file's first entry is >30 days old. The new `audit_log` module writes to a
dated filename every day (`nli-2026-06-05.jsonl`, `classifier-2026-06-05.jsonl`).
This matches the spec's "Rotated daily" intent and aligns the two event types
on the same shape. Task 1.5 includes a one-time migration step that renames any
existing `nli-audit.jsonl` to its dated form so it isn't orphaned.

**Pre-commit hooks.** Bryan's pre-commit hook already runs `cargo fmt + clippy + tests`
on every commit. The "verify clippy is clean" steps in this chunk are belt-and-
suspenders; if the hook is installed (`scripts/install-hooks.sh`), it will catch
the same issues before letting the commit land.

### Task 1.0: Verify branch state

**Files:** None.

- [ ] **Step 1: Confirm current branch**

Run: `git status -sb | head -1`
Expected: `## feat/cope-b-self-host` (or similar). If you see `## staging` or `## main`, STOP — the branch was supposed to be created during brainstorming. Run `git checkout feat/cope-b-self-host` (do not create a new one from current HEAD without checking with the user first).

- [ ] **Step 2: Confirm spec exists on this branch**

Run: `git log --oneline -- docs/superpowers/specs/2026-06-05-cope-b-self-hosted-design.md | head -1`
Expected: at least one commit referencing the spec. If empty, STOP and surface to the user.

### Task 1.1: Open chainlink subissues for Phase 6 epic (#185)

**Files:** None (chainlink CLI only)

Note: chainlink commands below assume the version shipped with this repo's `.chainlink/` config. If `chainlink issue link` is rejected, fall back to `chainlink issue update <child> --parent <parent>` or check `chainlink issue --help` for the current verb.

- [ ] **Step 1: Verify current chainlink focus**

Run: `chainlink session status`
Expected: `Working on: #185` (the Phase 6 epic). If not, run `chainlink session work 185`.

- [ ] **Step 2: Create subissue for Chunk 1**

Run: `chainlink issue quick "Phase 6.0 — audit_log generalization preflight" -p medium -l refactor`
Expected: output like `Created issue #186`. Note the ID.

- [ ] **Step 3: Link subissue to epic**

Run: `chainlink issue link 186 --parent 185`
(If your chainlink version uses a different link command, see `chainlink issue --help`.)

- [ ] **Step 4: Create remaining Phase 6 subissues**

For each, run `chainlink issue quick "..." -p <priority> -l feature` then `chainlink issue link <id> --parent 185`:
- `"Phase 6.1 — policy text + labeled fixtures (Bryan-authored)"` priority `high` label `feature`
- `"Phase 6.2 — RunPod GPU service (Dockerfile + handler + smoke)"` priority `high` label `feature`
- `"Phase 6.3 — Rust trait + RunPodCopeBClient + ZentropiClient refactor"` priority `high` label `feature`
- `"Phase 6.4 — A/B harness + accuracy gate (Step 4.5)"` priority `high` label `feature`
- `"Phase 6.5 — Confidence threshold calibration"` priority `high` label `feature`
- `"Phase 6.6 — Zentropi-hosted CoPE-B (or CoPE-A fallback)"` priority `medium` label `feature`
- `"Phase 6.7 — Staging gate (grimalkina re-scan)"` priority `high` label `feature`
- `"Phase 6.8 — Prod cutover + monitoring"` priority `high` label `feature`

Expected: chainlink reports each created and linked.

- [ ] **Step 5: Switch session focus to Chunk 1's subissue**

Run: `chainlink session work 186`
Expected: `Now working on: #186`.

### Task 1.2: Read existing NLI audit module before changing anything

**Files:**
- Read: `src/scoring/nli_audit.rs`
- Read: any caller (`grep -rln "nli_audit" src/ tests/`)

- [ ] **Step 1: Read the NLI audit module**

Run: `cat src/scoring/nli_audit.rs`
Expected (~108 lines): public function `log_nli_audit(entry: &NliAuditEntry, data_dir: Option<&Path>)`; `NliAuditEntry` with fields `timestamp`, `target_did`, `target_handle`, `pair_type`, `original_text`, `response_text`, `hypothesis_scores: HypothesisScores`, `hostility_score: f64`, optional `similarity: Option<f64>`; helper `should_rotate(first_line) -> bool` (public for testing); rotation via `rotate_audit_file` to a dated archive when first entry >30 days. Note: this uses `chrono` (RFC 3339 timestamps), not the `time` crate.

- [ ] **Step 2: Find call sites in source**

Run: `grep -rn "nli_audit\|log_nli_audit\|NliAuditEntry" src/`
Expected: at minimum `src/scoring/profile.rs` and `src/pipeline/amplification.rs`. Note every file/line so the refactor in 1.5 covers them.

- [ ] **Step 3: Find call sites in tests**

Run: `grep -rn "nli_audit\|log_nli_audit\|NliAuditEntry\|should_rotate" tests/`
Expected: `tests/unit_nli.rs` (not `tests/unit_nli_audit.rs` — there is no separate file). Note specifically `nli_audit_entry_serializes_to_json` and `nli_audit_entry_with_similarity` and any `should_rotate` tests. Task 1.5 must update these in lockstep.

### Task 1.3: Write the failing test for the generalized `audit_log` module

**Files:**
- Create: `tests/unit_audit_log.rs`

The test uses an `enabled: bool` constructor arg rather than reading env vars
directly, so tests don't depend on process-global env state (which makes
parallel test execution flaky). A separate `AuditWriter::from_env(...)`
factory wraps the env-var read for production callers; we test it explicitly
in the third test below.

- [ ] **Step 1: Create the test file**

Path: `tests/unit_audit_log.rs`

```rust
//! Unit tests for the generalized audit log writer.
//! Validates: event-type parameterization, JSONL line shape, daily rotation,
//! and the env-var gate that controls whether events are written at all.

use charcoal::scoring::audit_log::{
    format_log_path, AuditEvent, AuditWriter, ClassifierFields, EventKind, NliFields,
};
use charcoal::scoring::nli::HypothesisScores;
use chrono::TimeZone;
use serde_json::Value;
use std::fs;
use tempfile::tempdir;

fn sample_classifier_event() -> AuditEvent {
    AuditEvent::classifier(ClassifierFields {
        backend: "runpod".into(),
        model_id: "cope-b-a4b".into(),
        policy_version: "policy-v3".into(),
        prompt_hash: "hash-abc".into(),
        toxic: true,
        confidence: 0.93,
        latency_ms: 120,
    })
}

fn sample_nli_event() -> AuditEvent {
    AuditEvent::nli(NliFields {
        target_did: "did:plc:abc".into(),
        target_handle: "alice.bsky.social".into(),
        pair_type: "direct".into(),
        original_text: "some parent post".into(),
        response_text: "some reply".into(),
        hypothesis_scores: HypothesisScores {
            attack: 0.10,
            contempt: 0.05,
            misrepresent: 0.30,
            good_faith_disagree: 0.20,
            support: 0.50,
        },
        hostility_score: 0.42,
        similarity: Some(0.61),
    })
}

#[test]
fn audit_writer_writes_jsonl_one_event_per_line_when_enabled() {
    let dir = tempdir().unwrap();
    let writer = AuditWriter::new(dir.path(), EventKind::Classifier, /*enabled=*/ true).unwrap();

    writer.record(sample_classifier_event()).unwrap();
    writer.record(sample_classifier_event()).unwrap();

    let path = writer.current_path();
    let body = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 2, "one event per line");

    let first: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["kind"], "classifier");
    assert_eq!(first["backend"], "runpod");
    assert_eq!(first["model_id"], "cope-b-a4b");
    assert_eq!(first["policy_version"], "policy-v3");
    assert_eq!(first["toxic"], true);
    assert_eq!(first["confidence"], 0.93);
    assert_eq!(first["latency_ms"], 120);
    // Sanity: timestamp is RFC 3339-ish (parseable by chrono)
    let ts = first["timestamp"].as_str().unwrap();
    chrono::DateTime::parse_from_rfc3339(ts).expect("RFC 3339 timestamp");
}

#[test]
fn audit_writer_drops_events_when_disabled() {
    let dir = tempdir().unwrap();
    let writer = AuditWriter::new(dir.path(), EventKind::Classifier, /*enabled=*/ false).unwrap();
    writer.record(sample_classifier_event()).unwrap();
    // record() short-circuits before opening the file; the file must not exist.
    assert!(!writer.current_path().exists(),
        "disabled writer must not create the JSONL file");
}

#[test]
fn audit_writer_supports_nli_events_with_full_schema() {
    let dir = tempdir().unwrap();
    let writer = AuditWriter::new(dir.path(), EventKind::Nli, true).unwrap();

    writer.record(sample_nli_event()).unwrap();

    let body = fs::read_to_string(writer.current_path()).unwrap();
    let event: Value = serde_json::from_str(body.lines().next().unwrap()).unwrap();
    assert_eq!(event["kind"], "nli");
    assert_eq!(event["target_handle"], "alice.bsky.social");
    assert_eq!(event["pair_type"], "direct");
    assert_eq!(event["original_text"], "some parent post");
    assert_eq!(event["response_text"], "some reply");
    assert_eq!(event["hostility_score"], 0.42);
    assert_eq!(event["similarity"], 0.61);
    assert_eq!(event["hypothesis_scores"]["attack"], 0.10);
    assert_eq!(event["hypothesis_scores"]["support"], 0.50);
}

#[test]
fn audit_writer_rotates_daily_filename() {
    let dir = tempdir().unwrap();
    let p1 = format_log_path(
        dir.path(),
        EventKind::Classifier,
        chrono::Utc.with_ymd_and_hms(2026, 6, 5, 12, 0, 0).unwrap(),
    );
    let p2 = format_log_path(
        dir.path(),
        EventKind::Classifier,
        chrono::Utc.with_ymd_and_hms(2026, 6, 6, 3, 0, 0).unwrap(),
    );
    assert_ne!(p1, p2);
    assert!(p1.to_string_lossy().contains("classifier-2026-06-05"));
    assert!(p2.to_string_lossy().contains("classifier-2026-06-06"));
}

// NOTE: `AuditWriter::from_env` is a one-line wrapper around `std::env::var(...)`
// + the explicit `new` constructor. We do not exercise it in a unit test
// because `std::env::set_var` is process-global and cargo runs tests in
// parallel by default — a unit test there would race against any other test
// that reads `CHARCOAL_AUDIT_CLASSIFIER`. The integration test in
// `tests/web_classifier.rs` (Chunk 4) covers the env-gated path end-to-end
// via a child process with controlled env.
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test unit_audit_log -- --nocapture`
Expected: compile error — `audit_log` module does not exist (or `ClassifierFields`/`NliFields`/`AuditWriter` not found).

### Task 1.4: Implement the generalized `audit_log` module

**Files:**
- Create: `src/scoring/audit_log.rs`
- Modify: `src/scoring/mod.rs` (add `pub mod audit_log;`)
- Modify: `Cargo.toml` only if `tempfile` is missing from `[dev-dependencies]` (verify first)

We deliberately use `chrono` (already a direct dep at `Cargo.toml:60`) and DO NOT add the `time` crate — `nli_audit.rs` and other modules use `chrono`; adding `time` for the same job would bloat the dependency tree.

- [ ] **Step 1: Verify dev deps**

Run: `grep -n -E '^(tempfile|serde_json)' Cargo.toml`
Expected: `serde_json` present. If `tempfile` is missing from `[dev-dependencies]`, add it.

- [ ] **Step 2: Add `tempfile` if needed**

Edit `Cargo.toml`. Under `[dev-dependencies]`, if missing:

```toml
tempfile = "3"
```

- [ ] **Step 3: Create `src/scoring/audit_log.rs`**

```rust
//! Generalized audit log writer for classifier and NLI events.
//!
//! Each event is one JSONL line. Files rotate daily by UTC date — the filename
//! includes `YYYY-MM-DD`. The on-disk schema is `{kind, ...event-specific-fields}`.
//!
//! The writer takes an explicit `enabled` flag at construction so tests can
//! exercise both paths without env-var fiddling. Production callers use
//! [`AuditWriter::from_env`] which reads the per-kind env var.
//!
//! NOTE: this replaces the older `nli_audit` module which used a single
//! `nli-audit.jsonl` file rotated to dated archives when its first entry
//! exceeded 30 days. The new layout writes a fresh dated file every day.
//! Migration of any orphaned `nli-audit.jsonl` is handled in the migration
//! step (Task 1.5).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::scoring::nli::HypothesisScores;

#[derive(Debug, Clone, Copy)]
pub enum EventKind {
    Classifier,
    Nli,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Classifier => "classifier",
            EventKind::Nli => "nli",
        }
    }

    /// Env var that toggles whether events of this kind are written.
    pub fn env_var(self) -> &'static str {
        match self {
            EventKind::Classifier => "CHARCOAL_AUDIT_CLASSIFIER",
            EventKind::Nli => "CHARCOAL_AUDIT_NLI",
        }
    }
}

/// Pure path-formatting helper. Public so tests can exercise rotation
/// without running the clock forward.
pub fn format_log_path(dir: &Path, kind: EventKind, when: DateTime<Utc>) -> PathBuf {
    let date = when.format("%Y-%m-%d").to_string();
    dir.join(format!("{}-{}.jsonl", kind.as_str(), date))
}

/// Classifier-side event payload. Constructed via [`AuditEvent::classifier`].
#[derive(Debug, Clone)]
pub struct ClassifierFields {
    pub backend: String,
    pub model_id: String,
    pub policy_version: String,
    pub prompt_hash: String,
    pub toxic: bool,
    pub confidence: f32,
    pub latency_ms: u32,
}

/// NLI-side event payload. Mirrors the legacy `NliAuditEntry` field set.
#[derive(Debug, Clone)]
pub struct NliFields {
    pub target_did: String,
    pub target_handle: String,
    pub pair_type: String,
    pub original_text: String,
    pub response_text: String,
    pub hypothesis_scores: HypothesisScores,
    pub hostility_score: f64,
    pub similarity: Option<f64>,
}

/// Audit events are write-only (the writer never reads them back), so we only
/// derive `Serialize`. `HypothesisScores` in `src/scoring/nli.rs` similarly
/// derives only `Serialize`; adding `Deserialize` here would force adding it
/// there too, which we don't need.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AuditEvent {
    Classifier {
        timestamp: String,
        backend: String,
        model_id: String,
        policy_version: String,
        prompt_hash: String,
        toxic: bool,
        confidence: f32,
        latency_ms: u32,
    },
    Nli {
        timestamp: String,
        target_did: String,
        target_handle: String,
        pair_type: String,
        original_text: String,
        response_text: String,
        hypothesis_scores: HypothesisScores,
        hostility_score: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        similarity: Option<f64>,
    },
}

impl AuditEvent {
    pub fn classifier(fields: ClassifierFields) -> Self {
        AuditEvent::Classifier {
            timestamp: now_rfc3339(),
            backend: fields.backend,
            model_id: fields.model_id,
            policy_version: fields.policy_version,
            prompt_hash: fields.prompt_hash,
            toxic: fields.toxic,
            confidence: fields.confidence,
            latency_ms: fields.latency_ms,
        }
    }

    pub fn nli(fields: NliFields) -> Self {
        AuditEvent::Nli {
            timestamp: now_rfc3339(),
            target_did: fields.target_did,
            target_handle: fields.target_handle,
            pair_type: fields.pair_type,
            original_text: fields.original_text,
            response_text: fields.response_text,
            hypothesis_scores: fields.hypothesis_scores,
            hostility_score: fields.hostility_score,
            similarity: fields.similarity,
        }
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

pub struct AuditWriter {
    dir: PathBuf,
    kind: EventKind,
    enabled: bool,
}

impl AuditWriter {
    /// Build a writer with the gate set explicitly. Use in tests.
    pub fn new(dir: &Path, kind: EventKind, enabled: bool) -> Result<Self> {
        std::fs::create_dir_all(dir).context("create audit log dir")?;
        Ok(Self {
            dir: dir.to_path_buf(),
            kind,
            enabled,
        })
    }

    /// Build a writer reading the gate from the kind's env var.
    pub fn from_env(dir: &Path, kind: EventKind) -> Result<Self> {
        let enabled = std::env::var(kind.env_var()).ok().as_deref() == Some("1");
        Self::new(dir, kind, enabled)
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn current_path(&self) -> PathBuf {
        format_log_path(&self.dir, self.kind, Utc::now())
    }

    pub fn record(&self, event: AuditEvent) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let line = serde_json::to_string(&event).context("serialize audit event")?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.current_path())
            .context("open audit log")?;
        writeln!(file, "{}", line).context("write audit line")?;
        Ok(())
    }
}
```

- [ ] **Step 4: Register the module**

Edit `src/scoring/mod.rs`. Find the existing `pub mod nli_audit;` line and add immediately above it:

```rust
pub mod audit_log;
```

(Do not remove `nli_audit` yet; that happens in Task 1.5.)

- [ ] **Step 5: Verify `audit_log` items are reachable from the test crate**

Confirm `src/lib.rs` re-exports the `scoring` module publicly (it should already — verify with `grep '^pub mod scoring' src/lib.rs`). No change needed if already public. `HypothesisScores` already imports under `crate::scoring::nli` per the existing `nli_audit.rs`.

- [ ] **Step 6: Run the test**

Run: `cargo test --test unit_audit_log -- --nocapture`
Expected: all five tests pass. No env-var prefix needed — the writer uses explicit `enabled` arg.

- [ ] **Step 7: Run the full unit suite to confirm no regressions**

Run: `cargo test --lib`
Expected: existing tests still pass; `audit_log` module compiles cleanly.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml src/scoring/audit_log.rs src/scoring/mod.rs tests/unit_audit_log.rs
git commit -m 'feat(audit): generalized AuditWriter parameterized by EventKind

New src/scoring/audit_log.rs supports both classifier and NLI events
through a single rotator + writer. Per-kind env-var gate
(CHARCOAL_AUDIT_CLASSIFIER, CHARCOAL_AUDIT_NLI) read by AuditWriter::from_env;
tests use the explicit (enabled: bool) constructor. JSONL line shape
tagged via serde #[serde(tag = kind)]. Daily UTC rotation by filename.

This commit only adds the new module; nli_audit migration follows.
Operator note: layout changes from single nli-audit.jsonl with 30-day
archive rollover to dated nli-YYYY-MM-DD.jsonl files per day, matching
the spec design.

Chainlink #186 (Phase 6.0 preflight, parent #185).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>'
```

### Task 1.5: Migrate NLI call sites from `nli_audit` to `audit_log`

**Files:**
- Modify: every file emitted by `grep -rn "nli_audit\|log_nli_audit\|NliAuditEntry" src/`
  (canonically `src/scoring/profile.rs`, `src/pipeline/amplification.rs`)
- Modify: `tests/unit_nli.rs` — replace `nli_audit::should_rotate` / `NliAuditEntry` tests with the new `audit_log` API (or move equivalent coverage into `tests/unit_audit_log.rs`)
- Modify: `src/scoring/mod.rs` — remove `pub mod nli_audit;`
- Delete: `src/scoring/nli_audit.rs`
- Add: one-time migration of an orphaned `nli-audit.jsonl` at runtime

- [ ] **Step 1: Re-list call sites in source and tests**

Run:
```
grep -rn "nli_audit\|log_nli_audit\|NliAuditEntry\|should_rotate" src/ tests/
```
Expected: definitive list, including `tests/unit_nli.rs` lines that test `should_rotate` and `NliAuditEntry` serialization. Read every hit before editing.

- [ ] **Step 2: Update each source call site**

The existing call shape is a free function:

```rust
use crate::scoring::nli_audit::{log_nli_audit, NliAuditEntry};
// ...
log_nli_audit(
    &NliAuditEntry {
        timestamp: chrono::Utc::now().to_rfc3339(),
        target_did, target_handle, pair_type,
        original_text, response_text,
        hypothesis_scores, hostility_score, similarity,
    },
    data_dir.as_deref(),
);
```

Replace with:

```rust
use crate::scoring::audit_log::{AuditEvent, AuditWriter, EventKind, NliFields};
// ...
if let Some(dir) = data_dir.as_deref() {
    let writer = AuditWriter::from_env(dir, EventKind::Nli)
        .context("init NLI audit writer")?;
    let event = AuditEvent::nli(NliFields {
        target_did, target_handle, pair_type,
        original_text, response_text,
        hypothesis_scores, hostility_score, similarity,
    });
    if let Err(e) = writer.record(event) {
        tracing::warn!(error = %e, "Failed to write NLI audit JSONL");
    }
}
```

Preserve the existing `tracing::info!` log emission alongside the JSONL write — the new module only does the JSONL side; the tracing side stays in the caller (matches existing `log_nli_audit` behavior).

- [ ] **Step 3: Update `tests/unit_nli.rs`**

This file currently imports `nli_audit::{should_rotate, NliAuditEntry}` and contains tests like `nli_audit_entry_serializes_to_json` and `nli_audit_entry_with_similarity`. Two options:

- **Option A (preferred):** delete those specific test functions from `tests/unit_nli.rs` (they're now redundant — `tests/unit_audit_log.rs` covers the serde shape and the new module has no `should_rotate` since rotation is by filename, not entry age). Remove the `nli_audit` import line.
- **Option B:** rewrite each test in-place to construct `AuditEvent::nli(NliFields { ... })` and assert via `serde_json::to_value`. Keep them in `unit_nli.rs` if they exercise integration points beyond plain serde.

Pick Option A unless you find a non-redundant assertion. Document the choice in the commit message.

- [ ] **Step 4: Add the one-time orphan migration**

Operators with existing deployments will have a `nli-audit.jsonl` file on the
persistent volume. The new layout writes to `nli-2026-06-05.jsonl` etc., so the
old file would be orphaned. Add the helper directly in `src/scoring/audit_log.rs`
(public, beside `format_log_path`) and call it exactly once from
`src/db/mod.rs::Database::open` after schema migrations complete — this is
the canonical "data dir is known and ready" boundary, runs once per process
boot, and matches where other one-time DB migrations live.

In `src/scoring/audit_log.rs`, add:

```rust
/// One-time rename of any pre-generalization NLI audit file so it isn't orphaned
/// after rotation changes from "single file + 30-day archive" to "one file per day".
/// Safe to call on every boot; no-op if the file is absent.
pub fn migrate_legacy_nli_audit(dir: &Path) {
    let legacy = dir.join("nli-audit.jsonl");
    if !legacy.exists() {
        return;
    }
    let target = dir.join(format!(
        "nli-legacy-{}.jsonl",
        Utc::now().format("%Y-%m-%d")
    ));
    match std::fs::rename(&legacy, &target) {
        Ok(()) => tracing::info!(
            from = %legacy.display(),
            to = %target.display(),
            "Migrated legacy NLI audit file"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            "Failed to rename legacy nli-audit.jsonl"
        ),
    }
}
```

In `src/db/mod.rs` (or wherever `Database::open` lives — confirm with
`grep -n 'fn open' src/db/mod.rs src/db/sqlite.rs src/db/postgres.rs`), at the
end of the post-migration block, add:

```rust
if let Some(data_dir) = std::env::var("CHARCOAL_DATA_DIR").ok().map(PathBuf::from) {
    crate::scoring::audit_log::migrate_legacy_nli_audit(&data_dir);
}
```

Use whatever env var actually configures the data directory in Charcoal —
likely `CHARCOAL_DATA_DIR` or similar; verify with `grep -rn 'CHARCOAL_DATA_DIR\|data_dir' src/`. If no env var exists, place the call wherever the data dir is first computed (the canonical place is in `main.rs` immediately after argument parsing).

- [ ] **Step 5: Remove the old module**

Edit `src/scoring/mod.rs`: delete `pub mod nli_audit;`.
Run: `git rm src/scoring/nli_audit.rs`.

- [ ] **Step 6: Verify build**

Run: `cargo build --all-features`
Expected: clean build, no references to `nli_audit` remain.

- [ ] **Step 7: Run all tests**

Run: `cargo test --features web`
Expected: pass. `tests/unit_nli.rs` no longer references `nli_audit`; `tests/unit_audit_log.rs` runs in-process.

- [ ] **Step 8: Run clippy (pre-commit hook will repeat — this is belt-and-suspenders)**

Run: `cargo clippy --features web -- -D warnings`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add src/scoring/audit_log.rs src/scoring/mod.rs src/scoring/profile.rs src/pipeline/amplification.rs src/db/mod.rs tests/unit_nli.rs
git rm src/scoring/nli_audit.rs
git commit -m 'refactor(audit): migrate NLI call sites to generalized AuditWriter

Callers in src/scoring/profile.rs and src/pipeline/amplification.rs now
use AuditEvent::nli + EventKind::Nli via AuditWriter::from_env. Old
src/scoring/nli_audit.rs removed. Tracing side of log_nli_audit moved
to call sites (the new module only handles JSONL).

Operator-visible changes:
- Layout: nli-audit.jsonl (single file + 30-day archive) becomes
  nli-YYYY-MM-DD.jsonl (one file per day).
- migrate_legacy_nli_audit renames any pre-existing nli-audit.jsonl to
  nli-legacy-YYYY-MM-DD.jsonl on the first boot after deploy. Wired in
  Database::open (src/db/mod.rs) immediately after schema migrations.

tests/unit_nli.rs: removed redundant NliAuditEntry serde tests and
should_rotate_* tests (rotation is now filename-based by UTC date,
not entry-age based). Equivalent serde coverage lives in
tests/unit_audit_log.rs.

Chainlink #186 (Phase 6.0 preflight, parent #185).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>'
```

(If `Database::open` lives in `src/db/sqlite.rs` or `src/db/postgres.rs` instead of `src/db/mod.rs`, swap that path in the `git add` line.)

- [ ] **Step 10: Push**

Run: `git push -u origin feat/cope-b-self-host`
Expected: pushes branch (or updates if already pushed in design phase).

- [ ] **Step 11: Close subissue and switch to Chunk 2's**

Run: `chainlink issue close 186` then `chainlink session work <Chunk 2 subissue ID>` (the issue ID created for "Phase 6.1 — policy text + labeled fixtures").

---

## Chunk 2: Policy text + labeled fixtures (Bryan-authored)

This chunk produces three artifacts that require Bryan's judgment about what
counts as toxic in Charcoal's specific community context. The plan's job is to
specify the **contract** (format, fields, quality bars) — not the content. Bryan
fills in the content with optional Claude assistance for tedium (formatting,
parallel construction of variants).

**Subissue:** `Phase 6.1 — policy text + labeled fixtures (Bryan-authored)`. Confirm
focus with `chainlink session status`; if not active, `chainlink session work <id>`.

**Spec sections to re-read first:** Step 1 (policy authoring) and Step 4.5
(accuracy gate fixture requirements).

### Task 2.1: Create the `gpu/cope-b-runpod/` directory scaffold

**Files:**
- Create: `gpu/cope-b-runpod/policy.txt`
- Create: `gpu/cope-b-runpod/README.md` (stub; expanded in Chunk 3)
- Create: `gpu/cope-b-runpod/.gitkeep` if the directory will otherwise be empty pending Chunk 3 files

- [ ] **Step 1: Create the directory**

Run: `mkdir -p gpu/cope-b-runpod`
Expected: directory exists; `ls gpu/` shows `cope-b-runpod`.

- [ ] **Step 2: Stub the README**

Path: `gpu/cope-b-runpod/README.md`

```markdown
# Charcoal CoPE-B-A4B GPU service

vLLM-on-RunPod-Serverless harness for the Stage-2 toxicity classifier.
See `docs/superpowers/specs/2026-06-05-cope-b-self-hosted-design.md` for design.

Files (filled in by Chunk 3):
- `Dockerfile` — image build
- `handler.py` — RunPod worker entrypoint
- `prompt.py` — Gemma chat template + POLICY/CONTENT assembly
- `policy.txt` — toxicity policy (versioned per Bryan; **not** silently
  derivable from CoPE-A's hosted labeler)
- `runpod.yml` — endpoint config
- `tests/` — handler unit tests + smoke script
```

### Task 2.2: Author `gpu/cope-b-runpod/policy.txt`

**Files:**
- Create: `gpu/cope-b-runpod/policy.txt`

**Input:** `refs/labeler_prompt.txt` (81 lines) — the current Zentropi CoPE-A labeler policy snapshot. Re-read it before writing.

**Output format contract:**

CoPE-B expects the policy in the `POLICY` slot of the prompt — see the spec's
"GPU service" section. The slot is freeform text; CoPE-B uses it as instructions
on what classes of CONTENT count as `1` (toxic) and which count as `0` (clean).
Bryan owns the wording. Constraints:

- No `INSTRUCTIONS:` or `ANSWER:` headers (CoPE-B drops them; the chat template's
  role markers replace them)
- Aim for ~50–500 tokens (longer slows inference; shorter loses signal)
- Concrete examples are valuable; abstract definitions are not
- Cover at least: identity-based hostility, dehumanization, dogpiling/incitement,
  bad-faith concern trolling, sarcastic/coded hostility, news-commentary
  ambiguity (cf. chainlink #114)
- DO NOT mention model names, scoring formulas, or downstream tier thresholds —
  the model only needs to know how to classify

- [ ] **Step 1: Read the reference snapshot**

Run: `cat refs/labeler_prompt.txt`
Expected: full text. Note structure, edge cases handled, language patterns.

- [ ] **Step 2: Draft `policy.txt`**

Path: `gpu/cope-b-runpod/policy.txt`. This is Bryan-authored content; the plan does not pre-write it. Bryan should:

1. Open a working draft in a text editor
2. Translate the reference snapshot into CoPE-B's freeform `POLICY` style
3. Strip any CoPE-A-specific scaffolding (INSTRUCTIONS/ANSWER markers)
4. Add edge-case guidance for the categories above

If Claude assistance is wanted: paste the reference snapshot into chat and ask
for a structured draft, then revise. The final file is Bryan's call to make.

- [ ] **Step 3: Token-count check**

Run: `wc -w gpu/cope-b-runpod/policy.txt` (rough proxy; 1 word ≈ 1.3 tokens)
Expected: 40–400 words (≈ 50–500 tokens). Outside this range, consider whether the policy is too sparse or too verbose.

- [ ] **Step 4: Sanity-check (Colab preferred, local fallback)**

The spec mentions Zentropi published a runnable Colab notebook for CoPE-B-A4B at
https://colab.research.google.com/drive/1JD8OIa3yZYfVbeY81ao03lrvg0aS-6SQ.

**Preferred path:** open the Colab, replace its example POLICY with the contents
of `policy.txt`, and run ~10 hand-picked examples (5 clearly toxic, 5 clearly
clean). Confirm classification matches your expectation.

**Fallback if the Colab is unavailable** (URL 404, HF gating, runtime quota):
defer the sanity-check to Chunk 3's local `vllm serve` smoke test (Task 3.x,
runs locally on Bryan's M4 Pro Mac mini via GGUF quants or on whichever
machine has a GPU). Flag the deferral in the commit message:

```
sanity-check deferred to Chunk 3 smoke test (Colab unavailable: <reason>)
```

If neither path works, the policy iterates without quantitative grounding until
the GPU service is live in Chunk 3 — risky but not blocking, since Chunk 5's
accuracy gate is the formal quality bar.

- [ ] **Step 5: Commit**

```bash
git add gpu/cope-b-runpod/policy.txt gpu/cope-b-runpod/README.md
git commit -m 'feat(cope-b): seed policy.txt and gpu/cope-b-runpod/ scaffold

policy.txt is the Stage-2 toxicity policy in CoPE-B POLICY slot format,
adapted from the CoPE-A reference snapshot at refs/labeler_prompt.txt.
Removes INSTRUCTIONS/ANSWER headers (dropped by CoPE-B chat template);
covers identity hostility, dehumanization, dogpiling, concern trolling,
sarcasm, and news-commentary ambiguity (cf. chainlink #114).

Sanity-checked via Zentropi Colab on ~10 hand-picked examples.

Chainlink #<Phase 6.1 issue id>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>'
```

### Task 2.3: Author labeled fixtures

**Files:**
- Create: `tests/fixtures/cope_b/known_toxic.jsonl`
- Create: `tests/fixtures/cope_b/known_clean.jsonl`
- Create: `tests/fixtures/cope_b/edge_cases.jsonl`

**JSONL schema (per line):**

```json
{
  "id": "kt-001",
  "label": "toxic",
  "category": "identity-attack",
  "content": "[Parent post]: I'm trans and I had a long day.\n\n[Reply]: <toxic reply here>",
  "note": "optional one-line rationale"
}
```

Field rules:
- `id` — stable kebab-case identifier. `kt-` prefix for `known_toxic`, `kc-` for `known_clean`, `ec-` for `edge_cases`.
- `label` — one of: `"toxic"`, `"clean"`, `"uncertain"` (lowercase strings). `uncertain` is only valid in `edge_cases.jsonl` and Chunk 5's accuracy gate skips those rows.
- `category` — kebab-case category tag. **MUST be from the allowed-values set** (see below). Categories outside the set fail the verify step.
- `content` — the post text that gets passed into the CoPE-B `CONTENT` slot. **MUST match the exact envelope format produced by `src/toxicity/mod.rs::format_parent_reply` for reply pairs:**

  ```
  [Parent post]: <parent text>\n\n[Reply]: <reply text>
  ```

  Note: literal colons after `[Parent post]` and `[Reply]`, single space, then text; **double newline** (`\n\n` — a blank line) between the parent and reply blocks. For original posts (no parent), `content` is just the post body text with no envelope. Mismatching this format means Chunk 5's gate measures an off-distribution prompt — the fixtures must look exactly like what Charcoal generates at runtime.
- `note` — optional. One short sentence explaining why this example was chosen. Useful when Bryan re-reads the fixtures in 6 months.

**Allowed-values set for `category`:**

```
identity-attack
dehumanization
dogpile
concern-troll
coded-sarcasm
news-commentary
support
disagreement
meme
slang
counter-speech
reclamation
```

If a fixture needs a category outside this set, add the value to this list (in a separate commit) and document why. Don't proliferate near-duplicates (`identity-attack` vs `identity_attack` vs `identity-attacks` — all are violations of the kebab-case-lowercase rule).

**Quality bars (enforced by Chunk 5's accuracy gate):**
- `known_toxic.jsonl` ≥ 20 entries, ≥ 4 distinct categories **from the allowed set**
- `known_clean.jsonl` ≥ 20 entries, ≥ 4 distinct categories **from the allowed set**
- `edge_cases.jsonl` no minimum count; aim for ~10–20 thoughtful examples

**Sourcing guidance for Bryan (PII checklist):**

Fixtures are committed to the public repo. Before pasting any real-world quote:

- [ ] Strip @handles (no `@user.bsky.social`) — replace with role labels like "<user>" if needed
- [ ] Strip DIDs (no `did:plc:...`)
- [ ] Strip AT-URIs (`at://did:plc:...`)
- [ ] Strip post URLs (`https://bsky.app/profile/.../post/...`)
- [ ] Paraphrase distinctive multi-word phrases so the original post can't be located via Bluesky search (rewriting 50%+ of unique word choices is usually enough)
- [ ] Avoid quoting from accounts that are currently being harassed — using their words even paraphrased can amplify

Sources to draw from (after applying the PII checklist):
- `account_scores` rows tagged toxic/clean on prod
- Past `user_labels` entries the review queue has confirmed
- Bryan's own judgment for the edge-case set — sarcasm, reclaimed slurs in-group, counter-speech, news commentary on violence

- [ ] **Step 1: Create the fixtures directory**

Run: `mkdir -p tests/fixtures/cope_b`

- [ ] **Step 2: Author `known_toxic.jsonl`**

Bryan writes ≥ 20 entries by hand or with Claude scaffolding. Apply the PII checklist above to every example before committing. Each line is a complete JSON object (no pretty-printing).

- [ ] **Step 3: Author `known_clean.jsonl`**

Same shape, label `"clean"`. ≥ 20 entries, ≥ 4 distinct categories from the allowed-values set.

- [ ] **Step 4: Author `edge_cases.jsonl`**

Same shape, mix of labels including `"uncertain"`. Aim for cases where Charcoal's current pipeline has misclassified in the past (cf. chainlink #114 for news-commentary false positives).

- [ ] **Step 5: Validate all three files**

Run all three checks. Each must pass before continuing.

**JSONL validity (every line parses):**
```
for f in tests/fixtures/cope_b/known_toxic.jsonl tests/fixtures/cope_b/known_clean.jsonl tests/fixtures/cope_b/edge_cases.jsonl; do
  python3 -c "import json,sys; [json.loads(l) for l in open('$f')]" || { echo "INVALID: $f"; exit 1; }
done
echo "JSONL OK"
```
Expected: `JSONL OK`.

**Counts (≥20 in toxic + clean):**
```
[ $(wc -l < tests/fixtures/cope_b/known_toxic.jsonl) -ge 20 ] || { echo "known_toxic <20"; exit 1; }
[ $(wc -l < tests/fixtures/cope_b/known_clean.jsonl) -ge 20 ] || { echo "known_clean <20"; exit 1; }
echo "Counts OK"
```
Expected: `Counts OK`.

**Categories (≥4 distinct AND all from the allowed set):**
```
allowed='identity-attack dehumanization dogpile concern-troll coded-sarcasm news-commentary support disagreement meme slang counter-speech reclamation'
for f in tests/fixtures/cope_b/known_toxic.jsonl tests/fixtures/cope_b/known_clean.jsonl; do
  cats=$(jq -r '.category' "$f" | sort -u)
  count=$(echo "$cats" | wc -l)
  [ $count -ge 4 ] || { echo "$f <4 distinct categories"; exit 1; }
  for c in $cats; do
    echo " $allowed " | grep -q " $c " || { echo "$f has out-of-set category: $c"; exit 1; }
  done
done
echo "Categories OK"
```
Expected: `Categories OK`. If any line fails, fix the offending fixture before continuing.

**`uncertain` label only in `edge_cases.jsonl`:**
```
for f in tests/fixtures/cope_b/known_toxic.jsonl tests/fixtures/cope_b/known_clean.jsonl; do
  if jq -e 'select(.label == "uncertain")' "$f" >/dev/null; then
    echo "$f contains uncertain label (only allowed in edge_cases)"; exit 1
  fi
done
echo "Labels OK"
```
Expected: `Labels OK`. A `kt-` or `kc-` entry labeled `uncertain` would silently distort Chunk 5's gate; this check fails fast.

**Envelope format spot-check (parent/reply pairs match `format_parent_reply` exactly):**
```
jq -r 'select(.content | contains("[Parent post]")) | .content' tests/fixtures/cope_b/*.jsonl | head -20
```
Expected output: every visible `content` shows `[Parent post]: ...` then a blank line then `[Reply]: ...`. If any line uses different punctuation or spacing, fix it.

- [ ] **Step 6: Smoke-classify (Colab preferred, fallback to Chunk 3)**

Same Colab as Task 2.2 Step 4 (with the same fallback policy if it's unavailable). Feed each fixture line's `content` through the model with `policy.txt` as the POLICY. Eyeball the verdicts:
- `known_toxic` should classify mostly as `1` (toxic)
- `known_clean` should classify mostly as `0` (clean)
- `edge_cases` — observe and note disagreements; do not fix the policy here unless something is glaringly wrong (formal calibration is Step 5 of the migration, Chunk 5). Rows with `label == "uncertain"` are intentionally unscored; Chunk 5's gate skips them.

If `known_toxic` or `known_clean` accuracy looks <80% by eye, revise `policy.txt` (Task 2.2) before continuing.

- [ ] **Step 7: Commit**

```bash
git add tests/fixtures/cope_b/known_toxic.jsonl tests/fixtures/cope_b/known_clean.jsonl tests/fixtures/cope_b/edge_cases.jsonl
git commit -m 'test(cope-b): seed labeled fixtures for Step 4.5 accuracy gate

JSONL schema: id, label (toxic|clean|uncertain), category, content, note.
known_toxic.jsonl and known_clean.jsonl each have >=20 hand-curated
entries spanning >=4 categories; edge_cases.jsonl captures sarcasm,
counter-speech, news commentary on violent topics (cf. chainlink #114),
and reclaimed slurs.

content uses Charcoals "[Parent post]/[Reply]" envelope so fixtures are
drop-in inputs for ToxicityClassifier::classify.

Sanity-checked via Zentropi Colab against policy.txt; revisit threshold
calibration in Chunk 5 (migration Step 5).

Chainlink #<Phase 6.1 issue id>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>'
```

- [ ] **Step 8: Push**

Run: `git push origin feat/cope-b-self-host`
Expected: branch updated.

- [ ] **Step 9: Close subissue and switch to Chunk 3**

```
chainlink issue close <Phase 6.1 issue id>
chainlink session work <Phase 6.2 issue id>   # GPU service
```

---

## Chunk 3: RunPod GPU service (Dockerfile, vLLM handler, prompt assembly, tests, CI)

**Subissue:** `Phase 6.2 — RunPod GPU service`. Confirm with `chainlink session status`.

**Spec sections to re-read first:** the "GPU service" section and the "Image and endpoint lifecycle" subsection.

**Prerequisite:** Chunk 2 must be complete — `gpu/cope-b-runpod/policy.txt`
must exist before the Dockerfile build will succeed (Task 3.6 hard-fails
without it). Verify with `[ -f gpu/cope-b-runpod/policy.txt ] && echo OK`
at the start of this chunk.

**Hardware caveat for Bryan's M4 Mac mini:** `vllm serve` requires CUDA;
Apple Silicon won't run it. Local smoke testing (Task 3.6 Step 3) needs a
rented Linux+CUDA box or the deployed staging RunPod endpoint. CPU-only
pytest still works for `test_prompt.py` and `test_handler.py` (they mock
vLLM).

This chunk is Python-side only. The Rust side that calls into this service is Chunk 4. All work lives under `gpu/cope-b-runpod/`. Tests are written first (TDD); implementation follows.

### Task 3.1: Project metadata and pytest harness

**Files:**
- Create: `gpu/cope-b-runpod/pyproject.toml`
- Create: `gpu/cope-b-runpod/requirements.txt`
- Create: `gpu/cope-b-runpod/tests/__init__.py`
- Create: `gpu/cope-b-runpod/tests/conftest.py`

- [ ] **Step 1: Create `pyproject.toml`**

Path: `gpu/cope-b-runpod/pyproject.toml`

```toml
[project]
name = "charcoal-cope-b"
version = "0.1.0"
description = "Charcoal CoPE-B-A4B classifier service for RunPod"
requires-python = ">=3.12"

[tool.pytest.ini_options]
testpaths = ["tests"]
python_files = ["test_*.py"]
```

- [ ] **Step 2: Create `requirements.txt`**

Pinned versions match the spec's vLLM ≥ 0.20.2 and the model card's recommendation. RunPod's base image provides `runpod`; vLLM is the heavy import.

```
# Runtime
vllm==0.20.2
transformers>=4.50,<5
runpod>=1.7,<2
# Dev / test (installed locally for pytest runs; not in image)
pytest>=8
pytest-asyncio>=0.24
```

The Dockerfile (Task 3.6) only installs the runtime triplet; dev tooling stays out of the image.

- [ ] **Step 3: Empty test package marker**

Path: `gpu/cope-b-runpod/tests/__init__.py` — empty file.

- [ ] **Step 4: pytest conftest with shared fixtures**

Path: `gpu/cope-b-runpod/tests/conftest.py`

```python
"""Shared fixtures for handler + prompt tests."""

from pathlib import Path
import pytest

ROOT = Path(__file__).parent.parent
POLICY_PATH = ROOT / "policy.txt"


@pytest.fixture
def policy_text() -> str:
    """Loads the live policy.txt so tests reflect what production sees."""
    return POLICY_PATH.read_text()


@pytest.fixture
def sample_content_pair() -> str:
    """An envelope identical to Charcoal's format_parent_reply output."""
    return (
        "[Parent post]: I just got home after a brutal commute.\n\n"
        "[Reply]: Yeah, same — those train delays are killing me."
    )


@pytest.fixture
def sample_content_solo() -> str:
    """An original post (no parent), as Charcoal would pass it."""
    return "Excited to share a piece I've been working on about labor unions."
```

No commit yet — tests come next.

### Task 3.2: Write the failing prompt-assembly test

**Files:**
- Create: `gpu/cope-b-runpod/tests/test_prompt.py`

The test golden-files the exact prompt that `build_prompt` must produce. Any
drift (extra newline, missing role marker, header reordering) fails the test.
Spec section "Handler sketch" and the model card excerpt are the source of
truth for the body shape; the Gemma chat template wraps it.

- [ ] **Step 1: Create the test**

Path: `gpu/cope-b-runpod/tests/test_prompt.py`

```python
"""Unit tests for prompt.py — Gemma chat template + POLICY/CONTENT body.

The build_prompt() function must produce the exact text the model expects.
We verify the body shape against the model card's verbatim template
(no INSTRUCTIONS or ANSWER headers; literal POLICY and CONTENT slots) and
verify the Gemma chat-template wrapping by checking the role markers.
"""

import pytest


EXPECTED_BODY = """Examine the given POLICY and determine if the given CONTENT meets the criteria for ANY of the LABELS. Answer "1" if yes, and "0" if no.


POLICY
======

This is a test policy.


CONTENT
=======

[Parent post]: Hello.

[Reply]: World."""


def test_build_body_matches_model_card_template():
    """The body text fed into the chat template must match the model card
    structure exactly: two blank lines before POLICY header, '=' underline,
    blank line, policy slot, two blank lines, CONTENT header, etc."""
    from prompt import build_body

    body = build_body(
        policy="This is a test policy.",
        content="[Parent post]: Hello.\n\n[Reply]: World.",
    )
    assert body == EXPECTED_BODY


def test_build_prompt_wraps_body_in_gemma_chat_template(policy_text, sample_content_pair):
    """build_prompt() runs tokenizer.apply_chat_template with role=user. The
    resulting prompt must include the user-role marker and end with the
    assistant-generation prompt suffix so the model emits a 0/1 token next."""
    from prompt import build_prompt

    prompt = build_prompt(policy=policy_text, content=sample_content_pair)
    # Gemma chat-template markers (these are stable strings the template emits)
    assert "<start_of_turn>user" in prompt, "expected user-role start marker"
    assert "<start_of_turn>model" in prompt, "expected assistant-role generation prompt"
    # The body must appear inside the user turn (between user-start and end_of_turn)
    user_block = prompt.split("<start_of_turn>user")[1].split("<end_of_turn>")[0]
    assert "POLICY" in user_block
    assert "CONTENT" in user_block
    assert sample_content_pair in user_block


def test_build_prompt_handles_solo_content(policy_text, sample_content_solo):
    """Original posts (no parent) pass content through unchanged — no envelope.
    The model sees the bare body text in the CONTENT slot."""
    from prompt import build_prompt

    prompt = build_prompt(policy=policy_text, content=sample_content_solo)
    assert sample_content_solo in prompt
    # We did NOT prepend a [Parent post] envelope:
    assert "[Parent post]:" not in prompt or sample_content_solo.startswith("[Parent post]:")


def test_build_prompt_is_deterministic(policy_text, sample_content_pair):
    """Same inputs must produce byte-identical output. Prefix caching relies
    on this — a non-deterministic prompt invalidates the policy KV cache
    every call."""
    from prompt import build_prompt

    a = build_prompt(policy=policy_text, content=sample_content_pair)
    b = build_prompt(policy=policy_text, content=sample_content_pair)
    assert a == b


def test_build_prompt_policy_appears_before_content(policy_text):
    """Order matters for prefix caching: identical policy text must sit at
    the front so the same prefix is reused across calls with different
    content. Verify policy header precedes content header in the output."""
    from prompt import build_prompt

    prompt = build_prompt(policy=policy_text, content="anything")
    p_idx = prompt.index("POLICY")
    c_idx = prompt.index("CONTENT")
    assert p_idx < c_idx, "POLICY must precede CONTENT for prefix caching"


def test_build_body_handles_literal_braces_in_policy_and_content():
    """Policies often contain `{handle}`-style placeholders or JSON examples.
    A str.format()-based template would crash on these; sentinel-replace
    must pass them through verbatim."""
    from prompt import build_body

    body = build_body(
        policy="Rule {1}: don't address users as {their_handle}.",
        content="Reply to {parent_handle}: hello {there}",
    )
    assert "{1}" in body
    assert "{their_handle}" in body
    assert "{parent_handle}" in body
    assert "{there}" in body
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cd gpu/cope-b-runpod && python3 -m pytest tests/test_prompt.py -v`
Expected: `ModuleNotFoundError: prompt`. (Run from inside `gpu/cope-b-runpod/` so the test's `from prompt import ...` resolves; alternatively prepend `PYTHONPATH=.`.)

### Task 3.3: Implement `prompt.py`

**Files:**
- Create: `gpu/cope-b-runpod/prompt.py`

- [ ] **Step 1: Implement the module**

Path: `gpu/cope-b-runpod/prompt.py`

```python
"""Prompt assembly for the CoPE-B-A4B classifier.

The model expects two layers:
1. A POLICY/CONTENT body matching the structure on the HF model card
   (https://huggingface.co/zentropi-ai/cope-b-a4b — "Usage" section).
2. The body wrapped in Gemma-4's chat template via `apply_chat_template`,
   with `add_generation_prompt=True` so the model emits the next token
   (which will be the "1" or "0" verdict).

We expose `build_body` separately so unit tests can golden-file the body
without instantiating a tokenizer.
"""

from __future__ import annotations

# Body template — keep formatting EXACTLY as on the model card.
# Two blank lines before POLICY, '=' underline, blank line, slot, two blank
# lines before CONTENT, '=' underline, blank line, slot. Changing whitespace
# breaks the model's expected prefix.
#
# Sentinels `__POLICY__` / `__CONTENT__` are used instead of str.format so
# policy text can contain literal `{` or `}` (common in policies that
# discuss handle placeholders, JSON examples, etc.) without exploding.
_BODY_TEMPLATE = (
    'Examine the given POLICY and determine if the given CONTENT meets the '
    'criteria for ANY of the LABELS. Answer "1" if yes, and "0" if no.\n'
    '\n'
    '\n'
    'POLICY\n'
    '======\n'
    '\n'
    '__POLICY__\n'
    '\n'
    '\n'
    'CONTENT\n'
    '=======\n'
    '\n'
    '__CONTENT__'
)


def build_body(policy: str, content: str) -> str:
    """Return the POLICY/CONTENT body text, before chat-template wrapping."""
    return _BODY_TEMPLATE.replace("__POLICY__", policy).replace("__CONTENT__", content)


_TOKENIZER = None


def _get_tokenizer():
    global _TOKENIZER
    if _TOKENIZER is None:
        # Lazy-import to keep test files from forcing transformers on every
        # collection pass; tokenizer load is ~50 MB of metadata.
        from transformers import AutoTokenizer
        import os
        model_path = os.environ.get("MODEL_PATH", "zentropi-ai/cope-b-a4b")
        _TOKENIZER = AutoTokenizer.from_pretrained(model_path)
    return _TOKENIZER


def build_prompt(policy: str, content: str) -> str:
    """Build the full prompt for vLLM: body wrapped in the Gemma chat template,
    with an assistant generation prompt at the end so the model emits "1"/"0"."""
    body = build_body(policy=policy, content=content)
    tokenizer = _get_tokenizer()
    return tokenizer.apply_chat_template(
        [{"role": "user", "content": body}],
        tokenize=False,
        add_generation_prompt=True,
    )
```

- [ ] **Step 2: Run the test**

Run: `cd gpu/cope-b-runpod && PYTHONPATH=. python3 -m pytest tests/test_prompt.py -v`
Expected: `test_build_body_matches_model_card_template` passes immediately (no tokenizer needed). The remaining three tests will require the tokenizer; if `transformers` is not installed locally, those will skip / error. Either:
- Install dev requirements locally: `python3 -m pip install -r requirements.txt` (heavy — pulls vllm)
- Or skip the tokenizer-dependent tests locally and rely on CI:
  ```python
  pytest.importorskip("transformers")
  ```
  Adding this at the top of `test_prompt.py` makes the suite gracefully skip tokenizer-dependent tests when `transformers` is missing.

Add the `importorskip` guard now if your machine doesn't have `transformers`:

```python
import pytest
pytest.importorskip("transformers")  # required for chat-template tests
```

After adding it, re-run. Expected: `test_build_body_matches_model_card_template` passes; the others skip locally and run in CI.

- [ ] **Step 3: Commit prompt + tests**

```bash
git add gpu/cope-b-runpod/pyproject.toml gpu/cope-b-runpod/requirements.txt gpu/cope-b-runpod/tests/__init__.py gpu/cope-b-runpod/tests/conftest.py gpu/cope-b-runpod/tests/test_prompt.py gpu/cope-b-runpod/prompt.py
git commit -m 'feat(cope-b): prompt assembly with Gemma chat template + POLICY/CONTENT body

prompt.build_body returns the verbatim model-card body shape (POLICY
header + === underline + slot, then CONTENT header + === underline +
slot). prompt.build_prompt wraps the body via tokenizer.apply_chat_template
with role=user and add_generation_prompt=True so the model emits the
binary verdict token next.

Body is deterministic across calls so vLLMs prefix caching can reuse
the policy KV state for every classification.

test_prompt.py golden-files the body and asserts Gemma chat-template
markers + POLICY-before-CONTENT ordering. importorskip(transformers)
keeps the suite runnable on machines without the heavy ML deps.

Chainlink #<Phase 6.2 issue id>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>'
```

### Task 3.4: Write the failing handler test

**Files:**
- Create: `gpu/cope-b-runpod/tests/test_handler.py`

The handler test mocks `vllm.AsyncLLMEngine` so we don't load a 50 GB model
during pytest. We exercise the request → prompt → engine call → response
shape pipeline with controllable outputs.

- [ ] **Step 1: Create the test**

Path: `gpu/cope-b-runpod/tests/test_handler.py`

```python
"""Handler tests — RunPod request/response shape, verdict + confidence
calculation, and error handling. The vLLM engine is mocked so tests run
on CPU machines without GPU."""

import sys
from unittest.mock import MagicMock, patch
import pytest

# Stub heavy GPU-only deps before any handler import so collection works on CPU.
sys.modules.setdefault("vllm", MagicMock())
sys.modules.setdefault("runpod", MagicMock())

pytest.importorskip("transformers")  # build_prompt loads the tokenizer

pytestmark = pytest.mark.asyncio


async def _async_iter(*items):
    """Wrap a sequence of values as an async iterator (matches vLLM's
    AsyncLLMEngine.generate, which is an async generator yielding partial
    RequestOutputs)."""
    for item in items:
        yield item


def _mock_engine_result(
    token: str,
    logprob: float = -0.1,
    other_logprob: float = -3.0,
    decoded_prefix: str = "",
):
    """Build a MagicMock that looks like vllm's RequestOutput.outputs[0].
    `decoded_prefix` lets tests simulate Gemma's SentencePiece behavior
    where decoded_token may carry a leading space or ▁ marker."""
    other_token = "0" if token == "1" else "1"
    logprobs_map = {
        1: MagicMock(logprob=logprob, decoded_token=f"{decoded_prefix}{token}"),
        2: MagicMock(logprob=other_logprob, decoded_token=f"{decoded_prefix}{other_token}"),
    }
    out = MagicMock()
    out.text = token
    out.logprobs = [logprobs_map]
    result = MagicMock()
    result.outputs = [out]
    return result


@pytest.fixture
def patched_engine(monkeypatch, tmp_path):
    """Patch AsyncLLMEngine.from_engine_args at the import boundary so handler
    sees a mock instead of trying to load a real model."""
    policy_file = tmp_path / "policy.txt"
    policy_file.write_text("Test policy.")
    monkeypatch.setenv("MODEL_PATH", "zentropi-ai/cope-b-a4b")
    monkeypatch.setenv("POLICY_PATH", str(policy_file))

    fake_engine = MagicMock()
    # AsyncLLMEngine.generate is an async generator; replace with a callable
    # that returns an async iterator on every call. Tests set
    # fake_engine.generate_result on the returned MagicMock to control what
    # _async_iter yields.
    fake_engine.generate_result = _mock_engine_result(token="1")
    fake_engine.generate = MagicMock(
        side_effect=lambda *args, **kwargs: _async_iter(fake_engine.generate_result)
    )

    with patch("vllm.AsyncLLMEngine.from_engine_args", return_value=fake_engine):
        import importlib
        import handler  # type: ignore
        importlib.reload(handler)
        yield handler, fake_engine


async def test_handler_returns_toxic_true_when_model_emits_1(patched_engine):
    handler, fake_engine = patched_engine
    fake_engine.generate_result = _mock_engine_result(token="1", logprob=-0.05)
    result = await handler.handler({"id": "req-1", "input": {"content": "test"}})
    out = result["output"]
    assert out["toxic"] is True
    assert out["model"] == "cope-b-a4b"
    # Confidence is exp(logprob), so exp(-0.05) ≈ 0.95
    assert 0.9 < out["confidence"] < 1.0


async def test_handler_returns_toxic_false_when_model_emits_0(patched_engine):
    handler, fake_engine = patched_engine
    fake_engine.generate_result = _mock_engine_result(token="0", logprob=-0.2)
    result = await handler.handler({"id": "req-2", "input": {"content": "test"}})
    out = result["output"]
    assert out["toxic"] is False
    assert 0.7 < out["confidence"] < 0.9   # exp(-0.2) ≈ 0.819


async def test_handler_normalizes_decoded_token_with_sentinel_prefix(patched_engine):
    """Gemma's SentencePiece tokenizer may return decoded_token with a leading
    space or ▁ marker (`'▁1'` or `' 1'`). out.text.strip() is the bare token;
    the logprobs lookup must normalize both sides before comparing or the
    confidence calculation silently falls through to ValueError."""
    handler, fake_engine = patched_engine
    fake_engine.generate_result = _mock_engine_result(
        token="1", logprob=-0.05, decoded_prefix="▁"
    )
    result = await handler.handler({"id": "req-norm-1", "input": {"content": "test"}})
    assert result["output"]["toxic"] is True
    assert 0.9 < result["output"]["confidence"] < 1.0

    fake_engine.generate_result = _mock_engine_result(
        token="0", logprob=-0.1, decoded_prefix=" "
    )
    result = await handler.handler({"id": "req-norm-2", "input": {"content": "test"}})
    assert result["output"]["toxic"] is False


async def test_handler_returns_policy_version_from_env(patched_engine, monkeypatch):
    handler, fake_engine = patched_engine
    monkeypatch.setenv("POLICY_VERSION", "policy-v3-2026-07-01")
    import importlib
    importlib.reload(handler)
    # Reload reset the fake_engine reference; re-patch the new module's engine.
    # (Simpler: assert that handler reads POLICY_VERSION at module import.)
    fake_engine.generate_result = _mock_engine_result(token="1")
    handler._engine = fake_engine  # type: ignore[attr-defined]
    result = await handler.handler({"id": "req-3", "input": {"content": "test"}})
    assert result["output"]["policy_version"] == "policy-v3-2026-07-01"


async def test_handler_raises_on_missing_input(patched_engine):
    handler, _ = patched_engine
    with pytest.raises(KeyError):
        await handler.handler({"id": "req-4", "input": {}})


async def test_handler_raises_on_unexpected_model_output(patched_engine):
    """If the model emits something other than "0" or "1", surface the failure
    rather than silently falling back. Spec: "No silent fallbacks."""
    handler, fake_engine = patched_engine
    fake_engine.generate_result = _mock_engine_result(token="maybe", logprob=-1.0)
    with pytest.raises(ValueError, match="unexpected"):
        await handler.handler({"id": "req-5", "input": {"content": "test"}})
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cd gpu/cope-b-runpod && PYTHONPATH=. python3 -m pytest tests/test_handler.py -v`
Expected: `ModuleNotFoundError: handler` (or `vllm` if not installed locally — fine; the test patches it but the import-time check still needs the module attribute path).

### Task 3.5: Implement `handler.py`

**Files:**
- Create: `gpu/cope-b-runpod/handler.py`

- [ ] **Step 1: Implement the module**

Path: `gpu/cope-b-runpod/handler.py`

```python
"""RunPod Serverless worker entrypoint for the CoPE-B-A4B classifier.

vLLM AsyncLLMEngine handles the model + KV cache. Each request feeds a
prompt assembled via prompt.build_prompt, samples a single token greedily,
and returns the binary verdict + normalized confidence.

Spec: docs/superpowers/specs/2026-06-05-cope-b-self-hosted-design.md
"""

from __future__ import annotations

import math
import os
import uuid

import runpod
from vllm import AsyncLLMEngine, AsyncEngineArgs, SamplingParams

from prompt import build_prompt


MODEL_PATH = os.environ["MODEL_PATH"]
POLICY_PATH = os.environ["POLICY_PATH"]
POLICY_VERSION = os.environ.get("POLICY_VERSION", "policy-unversioned")

with open(POLICY_PATH, "r", encoding="utf-8") as fp:
    POLICY = fp.read()

# Build the engine once at module import.
_engine = AsyncLLMEngine.from_engine_args(
    AsyncEngineArgs(
        model=MODEL_PATH,
        dtype="bfloat16",
        max_model_len=4096,          # 256K default is wasteful for ~300-tok inputs
        max_num_seqs=32,             # tune empirically post-deploy
        enable_prefix_caching=True,  # critical — policy text is identical per call
    )
)

# Greedy single-token decode, top-2 logprobs so we can extract confidence.
_SAMPLING = SamplingParams(
    max_tokens=1,
    temperature=0.0,
    logprobs=2,
)


async def handler(event):
    """Classify a single content string. event = {"id": ..., "input": {"content": ...}}.

    Returns {"output": {"toxic": bool, "confidence": float, "model": str,
                        "policy_version": str}}.

    Raises:
        KeyError: input missing "content"
        ValueError: model emitted a token other than "0" or "1"
    """
    inp = event["input"]
    content = inp["content"]   # raises KeyError if missing — surfaced to caller

    prompt = build_prompt(policy=POLICY, content=content)
    request_id = event.get("id") or uuid.uuid4().hex

    # AsyncLLMEngine.generate is an async iterator; the last yield contains the
    # finished output. For max_tokens=1 there's exactly one yield.
    final = None
    async for partial in _engine.generate(prompt, _SAMPLING, request_id):
        final = partial
    if final is None:
        raise RuntimeError("vLLM engine produced no output")

    out = final.outputs[0]
    token = out.text.strip()
    if token not in {"0", "1"}:
        raise ValueError(f"unexpected model token: {token!r}")

    # Confidence: exp(logprob of emitted token). vLLM logprobs[0] is a dict
    # keyed by token_id; find the entry whose normalized decoded_token matches
    # `token`. Gemma's SentencePiece tokenizer may return decoded_token with
    # a leading space or ▁ (U+2581) marker; normalize both sides.
    logprob_map = out.logprobs[0]

    def _norm(s: str) -> str:
        return s.strip().lstrip("▁")

    emitted_logprob = next(
        (lp.logprob for lp in logprob_map.values() if _norm(lp.decoded_token) == token),
        None,
    )
    if emitted_logprob is None:
        raise ValueError(f"emitted token {token!r} missing from logprobs map")
    confidence = float(math.exp(emitted_logprob))

    return {
        "output": {
            "toxic": token == "1",
            "confidence": confidence,
            "model": "cope-b-a4b",
            "policy_version": POLICY_VERSION,
        }
    }


if __name__ == "__main__":
    runpod.serverless.start({"handler": handler})
```

- [ ] **Step 2: Run the test**

Run: `cd gpu/cope-b-runpod && PYTHONPATH=. python3 -m pytest tests/test_handler.py -v`
Expected: all 5 tests pass. If vLLM-import errors block collection (vLLM is GPU-only on import in some versions), add a stub at top of `test_handler.py`:

```python
import sys
sys.modules.setdefault("vllm", MagicMock())
sys.modules.setdefault("runpod", MagicMock())
```

(Inserted ABOVE the `pytest.importorskip("transformers")` line so the stubs are in place before handler is imported.)

- [ ] **Step 3: Commit**

```bash
git add gpu/cope-b-runpod/handler.py gpu/cope-b-runpod/tests/test_handler.py
git commit -m 'feat(cope-b): RunPod handler with vLLM AsyncLLMEngine + greedy single-token decode

handler.py loads model + policy once at module import (RunPod keeps the
process alive between requests). Per request: build_prompt -> engine.generate
(temperature=0, max_tokens=1, logprobs=2) -> extract emitted token -> verdict
+ confidence (exp(logprob)). Raises ValueError on tokens other than 0/1
per the spec no-silent-fallback rule.

Tests mock vllm to run on CPU; cover toxic/clean verdicts, confidence math,
POLICY_VERSION env propagation, missing-input error, and unexpected-token
error. importorskip(transformers) keeps the suite portable.

Chainlink #<Phase 6.2 issue id>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>'
```

### Task 3.6: Dockerfile, runpod.yml, smoke + prefix-cache scripts

**Files:**
- Create: `gpu/cope-b-runpod/Dockerfile`
- Create: `gpu/cope-b-runpod/runpod.yml`
- Create: `gpu/cope-b-runpod/tests/smoke_test.sh`
- Create: `gpu/cope-b-runpod/tests/test_prefix_cache.py`
- Modify: `gpu/cope-b-runpod/README.md` (expand from Chunk 2 stub)

- [ ] **Step 0: Verify policy.txt exists (Chunk 2 prerequisite)**

Run: `[ -f gpu/cope-b-runpod/policy.txt ] && echo OK || { echo "MISSING — complete Chunk 2 first"; exit 1; }`
Expected: `OK`. If missing, the rest of this task fails on `COPY` in the Dockerfile.

- [ ] **Step 1: Dockerfile**

Path: `gpu/cope-b-runpod/Dockerfile`

```dockerfile
# Pinned vLLM image; matches the version pinned in requirements.txt. The
# container digest is captured in CI when the image is published — see
# .github/workflows/build-cope-b-image.yml for the digest pin used at deploy.
FROM vllm/vllm-openai:v0.20.2

WORKDIR /app

# Copy app code first so requirements layer caches on weight changes (which
# are the slow part). vllm is already in the base image; we add runpod and
# transformers explicitly so handler+prompt imports don't depend on whatever
# transformers version the base image happens to ship (or lack).
COPY requirements.txt /app/requirements.txt
RUN python3 -m pip install --no-cache-dir \
        'runpod>=1.7,<2' \
        'transformers>=4.50,<5'

COPY handler.py prompt.py policy.txt /app/

# Bake weights into the image. Build-arg lets CI override the revision for
# pinned-version builds.
ARG MODEL_REVISION=main
ENV MODEL_PATH=/weights \
    POLICY_PATH=/app/policy.txt
RUN python3 -m pip install --no-cache-dir huggingface-hub && \
    huggingface-cli download zentropi-ai/cope-b-a4b \
        --local-dir /weights \
        --revision ${MODEL_REVISION}

# POLICY_VERSION is injected at build time by CI from the git short SHA + date.
ARG POLICY_VERSION=policy-unversioned
ENV POLICY_VERSION=${POLICY_VERSION}

CMD ["python3", "-u", "handler.py"]
```

- [ ] **Step 2: RunPod endpoint config**

Path: `gpu/cope-b-runpod/runpod.yml`

```yaml
# Source of truth for the RunPod Serverless endpoint config. Endpoint is
# created manually in the RunPod web console (one-time); deviations from
# this file should be reconciled.
name: charcoal-cope-b
gpu: NVIDIA A100 80GB PCIe
flashboot: true
scale_to_zero: true
idle_timeout: 60          # seconds; tune down to 5-10 after measuring warm-restore rate
min_workers: 0
max_workers: 3            # absorbs concurrent onboardings; cheap thanks to scale-to-zero
execution_timeout: 600    # 10-min hard cap per request
region: us-west           # match Railways production region (verify via `railway status`)
```

- [ ] **Step 3: Local smoke test script**

Path: `gpu/cope-b-runpod/tests/smoke_test.sh`

```bash
#!/usr/bin/env bash
# Local smoke test: serve the model under vLLM and walk fixture inputs.
# Default mode uses /v1/chat/completions (matches production handler's
# apply_chat_template path). Override with SMOKE_MODE=completions to use
# the raw /v1/completions endpoint with a hand-rolled body — useful when
# debugging whether the chat-template wrapper is hiding a problem.
#
# Requires a CUDA GPU (80 GB for full BF16). Apple Silicon will not run vllm.
# Run from gpu/cope-b-runpod/. Aborts after $MAX_FAILURES misses.
set -euo pipefail

POLICY_PATH=${POLICY_PATH:-policy.txt}
MODEL=${MODEL:-zentropi-ai/cope-b-a4b}
SMOKE_MODE=${SMOKE_MODE:-chat}        # chat | completions
MAX_FAILURES=${MAX_FAILURES:-5}        # abort after N misses

if [[ ! -f "$POLICY_PATH" ]]; then
    echo "ERROR: $POLICY_PATH not found"; exit 1
fi

vllm serve "$MODEL" \
    --dtype bfloat16 \
    --max-model-len 4096 \
    --enable-prefix-caching \
    --port 8000 &
VLLM_PID=$!
trap "kill $VLLM_PID 2>/dev/null || true" EXIT

# Wait up to 5 min for the OpenAI-compatible endpoint to come up.
echo "Waiting for vLLM to start..."
for _ in $(seq 1 60); do
    if curl -sf http://localhost:8000/v1/models >/dev/null; then
        break
    fi
    sleep 5
done

POLICY=$(cat "$POLICY_PATH")
FAIL=0

# Build the POLICY/CONTENT body the same way prompt.build_body does so the
# raw-completions and chat-completions modes share a body. The chat path then
# wraps the body in the Gemma role markers via the server's chat template;
# the completions path sends the body as a plain prompt.
build_body() {
    local content="$1"
    printf 'Examine the given POLICY and determine if the given CONTENT meets the criteria for ANY of the LABELS. Answer "1" if yes, and "0" if no.\n\n\nPOLICY\n======\n\n%s\n\n\nCONTENT\n=======\n\n%s' "$POLICY" "$content"
}

classify() {
    local content="$1"
    local body request
    body=$(build_body "$content")
    if [[ "$SMOKE_MODE" == "chat" ]]; then
        request=$(jq -nc --arg model "$MODEL" --arg body "$body" \
            '{model: $model, max_tokens: 1, temperature: 0, messages: [{role: "user", content: $body}]}')
        curl -sf -X POST http://localhost:8000/v1/chat/completions \
            -H 'content-type: application/json' -d "$request" \
            | jq -r '.choices[0].message.content' | tr -d '[:space:]'
    else
        request=$(jq -nc --arg model "$MODEL" --arg prompt "$body" \
            '{model: $model, max_tokens: 1, temperature: 0, prompt: $prompt}')
        curl -sf -X POST http://localhost:8000/v1/completions \
            -H 'content-type: application/json' -d "$request" \
            | jq -r '.choices[0].text' | tr -d '[:space:]'
    fi
}

walk_fixture() {
    local fixture="$1"
    while IFS= read -r line; do
        local id expected content want verdict
        id=$(echo "$line" | jq -r .id)
        expected=$(echo "$line" | jq -r .label)
        content=$(echo "$line" | jq -r .content)
        case "$expected" in
            toxic)   want=1 ;;
            clean)   want=0 ;;
            *)       continue ;;   # skip uncertain
        esac
        verdict=$(classify "$content")
        if [[ "$verdict" != "$want" ]]; then
            echo "FAIL $id: expected $want got $verdict"
            FAIL=$((FAIL + 1))
            if [[ $FAIL -ge $MAX_FAILURES ]]; then
                echo "Aborting after $MAX_FAILURES failures"
                return $FAIL
            fi
        else
            echo "OK   $id"
        fi
    done < "$fixture"
}

walk_fixture ../../tests/fixtures/cope_b/known_toxic.jsonl
walk_fixture ../../tests/fixtures/cope_b/known_clean.jsonl

echo "Mode: $SMOKE_MODE   Failures: $FAIL"
exit $FAIL
```

(Default mode hits `/v1/chat/completions`, which lets vLLM's server-side
chat template wrap the body in Gemma role markers — exactly what
`handler.py`'s `apply_chat_template` path does in production. Setting
`SMOKE_MODE=completions` falls back to the hand-rolled body for debugging
chat-template behavior.)

Mark executable: `chmod +x gpu/cope-b-runpod/tests/smoke_test.sh`.

- [ ] **Step 4: Prefix-cache benchmark test**

Path: `gpu/cope-b-runpod/tests/test_prefix_cache.py`

```python
"""Benchmark: assert that vLLMs prefix caching is actually firing.

We send N identical-policy requests with varying CONTENT and assert that
the median time-to-second-request is materially lower than time-to-first.
Without prefix caching, every call reprocesses the policy KV state — easy
~10x cost difference under our workload.

Requires a live vLLM endpoint (gpu/cope-b-runpod/tests/smoke_test.sh
must be running, or a deployed RunPod endpoint). Skip if neither is
available.
"""

import os
import statistics
import time
import urllib.request
import urllib.error
import json
import pytest


VLLM_URL = os.environ.get("VLLM_URL", "http://localhost:8000")


def _is_endpoint_up() -> bool:
    try:
        with urllib.request.urlopen(f"{VLLM_URL}/v1/models", timeout=2):
            return True
    except urllib.error.URLError:
        return False


pytestmark = pytest.mark.skipif(
    not _is_endpoint_up(),
    reason="vLLM endpoint not reachable; run smoke_test.sh or set VLLM_URL",
)


def _call(content: str) -> float:
    body = json.dumps({
        "model": os.environ.get("MODEL", "zentropi-ai/cope-b-a4b"),
        "prompt": "POLICY\n======\n\nshared policy text\n\nCONTENT\n=======\n\n" + content,
        "max_tokens": 1,
        "temperature": 0,
    }).encode()
    req = urllib.request.Request(
        f"{VLLM_URL}/v1/completions",
        data=body,
        headers={"content-type": "application/json"},
    )
    start = time.perf_counter()
    with urllib.request.urlopen(req, timeout=30) as r:
        r.read()
    return time.perf_counter() - start


def test_prefix_cache_warm_calls_are_materially_faster():
    # Warm the cache once
    first = _call("First content — establishes the prefix cache")
    # Now measure several warm calls
    warm = [_call(f"Warm content variant {i}") for i in range(5)]
    median_warm = statistics.median(warm)

    # Heuristic: warm median should be < 50% of cold first.
    # Tune this threshold once we have real numbers from Task 3.6 smoke runs.
    assert median_warm < first * 0.5, (
        f"Prefix caching not firing: first={first:.2f}s, median warm={median_warm:.2f}s. "
        f"Investigate --enable-prefix-caching flag."
    )
```

- [ ] **Step 5: Expand the README**

Path: `gpu/cope-b-runpod/README.md` — replace the Chunk 2 stub with the full operator doc:

```markdown
# Charcoal CoPE-B-A4B GPU service

vLLM-on-RunPod-Serverless harness for Charcoal's Stage-2 toxicity classifier.

## Files

- `Dockerfile` — image build. Bakes the model weights and `policy.txt` into the image.
- `handler.py` — RunPod Serverless worker entrypoint. Wraps vLLM's AsyncLLMEngine.
- `prompt.py` — Gemma chat template + POLICY/CONTENT body assembly.
- `policy.txt` — toxicity policy (versioned in git; see `docs/superpowers/specs/...` for authoring guidance).
- `runpod.yml` — RunPod endpoint config (manual via web console at create time).
- `requirements.txt` — Python runtime pins.
- `tests/test_prompt.py` — prompt assembly unit tests (CPU-only).
- `tests/test_handler.py` — handler unit tests with mocked vLLM.
- `tests/test_prefix_cache.py` — benchmark that prefix caching is firing.
- `tests/smoke_test.sh` — local end-to-end smoke against `vllm serve`.

## Local development

```bash
cd gpu/cope-b-runpod
python3 -m pip install -r requirements.txt    # heavy: pulls vllm
python3 -m pytest tests/                       # runs prompt + handler tests
./tests/smoke_test.sh                          # requires a CUDA GPU
```

## Deploying

Images are built and published by `.github/workflows/build-cope-b-image.yml`
on pushes to `staging` and `main` when files under `gpu/cope-b-runpod/**`
change. The workflow publishes to `ghcr.io/musicjunkieg/charcoal-cope-b:<sha>`
with a manifest digest pinned in the resulting GitHub Actions summary.

RunPod endpoint is configured per `runpod.yml`. Updates to that file
require manual reconciliation in the RunPod web console (no IaC yet).

## Policy changes

Editing `policy.txt` requires an image rebuild. CI bumps `POLICY_VERSION`
to `policy-<short-sha>-<date>` automatically. Audit log captures
`policy_version` per classification so a change can be located post-hoc.

## Region

Endpoint runs in `us-west` to minimize round-trip from Railway production.
Verify before creating: `railway status` should show a us-west default region.
```

- [ ] **Step 6: Commit infra + smoke + prefix-cache**

```bash
git add gpu/cope-b-runpod/Dockerfile gpu/cope-b-runpod/runpod.yml gpu/cope-b-runpod/tests/smoke_test.sh gpu/cope-b-runpod/tests/test_prefix_cache.py gpu/cope-b-runpod/README.md
git commit -m 'feat(cope-b): Dockerfile + runpod.yml + smoke + prefix-cache benchmark

Dockerfile bakes zentropi-ai/cope-b-a4b weights (MODEL_REVISION arg
defaults to main; CI pins per release) and policy.txt into the image.
POLICY_VERSION env injected at build time so audit logs capture the
exact policy + image combo per classification.

runpod.yml documents the endpoint config (A100 80GB, FlashBoot,
scale-to-zero, idle_timeout=60s, max_workers=3, execution_timeout=600s,
region=us-west).

smoke_test.sh runs vllm serve locally and walks every fixture line
through /v1/completions, asserting 0/1 verdicts match expected labels.

test_prefix_cache.py is a runtime benchmark — given a live endpoint
(local vllm serve or deployed RunPod), it asserts median warm-call
latency is <50% of cold first-call latency, catching silent prefix-
caching regressions between vLLM minor versions.

Chainlink #<Phase 6.2 issue id>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>'
```

### Task 3.7: GitHub Actions image build workflow

**Files:**
- Create: `.github/workflows/build-cope-b-image.yml`

- [ ] **Step 1: Create the workflow**

Path: `.github/workflows/build-cope-b-image.yml`

```yaml
name: Build CoPE-B image

on:
  push:
    branches: [main, staging]
    paths:
      - 'gpu/cope-b-runpod/**'
      - '.github/workflows/build-cope-b-image.yml'
  workflow_dispatch:

jobs:
  build:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      packages: write
    steps:
      - uses: actions/checkout@v4

      - name: Set up Buildx
        uses: docker/setup-buildx-action@v3

      - name: Log in to GHCR
        uses: docker/login-action@v3
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}

      - name: Derive policy version
        id: policyver
        run: |
          short_sha=$(git rev-parse --short HEAD)
          date=$(date -u +%Y-%m-%d)
          echo "value=policy-${short_sha}-${date}" >> "$GITHUB_OUTPUT"

      - name: Build and push
        uses: docker/build-push-action@v6
        with:
          context: ./gpu/cope-b-runpod
          file: ./gpu/cope-b-runpod/Dockerfile
          push: true
          tags: |
            ghcr.io/${{ github.repository_owner }}/charcoal-cope-b:${{ github.sha }}
            ghcr.io/${{ github.repository_owner }}/charcoal-cope-b:${{ github.ref_name }}
          build-args: |
            MODEL_REVISION=main
            POLICY_VERSION=${{ steps.policyver.outputs.value }}
          # Cache the weight-download layer between runs. The huggingface-cli
          # download layer is ~50 GB and cache hit is keyed by MODEL_REVISION,
          # so a policy.txt-only edit reuses the cached weight layer.
          cache-from: type=gha
          cache-to: type=gha,mode=max

      - name: Summary
        run: |
          echo "Built ghcr.io/${{ github.repository_owner }}/charcoal-cope-b:${{ github.sha }}" >> $GITHUB_STEP_SUMMARY
          echo "Policy version: ${{ steps.policyver.outputs.value }}" >> $GITHUB_STEP_SUMMARY
          echo "Next: update RunPod endpoint to use this image digest." >> $GITHUB_STEP_SUMMARY
```

- [ ] **Step 2: Commit**

```bash
git add .github/workflows/build-cope-b-image.yml
git commit -m 'ci(cope-b): GH Actions workflow to build + push image on gpu/ changes

Triggered on push to main or staging when gpu/cope-b-runpod/** changes
(or manually via workflow_dispatch). Builds the Dockerfile, pushes to
ghcr.io/<owner>/charcoal-cope-b tagged with both git SHA and branch
name. POLICY_VERSION build-arg is derived from short-sha + UTC date so
the value is unique per commit.

RunPod endpoint update is manual — after the workflow succeeds, the
image digest from the GH Actions summary gets pasted into the RunPod
endpoint config in the web console.

Chainlink #<Phase 6.2 issue id>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>'
```

- [ ] **Step 3: Push**

Run: `git push origin feat/cope-b-self-host`
Expected: branch updated; GH Actions workflow may run automatically on the next push touching `gpu/cope-b-runpod/**`.

- [ ] **Step 4: Close subissue and switch to Chunk 4**

```
chainlink issue close <Phase 6.2 issue id>
chainlink session work <Phase 6.3 issue id>   # Rust trait + RunPodCopeBClient
```

---

## Chunk 4: Rust adapter — trait, RunPodCopeBClient, ZentropiClient refactor, TwoStageVerdict refactor, retry, observability

**Subissue:** `Phase 6.3 — Rust trait + RunPodCopeBClient + ZentropiClient refactor`.

**Spec sections to re-read first:** "Architecture" (trait + Verdict shape + threshold ownership), "Failure modes", "Cold-start UX and timeout strategy", "Monitoring".

**Ground truth captured ahead of Chunk-4 writing** (confirm by re-running these greps; if outputs have drifted, update file:line refs accordingly):

- `src/toxicity/zentropi.rs:71-77` — `ZentropiClient { client, api_key, labeler_id, labeler_version_id }`
- `src/toxicity/zentropi.rs:108` — `pub async fn classify(&self, text: &str) -> Result<ZentropiResponse>`
- `src/toxicity/zentropi.rs:142` — `pub async fn classify_pair(&self, parent: &str, reply: &str) -> Result<ZentropiResponse>`
- `src/toxicity/zentropi.rs:34-38` — existing `MAX_RETRIES=3` + `INITIAL_BACKOFF=500ms` with no jitter; replaced in this chunk
- `src/toxicity/ensemble.rs:37-48` — `TwoStageVerdict { is_toxic, onnx_score, onnx_attributes, source, zentropi_confidence: Option<f64> }`
- `src/toxicity/ensemble.rs:52-61` — `VerdictSource::{OnnxCleared, ZentropiToxic, ZentropiSafe, OnnxFallback}`
- `src/toxicity/ensemble.rs:76` — `TwoStageToxicityScorer::new(primary: Box<dyn ToxicityScorer>, zentropi: Option<Arc<ZentropiClient>>)`
- `src/toxicity/ensemble.rs:82` — `pub fn has_zentropi(&self) -> bool`
- `src/toxicity/ensemble.rs:177` — `pub async fn classify_batch(...) -> Result<Vec<TwoStageVerdict>>`
- `src/toxicity/ensemble.rs:236-247` — `classify_batch_with_contexts` impl that downgrades to `BinaryVerdict`
- `src/toxicity/traits.rs:30-36` — `BinaryVerdict { is_toxic, onnx_score, onnx_attributes }` (unchanged)
- `Cargo.toml:36` — `async-trait = "0.1"` (present)
- `Cargo.toml:144` — `tower = "0.5"` (present — used for the cost-guardrail middleware later, but we use `backon` for retry-with-jitter since `tower::retry` carries more layers than needed for a single HTTP client)

### Task 4.0: Verify branch + open subissue

- [ ] **Step 1: Branch verify**

Run: `git status -sb | head -1`
Expected: `## feat/cope-b-self-host`.

- [ ] **Step 2: Confirm subissue focus**

Run: `chainlink session status`
Expected: `Working on: #<Phase 6.3 issue id>`. If not, `chainlink session work <id>`.

### Task 4.1: Add `backon` for retry-with-decorrelated-jitter

**Files:** `Cargo.toml`.

`backon` provides composable backoff strategies including decorrelated jitter, with first-class async support. Plain `tower::retry` would work but pulls more middleware than necessary here.

- [ ] **Step 1: Add the deps**

Edit `Cargo.toml`. Under `[dependencies]`:

```toml
backon = { version = "1", default-features = false, features = ["tokio-sleep"] }
thiserror = "2"
```

(`thiserror` is used by `RunPodError` in Task 4.5 to give backon's `.when()` an enum variant to match instead of string-grepping; spec §"Retry policy".)

- [ ] **Step 2: Verify it resolves**

Run: `cargo check --features web 2>&1 | tail -5`
Expected: clean compile. If a version compatibility issue surfaces with our other deps, pin to the latest `1.x` that resolves.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m 'feat(deps): add backon + thiserror for typed retry control

backon: ToxicityClassifier impls (RunPod and Zentropi) spread concurrent
retries across the backoff window via decorrelated jitter per spec
§"Retry policy" — prevents thundering-herd against the upstream during
transient failures.

thiserror: RunPodError carries typed retry classification so backons
.when() filter matches on an enum variant instead of stringified error
messages (brittle if errors are reworded).

Chainlink #<Phase 6.3 issue id>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>'
```

### Task 4.2: Write failing test for `ToxicityClassifier` trait + `ClassifierVerdict`

**Files:** Create `tests/unit_classifier.rs`.

- [ ] **Step 1: Create the test**

Path: `tests/unit_classifier.rs`

```rust
//! Unit tests for src/toxicity/classifier.rs — trait shape, ClassifierVerdict,
//! is_toxic helper threshold logic, and the StubClassifier used by integration
//! tests.

use charcoal::toxicity::classifier::{
    is_toxic, ClassifierVerdict, StubClassifier, ToxicityClassifier,
};
use serde_json::json;

#[tokio::test]
async fn stub_classifier_returns_scripted_verdict() {
    let stub = StubClassifier::with_script(vec![
        ClassifierVerdict {
            toxic_token: true,
            confidence: 0.91,
            latency_ms: 12,
            model_id: "stub".into(),
            policy_version: "stub-policy".into(),
        },
        ClassifierVerdict {
            toxic_token: false,
            confidence: 0.85,
            latency_ms: 12,
            model_id: "stub".into(),
            policy_version: "stub-policy".into(),
        },
    ]);

    let v1 = stub.classify("anything").await.unwrap();
    assert!(v1.toxic_token);
    assert_eq!(v1.model_id, "stub");

    let v2 = stub.classify("anything else").await.unwrap();
    assert!(!v2.toxic_token);
}

#[tokio::test]
async fn stub_classifier_exhaustion_errors_loudly() {
    let stub = StubClassifier::with_script(vec![]);
    let err = stub.classify("anything").await.unwrap_err();
    assert!(format!("{err}").contains("stub script exhausted"));
}

#[test]
fn classifier_verdict_serde_roundtrip() {
    let v = ClassifierVerdict {
        toxic_token: true,
        confidence: 0.73,
        latency_ms: 200,
        model_id: "cope-b-a4b".into(),
        policy_version: "policy-2026-07-01".into(),
    };
    let json = serde_json::to_value(&v).unwrap();
    assert_eq!(
        json,
        json!({
            "toxic_token": true,
            "confidence": 0.73,
            "latency_ms": 200,
            "model_id": "cope-b-a4b",
            "policy_version": "policy-2026-07-01",
        })
    );
}

#[tokio::test]
async fn is_toxic_applies_threshold_from_implementation() {
    // StubClassifier's threshold is 0.0 (trust the script's boolean).
    let stub = StubClassifier::with_script(vec![ClassifierVerdict {
        toxic_token: true,
        confidence: 0.10,
        latency_ms: 1,
        model_id: "stub".into(),
        policy_version: "stub".into(),
    }]);
    let v = stub.classify("x").await.unwrap();
    assert!(is_toxic(&stub as &dyn ToxicityClassifier, &v));

    // A classifier whose threshold is > confidence rejects, even when the
    // model said toxic_token=true.
    let stub_strict = StubClassifier::with_script_and_threshold(
        vec![ClassifierVerdict {
            toxic_token: true,
            confidence: 0.10,
            latency_ms: 1,
            model_id: "stub-strict".into(),
            policy_version: "stub".into(),
        }],
        /* threshold = */ 0.5,
    );
    let v2 = stub_strict.classify("x").await.unwrap();
    assert!(!is_toxic(&stub_strict as &dyn ToxicityClassifier, &v2));
}

#[tokio::test]
async fn classifier_trait_is_send_sync() {
    // Compile-time check: the trait object must be Send + Sync so it can live
    // inside an Arc shared across tasks.
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Box<dyn ToxicityClassifier>>();
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo test --test unit_classifier`
Expected: compile error — module `classifier` not found.

### Task 4.3: Implement `src/toxicity/classifier.rs`

**Files:**
- Create: `src/toxicity/classifier.rs`
- Modify: `src/toxicity/mod.rs` (add `pub mod classifier;`)

- [ ] **Step 1: Create the module**

Path: `src/toxicity/classifier.rs`

```rust
//! Stage-2 toxicity classifier trait.
//!
//! Implementations live in sibling modules:
//! - `runpod_cope_b` — self-hosted CoPE-B-A4B on RunPod Serverless
//! - `zentropi` — hosted CoPE API (kept for fallback)
//!
//! The trait owns the threshold via `threshold()`. Callers never pass a
//! threshold — see spec §"Backend selection and per-backend thresholds"
//! for why threshold drift via runtime override is forbidden.

use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;
use std::sync::Mutex;

/// Outcome of a single classification call. Stage-2 only — the full two-stage
/// verdict lives in `TwoStageVerdict`.
#[derive(Debug, Clone, Serialize)]
pub struct ClassifierVerdict {
    /// Did the model emit "1" (or its hosted-API equivalent)?
    pub toxic_token: bool,
    /// Normalized confidence in [0.0, 1.0].
    pub confidence: f32,
    /// Wall-clock latency for audit / metrics.
    pub latency_ms: u32,
    /// Mirrors `ToxicityClassifier::model_id()` — captured per-call so audit
    /// events carry the value without lifetime juggling.
    pub model_id: String,
    /// Mirrors `ToxicityClassifier::policy_version()`.
    pub policy_version: String,
}

#[async_trait]
pub trait ToxicityClassifier: Send + Sync {
    /// Classify a single text. For replies-with-parent, callers compose the
    /// envelope via `crate::toxicity::format_parent_reply` and pass the result
    /// as `content`. There is no `classify_pair` shortcut on the trait.
    async fn classify(&self, content: &str) -> Result<ClassifierVerdict>;
    fn name(&self) -> &'static str;
    fn model_id(&self) -> &'static str;
    fn policy_version(&self) -> &'static str;
    /// Sole source of truth for the threshold. Each impl returns its own
    /// `const f32` calibrated for the model it wraps.
    fn threshold(&self) -> f32;
}

/// Apply the implementation's threshold. Free function rather than a default
/// trait method so callers must pass a concrete `&dyn ToxicityClassifier` and
/// can't accidentally bypass the impl's threshold.
pub fn is_toxic(classifier: &dyn ToxicityClassifier, v: &ClassifierVerdict) -> bool {
    v.toxic_token && v.confidence >= classifier.threshold()
}

/// Scripted classifier for tests. Pops verdicts from the front of an internal
/// queue per `classify` call; errors when exhausted to keep tests honest.
pub struct StubClassifier {
    script: Mutex<Vec<ClassifierVerdict>>,
    threshold: f32,
}

impl StubClassifier {
    pub fn with_script(script: Vec<ClassifierVerdict>) -> Self {
        Self {
            script: Mutex::new(script),
            threshold: 0.0,
        }
    }

    pub fn with_script_and_threshold(script: Vec<ClassifierVerdict>, threshold: f32) -> Self {
        Self {
            script: Mutex::new(script),
            threshold,
        }
    }
}

#[async_trait]
impl ToxicityClassifier for StubClassifier {
    async fn classify(&self, _content: &str) -> Result<ClassifierVerdict> {
        let mut guard = self.script.lock().expect("StubClassifier script lock");
        if guard.is_empty() {
            anyhow::bail!("stub script exhausted");
        }
        Ok(guard.remove(0))
    }
    fn name(&self) -> &'static str { "stub" }
    fn model_id(&self) -> &'static str { "stub" }
    fn policy_version(&self) -> &'static str { "stub" }
    fn threshold(&self) -> f32 { self.threshold }
}
```

- [ ] **Step 2: Register the module**

Edit `src/toxicity/mod.rs` — add `pub mod classifier;` near other module declarations.

- [ ] **Step 3: Run tests**

Run: `cargo test --test unit_classifier`
Expected: all 5 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/toxicity/classifier.rs src/toxicity/mod.rs tests/unit_classifier.rs
git commit -m 'feat(toxicity): ToxicityClassifier trait + ClassifierVerdict + StubClassifier

Trait owns the threshold via threshold() — callers never override
(spec: "sole source of truth"). is_toxic free function applies the
threshold; placing it on the trait would let downstream code substitute
a different threshold by accident.

ClassifierVerdict captures model_id + policy_version per call so audit
events carry exact provenance. Serialize-only derive (write-only writer
in scoring::audit_log).

StubClassifier scripts a verdict queue for tests; errors loudly on
exhaustion to keep mock-driven tests honest.

Chainlink #<Phase 6.3 issue id>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>'
```

### Task 4.4: Write failing test for `RunPodCopeBClient`

**Files:** Append to `tests/unit_classifier.rs` (one test module per file).

- [ ] **Step 1: Add tests**

Append to `tests/unit_classifier.rs`:

```rust
mod runpod {
    use charcoal::toxicity::classifier::{ToxicityClassifier};
    use charcoal::toxicity::runpod_cope_b::RunPodCopeBClient;

    #[tokio::test]
    async fn runpod_client_constructs_with_valid_env_inputs() {
        let client = RunPodCopeBClient::new(
            "https://api.runpod.ai/v2/endpoint-id".into(),
            "test-api-key".into(),
        );
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn runpod_client_rejects_empty_credentials() {
        let err1 = RunPodCopeBClient::new("".into(), "key".into()).unwrap_err();
        assert!(format!("{err1}").contains("endpoint"));
        let err2 = RunPodCopeBClient::new("https://api.runpod.ai/v2/x".into(), "".into()).unwrap_err();
        assert!(format!("{err2}").contains("api key"));
    }

    // Wire-shape test: build the JSON body the client sends and assert structure.
    // Doesn't hit the network.
    #[test]
    fn runpod_client_request_body_shape() {
        use serde_json::json;
        let body = RunPodCopeBClient::build_request_body("hello world");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v, json!({"input": {"content": "hello world"}}));
    }

    // Response parse: well-formed
    #[test]
    fn runpod_client_parses_well_formed_response() {
        let raw = r#"{"output":{"toxic":true,"confidence":0.92,"model":"cope-b-a4b","policy_version":"policy-v3"}}"#;
        let parsed = RunPodCopeBClient::parse_response(raw, 250).unwrap();
        assert!(parsed.toxic_token);
        assert!((parsed.confidence - 0.92).abs() < 1e-4);
        assert_eq!(parsed.model_id, "cope-b-a4b");
        assert_eq!(parsed.policy_version, "policy-v3");
        assert_eq!(parsed.latency_ms, 250);
    }

    // Response parse: missing required field
    #[test]
    fn runpod_client_response_missing_output_field_errors() {
        let raw = r#"{"status":"COMPLETED"}"#;
        let err = RunPodCopeBClient::parse_response(raw, 0).unwrap_err();
        assert!(format!("{err}").to_lowercase().contains("output"));
    }

    // Threshold is a const baked into the impl
    #[test]
    fn runpod_threshold_is_const_per_spec() {
        let client = RunPodCopeBClient::new(
            "https://api.runpod.ai/v2/x".into(),
            "k".into(),
        ).unwrap();
        // Value tuned in Chunk 5 — assert it's in a reasonable range now.
        let t = client.threshold();
        assert!((0.0..=1.0).contains(&t), "threshold must be in [0,1]");
    }
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo test --test unit_classifier runpod`
Expected: compile error — `runpod_cope_b` module missing.

### Task 4.5: Implement `src/toxicity/runpod_cope_b.rs`

**Files:**
- Create: `src/toxicity/runpod_cope_b.rs`
- Modify: `src/toxicity/mod.rs` (add `pub mod runpod_cope_b;`)

- [ ] **Step 1: Implement the module**

Path: `src/toxicity/runpod_cope_b.rs`

```rust
//! Self-hosted CoPE-B-A4B classifier on RunPod Serverless.
//!
//! Wire shape:
//!   POST <endpoint_url>/runsync
//!   Authorization: Bearer <api_key>
//!   {"input": {"content": "<envelope>"}}
//!   -> {"output": {"toxic": bool, "confidence": float, "model": str, "policy_version": str}}
//!
//! Retries on 5xx with bounded decorrelated jitter. 4xx surfaces immediately
//! (config / contract issue). Timeout is split between warm-up and steady-
//! state — see `RunPodCopeBClient::classify_with_timeout`.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use backon::{ExponentialBuilder, Retryable};
use serde::Deserialize;
use std::time::{Duration, Instant};
use thiserror::Error;

use super::classifier::{ClassifierVerdict, ToxicityClassifier};

/// TODO(migration-step-5): recalibrate against labeled fixtures.
/// See docs/superpowers/specs/2026-06-05-cope-b-self-hosted-design.md §"Step 5".
/// 0.5 is a placeholder — a model emitting a binary token with logprob-based
/// confidence concentrates probability sharply, so the real threshold may be
/// closer to 0.7+. Update via code change, never env (spec §"Backend selection").
pub const COPE_B_THRESHOLD: f32 = 0.5;

const INITIAL_BACKOFF_MS: u64 = 500;

#[derive(Debug, Clone)]
pub struct RunPodCopeBClient {
    client: reqwest::Client,
    endpoint_url: String,
    api_key: String,
    steady_timeout: Duration,
    warmup_timeout: Duration,
    max_retries: u32,
}

#[derive(Debug, Deserialize)]
struct RawResponseBody {
    output: RawOutput,
}

#[derive(Debug, Deserialize)]
struct RawOutput {
    toxic: bool,
    confidence: f32,
    #[serde(default = "default_model")]
    model: String,
    /// Chunk 3's handler.py always emits policy_version; we default if the
    /// field is missing (older handler, test stubs) so deserialization never
    /// hard-fails on an otherwise-valid response.
    #[serde(default = "default_policy_version")]
    policy_version: String,
}

fn default_model() -> String { "cope-b-a4b".into() }
fn default_policy_version() -> String { "policy-unknown".into() }

/// Typed retry classification so backon's `.when()` filter checks an enum
/// variant instead of grepping stringified error messages.
#[derive(Debug, Error)]
enum RunPodError {
    #[error("RunPod transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("RunPod HTTP 5xx: {0}")]
    ServerError(reqwest::StatusCode),
    #[error("RunPod HTTP {0} (non-retryable)")]
    ClientError(reqwest::StatusCode),
}

impl RunPodError {
    fn is_retryable(&self) -> bool {
        matches!(self, RunPodError::Transport(_) | RunPodError::ServerError(_))
    }
}

/// Tracks retries inside the backon closure so the metrics module can emit
/// a real `classifier_retry_count` instead of a hardcoded zero.
#[derive(Default, Clone)]
struct RetryCounter(std::sync::Arc<std::sync::atomic::AtomicU32>);

impl RetryCounter {
    fn bump(&self) {
        use std::sync::atomic::Ordering;
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn get(&self) -> u32 {
        use std::sync::atomic::Ordering;
        self.0.load(Ordering::Relaxed)
    }
}

impl RunPodCopeBClient {
    pub fn new(endpoint_url: String, api_key: String) -> Result<Self> {
        if endpoint_url.is_empty() {
            bail!("RunPod endpoint URL is required");
        }
        if api_key.is_empty() {
            bail!("RunPod api key is required");
        }
        // Read timeouts + retries from env per spec §"Environment variables";
        // fall back to spec defaults if unset.
        let steady_ms = std::env::var("CHARCOAL_CLASSIFIER_TIMEOUT_MS")
            .ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(60_000);
        let warmup_ms = std::env::var("CHARCOAL_CLASSIFIER_WARMUP_TIMEOUT_MS")
            .ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(180_000);
        let max_retries = std::env::var("CHARCOAL_CLASSIFIER_MAX_RETRIES")
            .ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(3);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(steady_ms))
            .build()
            .context("build reqwest client")?;
        Ok(Self {
            client,
            endpoint_url,
            api_key,
            steady_timeout: Duration::from_millis(steady_ms),
            warmup_timeout: Duration::from_millis(warmup_ms),
            max_retries,
        })
    }

    pub fn build_request_body(content: &str) -> String {
        serde_json::json!({ "input": { "content": content } }).to_string()
    }

    pub fn parse_response(raw: &str, latency_ms: u32) -> Result<ClassifierVerdict> {
        let parsed: RawResponseBody = serde_json::from_str(raw)
            .with_context(|| format!("parse RunPod response body: {raw}"))?;
        Ok(ClassifierVerdict {
            toxic_token: parsed.output.toxic,
            confidence: parsed.output.confidence,
            latency_ms,
            model_id: parsed.output.model,
            policy_version: parsed.output.policy_version,
        })
    }

    /// Single attempt — issued from inside the retry loop in classify_with_timeout.
    /// Returns the JSON body string on 2xx, a typed RunPodError otherwise.
    async fn attempt(
        client: &reqwest::Client,
        url: &str,
        api_key: &str,
        body: &str,
        timeout: Duration,
    ) -> std::result::Result<String, RunPodError> {
        let resp = client
            .post(url)
            .bearer_auth(api_key)
            .header("content-type", "application/json")
            .timeout(timeout)
            .body(body.to_string())
            .send()
            .await?;
        let status = resp.status();
        if status.is_success() {
            Ok(resp.text().await?)
        } else if status.is_server_error() {
            Err(RunPodError::ServerError(status))
        } else {
            Err(RunPodError::ClientError(status))
        }
    }

    async fn classify_with_timeout(
        &self,
        content: &str,
        timeout: Duration,
    ) -> Result<(ClassifierVerdict, u32)> {
        let body = Self::build_request_body(content);
        let url = format!("{}/runsync", self.endpoint_url.trim_end_matches('/'));
        let start = Instant::now();
        let retries = RetryCounter::default();

        // Owned clones moved into the closure satisfy backon's FnMut+'static
        // bound. Each retry calls the closure again; clones are cheap (reqwest::Client
        // is Arc-internal, the strings are small).
        let client = self.client.clone();
        let url_owned = url;
        let api_key = self.api_key.clone();
        let body_owned = body;
        let retries_in = retries.clone();

        let attempt = move || {
            let client = client.clone();
            let url = url_owned.clone();
            let key = api_key.clone();
            let body = body_owned.clone();
            let retries = retries_in.clone();
            async move {
                let r = Self::attempt(&client, &url, &key, &body, timeout).await;
                if r.is_err() {
                    retries.bump();
                }
                r
            }
        };

        let response = attempt
            .retry(
                ExponentialBuilder::default()
                    .with_min_delay(Duration::from_millis(INITIAL_BACKOFF_MS))
                    .with_max_times(self.max_retries as usize)
                    .with_jitter(),
            )
            .when(|e: &RunPodError| e.is_retryable())
            .await?;

        let latency_ms: u32 = start.elapsed().as_millis().try_into().unwrap_or(u32::MAX);
        // RetryCounter bumps on every failed attempt. Each failure that doesn't
        // exhaust the budget triggers exactly one retry; the final successful
        // attempt does NOT bump. So `get()` already equals retries-issued.
        let observed = retries.get();
        Ok((Self::parse_response(&response, latency_ms)?, observed))
    }
}

#[async_trait]
impl ToxicityClassifier for RunPodCopeBClient {
    async fn classify(&self, content: &str) -> Result<ClassifierVerdict> {
        let (verdict, retries) = self.classify_with_timeout(content, self.steady_timeout).await?;
        crate::observability::classifier_metrics::record_request(
            self.name(),
            verdict.latency_ms,
            verdict.toxic_token,
            retries,
        );
        Ok(verdict)
    }
    fn name(&self) -> &'static str { "runpod-cope-b" }
    fn model_id(&self) -> &'static str { "cope-b-a4b" }
    fn policy_version(&self) -> &'static str {
        // Default for trait-level callers (e.g. health-check banner). The
        // real per-call value lives on ClassifierVerdict.policy_version,
        // which carries the response field — Chunk 3's handler.py sets it
        // from the image's POLICY_VERSION build-arg.
        "policy-unknown"
    }
    fn threshold(&self) -> f32 { COPE_B_THRESHOLD }
}

/// Helper for the scan manager: invoke once at the start of a scan to absorb
/// FlashBoot cold start into the "warming up" UX message. Same retry policy,
/// longer timeout.
pub async fn warm_up(client: &RunPodCopeBClient) -> Result<()> {
    let (_, _retries) = client
        .classify_with_timeout("[Parent post]: warm-up\n\n[Reply]: warm-up", client.warmup_timeout)
        .await?;
    Ok(())
}
```

- [ ] **Step 2: Register the module**

Edit `src/toxicity/mod.rs` — add `pub mod runpod_cope_b;`.

- [ ] **Step 3: Run tests**

Run: `cargo test --test unit_classifier runpod`
Expected: all 6 runpod tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/toxicity/runpod_cope_b.rs src/toxicity/mod.rs tests/unit_classifier.rs
git commit -m 'feat(toxicity): RunPodCopeBClient implementing ToxicityClassifier

POST /runsync to the RunPod Serverless endpoint with bearer auth.
Retries on 5xx via backons ExponentialBuilder + with_jitter (decorrelated
jitter spreads concurrent retries; spec §"Retry policy"). 4xx surfaces
immediately as non-retryable. Steady-state timeout 60s; warm_up() helper
uses a 180s timeout for the scan-start ping.

COPE_B_THRESHOLD is a Rust const (recalibrated via code change in Step 5,
not env var — spec §"Backend selection and per-backend thresholds").

Chainlink #<Phase 6.3 issue id>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>'
```

### Task 4.6: Refactor `ZentropiClient` to implement `ToxicityClassifier`

The existing `ZentropiClient` has its own `classify` and `classify_pair` methods returning `ZentropiResponse`. The trait drops `classify_pair`; callers will pass a pre-formatted envelope via `crate::toxicity::format_parent_reply`. ZentropiClient's request body takes a single `content_text` field, so this works without changing the wire format.

**Important:** rename the inherent `ZentropiClient::classify` to `ZentropiClient::classify_raw` so there is exactly ONE `classify` (the trait method). Keeping both names risks subtle method-resolution surprises when receivers shift between `&ZentropiClient` (inherent wins) and `&dyn ToxicityClassifier` (trait wins) — they return different types (`ZentropiResponse` vs `ClassifierVerdict`), so the compiler will flag mismatches, but the diagnostics are confusing.

**Files:**
- Modify: `src/toxicity/zentropi.rs`
- Modify: any call site of the old `ZentropiClient::classify` (run `grep -n 'ZentropiClient::classify\b\|\.classify(' src/ tests/`)

- [ ] **Step 1: Rename the inherent method**

In `src/toxicity/zentropi.rs:108`, rename `pub async fn classify(...)` to `pub async fn classify_raw(...)`. Same for `classify_pair` → leave that named `classify_pair` since the trait doesn't have one (no conflict).

- [ ] **Step 2: Update call sites of the rename**

The grep should show: the inherent `classify` is called by `classify_pair` itself (internally) and by `src/toxicity/ensemble.rs` (which is being refactored in Task 4.7 anyway, so the rename + trait swap happen together). If `src/bin/charcoal-zentropi-check` or similar exists, update it too.

- [ ] **Step 3: Add trait impl**

Append to `src/toxicity/zentropi.rs`:

```rust
use crate::toxicity::classifier::{ClassifierVerdict, ToxicityClassifier};

/// Calibrated threshold for the Zentropi-hosted backend. CoPE-A's hosted API
/// returns a confidence Zentropi has already calibrated — preserve current
/// behavior by accepting any toxic verdict regardless of confidence.
pub const ZENTROPI_THRESHOLD: f32 = 0.0;

#[async_trait::async_trait]
impl ToxicityClassifier for ZentropiClient {
    async fn classify(&self, content: &str) -> anyhow::Result<ClassifierVerdict> {
        let start = std::time::Instant::now();
        let resp = self.classify_raw(content).await?;
        let latency_ms: u32 = start.elapsed().as_millis().try_into().unwrap_or(u32::MAX);
        let verdict = ClassifierVerdict {
            toxic_token: resp.is_toxic(),
            confidence: resp.confidence as f32,
            latency_ms,
            model_id: self.model_id().to_string(),
            policy_version: self.policy_version().to_string(),
        };
        crate::observability::classifier_metrics::record_request(
            self.name(),
            verdict.latency_ms,
            verdict.toxic_token,
            /* retries = */ 0,    // ZentropiClient uses its own retry loop; surfacing the count is a follow-up
        );
        Ok(verdict)
    }
    fn name(&self) -> &'static str { "zentropi-hosted" }
    fn model_id(&self) -> &'static str { "cope-a-9b" }   // bumped to cope-b in Chunk 6 if hosted CoPE-B lands
    fn policy_version(&self) -> &'static str {
        // The hosted labeler version is identified by ZENTROPI_LABELER_VERSION_ID
        // at construction; this static accessor is a placeholder until the
        // version ID is plumbed through (deferred to Chunk 6 alongside hosted
        // CoPE-B research).
        "zentropi-labeler"
    }
    fn threshold(&self) -> f32 { ZENTROPI_THRESHOLD }
}
```

- [ ] **Step 4: Append tests to `tests/unit_classifier.rs`**

```rust
mod zentropi_trait {
    use charcoal::toxicity::classifier::ToxicityClassifier;
    use charcoal::toxicity::zentropi::{ZentropiClient, ZENTROPI_THRESHOLD};

    #[test]
    fn zentropi_threshold_preserves_current_behavior() {
        // Spec: existing CoPE-A behavior is "label == 1 = toxic", regardless
        // of confidence value. Threshold 0.0 matches that semantically since
        // any toxic_token=true && confidence >= 0.0 is true.
        assert_eq!(ZENTROPI_THRESHOLD, 0.0);
    }

    #[test]
    fn zentropi_client_implements_trait_with_static_ids() {
        // Avoid the network: build a client with placeholder creds and verify
        // the trait's accessor methods return the documented constants.
        let client = ZentropiClient::new(
            "k".into(),
            "labeler-id".into(),
            None,
        ).unwrap();
        let dyn_ref: &dyn ToxicityClassifier = &client;
        assert_eq!(dyn_ref.name(), "zentropi-hosted");
        assert_eq!(dyn_ref.threshold(), ZENTROPI_THRESHOLD);
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test --test unit_classifier zentropi_trait`
Expected: pass.

- [ ] **Step 6: Commit**

```bash
git add src/toxicity/zentropi.rs tests/unit_classifier.rs
git commit -m 'refactor(zentropi): implement ToxicityClassifier trait

Renames the inherent classify(&self, text) to classify_raw to eliminate
method-resolution ambiguity with the trait method of the same name.
classify_pair is unchanged but no longer reachable through the trait —
callers compose envelopes via format_parent_reply.

ZENTROPI_THRESHOLD = 0.0 preserves existing CoPE-A semantics
(label == "1" => toxic, regardless of confidence). Bumped in Chunk 6
if the Zentropi-hosted CoPE-B variant lands.

Chainlink #<Phase 6.3 issue id>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>'
```

### Task 4.7: Atomic refactor — TwoStageVerdict v2 + factory wiring (single commit)

**Why one commit:** Step 1's grep shows the actual call sites:
`src/main.rs` and `src/web/scan_job.rs` both pass `Option<Arc<ZentropiClient>>`
to `TwoStageToxicityScorer::new`. The signature change (`Option<...>` →
`Arc<dyn ToxicityClassifier>`) means an intermediate commit that touches
`ensemble.rs` without `main.rs`/`scan_job.rs` won't build — the pre-push
hook rejects it. Land them together.

Downstream code (`src/scoring/profile.rs`, `src/pipeline/amplification.rs`,
report rendering) consumes the simpler `BinaryVerdict` through the
`ToxicityScorer::classify_batch_with_contexts` adapter at
`ensemble.rs:236-247`. That adapter shields callers from the field renames,
so the migration grep is short by design.

**Files (all in one commit):**
- Modify: `src/toxicity/ensemble.rs` — core refactor
- Modify: `src/toxicity/classifier.rs` — add `build_from_env` factory (was Task 4.9 Step 1)
- Modify: `src/main.rs` — swap factory in for old `Option<Arc<ZentropiClient>>` arg (was Task 4.9 Step 2)
- Modify: `src/web/scan_job.rs` — same swap
- Modify: `tests/unit_ensemble.rs`, `tests/composition.rs`, `tests/unit_scoring.rs` — field renames
- Modify: `Cargo.toml` — `serial_test = "3"` in `[dev-dependencies]` if missing

- [ ] **Step 1: Snapshot current field + call-site consumers**

Run: `grep -rn "zentropi_confidence\|ZentropiToxic\|ZentropiSafe\|OnnxFallback\|has_zentropi\|TwoStageToxicityScorer::new" src/ tests/`
Expected: hits in `src/toxicity/ensemble.rs` (struct + variants), `src/main.rs:~1154` (constructor call), `src/web/scan_job.rs:~282` (constructor call), and various tests. **NO hits in `src/scoring/profile.rs` or `src/pipeline/amplification.rs`** — that's expected; those consume `BinaryVerdict` via the trait adapter.

- [ ] **Step 2: Refactor `ensemble.rs`**

In `src/toxicity/ensemble.rs`:

- Imports — replace `use super::zentropi::ZentropiClient;` with `use super::classifier::{ClassifierVerdict, ToxicityClassifier};` (keep zentropi import only if used elsewhere in the file).
- `TwoStageVerdict`: rename `zentropi_confidence: Option<f64>` → `classifier_confidence: Option<f32>`; add `classifier_model_id: Option<String>` and `classifier_policy_version: Option<String>` fields.
- `VerdictSource`: rename `ZentropiToxic` → `ClassifierToxic`, `ZentropiSafe` → `ClassifierSafe`; **remove** `OnnxFallback` (no silent fallback in new design).
- `TwoStageToxicityScorer`:
  - Field: `zentropi: Option<Arc<ZentropiClient>>` → `classifier: Arc<dyn ToxicityClassifier>` (no Option — required).
  - Constructor: `new(primary, classifier)` (drop the `Option`; callers must construct one via the factory in Task 4.9).
  - Remove `has_zentropi()`; replace with `pub fn classifier_name(&self) -> &'static str { self.classifier.name() }` for scan-start logging.
  - In `classify_post`, replace the `match &self.zentropi { Some(c) => ..., None => ... }` block with an unconditional call to `self.classifier.classify(primary_input).await`. The `OnnxFallback` branch goes away entirely; classifier errors bubble up via `?`.
  - When constructing `TwoStageVerdict::ClassifierToxic` / `ClassifierSafe`, populate `classifier_confidence: Some(verdict.confidence)`, `classifier_model_id: Some(verdict.model_id.clone())`, `classifier_policy_version: Some(verdict.policy_version.clone())`.

Embedded reference structure (use as template; don't paste verbatim — apply diffs against the real file):

```rust
pub struct TwoStageVerdict {
    pub is_toxic: bool,
    pub onnx_score: f64,
    pub onnx_attributes: super::traits::ToxicityAttributes,
    pub source: VerdictSource,
    pub classifier_confidence: Option<f32>,
    pub classifier_model_id: Option<String>,
    pub classifier_policy_version: Option<String>,
}

pub enum VerdictSource {
    OnnxCleared,
    ClassifierToxic,
    ClassifierSafe,
}

pub struct TwoStageToxicityScorer {
    primary: Box<dyn ToxicityScorer>,
    classifier: Arc<dyn ToxicityClassifier>,
}
```

- [ ] **Step 3: Add the factory to `classifier.rs`**

Append to `src/toxicity/classifier.rs`:

```rust
use std::sync::Arc;

/// Read `CHARCOAL_CLASSIFIER` and build the configured backend. Returns
/// `Err` when the var is unset, empty, or holds an unrecognized value —
/// the binary refuses to boot in those cases (spec §"Backend selection").
pub fn build_from_env() -> Result<Arc<dyn ToxicityClassifier>> {
    let kind = std::env::var("CHARCOAL_CLASSIFIER")
        .ok()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!(
            "CHARCOAL_CLASSIFIER must be set (one of: runpod, zentropi)"
        ))?;

    match kind.as_str() {
        "runpod" => {
            let endpoint = std::env::var("RUNPOD_ENDPOINT_URL")
                .context("RUNPOD_ENDPOINT_URL must be set for CHARCOAL_CLASSIFIER=runpod")?;
            let api_key = std::env::var("RUNPOD_API_KEY")
                .context("RUNPOD_API_KEY must be set for CHARCOAL_CLASSIFIER=runpod")?;
            let client = crate::toxicity::runpod_cope_b::RunPodCopeBClient::new(endpoint, api_key)?;
            Ok(Arc::new(client))
        }
        "zentropi" => {
            let api_key = std::env::var("ZENTROPI_API_KEY")
                .context("ZENTROPI_API_KEY must be set for CHARCOAL_CLASSIFIER=zentropi")?;
            let labeler_id = std::env::var("ZENTROPI_LABELER_ID")
                .context("ZENTROPI_LABELER_ID must be set for CHARCOAL_CLASSIFIER=zentropi")?;
            let labeler_version_id = std::env::var("ZENTROPI_LABELER_VERSION_ID").ok();
            let client = crate::toxicity::zentropi::ZentropiClient::new(
                api_key, labeler_id, labeler_version_id,
            )?;
            Ok(Arc::new(client))
        }
        other => Err(anyhow::anyhow!(
            "CHARCOAL_CLASSIFIER={other:?} is not a known backend (runpod | zentropi)"
        )),
    }
}
```

- [ ] **Step 4: Wire factory into `main.rs` and `scan_job.rs`**

In `src/main.rs` and `src/web/scan_job.rs`, replace the existing
`TwoStageToxicityScorer::new(primary, Some(Arc::new(ZentropiClient::new(...)?)))` call
with `TwoStageToxicityScorer::new(primary, crate::toxicity::classifier::build_from_env()?)`.
Drop the old `ZentropiClient::new` direct construction at those sites — it's now the factory's job.

- [ ] **Step 5: Migrate test fixtures and assertions**

For each existing test file that constructs `TwoStageVerdict` or `TwoStageToxicityScorer`:
- Replace `zentropi_confidence: Some(0.9)` → `classifier_confidence: Some(0.9)` (note `f32`; cast literals if needed).
- Add the new `classifier_model_id` and `classifier_policy_version` fields to every struct-literal that constructs a `TwoStageVerdict` (`Some("stub".into())` is fine in tests).
- Replace `VerdictSource::ZentropiToxic` → `VerdictSource::ClassifierToxic` (same for Safe).
- Any test that exercised the `OnnxFallback` path is now testing dead code — convert it to assert that the failure propagates as `Err` instead.
- Any test constructing `TwoStageToxicityScorer::new(primary, None)` must pass `Arc::new(StubClassifier::with_script(...))` instead.

- [ ] **Step 6: Add factory tests**

Append to `tests/unit_classifier.rs`:

```rust
mod factory {
    use charcoal::toxicity::classifier::build_from_env;
    use serial_test::serial;

    /// All env-mutating tests in this binary share the same serial key so
    /// they coordinate across modules (e.g. with the from_env tests in
    /// tests/unit_audit_log.rs that touch CHARCOAL_AUDIT_*). Don't use
    /// bare #[serial] — it doesn't cross-coordinate by key.
    const ENV_KEY: &str = "charcoal_classifier_env";

    /// Save + restore the existing value so tests don't break a developer's
    /// shell-exported env.
    struct EnvGuard {
        prior: Option<String>,
    }
    impl EnvGuard {
        fn new() -> Self {
            Self { prior: std::env::var("CHARCOAL_CLASSIFIER").ok() }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => std::env::set_var("CHARCOAL_CLASSIFIER", v),
                None    => std::env::remove_var("CHARCOAL_CLASSIFIER"),
            }
        }
    }

    #[test]
    #[serial(charcoal_classifier_env)]
    fn build_fails_when_classifier_unset() {
        let _g = EnvGuard::new();
        std::env::remove_var("CHARCOAL_CLASSIFIER");
        let err = build_from_env().unwrap_err();
        assert!(format!("{err}").contains("CHARCOAL_CLASSIFIER"));
    }

    #[test]
    #[serial(charcoal_classifier_env)]
    fn build_fails_on_unrecognized_backend() {
        let _g = EnvGuard::new();
        std::env::set_var("CHARCOAL_CLASSIFIER", "not-a-backend");
        let err = build_from_env().unwrap_err();
        assert!(format!("{err}").contains("not a known backend"));
    }
}
```

Add `serial_test = "3"` to `[dev-dependencies]` if missing.

- [ ] **Step 7: Verify failure mode locally**

Run: `CHARCOAL_CLASSIFIER= cargo run --features web -- serve 2>&1 | head -5`
Expected: clear error mentioning `CHARCOAL_CLASSIFIER must be set`.

Run: `CHARCOAL_CLASSIFIER=bogus cargo run --features web -- serve 2>&1 | head -5`
Expected: clear error mentioning the unrecognized backend.

- [ ] **Step 8: Run full test suite**

Run: `cargo test --features web 2>&1 | tail -20`
Expected: full green. If any test was exercising silent-fallback behavior, the rewrite must assert error propagation instead.

- [ ] **Step 9: Run clippy**

Run: `cargo clippy --features web -- -D warnings`
Expected: clean.

- [ ] **Step 10: Commit (atomic)**

```bash
git add Cargo.toml src/toxicity/ensemble.rs src/toxicity/classifier.rs src/main.rs src/web/scan_job.rs tests/unit_classifier.rs tests/unit_ensemble.rs tests/unit_scoring.rs tests/composition.rs
git commit -m 'refactor(ensemble): TwoStageVerdict v2 + ToxicityClassifier wiring (atomic)

Single commit because the field/signature changes in ensemble.rs are
not buildable in isolation — main.rs and scan_job.rs call
TwoStageToxicityScorer::new and would not compile against the new
signature without the factory wiring landing in the same commit.

ensemble.rs:
  - zentropi_confidence: Option<f64> -> classifier_confidence: Option<f32>
  - new classifier_model_id + classifier_policy_version fields on
    TwoStageVerdict for per-verdict provenance (audit log)
  - VerdictSource: ZentropiToxic/Safe -> ClassifierToxic/Safe;
    OnnxFallback removed (no silent fallback per spec)
  - TwoStageToxicityScorer field zentropi: Option<Arc<ZentropiClient>>
    -> classifier: Arc<dyn ToxicityClassifier>; Option gone

classifier.rs:
  - build_from_env() reads CHARCOAL_CLASSIFIER and returns
    Arc<dyn ToxicityClassifier>; validates per-backend env vars
    (RUNPOD_ENDPOINT_URL + RUNPOD_API_KEY for runpod, ZENTROPI_API_KEY +
    ZENTROPI_LABELER_ID for zentropi). Unset / empty / unknown =>
    binary refuses to boot.

main.rs + scan_job.rs:
  - swap Option<Arc<ZentropiClient>> construction for build_from_env()?

Tests: field-rename / variant-rename / struct-arg migration across
unit_ensemble, unit_scoring, composition, plus new factory tests
in unit_classifier (env-mutating; use #[serial(charcoal_classifier_env)]
and an EnvGuard RAII to coordinate with other env-mutating tests).

Chainlink #<Phase 6.3 issue id>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>'
```

### Task 4.10: classifier metrics module + scan-start banner emission

**Files:**
- Create: `src/observability/classifier_metrics.rs`
- Modify: `src/observability/mod.rs` (add module declaration)
- Modify: `src/web/scan_job.rs` (emit `classifier_backend_selected_total` once at scan start)

Note: `record_request` is already called inline from `RunPodCopeBClient::classify`
and `ZentropiClient`'s trait impl (Tasks 4.5, 4.6), so this task only sets up the
module + the scan-start banner. Retry counts from RunPod come from its
`RetryCounter`; Zentropi's existing retry loop doesn't surface a count yet —
left as a follow-up (tracked under Phase 6.3 issue's comments).

- [ ] **Step 1: Create the metrics module**

Path: `src/observability/classifier_metrics.rs`

```rust
//! Per-classification metrics emitted via `tracing::info!`.
//!
//! Backend identity is carried in the `backend` label so the same metric
//! names cover both runpod and zentropi backends — see spec §"Monitoring"
//! for the full list. Aggregation into per-scan totals happens in
//! `src/web/scan_job.rs` (see Chunk 7 staging metrics surfacing).

use tracing::info;

pub fn record_request(backend: &str, latency_ms: u32, toxic: bool, retries: u32) {
    info!(
        metric = "classifier_request_latency_ms",
        backend = backend,
        latency_ms = latency_ms,
        toxic = toxic,
    );
    info!(
        metric = "classifier_classification_count",
        backend = backend,
        toxic = toxic,
    );
    if retries > 0 {
        info!(metric = "classifier_retry_count", backend = backend, count = retries);
    }
}

pub fn record_cold_start(backend: &str, latency_ms: u32) {
    info!(metric = "classifier_cold_start_detected", backend = backend, latency_ms = latency_ms);
}

pub fn record_backend_selected(backend: &str) {
    info!(metric = "classifier_backend_selected_total", backend = backend);
}

pub fn estimate_cost_cents(backend: &str, elapsed_ms: u32) -> u32 {
    // Per spec: RunPod A100 80GB = $2.72/hr ~= 0.0756 cents/sec ~= 7.56e-5 cents/ms.
    // Zentropi: hosted, billed per-call — return 0 here; per-call billing tracking
    // happens at a different layer.
    match backend {
        "runpod-cope-b" => ((elapsed_ms as f64) * 7.56e-5).round() as u32,
        _ => 0,
    }
}
```

- [ ] **Step 2: Register the module**

Edit `src/observability/mod.rs` — add `pub mod classifier_metrics;`.

- [ ] **Step 3: Wire scan-start banner**

In `src/web/scan_job.rs` where the scan begins (after the `scorer` is built), call:

```rust
crate::observability::classifier_metrics::record_backend_selected(scorer.classifier_name());
```

`classifier_name()` was added as the replacement for `has_zentropi()` in Task 4.7
(it returns `self.classifier.name()`).

- [ ] **Step 4: Add tests for retry-with-jitter + 4xx non-retryable path**

The spec's "Retry policy" requires exponential backoff + jitter; this is the
test that locks in the behavior. Use `wiremock` (already in dev-deps for
existing `tests/web_*` suites — verify with `grep wiremock Cargo.toml`).
If not present, add `wiremock = "0.6"` to `[dev-dependencies]`.

Append to `tests/unit_classifier.rs`:

```rust
mod retry {
    use charcoal::toxicity::classifier::ToxicityClassifier;
    use charcoal::toxicity::runpod_cope_b::RunPodCopeBClient;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn retries_on_5xx_then_succeeds() {
        let server = MockServer::start().await;
        // First two requests return 503; third returns 200.
        let ok = r#"{"output":{"toxic":false,"confidence":0.1,"model":"cope-b-a4b","policy_version":"policy-v3"}}"#;
        Mock::given(method("POST"))
            .and(path("/runsync"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/runsync"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(ok, "application/json"))
            .mount(&server)
            .await;

        let client = RunPodCopeBClient::new(server.uri(), "k".into()).unwrap();
        let dyn_ref: &dyn ToxicityClassifier = &client;
        let v = dyn_ref.classify("hello").await.unwrap();
        assert!(!v.toxic_token);
    }

    #[tokio::test]
    async fn does_not_retry_on_4xx() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/runsync"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)   // wiremock asserts the mock fired exactly once → no retry
            .mount(&server)
            .await;

        let client = RunPodCopeBClient::new(server.uri(), "k".into()).unwrap();
        let dyn_ref: &dyn ToxicityClassifier = &client;
        let err = dyn_ref.classify("hello").await.unwrap_err();
        assert!(format!("{err}").contains("401"));
    }

    #[tokio::test]
    async fn warm_up_helper_runs_against_endpoint() {
        let server = MockServer::start().await;
        let ok = r#"{"output":{"toxic":false,"confidence":0.0,"model":"cope-b-a4b","policy_version":"policy-v3"}}"#;
        Mock::given(method("POST"))
            .and(path("/runsync"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(ok, "application/json"))
            .expect(1)
            .mount(&server)
            .await;

        let client = RunPodCopeBClient::new(server.uri(), "k".into()).unwrap();
        charcoal::toxicity::runpod_cope_b::warm_up(&client).await.unwrap();
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test --features web`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add src/observability/classifier_metrics.rs src/observability/mod.rs src/web/scan_job.rs tests/unit_classifier.rs Cargo.toml
git commit -m 'feat(observability): classifier_* metric prefix with backend label

record_request / record_cold_start / record_backend_selected emit
tracing INFO lines with the metric name as a field so log scrapers can
aggregate without parsing string formats. backend label carries which
classifier impl produced the event.

estimate_cost_cents bucket per backend: RunPod uses elapsed-ms * the
$2.72/hr rate; Zentropi is hosted and returns 0 (per-call billing
tracked elsewhere).

scan_job.rs emits classifier_backend_selected_total once at scan start
so log aggregation can attribute which backend produced the scans
verdicts.

Adds tests/unit_classifier.rs#retry covering 5xx-then-success
(retries fire), 4xx (no retries), and warm_up smoke (helper hits the
endpoint with the warmup timeout). Uses wiremock.

Chainlink #<Phase 6.3 issue id>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>'
```

### Task 4.11: Full-suite green + clippy + push

- [ ] **Step 1: Full test suite**

Run: `cargo test --features web`
Expected: pass.

- [ ] **Step 2: Postgres feature test pass**

Run: `cargo test --features postgres 2>&1 | tail -10`
Expected: pass (if `DATABASE_URL` is set locally) or compile-only pass. If failing because no Postgres available locally, push and let CI verify.

- [ ] **Step 3: Clippy across feature matrix**

Run:
```
cargo clippy --features web -- -D warnings
cargo clippy --features postgres -- -D warnings
cargo clippy -- -D warnings
```
Expected: all clean.

- [ ] **Step 4: Push**

Run: `git push origin feat/cope-b-self-host`
Expected: branch updated; GH Actions runs.

- [ ] **Step 5: Close subissue and switch to Chunk 5**

```
chainlink issue close <Phase 6.3 issue id>
chainlink session work <Phase 6.4 issue id>   # A/B harness + accuracy gate
```

---
