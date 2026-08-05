# Batch the RunPod Classifier — Implementation Plan (#186)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Send N texts per RunPod `/runsync` request instead of one, collapsing the queue-bound warm-idle GPU waste toward the compute floor (~$6-10 → ~$1 per onboarding).

**Architecture:** The `ToxicityClassifier` trait gains an additive `classify_batch` (default loops `classify`, so Zentropi/Stub are unchanged) plus `max_batch_size`. The RunPod client overrides both: one HTTP job carries a list of contents; vLLM's existing `max_num_seqs=32` continuous batching does the on-GPU parallelism. The burst phase chunks the pending queue by `max_batch_size` and fans out over chunks. Per-item un-decodable slots fail open to a benign sentinel verdict; request-level errors keep today's cost-cap / transient / permanent semantics.

**Tech Stack:** Rust (async-trait, tokio, futures, backon, reqwest, serde, thiserror, anyhow), Python 3 (vLLM AsyncLLMEngine handler, pytest), SQLite/Postgres via the `Database` trait.

**Design spec:** `docs/superpowers/specs/2026-06-27-batch-runpod-classifier-design.md`

## Global Constraints

- **TDD, RED→GREEN.** Write the failing test first, run it to confirm it fails, then the minimal implementation, then confirm green. Never edit a test to make it pass unless the test itself is wrong.
- **No silent fallbacks.** A per-item decode error is recorded as an *explicitly labelled* benign sentinel (`model_id="decode-error"`), `warn!`-logged, and metered — never silently swallowed.
- **Positional alignment.** Batch inputs and outputs are index-aligned; a `verdicts.len() != contents.len()` mismatch is a contract violation → outer `Err`.
- **Rust:** `?` for error propagation (no `.unwrap()`), `anyhow::Result` at app level, run `cargo clippy` clean. Comments explain *why*.
- **Commits:** conventional, atomic, one per task. Stage files explicitly by name — NEVER `git add -A`/`.`/`-am`. NEVER heredocs; use single-quoted multi-line strings. End commit messages with the Co-Authored-By / Claude-Session trailers used in this repo.
- **Test commands:** Rust `cargo test --features web`; clippy `cargo clippy --features web --all-targets`. Python `python3 -m pytest gpu/cope-b-runpod/tests/ -v` (run from repo root; the suite stubs vLLM/runpod so it runs on CPU).
- **Branch:** `feat/batch-runpod-classifier` (already created off staging 442ef6c). Do NOT merge; PRs are human-reserved.
- **Chainlink:** issue #186 is active (`chainlink session work 186`). Log deciduous action+outcome per task and link to decision node 338.

---

## File Structure

| File | Change | Responsibility |
|------|--------|----------------|
| `src/toxicity/classifier.rs` | modify | Add `ItemOutcome` enum + `classify_batch` (default) + `max_batch_size` (default 1) to the trait. |
| `gpu/cope-b-runpod/handler.py` | modify | Batch-only handler: `contents: list[str]` → `{"verdicts": [...]}`, per-item isolation. |
| `gpu/cope-b-runpod/tests/test_handler.py` | modify | Rewrite tests to the batch wire shape + isolation. |
| `src/toxicity/runpod_cope_b.rs` | modify | Override `classify_batch`/`max_batch_size`; batch request body; parse verdicts array; `classify` delegates. |
| `src/observability/classifier_metrics.rs` | modify | Add `record_decode_error(backend, count)`. |
| `src/pipeline/scan_phases/burst.rs` | modify | Chunk by `max_batch_size`, fan out over chunks, positional zip, benign sentinel, `BurstOutcome::Complete { errored }`. |
| `src/pipeline/scan_phases/mod.rs` | modify | Handle `Complete { errored }` → `degraded` when `errored > 0`. |

---

## Task 1: Trait — `ItemOutcome`, `classify_batch`, `max_batch_size`

**Files:**
- Modify: `src/toxicity/classifier.rs`
- Test: `src/toxicity/classifier.rs` (`#[cfg(test)]` module — add tests inline; the file already has `StubClassifier`)

**Interfaces:**
- Consumes: existing `ClassifierVerdict`, `ToxicityClassifier`, `StubClassifier`.
- Produces:
  - `pub enum ItemOutcome { Verdict(ClassifierVerdict), Error(String) }`
  - `async fn classify_batch(&self, contents: &[String]) -> Result<Vec<ItemOutcome>>` (trait method, default impl loops `classify`, first `Err` short-circuits)
  - `fn max_batch_size(&self) -> usize` (trait method, default `1`)

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block at the bottom of `src/toxicity/classifier.rs` (create the module if absent — mirror the existing test style in the file):

```rust
#[cfg(test)]
mod batch_trait_tests {
    use super::*;

    fn verdict(toxic: bool) -> ClassifierVerdict {
        ClassifierVerdict {
            toxic_token: toxic,
            confidence: 0.9,
            latency_ms: 1,
            model_id: "stub".into(),
            policy_version: "stub".into(),
        }
    }

    #[tokio::test]
    async fn default_classify_batch_maps_each_content_in_order() {
        // A 2-verdict script → classify_batch over 2 inputs yields 2 Verdicts,
        // in input order.
        let c = StubClassifier::with_script(vec![verdict(true), verdict(false)]);
        let out = c
            .classify_batch(&["a".to_string(), "b".to_string()])
            .await
            .expect("batch ok");
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], ItemOutcome::Verdict(ref v) if v.toxic_token));
        assert!(matches!(out[1], ItemOutcome::Verdict(ref v) if !v.toxic_token));
    }

    #[tokio::test]
    async fn default_classify_batch_short_circuits_on_first_error() {
        // Only one scripted verdict; the second classify() bails (script
        // exhausted) → the whole batch surfaces an outer Err (today's
        // request-level semantics; the default impl never yields ItemOutcome::Error).
        let c = StubClassifier::with_script(vec![verdict(true)]);
        let res = c
            .classify_batch(&["a".to_string(), "b".to_string()])
            .await;
        assert!(res.is_err(), "second item exhausts the script → outer Err");
    }

    #[test]
    fn default_max_batch_size_is_one() {
        let c = StubClassifier::with_script(vec![]);
        assert_eq!(c.max_batch_size(), 1);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --features web classifier::batch_trait_tests 2>&1 | tail -20`
Expected: FAIL to compile — `cannot find type ItemOutcome`, `no method classify_batch`, `no method max_batch_size`.

- [ ] **Step 3: Add the enum and trait methods**

In `src/toxicity/classifier.rs`, add the enum just above the `ToxicityClassifier` trait:

