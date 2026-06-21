# Self-hosted CoPE-B-A4B classifier on RunPod Serverless

**Status:** Draft  
**Date:** 2026-06-05  
**Author:** Bryan Guffey (architect) + Claude (implementation)  
**Tracking issue:** chainlink #185 (Phase 6 epic)  
**Implementation branch:** `feat/cope-b-self-host`

## Context and motivation

Charcoal's contextual scoring currently relies on Zentropi's hosted API running the
CoPE-A model. Two problems are coming to a head:

1. **Zentropi-side reliability gaps.** The 2026-06-05 grimalkina scan (chainlink #182)
   produced 45 HTTP 403 Forbidden responses with HTML bodies — likely a CDN-level
   block, not a service-layer issue. The 2026-05-17 chaosgreml.in scan saw
   sustained Zentropi 429 rate-limit storms. Charcoal falls back to ONNX threshold
   on each failure, silently degrading scoring quality on affected posts.
2. **Onboarding UX cost.** A typical onboarding scan runs ~112k post
   classifications. Sustained throughput on the hosted API is unpredictable and
   not under our control.

Zentropi has released **CoPE-B-A4B** on HuggingFace under Apache 2.0
([`zentropi-ai/cope-b-a4b`](https://huggingface.co/zentropi-ai/cope-b-a4b)). Self-hosting
the new model on GPU infrastructure gives us:

- Deterministic throughput (we own the GPU's queue depth)
- Predictable per-scan cost
- An upgrade path (CoPE-B reportedly concentrates more probability mass than CoPE-A)
- A swappable adapter architecture that future-proofs the scoring backend

## Goals

- Self-host CoPE-B-A4B on RunPod Serverless A100 80GB
- Build a Rust adapter trait so backends are runtime-swappable
- Hit **5000 classifications in < 10 min** per onboarding scan
- Stay under **$1 per onboarding** for GPU compute
- Scale-to-zero between scans; minimize cold-start UX impact
- Migrate from CoPE-A to CoPE-B safely with side-by-side characterization

## Non-goals

- Multi-region deployment (single region is sufficient for current scale)
- Custom fine-tuning of CoPE-B (out of scope; we use it off-the-shelf)
- Dropping the ONNX clean-pass filter (Stage 1 stays; CoPE-B replaces only Stage 2)
- Replacing the NLI cross-encoder (separate model with different role)

## Architecture

```
Charcoal (Rust, on Railway)
  ├── src/toxicity/
  │     ├── ensemble.rs           ← unchanged; two-stage pipeline still gates on ONNX
  │     ├── onnx.rs               ← unchanged; clean-pass filter
  │     ├── classifier.rs         ← NEW: ToxicityClassifier trait + factory
  │     ├── runpod_cope_b.rs      ← NEW: HTTP client for RunPod /runsync
  │     └── zentropi.rs           ← refactored to implement the trait; pointed at CoPE-B
  │
  └── (calls out via reqwest to GPU service)

RunPod Serverless (gpu/cope-b-runpod/, NEW directory in repo)
  ├── Dockerfile                  ← vLLM ≥ 0.20.2 on cuda:12.4, weights baked in
  ├── handler.py                  ← RunPod worker entry; wraps vLLM AsyncEngine
  ├── prompt.py                   ← assembles Gemma chat template + POLICY/CONTENT body
  ├── policy.txt                  ← Charcoal's toxicity policy text (versioned in git)
  └── runpod.yml                  ← endpoint config
```

### The classifier trait and how it composes with the existing ensemble

The trait represents *only the Stage-2 classifier outcome* — not the full
two-stage verdict. The existing `TwoStageVerdict` (in `src/toxicity/ensemble.rs`)
stays as the type the scoring pipeline consumes; it now composes ONNX output +
classifier output rather than ONNX + Zentropi specifically.

```rust
// NEW: src/toxicity/classifier.rs
#[async_trait]
pub trait ToxicityClassifier: Send + Sync {
    /// Classify a single text. For replies-with-parent, callers compose the
    /// envelope via `format_parent_reply(parent, reply)` and pass it as `content`
    /// — there is no `classify_pair` shortcut. This keeps prompt assembly
    /// uniform across backends (RunPod bakes the envelope into the Gemma
    /// CONTENT slot; Zentropi-hosted sees the same envelope text).
    async fn classify(&self, content: &str) -> Result<ClassifierVerdict>;
    /// Stable identifier for logging / startup banner.
    fn name(&self) -> &'static str;
    /// Model identity for audit JSONL — e.g. "cope-b-a4b" or "cope-a-9b".
    fn model_id(&self) -> &'static str;
    /// Policy version applied at request time (e.g. "policy-v3-2026-07-01").
    /// Self-hosted backends read from a constant baked at image build time;
    /// Zentropi-hosted returns the configured labeler version ID.
    fn policy_version(&self) -> &'static str;
    /// Per-backend confidence threshold. Set as a const on each impl, calibrated
    /// in migration Step 5 for the model the impl wraps. **Sole source of
    /// truth for thresholds** — callers must not override.
    fn threshold(&self) -> f32;
}

#[derive(Debug, Clone)]
pub struct ClassifierVerdict {
    /// Raw binary output: did the model emit "1" (or its hosted-API equivalent)?
    pub toxic_token: bool,
    /// Normalized confidence in [0.0, 1.0] — logprob of emitted token for
    /// self-hosted, hosted-API confidence for Zentropi.
    pub confidence: f32,
    /// Wall-clock latency for audit / metrics.
    pub latency_ms: u32,
    /// Model identity (mirrors the trait's `model_id()`) — captured per-call so
    /// audit events carry it without lifetime juggling.
    pub model_id: String,
    /// Policy version applied to this call. See `ToxicityClassifier::policy_version`.
    pub policy_version: String,
}

// Width and allocation notes:
// - All classifier-side scalars (`confidence`, `threshold()`) use `f32`. ONNX
//   continuous score remains `f64`. The `TwoStageVerdict.classifier_confidence`
//   field is `Option<f32>` to match.
// - `model_id: String` and `policy_version: String` allocate per call. Sources
//   are `&'static str` on the trait, so a `String::from` (or `Cow<'static, str>`)
//   widening is needed. We accept the allocation: ~2 small strings per
//   classification × ~hundreds-of-thousands per onboarding is negligible cost
//   compared to GPU inference, and the alternative (lifetimes on
//   `ClassifierVerdict`) makes downstream audit & serde awkward.

/// Helper used by `TwoStageToxicityScorer` — never called with an externally
/// supplied threshold. Threshold ownership stays with the implementation.
pub fn is_toxic(classifier: &dyn ToxicityClassifier, v: &ClassifierVerdict) -> bool {
    v.toxic_token && v.confidence >= classifier.threshold()
}
```

Refactor of `TwoStageVerdict` and `VerdictSource`:

```rust
// src/toxicity/ensemble.rs — modified
pub struct TwoStageVerdict {
    pub is_toxic: bool,
    pub onnx_score: f64,                              // unchanged width — ONNX is `f64`
    pub onnx_attributes: super::traits::ToxicityAttributes,
    pub source: VerdictSource,
    pub classifier_confidence: Option<f32>,           // was `zentropi_confidence: Option<f64>`
    pub classifier_model_id: Option<String>,          // NEW: which model fired
    pub classifier_policy_version: Option<String>,    // NEW: which policy fired
}

pub enum VerdictSource {
    OnnxCleared,                    // unchanged
    ClassifierToxic,                // renamed from ZentropiToxic
    ClassifierSafe,                 // renamed from ZentropiSafe
    // OnnxFallback REMOVED — no silent fallback in new design
}

pub struct TwoStageToxicityScorer {
    primary: Box<dyn ToxicityScorer>,                // unchanged (ONNX)
    classifier: Arc<dyn ToxicityClassifier>,         // was Option<Arc<ZentropiClient>>; now required
    // NOTE: no `classifier_threshold` field. The threshold is owned by the
    // classifier implementation and accessed via `classifier.threshold()`.
    // This prevents the threshold from drifting away from the model it was
    // calibrated for during fallback or in tests.
}
```

`zentropi_confidence: Option<f64>` field rename (now `classifier_confidence: Option<f32>`)
+ `OnnxFallback` variant removal + `classify_pair` trait method removal are
breaking changes to call sites in `src/scoring/profile.rs`, audit logging, and
ensemble doc comments (e.g. `src/toxicity/ensemble.rs:99` references
`Zentropi's classify_pair`). Implementation step 3 must migrate all of:

- `TwoStageVerdict` field consumers in scoring + reporting
- `VerdictSource` match arms (replace `ZentropiToxic`/`ZentropiSafe`/`OnnxFallback`)
- Doc comments referencing `classify_pair`
- Tests in `tests/unit_scoring.rs` and `tests/composition.rs`

Step 3 also includes a `cargo grep` pass for `classify_pair` / `ZentropiToxic`
/ `OnnxFallback` to scrub stale identifiers.

### Backend selection and per-backend thresholds

Selection at startup via `CHARCOAL_CLASSIFIER`:

- `runpod` → `RunPodCopeBClient` with `const COPE_B_THRESHOLD: f32` (Rust source; tuned in Step 5)
- `zentropi` → `ZentropiClient` with its own `const` threshold (Rust source; existing value carried forward unless Zentropi-hosted CoPE-B requires recalibration)
- unset or unreachable → app refuses to boot (`anyhow::bail!`)

**Critical:** each backend carries its own threshold constant. In the fallback
gap scenario (Zentropi-hosted CoPE-B not available at cutover, see Step 6),
Zentropi's threshold remains tuned for CoPE-A; RunPod's threshold is tuned for
CoPE-B. No cross-contamination of calibrations.

No ONNX-only degraded mode. ONNX stays the clean-pass filter at Stage 1; the
LLM classifier is required at Stage 2. Tests use a `StubClassifier`.

### Failure modes

- GPU service 5xx → retry with **exponential backoff + decorrelated jitter** up to
  N attempts (default 3), then surface the scoring failure to the user. Jitter
  prevents thundering-herd on the upstream when many concurrent classifications
  hit the same transient failure window. **No silent fallback to ONNX threshold.**
- GPU service 4xx → log and fail the scoring job (indicates a bug or misconfig).
- Startup with no reachable backend → boot fails loudly.
- Cost ceiling exceeded mid-scan → abort, log loudly, surface to user.

### Cold-start UX and timeout strategy

On the first request after idle, the scan manager sets
`scan_state.status = "warming_classifier"` so the SvelteKit UI shows a
"warming up classifier (~30s)" message rather than a stalled spinner.

**Two-tier timeout:**
- `CHARCOAL_CLASSIFIER_TIMEOUT_MS=60000` is the *steady-state* request timeout,
  applied to second-and-subsequent requests in a scan
- `CHARCOAL_CLASSIFIER_WARMUP_TIMEOUT_MS=180000` (3 min) is applied only to
  the first request after idle (i.e., a likely cold start). This accommodates
  the worst-case FlashBoot cold path (30–60s) plus the request itself, plus
  some safety margin for image-not-cached scenarios.

The scan manager detects "first request after idle" by tracking
`last_classifier_call_at` per backend; if more than `idle_timeout` seconds have
passed, the next call uses the warmup timeout.

**Optional warm-up ping (Step 7 onwards):**
The scan job can fire a synchronous `classifier-check` ping at the very start
of a scan (before any user-facing scoring) to absorb cold start into the
"warming up" UX message. If the ping succeeds quickly, scoring begins
immediately on a warm worker; if the ping reveals a deep cold start, the user
sees the wait time accounted for upfront rather than mid-scan.

**Retry interaction:** `CHARCOAL_CLASSIFIER_MAX_RETRIES=3` only fires after a
timeout *expires*. With the two-tier timeout, a cold-start case won't burn
through retries on the first cold call.

**Image-not-cached case** (first-ever request after image push): can take 2–5
minutes. This is operationally exceptional and not user-facing — it happens
during deploy, not during normal onboarding. If it occurs during onboarding
(e.g., a deploy mid-day), the scan fails with a clear "GPU service initializing
after recent deploy, please retry in 5 min" error.

## GPU service (RunPod side)

### Container image

```dockerfile
FROM vllm/vllm-openai:v0.20.2     # cuda 12.4 base
COPY policy.txt /app/policy.txt
COPY handler.py prompt.py /app/
RUN huggingface-cli download zentropi-ai/cope-b-a4b \
    --local-dir /weights --revision main
ENV MODEL_PATH=/weights POLICY_PATH=/app/policy.txt
CMD ["python", "-u", "handler.py"]
```

Final image ~55–60 GB. Weights baked into the image to eliminate HuggingFace
download on cold boot. Alternative — network volume — is left for later if
weight iteration speed becomes a bottleneck.

### Handler sketch

```python
import os, runpod
from vllm import AsyncLLMEngine, AsyncEngineArgs, SamplingParams
from prompt import build_prompt

engine = AsyncLLMEngine.from_engine_args(AsyncEngineArgs(
    model=os.environ["MODEL_PATH"],
    dtype="bfloat16",
    max_model_len=4096,            # 256K is wasteful for our 300-token inputs
    max_num_seqs=32,               # tune empirically
    enable_prefix_caching=True,    # policy text identical per call → big win
))
POLICY = open(os.environ["POLICY_PATH"]).read()
SAMPLING = SamplingParams(max_tokens=1, temperature=0, logprobs=2)

async def handler(event):
    inp = event["input"]
    prompt = build_prompt(POLICY, inp["content"])
    result = await engine.generate(prompt, SAMPLING, request_id=event["id"])
    token = result.outputs[0].text.strip()
    logprob_map = result.outputs[0].logprobs[0]
    confidence = normalize_logprob(logprob_map, token)
    return {"toxic": token == "1", "confidence": confidence, "model": "cope-b-a4b"}

runpod.serverless.start({"handler": handler})
```

### Wire format

```json
// Request: POST /runsync
{ "input": { "content": "[Parent post] ... [Reply] ..." } }

// Response
{ "output": { "toxic": true, "confidence": 0.94, "model": "cope-b-a4b" } }
```

### RunPod endpoint config

```yaml
name: charcoal-cope-b
gpu: NVIDIA A100 80GB PCIe
flashboot: true
scale_to_zero: true
idle_timeout: 60          # tune down to 5–10s after measuring warm-restore rate
min_workers: 0
max_workers: 3            # absorbs concurrent onboardings; cheap with scale-to-zero
execution_timeout: 600    # 10 min hard cap per request
```

### Image and endpoint lifecycle

The GPU image and the RunPod endpoint are infrastructure artifacts that need
explicit operational discipline; `policy.txt` is versioned in git but only
takes effect after image rebuild and endpoint redeploy.

**Image build:**
- Trigger: GitHub Actions workflow on push to `main` or `staging` when files
  under `gpu/cope-b-runpod/**` change
- Registry: GitHub Container Registry (`ghcr.io/musicjunkieg/charcoal-cope-b`)
- Container digest pinned per release (not just the `v0.20.2` tag) so we have
  a known-good rollback point if vLLM minor versions break MoE behavior
- Cache layer for the ~50 GB weight download keyed on model revision hash, so
  rebuilds for `policy.txt`-only changes complete in ~5 min instead of ~45 min

**Endpoint creation (one-time, documented):**
- Manual via RunPod web console for initial setup (avoid IaC overhead until
  scale demands it)
- Endpoint config (`runpod.yml`) is the source of truth; deviations get noticed
  and reconciled
- Endpoint ID written to Railway env var `RUNPOD_ENDPOINT_ID` on prod + staging

**Region selection:**
- Railway production runs in us-west by default
- RunPod endpoint pinned to a nearby US region (us-west or us-east, whichever
  has best A100 80GB availability at endpoint-creation time)
- Cross-region round-trip target: < 50 ms

**Policy change checklist** (when `policy.txt` is edited):
1. Update fixtures if policy semantics shifted
2. Push to `feat/` branch, GH Actions rebuilds image
3. Tag image with policy version (e.g., `policy-v3-2026-07-01`)
4. Deploy to staging endpoint first, re-run A/B harness
5. Promote to prod endpoint via env-var swap

**Secrets rotation runbook** (`RUNPOD_API_KEY`, `ZENTROPI_API_KEY`):
1. Generate new key in RunPod/Zentropi console
2. Update Railway env var on staging, restart service, verify health check passes
3. Repeat for prod
4. Revoke old key in RunPod/Zentropi console
5. No app-side caching — keys are read fresh from env on each request

### Performance levers

1. `enable_prefix_caching=True` — policy text is identical per call (thousands of
   tokens). vLLM caches its KV state. Probably the single biggest throughput win.
2. `max_model_len=4096` — drops from CoPE-B's 256K default, frees ~5–10 GB of
   KV cache for higher `max_num_seqs`.
3. `max_tokens=1, temperature=0` — greedy single-token decode.
4. `logprobs=2` — returns top-2 token probabilities so we can extract the
   confidence as the normalized logprob of the emitted `0` or `1`.

### Expected throughput (UNVERIFIED — must validate in Step 2)

Estimates based on vLLM + MoE behavior with greedy single-token decoding and
prefix caching. **No vendor-published numbers exist for CoPE-B-A4B on A100.**
These are educated guesses to be replaced with measured values during Step 2.

- Sustained (estimate): **50–150 req/s per worker** with prefix caching
- Cold start (image cached, FlashBoot warm): **2–5 s**
- Cold start (image cached, FlashBoot cold): **30–60 s**
- Cold start (image not cached): **2–5 min** for the first-ever request after image push

**Step 2 must measure actual sustained req/s against a Charcoal-shaped input
set on A100 80GB.** Minimum acceptable threshold to keep the architecture as
designed: **≥ 20 req/s sustained** per worker. Below that, revisit options:
H100 upgrade despite cost penalty, multi-worker fan-out with smaller batches,
or revisit Modal vs RunPod decision.

Target of 8.3 req/s sustained has 6–18× headroom *if* the estimate holds — but
the architecture's resilience to concurrent onboardings depends on it being
materially above the threshold.

## CoPE-A → CoPE-B migration plan

Eight steps, ordered to fail loudly and reversibly. Steps 1–4 happen on the
`feat/cope-b-self-host` branch with zero prod impact. Step 7 is the staging
gate. Step 8 is the prod cutover.

### Step 1 — Author the policy text AND labeled fixtures

Both artifacts authored together because Step 4.5 (accuracy gate) depends on
the fixtures existing, and the fixtures define the policy in practice.

**`gpu/cope-b-runpod/policy.txt`** defines "toxic" for CoPE-B — the artifact
that plays the role `ZENTROPI_LABELER_ID` plays invisibly on the hosted API.

- Start from the reference snapshot at `refs/labeler_prompt.txt`
- Translate into CoPE-B's `POLICY` slot format (no INSTRUCTIONS/ANSWER headers)
- Run ~50 known-toxic and known-clean examples through a CoPE-B Colab
  notebook to sanity-check the policy
- Commit to git so policy is versioned alongside code

**Labeled fixtures** (`tests/fixtures/cope_b/`):
- `known_toxic.jsonl` — minimum 20 hand-curated toxic examples
- `known_clean.jsonl` — minimum 20 hand-curated clean examples
- `edge_cases.jsonl` — sarcasm, counter-speech, reclaimed slurs, news
  commentary on violent topics (cf. chainlink #114)

**Both steps require real human judgment from Bryan** about what counts as
toxic in Charcoal's specific community context. Cannot be fully automated.

**Step 4.5 blocks** if `known_toxic.jsonl` or `known_clean.jsonl` is missing
OR has fewer than 20 entries — the accuracy gate can't run on insufficient
samples. The implementation should fail loudly with a clear "fixture set
too small or missing" error. Missing files are treated identically to empty ones.

### Step 2 — Build the RunPod GPU service

- Container + handler + prompt assembly per the GPU service section
- Local smoke test: `vllm serve` + a curl script with the 10 hand-picked inputs
- Push the image to RunPod, verify cold-start time and warm throughput

### Step 3 — Rust adapter trait + RunPodCopeBClient

- `ToxicityClassifier` trait, `RunPodCopeBClient`, `StubClassifier`
- Unit tests cover prompt assembly (Gemma chat template), JSON parsing,
  retry/backoff, timeout, threshold logic
- Charcoal still defaults `CHARCOAL_CLASSIFIER=zentropi` on prod until step 8

### Step 4 — A/B characterization harness

New dev CLI: `charcoal classify-compare --input <jsonl> --backends cope-a-zentropi,cope-b-runpod`.
Runs the same input through both backends and logs both verdicts + confidences.

- Run on labeled examples from `user_labels`, a grimalkina-scan sample, and a
  hand-curated edge-case set Bryan provides
- **Agreement-with-CoPE-A is NOT a gate.** Per Bryan's framing, agreement-for-its-own-sake
  is the wrong target. The A/B output is used to characterize where CoPE-A and
  CoPE-B disagree and judge whether the differences make decisions better or
  worse on labeled cases.
- Reused as a regression-detection tool any time policy text or threshold changes

### Step 4.5 — Accuracy gate on labeled fixtures (HARD GATE)

Before any prod cutover, CoPE-B must demonstrate measurable quality on
Charcoal's hand-curated labeled fixtures (`tests/fixtures/cope_b/`):

- **Known-toxic fixture set** (≥20 hand-curated examples): CoPE-B must classify
  ≥ 90% as toxic.
- **Known-clean fixture set** (≥20 hand-curated examples): CoPE-B must classify
  ≥ 90% as clean.
- **Edge case fixture set** (sarcasm, counter-speech, news commentary on violent
  topics — cf. chainlink #114, reclaimed slurs): no hard gate, but disagreements
  are reviewed by Bryan. A pattern of regressions vs CoPE-A → halt migration
  and revisit policy text.

The 90%/90% bar is intentionally floor-level; Phase 5's binary toxicity rate
calibration (chainlink #135) is already fragile, so an explicit floor here
prevents quality regressions from cascading into tier shifts.

This gate runs after Step 5 (threshold calibration), since the threshold
affects accuracy.

### Step 5 — Recalibrate confidence threshold

CoPE-B's logprobs concentrate differently than CoPE-A's. Using A/B output:

- Pick the CoPE-B threshold that maximizes accuracy on labeled examples, or
- Match CoPE-A on the unlabeled distribution if labels are too thin

Update the per-backend threshold constant in Rust source — `const COPE_B_THRESHOLD: f32`
on `RunPodCopeBClient`. This is a **code change**, not an env var, to keep the
calibrated value versioned in git alongside the policy text that it was tuned
for. Threshold drift via runtime override would silently invalidate the
labeled-fixture accuracy gate. The `ZentropiClient` similarly holds its own
threshold constant.

### Step 6 — Zentropi-hosted CoPE-B for fallback

Research Zentropi's hosted CoPE-B API (call parameter? new `ZENTROPI_LABELER_VERSION_ID`?
new endpoint?). Update `ZentropiClient` to call CoPE-B via hosted. Re-run A/B
harness to confirm RunPod-CoPE-B ≈ Zentropi-hosted-CoPE-B.

**Fallback gap policy:** if Zentropi can't host CoPE-B at cutover time, the
fallback path runs CoPE-A under its own threshold. Because per-backend thresholds
are stored on each implementation (see Architecture section), there's no
threshold-bleed between primary and fallback — Zentropi-CoPE-A keeps its
existing CoPE-A threshold; RunPod-CoPE-B uses the Step-5-calibrated threshold.

Fallback fires rarely; when it does, the user gets the old model's verdict at
the old model's calibration. Acceptable degradation. Document clearly in the
runtime audit log so post-hoc analysis can identify which classifier produced
which verdict.

Once Zentropi-hosted CoPE-B is available, the fallback path is also CoPE-B
end-to-end with a (possibly different) hosted-side threshold.

### Step 7 — Staging rollout

- Deploy adapter on `staging`
- Flip `CHARCOAL_CLASSIFIER=runpod`
- Trigger a full scan — re-scan grimalkina because we have a known baseline
  from chainlink #182
- Compare staging vs prod-on-CoPE-A: scan duration, cost, tier distribution
  shift, % flagged accounts changed
- Hold for at least one big scan before promoting to prod

### Step 8 — Prod cutover

- Env var flip on prod, watch first scans closely
- Rollback path: revert env var + redeploy (~10 min)
- Keep Zentropi configured as fallback for 2–4 weeks before deprecation

## Testing strategy

Tests written first per Bryan's TDD mandate.

### Rust unit tests (`tests/unit_classifier.rs`)

- `ClassifierVerdict` serde roundtrip (model_id, policy_version, confidence, etc.)
- Gemma chat template prompt assembly — golden-file test, asserts known input
  → exact known prompt string including BOS/EOS tokens and role markers
- Charcoal envelope `[Parent post] / [Reply]` integration into `CONTENT` slot
- JSON wire-format parse (success, error, malformed)
- Confidence threshold logic with boundary cases
- Retry policy: exponential backoff with decorrelated jitter, max attempts, timeout escalation (golden-file test on the jitter window bounds so future tuning doesn't silently widen)

### Rust integration tests (`tests/web_classifier.rs`, `--features web`)

- Full ensemble flow with `StubClassifier` returning scripted verdicts:
  confirms `ensemble.rs` still calls ONNX first and only routes to the trait for
  non-clean posts
- Failure injection: stub returns 5xx → retry → eventual failure surfaces to
  the scoring pipeline
- `CHARCOAL_CLASSIFIER` env var selects the right backend at startup; unset
  causes boot to fail loudly

### GPU service tests (`gpu/cope-b-runpod/tests/`)

- `pytest` for the Python handler: prompt assembly, vLLM mock, response shape
- Local smoke test script: `vllm serve` + curl script with 10 hand-picked
  inputs (5 clearly-toxic, 5 clearly-clean), assert all 10 classify correctly
- **Prefix-caching benchmark**: send N identical-policy requests with varying
  CONTENT, assert that median time-to-second-request is materially lower than
  time-to-first (e.g., ≤ 50% of first-request latency). Detects silent prefix-
  caching breakage between vLLM versions.
- **vLLM version pinning**: container digest pinned per release in `Dockerfile`
  comments and CI workflow; not just the `:v0.20.2` tag
- "Before we deploy" gate. Cheaper than burning RunPod credits on broken policy.

### Concurrent-onboarding load test (Step 7, staging only)

Before prod cutover, run a synthetic load test on staging:
- 3 concurrent simulated onboarding scans, each issuing ~1000 classifications
- Assert: all complete without 5xx storms, cost stays within ceiling, latency
  remains within target
- This is the test that catches `max_workers=3` being too low or too high,
  cold-start cascade under burst, and queue starvation

### Test fixtures

- `tests/fixtures/cope_b/known_toxic.jsonl` — 20+ hand-curated toxic examples
- `tests/fixtures/cope_b/known_clean.jsonl` — 20+ clearly-benign examples
- `tests/fixtures/cope_b/edge_cases.jsonl` — sarcasm, counter-speech, news
  commentary on violent topics (cf. chainlink #114), reclaimed slurs

Fixtures are versioned alongside code. Any policy or threshold change re-runs
them with visual inspection.

### Staging gate (Step 7)

- Re-scan a known-baseline user (grimalkina) on staging
- Assert scan completes, tier distribution doesn't shift wildly, no panics
- Human-eyeballs gate before prod cutover

## Cost, throughput, and monitoring

### Per-onboarding cost model (RunPod A100 80GB at $2.72/hr)

Single-onboarding case (one worker):

| Phase | Cost basis | Per onboarding |
|-------|------------|----------------|
| Cold start, image cached, FlashBoot warm | 2–5 s | ~$0.002 |
| Cold start, image cached, FlashBoot cold | 30–60 s | ~$0.045 |
| Inference, 10 min worst case | 600 s | ~$0.45 |
| Inference, 2–3 min likely | 180 s | ~$0.14 |
| Idle window (60 s default before scale-to-zero) | 60 s | ~$0.045 |
| **Realistic total per onboarding** | | **$0.20 – $0.55** |

Concurrent-onboarding case (worker billing is per-instance, not per-user):

| Scenario | Workers active | Wall clock | Total $ | Per onboarding |
|----------|----------------|------------|---------|----------------|
| 2 concurrent (3 min each, separate workers) | 2 | 3 min | ~$0.27 | ~$0.14 |
| 3 concurrent (max_workers cap, 3 min each) | 3 | 3 min | ~$0.41 | ~$0.14 |
| 4+ concurrent (queue beyond max_workers) | 3 | 5–7 min | ~$0.55–$0.95 | ~$0.18–$0.24 |

Per-onboarding cost stays under the $1 ceiling even under burst load, because
single-worker throughput has 6–18× headroom on our 8.3 req/s target — more
workers don't help much when one worker isn't saturated.

**Retry amplification:** the default `CHARCOAL_CLASSIFIER_MAX_RETRIES=3` with
exponential backoff + jitter means a transient 5xx cluster bills the worker
time for each retry attempt plus the backoff sleep (worker stays alive during
sleep). Jitter spreads concurrent retries across the backoff window, reducing
peak retry cost relative to no-jitter exponential backoff.
Worst case for a single classification under retry pressure: ~6× the
single-call cost. Aggregate effect on a scan is small (rare events × low base
cost), but it's not zero — flagged here so the cost guardrail can catch a
pathological retry storm.

Egress is free on RunPod, so Railway→RunPod traffic doesn't add cost. The
dominant tunable lever is `idle_timeout` — start at 60 s, tune down to 5–10 s
once warm-restore probability is measured.

### Cost guardrail

Runtime check (not just observability):

- Track running cost estimate during a scan. If it exceeds a hard ceiling
  (default $2/scan), abort the scan and log loudly. Surface a clear error to
  the user.
- `CHARCOAL_SCAN_COST_CEILING_CENTS=200` (default).
- **Scope:** per scan attempt instance — not per user-day. A user whose scan
  aborts due to overrun can retry (their next scan starts a fresh budget).
  This prevents user-level lock-out from a single misbehaving scan and matches
  the "fail loudly" stance.
- **Not a billing cap.** The ceiling is a safety brake to abort runaway
  individual scans. Account-level monthly billing limits should be configured
  separately on the RunPod side (operational concern outside this spec).
- Backstop against "RunPod billing bug or our concurrency is wrong." Better to
  lose one scan than discover a $400 surprise the next morning.

### Throughput budget

| Metric | Target | Expected on A100 |
|--------|--------|------------------|
| Sustained req/s per worker | 8.3 | 50–150 with prefix caching |
| Cold-start p95 | < 30 s | 2–5 s warm / 30–60 s cold |
| Concurrent users absorbed | 1 hard, 3 soft | 3 workers × 50 req/s = 150 burst |
| 5000 classifications wall clock | < 10 min | 30 s – 2 min on warm worker |

### Monitoring

**RunPod dashboard (free):** worker uptime, cold-start counts and p95,
GPU utilization, request count, error rate.

**Charcoal-side classifier metrics** (`src/observability/classifier_metrics.rs`,
emitted via `tracing::info!`):

- `classifier_request_latency_ms` (histogram, labeled by backend name)
- `classifier_cold_start_detected` (counter — latency > 5 s on first call after idle)
- `classifier_retry_count` (counter)
- `classifier_backend_selected_total` (counter, labeled `backend=runpod|zentropi`) —
  emitted once at startup. **Not a runtime auto-fallback signal**; the design has
  no silent fallback. This metric reflects which backend was selected at boot
  based on `CHARCOAL_CLASSIFIER` and availability checks.
- `classifier_classification_count` (counter, labeled `toxic=true|false`)
- `classifier_cost_estimate_cents` (gauge — elapsed RunPod time × rate)
- `classifier_idle_window_seconds` (histogram — time between scan-end and next
  scan-start per worker; used to tune `runpod.yml` `idle_timeout` data-driven)

Metric names use the generic `classifier_*` prefix (not `cope_b_*`) so the
adapter stays backend-agnostic in observability too. Backend identity is
carried in the `backend` label.

Aggregated per scan and written to a new `scan_metrics` JSONB column on the
scan row. Scan-complete log line carries `classifier_cost_cents=X` and
`classifier_backend=runpod|zentropi`.

### Audit log (generalized)

Charcoal already has NLI audit JSONL infrastructure (`src/scoring/nli_audit.rs`).
Rather than introduce a parallel rotator, **generalize** the existing module
into `src/scoring/audit_log.rs` parameterized by event type (`nli`,
`classifier`), with a common JSONL writer and rotation policy.

Classifier audit event fields: timestamp, backend, model_id, **policy_version**,
prompt_hash (content hash, not full text — privacy), verdict, confidence,
latency_ms.

`backend` already captures whether a call ran on RunPod or Zentropi; no
separate `fallback_invoked` flag is needed. The "fallback" path is just the
operator selecting `CHARCOAL_CLASSIFIER=zentropi` at startup; per-call audit
shows that via `backend=zentropi`.

Carrying `policy_version` separately from `model_id` is critical because
two different content surfaces drive the verdict — model weights and policy
text. A policy-text-only change rebuilds the image and bumps the policy version
tag (cf. "Image and endpoint lifecycle"). Without `policy_version` in the
audit log, the same `model_id` would silently span two different effective
classifiers across a policy edit, making post-hoc analysis ambiguous.

Enabled via `CHARCOAL_AUDIT_CLASSIFIER=1`. Used for:
- A/B harness output capture
- Debugging surprising verdicts after the fact
- Recalibration if/when we change policy text

Migrating the existing NLI audit to the generalized module is a separate small
PR that lands before this work (so this spec's audit module change is purely
additive).

### Health check

`charcoal classifier-check` new CLI command pings the configured backend with
3 known inputs (1 toxic, 1 clean, 1 edge case), asserts expected verdicts,
reports latency. Run after env-var changes and as a Railway healthcheck preflight.

## Environment variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `CHARCOAL_CLASSIFIER` | (none — boot fails) | `runpod` or `zentropi` |
| `RUNPOD_API_KEY` | (none) | RunPod auth bearer token |
| `RUNPOD_ENDPOINT_ID` | (none) | RunPod endpoint UUID |
| `ZENTROPI_API_KEY` | (none) | Existing — kept for fallback |
| `ZENTROPI_LABELER_ID` | (none) | Existing — points at CoPE-B once available, else CoPE-A |
| `ZENTROPI_LABELER_VERSION_ID` | (none) | Existing — pins labeler version |
| `CHARCOAL_SCAN_COST_CEILING_CENTS` | `200` | Hard cap per scan, aborts on overrun |
| `CHARCOAL_AUDIT_CLASSIFIER` | `0` | Set `1` to emit per-call audit JSONL |
| `CHARCOAL_CLASSIFIER_TIMEOUT_MS` | `60000` | Steady-state per-request timeout |
| `CHARCOAL_CLASSIFIER_WARMUP_TIMEOUT_MS` | `180000` | First-call-after-idle timeout (cold start) |
| `CHARCOAL_CLASSIFIER_MAX_RETRIES` | `3` | Bounded retries on 5xx (exponential backoff + decorrelated jitter) |

## Open questions and TBDs

1. **Zentropi-hosted CoPE-B availability** — confirmed during Step 6. If
   unavailable, fallback runs CoPE-A.
2. **Empirical throughput on A100** — Zentropi has not published numbers.
   Measured during Step 2 smoke test.
3. **Optimal `max_num_seqs`** — depends on actual KV cache footprint with our
   policy size. Tuned empirically during Step 2.
4. **Confidence threshold for CoPE-B** — set in Step 5 from A/B data.
5. **Policy text content** — authored in Step 1 by Bryan + Claude collaboration,
   versioned in git.

## Related work and references

- chainlink #182 — grimalkina scan baseline (Zentropi 403 storm finding)
- chainlink #181 — Zentropi concurrency follow-up (now lower priority)
- chainlink #183 — Zentropi 403 investigation (separate concern)
- chainlink #114 — toxicity false positives on news commentary (relevant to
  policy text + edge case fixtures)
- chainlink #176 — Zentropi labeler policy improvements (input for Step 1 policy)
- HuggingFace: [`zentropi-ai/cope-b-a4b`](https://huggingface.co/zentropi-ai/cope-b-a4b)
- CoPE paper: [arXiv:2512.18027](https://arxiv.org/abs/2512.18027)
- RunPod FlashBoot: [blog](https://www.runpod.io/blog/introducing-flashboot-serverless-cold-start)
- vLLM: [project](https://github.com/vllm-project/vllm)
