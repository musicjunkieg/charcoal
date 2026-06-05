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

### The classifier trait

```rust
#[async_trait]
pub trait ToxicityClassifier: Send + Sync {
    async fn classify(&self, content: &str) -> Result<Verdict>;
    async fn classify_pair(&self, parent: &str, reply: &str) -> Result<Verdict>;
    fn name(&self) -> &'static str;
}

pub struct Verdict {
    pub toxic: bool,           // binary classification
    pub confidence: f32,        // normalized logprob of emitted token
    pub model_id: String,       // for audit log
    pub latency_ms: u32,
}
```

Backend selection happens at startup via `CHARCOAL_CLASSIFIER`:

- `runpod` → `RunPodCopeBClient` (primary, self-hosted)
- `zentropi` → `ZentropiClient` (fallback; uses Zentropi-hosted CoPE-B if available, else CoPE-A)
- unset or unreachable → app refuses to boot (`anyhow::bail!`)

No ONNX-only degraded mode. ONNX stays the clean-pass filter at Stage 1; the
LLM classifier is required at Stage 2. Tests use a `StubClassifier`.

### Failure modes

- GPU service 5xx → retry with exponential backoff up to N attempts, then surface
  the scoring failure to the user. **No silent fallback to ONNX threshold.**
- GPU service 4xx → log and fail the scoring job (indicates a bug or misconfig).
- Startup with no reachable backend → boot fails loudly.

### Cold-start UX

On the first request after idle, the scan manager sets
`scan_state.status = "warming_classifier"` so the SvelteKit UI shows a
"warming up classifier (~30s)" message rather than a stalled spinner.

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

### Performance levers

1. `enable_prefix_caching=True` — policy text is identical per call (thousands of
   tokens). vLLM caches its KV state. Probably the single biggest throughput win.
2. `max_model_len=4096` — drops from CoPE-B's 256K default, frees ~5–10 GB of
   KV cache for higher `max_num_seqs`.
3. `max_tokens=1, temperature=0` — greedy single-token decode.
4. `logprobs=2` — returns top-2 token probabilities so we can extract the
   confidence as the normalized logprob of the emitted `0` or `1`.

### Expected throughput

- Sustained: **50–150 req/s per worker** with prefix caching on our short inputs
- Cold start (image cached, FlashBoot warm): **2–5 s**
- Cold start (image cached, FlashBoot cold): **30–60 s**
- Cold start (image not cached): **2–5 min** for the first-ever request after image push

Target of 8.3 req/s sustained has 6–18× headroom.

## CoPE-A → CoPE-B migration plan

Eight steps, ordered to fail loudly and reversibly. Steps 1–4 happen on the
`feat/cope-b-self-host` branch with zero prod impact. Step 7 is the staging
gate. Step 8 is the prod cutover.

### Step 1 — Author the policy text

`gpu/cope-b-runpod/policy.txt` defines "toxic" for CoPE-B — the artifact that
plays the role `ZENTROPI_LABELER_ID` plays invisibly on the hosted API.

- Start from the reference snapshot at `refs/labeler_prompt.txt`
- Translate into CoPE-B's `POLICY` slot format (no INSTRUCTIONS/ANSWER headers)
- Run ~50 known-toxic and known-clean examples through a CoPE-B Colab
  notebook to sanity-check the policy
- Commit to git so policy is versioned alongside code

**This step requires real human judgment from Bryan** about what counts as
toxic in Charcoal's specific community context — sarcasm, counter-speech,
reclaimed slurs, news commentary on violent topics. Cannot be fully automated.

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
- Output is **informational, not a hard gate** — used to characterize where
  CoPE-A and CoPE-B disagree and judge whether those differences matter
  qualitatively based on accuracy on labeled cases
- Reused as a regression-detection tool any time policy text or threshold changes

### Step 5 — Recalibrate confidence threshold

CoPE-B's logprobs concentrate differently than CoPE-A's. Using A/B output:

- Pick the CoPE-B threshold that maximizes accuracy on labeled examples, or
- Match CoPE-A on the unlabeled distribution if labels are too thin

Update the threshold constant in scoring code.

### Step 6 — Zentropi-hosted CoPE-B for fallback

Research Zentropi's hosted CoPE-B API (call parameter? new `ZENTROPI_LABELER_VERSION_ID`?
new endpoint?). Update `ZentropiClient` to call CoPE-B via hosted. Re-run A/B
harness to confirm RunPod-CoPE-B ≈ Zentropi-hosted-CoPE-B.

**Fallback gap policy:** if Zentropi can't host CoPE-B at cutover time, the
fallback path runs CoPE-A and we document the mismatch. Fallback fires rarely;
when it does, the user gets the old model's verdict. Acceptable degradation.

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

- `Verdict` serde roundtrip
- Gemma chat template prompt assembly — golden-file test, asserts known input
  → exact known prompt string including BOS/EOS tokens and role markers
- Charcoal envelope `[Parent post] / [Reply]` integration into `CONTENT` slot
- JSON wire-format parse (success, error, malformed)
- Confidence threshold logic with boundary cases
- Retry policy: exponential backoff, max attempts, timeout escalation

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
- "Before we deploy" gate. Cheaper than burning RunPod credits on broken policy.

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

| Phase | Cost basis | Per onboarding |
|-------|------------|----------------|
| Cold start, image cached, FlashBoot warm | 2–5 s | ~$0.002 |
| Cold start, image cached, FlashBoot cold | 30–60 s | ~$0.045 |
| Inference, 10 min worst case | 600 s | ~$0.45 |
| Inference, 2–3 min likely | 180 s | ~$0.14 |
| Idle window (60 s default before scale-to-zero) | 60 s | ~$0.045 |
| **Realistic total** | | **$0.20 – $0.55** |

Egress is free on RunPod, so Railway→RunPod traffic doesn't add cost. The
dominant tunable lever is `idle_timeout` — start at 60 s, tune down to 5–10 s
once warm-restore probability is measured.

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

- `cope_b_request_latency_ms` (histogram)
- `cope_b_cold_start_detected` (counter — latency > 5 s on first call after idle)
- `cope_b_retry_count` (counter)
- `cope_b_fallback_to_zentropi_count` (counter — non-zero is a signal)
- `cope_b_classification_count` (counter, labeled `toxic=true|false`)
- `cope_b_cost_estimate_cents` (gauge — elapsed RunPod time × rate)

Aggregated per scan and written to a new `scan_metrics` JSONB column on the
scan row. Scan-complete log line carries `classifier_cost_cents=X`.

### Cost guardrail

Runtime check (not just observability):

- Track running cost estimate during a scan. If it exceeds a hard ceiling
  (default $2/scan), abort the scan and log loudly.
- `CHARCOAL_SCAN_COST_CEILING_CENTS=200` default.
- Backstop against "RunPod billing bug or our concurrency is wrong." Better to
  lose one scan than discover a $400 surprise the next morning.

### Audit log

`src/scoring/classifier_audit.rs` (new) writes one JSONL line per classification
when `CHARCOAL_AUDIT_CLASSIFIER=1`. Fields: timestamp, model_id, prompt_hash,
verdict, confidence, latency_ms. Rotated daily. Used for:

- A/B harness output capture
- Debugging surprising verdicts after the fact
- Recalibration if/when we change policy text

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
| `CHARCOAL_CLASSIFIER_TIMEOUT_MS` | `60000` | Per-request timeout |
| `CHARCOAL_CLASSIFIER_MAX_RETRIES` | `3` | Bounded retries on 5xx |

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