```rust
/// One slot of a batch classification result. The request already succeeded
/// (HTTP 200, job COMPLETED); this distinguishes a decodable verdict from a
/// single un-decodable slot. Request-level failures (transport / 5xx / 4xx /
/// cost ceiling) are the outer `Result::Err`, never this enum.
#[derive(Debug, Clone)]
pub enum ItemOutcome {
    /// The slot decoded to a verdict.
    Verdict(ClassifierVerdict),
    /// The job completed but this slot's content did not decode to "0"/"1".
    /// Carries the backend's error detail for logging.
    Error(String),
}
```

Add these two methods inside the `#[async_trait] pub trait ToxicityClassifier` block (after `classify`, before `name`):

```rust
    /// Classify many texts in one backend round-trip, returning one
    /// [`ItemOutcome`] per input in the SAME order. The default implementation
    /// simply loops [`classify`]; the first request-level error short-circuits
    /// to an outer `Err` (so backends without a real batch endpoint — Zentropi,
    /// the test stub — behave exactly as they do today). Backends with native
    /// batching (RunPod) override this.
    async fn classify_batch(&self, contents: &[String]) -> Result<Vec<ItemOutcome>> {
        let mut out = Vec::with_capacity(contents.len());
        for content in contents {
            out.push(ItemOutcome::Verdict(self.classify(content).await?));
        }
        Ok(out)
    }

    /// Maximum number of texts to send per [`classify_batch`] request. Default
    /// `1` (today's one-text-per-call behaviour); RunPod overrides from
    /// `CHARCOAL_RUNPOD_BATCH_SIZE`. The burst phase chunks its queue by this.
    fn max_batch_size(&self) -> usize {
        1
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --features web classifier::batch_trait_tests 2>&1 | tail -20`
Expected: PASS (3 tests). Then `cargo test --features web 2>&1 | tail -5` — the whole suite still green (additive change).

- [ ] **Step 5: Clippy + commit**

Run: `cargo clippy --features web --all-targets 2>&1 | tail -5` → no warnings.

```bash
git add src/toxicity/classifier.rs
git commit -m 'feat(toxicity): add classify_batch + max_batch_size to the classifier trait (#186)

Additive: ItemOutcome enum (per-slot Verdict|Error), a default classify_batch
that loops classify (Zentropi/Stub stay byte-identical to today), and
max_batch_size default 1. Foundation for RunPod batching.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01BYxHfK818WVCcq5oEX3S12'
```

---

## Task 2: Handler — batch-only, per-item isolation (Python)

**Files:**
- Modify: `gpu/cope-b-runpod/handler.py`
- Test: `gpu/cope-b-runpod/tests/test_handler.py`

**Interfaces:**
- Consumes: `prompt.build_prompt`, existing `decode_verdict`, `POLICY`, `POLICY_VERSION`, `_get_engine`, `_sampling_params`.
- Produces: `handler(event)` returns `{"verdicts": [slot, ...]}` where each `slot` is either
  `{"ok": true, "toxic": bool, "confidence": float, "model": str, "policy_version": str}`
  or `{"ok": false, "error": str}`, in input order. Also a module-level
  `result_slot(out) -> dict` helper (pure, testable without an engine).

- [ ] **Step 1: Rewrite the handler tests to the batch shape**

Replace the body of `gpu/cope-b-runpod/tests/test_handler.py` FROM the first `async def test_...` (line ~91) to the end of file with the following. Keep everything above (the `sys.modules` stubs, `_async_iter`, `_mock_engine_result`, `patched_engine` fixture) unchanged.

```python
def _multi_engine(fake_engine, results):
    """Make fake_engine.generate return a fresh async-iterator per call, popping
    one prebuilt RequestOutput per call in order. Lets a batch test give each
    content its own token."""
    it = iter(results)
    fake_engine.generate = MagicMock(
        side_effect=lambda *a, **k: _async_iter(next(it))
    )


async def test_handler_batch_returns_verdicts_in_input_order(patched_engine):
    handler, fake_engine = patched_engine
    _multi_engine(
        fake_engine,
        [_mock_engine_result(token="1", logprob=-0.05),
         _mock_engine_result(token="0", logprob=-0.2)],
    )
    result = await handler.handler(
        {"id": "req-b1", "input": {"contents": ["hostile", "benign"]}}
    )
    verdicts = result["verdicts"]
    assert len(verdicts) == 2
    assert verdicts[0]["ok"] is True and verdicts[0]["toxic"] is True
    assert verdicts[1]["ok"] is True and verdicts[1]["toxic"] is False
    assert verdicts[0]["model"] == "cope-b-a4b"


async def test_handler_batch_isolates_undecodable_slot(patched_engine):
    # Middle slot emits a non-binary token → that slot is ok:false, its
    # siblings are unaffected (per-item isolation, no whole-batch failure).
    handler, fake_engine = patched_engine
    _multi_engine(
        fake_engine,
        [_mock_engine_result(token="1"),
         _mock_engine_result(token="maybe"),
         _mock_engine_result(token="0")],
    )
    result = await handler.handler(
        {"id": "req-b2", "input": {"contents": ["a", "b", "c"]}}
    )
    verdicts = result["verdicts"]
    assert len(verdicts) == 3
    assert verdicts[0]["ok"] is True
    assert verdicts[1]["ok"] is False and "error" in verdicts[1]
    assert verdicts[2]["ok"] is True and verdicts[2]["toxic"] is False


async def test_handler_batch_length_matches_input(patched_engine):
    handler, fake_engine = patched_engine
    _multi_engine(fake_engine, [_mock_engine_result(token="1") for _ in range(4)])
    result = await handler.handler(
        {"id": "req-b3", "input": {"contents": ["a", "b", "c", "d"]}}
    )
    assert len(result["verdicts"]) == 4


async def test_handler_batch_empty_contents(patched_engine):
    handler, _ = patched_engine
    result = await handler.handler({"id": "req-b4", "input": {"contents": []}})
    assert result["verdicts"] == []


async def test_handler_returns_policy_version_from_env(patched_engine, monkeypatch):
    handler, fake_engine = patched_engine
    monkeypatch.setenv("POLICY_VERSION", "policy-v3-2026-07-01")
    import importlib
    importlib.reload(handler)
    handler.build_prompt = lambda policy, content: f"<prompt>{content}</prompt>"
    _multi_engine(fake_engine, [_mock_engine_result(token="1")])
    handler._engine = fake_engine  # type: ignore[attr-defined]
    result = await handler.handler({"id": "req-b5", "input": {"contents": ["test"]}})
    assert result["verdicts"][0]["policy_version"] == "policy-v3-2026-07-01"


async def test_handler_raises_on_missing_contents(patched_engine):
    handler, _ = patched_engine
    with pytest.raises(KeyError):
        await handler.handler({"id": "req-b6", "input": {}})


def test_result_slot_ok_for_binary_token():
    # result_slot is pure: given a decoded output it returns an ok slot.
    import handler  # type: ignore
    out = _mock_engine_result(token="1", logprob=-0.05).outputs[0]
    slot = handler.result_slot(out)
    assert slot["ok"] is True and slot["toxic"] is True
    assert 0.9 < slot["confidence"] < 1.0


def test_result_slot_error_for_bad_token():
    import handler  # type: ignore
    out = _mock_engine_result(token="maybe", logprob=-1.0).outputs[0]
    slot = handler.result_slot(out)
    assert slot["ok"] is False and "error" in slot
```

