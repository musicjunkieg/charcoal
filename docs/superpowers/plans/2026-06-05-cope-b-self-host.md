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