Note: `test_result_slot_*` are plain (non-async) functions; the module-level
`pytestmark = pytest.mark.asyncio` only affects coroutine tests, so these run
fine as sync tests.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `python3 -m pytest gpu/cope-b-runpod/tests/test_handler.py -v 2>&1 | tail -30`
Expected: FAIL — `KeyError: 'content'` (old handler reads `content`, tests now send `contents`), and `AttributeError: module 'handler' has no attribute 'result_slot'`.

- [ ] **Step 3: Rewrite the handler for batch**

In `gpu/cope-b-runpod/handler.py`: add `import asyncio` near the top imports, add the `result_slot` helper, and replace the `async def handler(event)` function. Leave `normalize_logprob`, `decode_verdict`, `_build_engine`, `_sampling_params`, `_get_engine`, and the `__main__` block unchanged.

```python
def result_slot(out) -> dict:
    """Convert one vLLM output (`final.outputs[0]`) into a per-item result slot.

    Wraps `decode_verdict` so a single un-decodable slot (model emitted a
    non-binary token, or the logprobs map is malformed) becomes an explicit
    `{"ok": false, "error": ...}` entry instead of raising and failing the
    whole batch. A decodable slot returns the full verdict with `ok: true`.
    """
    try:
        toxic, confidence = decode_verdict(out.text, out.logprobs[0])
    except (ValueError, KeyError, IndexError, TypeError) as exc:
        return {"ok": False, "error": str(exc)}
    return {
        "ok": True,
        "toxic": toxic,
        "confidence": confidence,
        "model": "cope-b-a4b",
        "policy_version": POLICY_VERSION,
    }


async def handler(event):
    """Classify a batch of content strings.

    event = {"id": ..., "input": {"contents": ["<envelope>", ...]}}

    Returns {"verdicts": [slot, ...]} where each slot is an ok verdict or an
    ok:false error, positionally aligned with `contents`. RunPod Serverless
    wraps this in its own top-level "output" field, so the wire response is
    {"output": {"verdicts": [...]}} — what RunPodCopeBClient.classify_batch
    expects.

    Raises:
        KeyError: input missing "contents".
    """
    contents = event["input"]["contents"]  # KeyError if missing — surfaced to caller
    base_id = event.get("id") or uuid.uuid4().hex

    engine = _get_engine()
    sampling = _sampling_params()

    async def run_one(index: int, content: str) -> dict:
        prompt = build_prompt(policy=POLICY, content=content)
        # vLLM requires a unique request_id per concurrent generate() call.
        request_id = f"{base_id}-{index}"
        final = None
        async for partial in engine.generate(prompt, sampling, request_id):
            final = partial
        if final is None:
            return {"ok": False, "error": "vLLM engine produced no output"}
        return result_slot(final.outputs[0])

    # gather preserves input order in its result list, so the verdicts stay
    # positionally aligned with `contents`. vLLM's AsyncLLMEngine batches the
    # concurrent generate() calls via continuous batching (max_num_seqs=32).
    verdicts = await asyncio.gather(
        *(run_one(i, c) for i, c in enumerate(contents))
    )
    return {"verdicts": list(verdicts)}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `python3 -m pytest gpu/cope-b-runpod/tests/ -v 2>&1 | tail -30`
Expected: PASS (all handler tests + the untouched `test_prompt.py` / `test_prefix_cache.py`).

- [ ] **Step 5: Commit**

```bash
git add gpu/cope-b-runpod/handler.py gpu/cope-b-runpod/tests/test_handler.py
git commit -m 'feat(gpu): batch-only handler with per-item isolation (#186)

Handler now takes {"input":{"contents":[...]}} and returns {"verdicts":[...]}
in input order via asyncio.gather over vLLM continuous batching. A single
un-decodable slot becomes {"ok":false,"error":...} (new pure result_slot
helper) instead of failing the whole job. Tests rewritten to the batch shape.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01BYxHfK818WVCcq5oEX3S12'
```

---

## Task 3: RunPod client — batch request/parse, `classify` delegates

**Files:**
- Modify: `src/toxicity/runpod_cope_b.rs`
- Test: `src/toxicity/runpod_cope_b.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: Task 1's `ItemOutcome`; existing `ClassifierVerdict`, `ScanCostMeter`, `RunPodError`, retry/poll machinery.
- Produces:
  - `pub fn build_batch_request_body(contents: &[String]) -> String` → `{"input":{"contents":[...]}}`
  - `fn parse_batch_response(raw: &str, latency_ms: u32) -> Result<Vec<ItemOutcome>>`
  - `RunPodCopeBClient::classify_batch` override + `max_batch_size` (env `CHARCOAL_RUNPOD_BATCH_SIZE`, default 32, clamp 1..=128)
  - `classify` delegates to `classify_batch(&[content])`.

- [ ] **Step 1: Write the failing tests**

Add these tests to the existing `#[cfg(test)] mod tests` in `src/toxicity/runpod_cope_b.rs`. Also REPLACE the two JSON fixtures `completed_json_with_timing` / `completed_json_without_timing` to the batch `verdicts` shape and update the four tests that use them (shown here in full — these supersede the current single-`output` versions).

```rust
    // ── Batch wire fixtures (supersede the single-output fixtures) ──
    fn batch_json_with_timing() -> &'static str {
        r#"{
            "id": "abc-123",
            "status": "COMPLETED",
            "delayTime": 4800,
            "executionTime": 700,
            "output": { "verdicts": [
                {"ok": true, "toxic": false, "confidence": 0.1,
                 "model": "cope-b-a4b", "policy_version": "policy-v1"}
            ] }
        }"#
    }

    fn batch_json_without_timing() -> &'static str {
        r#"{
            "id": "def-456",
            "status": "COMPLETED",
            "output": { "verdicts": [
                {"ok": true, "toxic": true, "confidence": 0.9,
                 "model": "cope-b-a4b", "policy_version": "policy-v1"}
            ] }
        }"#
    }

    #[test]
    fn build_batch_body_wraps_contents_list() {
        let body =
            RunPodCopeBClient::build_batch_request_body(&["a".into(), "b".into()]);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["input"]["contents"][0], "a");
        assert_eq!(v["input"]["contents"][1], "b");
    }

    #[test]
    fn parse_batch_maps_mixed_ok_and_error_slots_in_order() {
        let raw = r#"{
            "status": "COMPLETED",
            "output": { "verdicts": [
                {"ok": true, "toxic": true, "confidence": 0.8,
                 "model": "cope-b-a4b", "policy_version": "p1"},
                {"ok": false, "error": "unexpected model token: 'maybe'"}
            ] }
        }"#;
        let out = RunPodCopeBClient::parse_batch_response(raw, 1234).unwrap();
        assert_eq!(out.len(), 2);
        match &out[0] {
            ItemOutcome::Verdict(v) => {
                assert!(v.toxic_token);
                assert!((v.confidence - 0.8).abs() < f32::EPSILON);
                assert_eq!(v.latency_ms, 1234);
            }
            _ => panic!("slot 0 should be a Verdict"),
        }
        assert!(matches!(out[1], ItemOutcome::Error(ref e) if e.contains("maybe")));
    }

    #[test]
    fn parse_batch_rejects_out_of_range_confidence_as_item_error() {
        // A NaN/out-of-[0,1] confidence must not silently skew thresholds; the
        // slot becomes an ItemOutcome::Error, not a Verdict.
        let raw = r#"{
            "status": "COMPLETED",
            "output": { "verdicts": [
                {"ok": true, "toxic": true, "confidence": 1.7,
                 "model": "cope-b-a4b", "policy_version": "p1"}
            ] }
        }"#;
        let out = RunPodCopeBClient::parse_batch_response(raw, 1).unwrap();
        assert!(matches!(out[0], ItemOutcome::Error(_)));
    }

    #[test]
    fn parse_batch_timing_fields_parse_from_envelope() {
        let env: JobEnvelope =
            serde_json::from_str(batch_json_with_timing()).expect("deserialize");
        assert_eq!(env.delay_time_ms, Some(4800));
        assert_eq!(env.execution_time_ms, Some(700));
    }

    #[test]
    fn parse_batch_single_verdict_with_timing() {
        let out = RunPodCopeBClient::parse_batch_response(batch_json_with_timing(), 5500)
            .expect("parse ok");
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], ItemOutcome::Verdict(ref v)
            if !v.toxic_token && (v.confidence - 0.1).abs() < f32::EPSILON));
    }

    #[test]
    fn parse_batch_single_verdict_without_timing() {
        let out = RunPodCopeBClient::parse_batch_response(batch_json_without_timing(), 1000)
            .expect("parse ok");
        assert!(matches!(out[0], ItemOutcome::Verdict(ref v) if v.toxic_token));
    }
```

Also update the top `use` line in the test module if needed — `ItemOutcome` comes from `super::super::classifier::ItemOutcome`; add `use crate::toxicity::classifier::ItemOutcome;` inside `mod tests`.

DELETE the now-superseded tests `test_runpod_timing_fields_parse_from_completed_envelope`, `test_parse_response_succeeds_with_timing_fields`, `test_timing_fields_absent_deserialize_to_none`, `test_parse_response_backward_compat_without_timing`, and the `completed_json_with_timing` / `completed_json_without_timing` fns (their behaviour is now covered by the `parse_batch_*` tests against the batch envelope). Keep `exhausted_server_error_maps_to_transient`, `cost_capped_surfaces_as_ceiling_exceeded`, `exhausted_client_error_stays_permanent`, `classifier_max_retries_default_is_widened`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --features web runpod_cope_b 2>&1 | tail -25`
Expected: FAIL to compile — `no function build_batch_request_body`, `no function parse_batch_response`, `JobEnvelope` field `output` type mismatch (still `Option<RawOutput>`).

- [ ] **Step 3: Change the wire types**

In `src/toxicity/runpod_cope_b.rs`, add `use super::classifier::ItemOutcome;` to the existing classifier import line (currently `use super::classifier::{ClassifierTransientError, ClassifierVerdict, ToxicityClassifier};`).

Replace the `RawOutput` struct + `JobEnvelope.output` field type with the batch shape:

```rust
/// Batch output body: the handler returns `{"verdicts": [...]}` under RunPod's
/// `output` wrapper.
#[derive(Debug, Deserialize)]
struct RawBatchOutput {
    verdicts: Vec<RawItem>,
}

/// One verdict slot. `ok:true` carries the verdict fields; `ok:false` carries
/// `error`. Fields are optional so a slot of either shape deserialises; the
/// mapping in `parse_batch_response` enforces which fields must be present.
#[derive(Debug, Deserialize)]
struct RawItem {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    toxic: Option<bool>,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default = "default_model")]
    model: String,
    #[serde(default = "default_policy_version")]
    policy_version: String,
    #[serde(default)]
    error: Option<String>,
}
```

Change `JobEnvelope`'s field from `output: Option<RawOutput>` to `output: Option<RawBatchOutput>`. Keep `default_model` / `default_policy_version`. Delete the old `RawOutput` struct.

- [ ] **Step 4: Add `build_batch_request_body` + `parse_batch_response` + batch parse**

Replace `build_request_body` with the batch version, and replace `parse_job` / `parse_response` with the batch equivalents. Add the mapping that turns a `RawBatchOutput` into `Vec<ItemOutcome>`:

```rust
    pub fn build_batch_request_body(contents: &[String]) -> String {
        serde_json::json!({ "input": { "contents": contents } }).to_string()
    }

    /// Map one completed batch envelope into per-slot outcomes. A slot whose
    /// `ok` is false, or whose `confidence` is missing/NaN/out-of-[0,1], becomes
    /// an `ItemOutcome::Error` (no silent fallback) rather than a Verdict.
    fn map_verdicts(out: RawBatchOutput, latency_ms: u32) -> Vec<ItemOutcome> {
        out.verdicts
            .into_iter()
            .map(|item| {
                if !item.ok {
                    return ItemOutcome::Error(
                        item.error.unwrap_or_else(|| "unspecified item error".into()),
                    );
                }
                let (Some(toxic), Some(confidence)) = (item.toxic, item.confidence) else {
                    return ItemOutcome::Error("ok slot missing toxic/confidence".into());
                };
                if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
                    return ItemOutcome::Error(format!(
                        "confidence out of contract (finite [0,1]): {confidence}"
                    ));
                }
                ItemOutcome::Verdict(ClassifierVerdict {
                    toxic_token: toxic,
                    confidence,
                    latency_ms,
                    model_id: item.model,
                    policy_version: item.policy_version,
                })
            })
            .collect()
    }

    /// Interpret one job-envelope response as either a terminal batch of
    /// outcomes or a pending signal (poll `/status`).
    fn parse_batch_job(raw: &str, latency_ms: u32) -> Result<BatchJobOutcome> {
        let env: JobEnvelope = serde_json::from_str(raw)
            .with_context(|| format!("parse RunPod response body: {raw}"))?;
        let status = env.status.as_deref().unwrap_or("").to_ascii_uppercase();

        if matches!(status.as_str(), "FAILED" | "CANCELLED" | "TIMED_OUT") {
            let detail = env
                .error
                .map(|e| e.to_string())
                .unwrap_or_else(|| raw.to_string());
            bail!("RunPod job {status}: {detail}");
        }

        let delay_time_ms = env.delay_time_ms;
        let execution_time_ms = env.execution_time_ms;

        if let Some(out) = env.output {
            crate::observability::classifier_metrics::record_runpod_timing(
                delay_time_ms,
                execution_time_ms,
                latency_ms,
            );
            return Ok(BatchJobOutcome::Completed(Self::map_verdicts(out, latency_ms)));
        }

        if matches!(status.as_str(), "IN_QUEUE" | "IN_PROGRESS") {
            let id = env
                .id
                .ok_or_else(|| anyhow::anyhow!("RunPod {status} response missing job id: {raw}"))?;
            return Ok(BatchJobOutcome::Pending(id));
        }
        bail!("RunPod job {status:?} returned no output: {raw}");
    }

    /// Test/entry helper: parse a terminal batch response into outcomes.
    pub fn parse_batch_response(raw: &str, latency_ms: u32) -> Result<Vec<ItemOutcome>> {
        match Self::parse_batch_job(raw, latency_ms)? {
            BatchJobOutcome::Completed(v) => Ok(v),
            BatchJobOutcome::Pending(id) => {
                bail!("RunPod job {id} not terminal in the /runsync response (still pending)")
            }
        }
    }
```

Replace the `enum JobOutcome` definition with:

```rust
/// Result of interpreting a single batch job-envelope response.
enum BatchJobOutcome {
    Completed(Vec<ItemOutcome>),
    /// Job accepted but not finished yet; carries the id to poll `/status` with.
    Pending(String),
}
```

- [ ] **Step 5: Rework `classify_with_timeout` into a batch call + update `poll_status`**

Rename `classify_with_timeout` to `classify_batch_with_timeout(&self, contents: &[String], timeout) -> Result<(Vec<ItemOutcome>, u32)>`. The body is identical to the current one EXCEPT: build the body with `Self::build_batch_request_body(contents)`, and the terminal parse uses `parse_batch_job` returning the Vec. Change the final match:

```rust
        let verdicts = match Self::parse_batch_job(&response, latency_ms)? {
            BatchJobOutcome::Completed(v) => v,
            BatchJobOutcome::Pending(id) => {
                let _g = self.meter.guard();
                self.poll_status(&id, start, timeout).await?
            }
        };
        Ok((verdicts, observed))
```

Change `poll_status`'s return type to `Result<Vec<ItemOutcome>>` and its terminal match to use `parse_batch_job` / `BatchJobOutcome`:

```rust
            match Self::parse_batch_job(&body, latency_ms)? {
                BatchJobOutcome::Completed(v) => return Ok(v),
                BatchJobOutcome::Pending(_) => continue,
            }
```

- [ ] **Step 6: Implement the trait methods (`classify_batch`, `classify`, `max_batch_size`)**

Add a batch-size env reader near the other env helpers:

```rust
/// Resolve the RunPod batch size from env. Missing/garbage → 32; clamped 1..=128.
fn runpod_batch_size(raw: Option<&str>) -> usize {
    raw.and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(32)
        .clamp(1, 128)
}
```

Replace the `#[async_trait] impl ToxicityClassifier for RunPodCopeBClient` `classify` method and add the two new methods:

```rust
    async fn classify(&self, content: &str) -> Result<ClassifierVerdict> {
        // Single classify rides the batch path (batch-only wire contract): send
        // a 1-element batch and unwrap slot 0.
        let (mut outcomes, retries) = self
            .classify_batch_with_timeout(&[content.to_string()], self.steady_timeout)
            .await?;
        let outcome = outcomes.drain(..).next().ok_or_else(|| {
            anyhow::anyhow!("RunPod returned an empty batch for a single classify")
        })?;
        match outcome {
            ItemOutcome::Verdict(verdict) => {
                crate::observability::classifier_metrics::record_request(
                    self.name(),
                    verdict.latency_ms,
                    verdict.toxic_token,
                    retries,
                );
                Ok(verdict)
            }
            ItemOutcome::Error(e) => Err(anyhow::anyhow!("RunPod decode error: {e}")),
        }
    }

    async fn classify_batch(&self, contents: &[String]) -> Result<Vec<ItemOutcome>> {
        let (outcomes, retries) = self
            .classify_batch_with_timeout(contents, self.steady_timeout)
            .await?;
        // One metric line for the request; per-slot toxic flags aren't summed
        // here (finalize aggregates verdicts). Record retries once per batch.
        crate::observability::classifier_metrics::record_request(
            self.name(),
            0,
            false,
            retries,
        );
        Ok(outcomes)
    }

    fn max_batch_size(&self) -> usize {
        runpod_batch_size(std::env::var("CHARCOAL_RUNPOD_BATCH_SIZE").ok().as_deref())
    }
```

Update `warm_up` to call the batch path:

```rust
pub async fn warm_up(client: &RunPodCopeBClient) -> Result<()> {
    let (_, _retries) = client
        .classify_batch_with_timeout(
            &["[Parent post]: warm-up\n\n[Reply]: warm-up".to_string()],
            client.warmup_timeout,
        )
        .await?;
    Ok(())
}
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test --features web runpod_cope_b 2>&1 | tail -25`
Expected: PASS. Then `cargo test --features web 2>&1 | tail -5` — whole suite green (the burst still calls `classify`, which now delegates; behaviour unchanged for it).

- [ ] **Step 8: Clippy + commit**

Run: `cargo clippy --features web --all-targets 2>&1 | tail -5` → clean. Add `#[allow(dead_code)]` to `parse_batch_response` only if clippy flags it as unused outside tests — it is a `pub` test/entry helper, so it should not be flagged.

```bash
git add src/toxicity/runpod_cope_b.rs
git commit -m 'feat(toxicity): RunPod client sends batched contents, parses verdicts array (#186)

classify_batch is now the single HTTP chokepoint: body {"input":{"contents":[...]}},
parse {"output":{"verdicts":[...]}} positionally into Vec<ItemOutcome> (per-slot
ok/error, confidence still validated finite in [0,1]). classify() delegates to a
1-element batch; max_batch_size from CHARCOAL_RUNPOD_BATCH_SIZE (default 32).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01BYxHfK818WVCcq5oEX3S12'
```

---

## Task 4: Burst + orchestrator — chunk, fan out, benign sentinel, degraded

**Files:**
- Modify: `src/observability/classifier_metrics.rs`
- Modify: `src/pipeline/scan_phases/burst.rs`
- Modify: `src/pipeline/scan_phases/mod.rs`
- Test: `tests/unit_scan_phases.rs` — existing module `burst_tests` (helpers `open_burst_db()`, `pending_row(acct, suffix)`, `ok_verdict()`, const `BURST_USER`)

**Interfaces:**
- Consumes: Task 1's `ItemOutcome` + `max_batch_size`; existing `Database`, `VerdictRow`, `is_toxic`, `CostCeilingExceeded`, `ClassifierTransientError`.
- Produces:
  - `classifier_metrics::record_decode_error(backend: &str, count: u32)`
  - `pub enum BurstOutcome { Complete { errored: usize }, CostCapped, Interrupted }`
  - `run_burst` returns `Complete { errored }` on full drain; orchestrator sets `degraded` when `errored > 0`.

- [ ] **Step 1: Add the decode-error metric**

In `src/observability/classifier_metrics.rs`, add:

```rust
/// A batch slot could not be decoded into a verdict and was recorded as a
/// benign sentinel. Emitted per errored slot so the rate is visible in logs.
pub fn record_decode_error(backend: &str, count: u32) {
    info!(
        metric = "classifier_decode_errors",
        backend = backend,
        count = count,
    );
}
```

- [ ] **Step 2: Write the failing burst tests**

Add two batch classifier doubles + three tests INSIDE the existing `mod burst_tests` in `tests/unit_scan_phases.rs`. Add `ItemOutcome` to the module's classifier `use` line and `use std::sync::atomic::{AtomicUsize, Ordering};`. Reuse the existing `open_burst_db`, `pending_row`, `ok_verdict`, and `BURST_USER` helpers.

Two doubles are needed because `buffer_unordered` does not guarantee which chunk's future runs first — so a fixed multi-call script can't be positionally trusted. `EchoBatch` returns exactly `contents.len()` verdicts (length always matches its input, ordering-safe) and counts calls; `OneShotBatch` is used ONLY in single-chunk tests where exactly one `classify_batch` call happens (ordering is a non-issue).

```rust
    // ── batch doubles (Task 4) ──────────────────────────────────────────────

    /// Returns one benign Verdict per input (length always matches the chunk),
    /// counting calls. Ordering-safe under buffer_unordered.
    struct EchoBatch {
        batch_size: usize,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ToxicityClassifier for EchoBatch {
        async fn classify(&self, _c: &str) -> Result<ClassifierVerdict> {
            anyhow::bail!("EchoBatch: use classify_batch")
        }
        async fn classify_batch(&self, contents: &[String]) -> Result<Vec<ItemOutcome>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(contents.iter().map(|_| ItemOutcome::Verdict(ok_verdict())).collect())
        }
        fn name(&self) -> &'static str { "echo-batch" }
        fn model_id(&self) -> &'static str { "echo-batch" }
        fn policy_version(&self) -> &'static str { "echo-batch" }
        fn threshold(&self) -> f32 { 0.0 }
        fn max_batch_size(&self) -> usize { self.batch_size }
    }

    /// Returns a single scripted result on its one and only call. Use ONLY when
    /// the test produces exactly one chunk (rows <= batch_size).
    struct OneShotBatch {
        result: Mutex<Option<Result<Vec<ItemOutcome>>>>,
        batch_size: usize,
    }

    #[async_trait]
    impl ToxicityClassifier for OneShotBatch {
        async fn classify(&self, _c: &str) -> Result<ClassifierVerdict> {
            anyhow::bail!("OneShotBatch: use classify_batch")
        }
        async fn classify_batch(&self, _contents: &[String]) -> Result<Vec<ItemOutcome>> {
            self.result
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Err(anyhow::anyhow!("OneShotBatch called twice")))
        }
        fn name(&self) -> &'static str { "oneshot-batch" }
        fn model_id(&self) -> &'static str { "oneshot-batch" }
        fn policy_version(&self) -> &'static str { "oneshot-batch" }
        fn threshold(&self) -> f32 { 0.0 }
        fn max_batch_size(&self) -> usize { self.batch_size }
    }

    // ── Task 4 tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn burst_per_item_error_records_benign_sentinel_and_counts_errored() {
        // Two rows, batch_size 32 → one chunk, one classify_batch call. Slot 0 a
        // real verdict, slot 1 an un-decodable error → benign sentinel
        // (model="decode-error") + Complete{errored:1}. Both rows end 'done'.
        let db = open_burst_db().await;
        let acct = "did:plc:burstbatcherr";
        let rows: Vec<QueueRow> = (0..2).map(|i| pending_row(acct, &i.to_string())).collect();
        db.enqueue_classifications(BURST_USER, &rows).await.unwrap();

        let classifier: Arc<dyn ToxicityClassifier> = Arc::new(OneShotBatch {
            result: Mutex::new(Some(Ok(vec![
                ItemOutcome::Verdict(ok_verdict()),
                ItemOutcome::Error("unexpected model token: 'maybe'".into()),
            ]))),
            batch_size: 32,
        });

        let outcome = run_burst(&db, BURST_USER, &classifier, 4, 100).await.unwrap();
        assert_eq!(outcome, BurstOutcome::Complete { errored: 1 });

        assert_eq!(db.count_pending_classifications(BURST_USER).await.unwrap(), 0);
        let verdicts = db.fetch_account_verdicts(BURST_USER, acct).await.unwrap();
        assert_eq!(verdicts.len(), 2);
        assert!(verdicts.iter().all(|v| v.status == "done"));
        assert!(
            verdicts.iter().any(|v| v.model_id.as_deref() == Some("decode-error")),
            "the errored slot should be recorded as a decode-error sentinel"
        );
    }

    #[tokio::test]
    async fn burst_chunks_by_max_batch_size() {
        // 3 rows, batch_size 2 → chunks of [2, 1] → exactly 2 classify_batch
        // calls. EchoBatch's per-input length is always correct, so ordering
        // under buffer_unordered doesn't matter.
        let db = open_burst_db().await;
        let acct = "did:plc:burstchunks";
        let rows: Vec<QueueRow> = (0..3).map(|i| pending_row(acct, &i.to_string())).collect();
        db.enqueue_classifications(BURST_USER, &rows).await.unwrap();

        let echo = Arc::new(EchoBatch { batch_size: 2, calls: AtomicUsize::new(0) });
        let classifier: Arc<dyn ToxicityClassifier> = echo.clone();

        let outcome = run_burst(&db, BURST_USER, &classifier, 4, 100).await.unwrap();
        assert_eq!(outcome, BurstOutcome::Complete { errored: 0 });
        assert_eq!(echo.calls.load(Ordering::SeqCst), 2, "3 rows / batch 2 = 2 chunks");
        assert_eq!(db.count_pending_classifications(BURST_USER).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn burst_request_level_cost_cap_still_returns_costcapped() {
        // One chunk; the batch request returns a request-level CostCeilingExceeded
        // → BurstOutcome::CostCapped (unchanged semantics), rows stay pending.
        let db = open_burst_db().await;
        let acct = "did:plc:burstcap";
        let rows: Vec<QueueRow> = (0..2).map(|i| pending_row(acct, &i.to_string())).collect();
        db.enqueue_classifications(BURST_USER, &rows).await.unwrap();

        let classifier: Arc<dyn ToxicityClassifier> = Arc::new(OneShotBatch {
            result: Mutex::new(Some(Err(CostCeilingExceeded {
                est_cents: 600,
                ceiling_cents: 500,
            }
            .into()))),
            batch_size: 32,
        });

        let outcome = run_burst(&db, BURST_USER, &classifier, 4, 100).await.unwrap();
        assert_eq!(outcome, BurstOutcome::CostCapped);
        assert_eq!(db.count_pending_classifications(BURST_USER).await.unwrap(), 2);
    }
```

`BurstOutcome` must `#[derive(PartialEq, Eq)]` for `assert_eq!` — it already derives `Debug, Clone, Copy, PartialEq, Eq`, and a `{ errored: usize }` variant keeps all of those. Confirm the derive still compiles after Step 4.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --features web burst_tests 2>&1 | tail -25`
Expected: FAIL to compile — `BurstOutcome::Complete` is a unit variant (no `{ errored }`), and the pattern `Complete { errored: 1 }` doesn't match; also `no method classify_batch`/`max_batch_size` overridable is fine (they exist from Task 1).

- [ ] **Step 4: Change `BurstOutcome` + the run_burst loop**

In `src/pipeline/scan_phases/burst.rs`, change the enum variant:

```rust
pub enum BurstOutcome {
    /// All pending rows were classified. `errored` counts slots that failed to
    /// decode and were recorded as benign sentinels (scan is degraded if > 0).
    Complete { errored: usize },
    CostCapped,
    Interrupted,
}
```

Add `use crate::toxicity::classifier::{is_toxic, ClassifierTransientError, ItemOutcome, ToxicityClassifier};` (add `ItemOutcome`). Replace the `loop { … }` body of `run_burst` with the chunked version:

```rust
    let batch_size = classifier.max_batch_size().max(1);
    let mut total_errored: usize = 0;

    loop {
        let pending = db
            .fetch_pending_classifications(user_did, burst_batch)
            .await?;
        if pending.is_empty() {
            return Ok(BurstOutcome::Complete { errored: total_errored });
        }

        // Build (account_did, post_uri, envelope) per row, then chunk by
        // max_batch_size. Each chunk becomes one classify_batch request.
        let items: Vec<(String, String, String)> = pending
            .into_iter()
            .map(|row| {
                let input = match &row.context_text {
                    Some(ctx) => crate::toxicity::format_parent_reply(ctx, &row.text),
                    None => row.text.clone(),
                };
                (row.account_did, row.post_uri, input)
            })
            .collect();
        let chunks: Vec<Vec<(String, String, String)>> =
            items.chunks(batch_size).map(|c| c.to_vec()).collect();

        let mut stream = futures::stream::iter(chunks)
            .map(|chunk| {
                let classifier = classifier.clone();
                async move {
                    let contents: Vec<String> =
                        chunk.iter().map(|(_, _, input)| input.clone()).collect();
                    let result = classifier.classify_batch(&contents).await;
                    (chunk, result)
                }
            })
            .buffer_unordered(burst_concurrency);

        let mut verdicts: Vec<VerdictRow> = Vec::new();
        let mut cost_capped = false;
        let mut interrupted = false;
        let mut other_error: Option<anyhow::Error> = None;

        while let Some((chunk, result)) = stream.next().await {
            match result {
                Ok(outcomes) => {
                    // Once a stop condition fired, drain remaining chunks without
                    // accumulating so the next batch never starts.
                    if cost_capped || interrupted {
                        continue;
                    }
                    if outcomes.len() != chunk.len() {
                        // Contract violation: positional alignment broken. Abort
                        // (permanent) after persisting prior successes.
                        if other_error.is_none() {
                            other_error = Some(anyhow::anyhow!(
                                "RunPod batch length mismatch: {} verdicts for {} inputs",
                                outcomes.len(),
                                chunk.len()
                            ));
                        }
                        continue;
                    }
                    for ((account_did, post_uri, _input), outcome) in
                        chunk.into_iter().zip(outcomes)
                    {
                        match outcome {
                            ItemOutcome::Verdict(v) => verdicts.push(VerdictRow {
                                account_did,
                                post_uri,
                                toxic_token: is_toxic(classifier.as_ref(), &v),
                                confidence: v.confidence,
                                model_id: v.model_id,
                                policy_version: v.policy_version,
                            }),
                            ItemOutcome::Error(detail) => {
                                // Fail open to benign: an un-decodable post can
                                // never inflate a false threat. Explicitly labelled
                                // + logged + metered — not a silent fallback.
                                warn!(
                                    account_did = %account_did,
                                    post_uri = %post_uri,
                                    error = %detail,
                                    "classifier decode error — recording benign sentinel, scan degraded"
                                );
                                crate::observability::classifier_metrics::record_decode_error(
                                    classifier.name(),
                                    1,
                                );
                                total_errored += 1;
                                verdicts.push(VerdictRow {
                                    account_did,
                                    post_uri,
                                    toxic_token: false,
                                    confidence: 0.0,
                                    model_id: "decode-error".to_string(),
                                    policy_version: classifier.policy_version().to_string(),
                                });
                            }
                        }
                    }
                }
                Err(err) => {
                    if err.downcast_ref::<CostCeilingExceeded>().is_some() {
                        cost_capped = true;
                    } else if err.downcast_ref::<ClassifierTransientError>().is_some() {
                        warn!(
                            error = %err,
                            "classifier transient failure — interrupting burst, scan resumable"
                        );
                        interrupted = true;
                    } else if other_error.is_none() {
                        other_error = Some(err);
                    }
                }
            }
        }

        if !verdicts.is_empty() {
            db.record_classification_verdicts(user_did, &verdicts)
                .await?;
        }

        if cost_capped {
            return Ok(BurstOutcome::CostCapped);
        }
        if let Some(err) = other_error {
            return Err(err);
        }
        if interrupted {
            return Ok(BurstOutcome::Interrupted);
        }
        // All chunks in this batch succeeded — continue to the next batch.
    }
```

Keep the defensive clamps at the top of `run_burst` (`burst_concurrency.clamp(1,64)`, `burst_batch.clamp(1,10_000)`).

- [ ] **Step 5: Update the orchestrator**

In `src/pipeline/scan_phases/mod.rs`, change the `BurstOutcome::Complete` match arm (currently `BurstOutcome::Complete => { … }`) to bind and act on `errored`:

```rust
            BurstOutcome::Complete { errored } => {
                if errored > 0 {
                    // Some posts failed to decode and were recorded as benign
                    // sentinels — the scan is incomplete/degraded.
                    summary.degraded = true;
                }
                info!(
                    phase = "burst",
                    outcome = "complete",
                    errored = errored,
                    "burst phase complete"
                );
                db.set_scan_state(user_did, "scan_phase", ScanPhase::Finalize.as_str())
                    .await?;
            }
```

`recover_account_inner` uses `matches!(run_burst(...).await?, BurstOutcome::CostCapped)` — a data-carrying `Complete` variant does not affect that pattern, so no change is needed there. Confirm it still compiles.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --features web burst_tests 2>&1 | tail -25`
Expected: PASS (existing burst tests + the 3 new ones). Existing tests match `matches!(outcome, BurstOutcome::Complete)` (unit variant) which no longer compiles — update every occurrence to the struct form. Find them:

Run: `grep -rn 'BurstOutcome::Complete\b' tests/ src/ | grep -v '{ errored'`

Change `matches!(outcome, BurstOutcome::Complete)` → `matches!(outcome, BurstOutcome::Complete { .. })`, and any `assert_eq!(outcome, BurstOutcome::Complete)` → `assert_eq!(outcome, BurstOutcome::Complete { errored: 0 })` (these existing drains have no decode errors). These are test-only pattern updates, not behaviour changes. Then the full suite:
Run: `cargo test --features web 2>&1 | tail -8`
Expected: all green.

- [ ] **Step 7: Clippy + commit**

Run: `cargo clippy --features web --all-targets 2>&1 | tail -5` → clean. Also run the no-features + postgres clippy to match repo CI gates:
`cargo clippy --all-targets 2>&1 | tail -3` and `cargo clippy --features postgres --all-targets 2>&1 | tail -3`.

```bash
git add src/observability/classifier_metrics.rs src/pipeline/scan_phases/burst.rs src/pipeline/scan_phases/mod.rs
git commit -m 'feat(burst): batch the classifier queue, fail-open on decode errors (#186)

run_burst chunks pending rows by classifier.max_batch_size() and fans out over
chunks (buffer_unordered = concurrent jobs). Positional zip back to
(account_did, post_uri); an un-decodable slot is recorded as a benign
decode-error sentinel + warn + metric. BurstOutcome::Complete{errored} flips the
scan degraded flag when > 0. Request-level cost-cap/transient/permanent
semantics unchanged.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01BYxHfK818WVCcq5oEX3S12'
```

---

## Task 5: Config default + docs touch-up

**Files:**
- Modify: `CLAUDE.md` (env var reference) OR the deployment env doc if one exists — add `CHARCOAL_RUNPOD_BATCH_SIZE` (default 32) and note in-flight texts = `burst_concurrency × batch_size`.

**Interfaces:** none (docs only).

- [ ] **Step 1: Document the new env var**

Add a one-line entry wherever the other `CHARCOAL_*` classifier/burst env vars are documented (grep for `CHARCOAL_BURST_CONCURRENCY`):

```
CHARCOAL_RUNPOD_BATCH_SIZE — texts per RunPod /runsync request (default 32 = handler max_num_seqs, clamp 1..=128). In-flight texts ≈ CHARCOAL_BURST_CONCURRENCY × this.
```

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m 'docs: document CHARCOAL_RUNPOD_BATCH_SIZE (#186)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01BYxHfK818WVCcq5oEX3S12'
```

---

## Post-implementation

- [ ] Full gate: `cargo test --features web` green; clippy clean on web / default / postgres; `python3 -m pytest gpu/cope-b-runpod/tests/ -v` green.
- [ ] Push `feat/batch-runpod-classifier`; open PR → `staging`. Body notes the **rollout ordering**: the RunPod endpoint image must be rebuilt/updated (batch handler) **before** the app revision that sends batches (client always sends `contents`; an old handler reading `content` would KeyError). CI builds the image via `.github/workflows/build-cope-b-image.yml`.
- [ ] After staging deploy: re-run an onboarding scan and confirm via the #185 cost meter that RunPod request count drops ~Nx and $/onboarding falls toward ~$1. Tune `CHARCOAL_RUNPOD_BATCH_SIZE` / `CHARCOAL_BURST_CONCURRENCY` from the observed `classifier_runpod_timing` delay-vs-exec split.
- [ ] Fold into draft PR #63 (Phase 6 → main) when promoting.

## Self-Review notes (spec coverage)

- Batch-only wire contract → Task 2 (handler) + Task 3 (client body/parse). ✅
- Per-item isolation → Task 2 (`result_slot` try/except) + Task 3 (`map_verdicts`) + Task 4 (benign sentinel). ✅
- Positional alignment + length check → Task 3 (order-preserving map) + Task 4 (`outcomes.len() != chunk.len()` → abort). ✅
- Fail-open-to-benign, no schema change → Task 4 (sentinel via existing `record_classification_verdicts`). ✅
- Trait additive; Zentropi/Stub unchanged → Task 1 (defaults, `max_batch_size=1`). ✅
- `BurstOutcome::Complete { errored }` → degraded → Task 4 (burst) + orchestrator. ✅
- Cost meter one guard per batch request → inherited from `classify_batch_with_timeout` (the renamed chokepoint keeps the existing per-attempt guard). ✅
- `CHARCOAL_RUNPOD_BATCH_SIZE` default 32 → Task 3 (`runpod_batch_size`) + Task 5 (docs). ✅
- Decode-error metric → Task 4 (`record_decode_error`). ✅
