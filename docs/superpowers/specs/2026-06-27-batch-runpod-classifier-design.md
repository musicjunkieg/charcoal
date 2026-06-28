# Batch the RunPod classifier — design (#186)

**Status:** approved, ready for implementation plan
**Branch:** `feat/batch-runpod-classifier`
**Date:** 2026-06-27
**Relates:** #185 (real-time cost meter), #187 (GPU/DC resilience), #207 (onboarding speed/cost), #206 (cost backstop), #208 (classification decouple)

## Problem

Onboarding cost is dominated by warm-idle GPU time, not compute. The 2026-06-26
staging burst made **4421 classifier calls**, each with RunPod `executionTime`
~130ms (a single greedy `0`/`1` token, prefill-dominated). Actual GPU compute
≈ 4421 × 0.13s ≈ 9.6 GPU-min ≈ **$0.53**; real cost was **~$6-10** — ~90-95%
warm-idle waste because the burst is **queue-bound** (RunPod `delayTime` ~3-4s
vs `executionTime` ~0.13s) and sends **one text per `/runsync` job**.

Root cause: the Rust client sends one text per RunPod job. The vLLM handler
already supports batching (`max_num_seqs=32`, `AsyncLLMEngine` continuous
batching) but each serverless job carries a single text, so that batch capacity
sits idle.

**Goal:** send batches of N texts per `/runsync` request. ~10-30× fewer requests,
the burst finishes in minutes instead of ~36, and worker-hours collapse toward
the ~$0.53 compute floor — the $1/onboarding target. Verified post-deploy by the
#185 honest cost meter.

## Approved design decisions

1. **Batch-only wire contract.** The handler accepts only
   `{"input":{"contents":[...]}}`; the Rust client always sends a list (a single
   `classify()` becomes a 1-element batch). One internal HTTP code path.
2. **Per-item isolation.** A slot that the model can't decode into `0`/`1` does
   not fail the whole batch.
3. **Positional index alignment.** Ordered list in, ordered list out; the client
   zips verdicts back to `(account_did, post_uri)` by index with a strict
   length-equality check. No `post_uri` leaves the app to the GPU box.
4. **Per-item decode error → fail-open to benign.** Record a sentinel verdict
   (`toxic_token=false`, `model_id="decode-error"`) via the existing
   `record_classification_verdicts`, plus a `warn!`, a metric, and a scan
   `degraded` flag. No schema change, no finalize change, no livelock.

### Why fail-open-to-benign (decision 4)

`finalize.rs::verdict_for` (src/pipeline/scan_phases/finalize.rs:247-263) requires
every sampled post to have a `status='done'` row **with** a non-NULL
`toxic_token`; a `done` row with NULL `toxic_token` is treated as inconsistent →
the whole account returns `NeedsRegather`. So an errored row left as
`done`-with-NULL would trigger a full account re-gather + re-burst storm (and the
post would re-fail deterministically). The honest, minimal mechanism that
satisfies the existing finalize gate without a schema migration is to record a
real but explicitly-labelled benign verdict:

- **Safe for the domain:** Charcoal's entire bias is that benign can never inflate
  a false threat. An unclassifiable post counted as non-toxic cannot create a
  false positive. (Followers/strangers are the safe direction; over-flagging is
  the harm.)
- **Honest, not silent:** every occurrence is `warn!`-logged, counted in a
  `classifier_decode_errors` metric, and flips the scan `degraded` flag. The
  `model_id="decode-error"` sentinel makes errored rows queryable after the fact.
- **Cost:** 1/N rate-denominator noise per errored post — negligible, and decode
  errors are realistically ~never (the 4421-row staging run had zero).

The model emits one greedy token from `{0,1}` essentially always, so this path is
cheap insurance, not a hot path.

## Architecture

The burst stops firing one `/runsync` job per post. It chunks the pending queue
into batches of N and sends one job per batch; vLLM's continuous batching does
the parallelism on-GPU.

```
fetch burst_batch pending rows                              [unchanged]
  → build envelope per row                                  [unchanged:
      reply  → format_parent_reply(context_text, text)
      other  → raw text]
  → chunk rows by classifier.max_batch_size()               [N=32 RunPod, 1 Zentropi]
  → buffer_unordered(burst_concurrency) over CHUNKS         [each chunk = 1 classify_batch call]
  → per chunk: zip verdicts back to (account_did, post_uri) by POSITION
  → record verdicts (real + benign sentinels); handle request-level errors as today
```

## Components

### 1. Trait (`src/toxicity/classifier.rs`) — additive

```rust
/// One slot of a batch result. The request already succeeded (HTTP 200, job
/// COMPLETED); this distinguishes a decodable verdict from a single un-decodable
/// slot. Request-level failures are the outer `Result::Err`, not this enum.
pub enum ItemOutcome {
    Verdict(ClassifierVerdict),
    /// The job completed but this slot's content did not decode to "0"/"1".
    Error(String),
}

#[async_trait]
pub trait ToxicityClassifier: Send + Sync {
    async fn classify(&self, content: &str) -> Result<ClassifierVerdict>;

    /// Classify many texts in one backend round-trip. Default impl loops
    /// `classify`; the first request-level error short-circuits to outer `Err`.
    async fn classify_batch(&self, contents: &[String]) -> Result<Vec<ItemOutcome>> {
        let mut out = Vec::with_capacity(contents.len());
        for c in contents {
            out.push(ItemOutcome::Verdict(self.classify(c).await?));
        }
        Ok(out)
    }

    /// Max texts to send per `classify_batch` request. Default 1 (= today's
    /// one-text-per-call behaviour). RunPod overrides from env (default 32).
    fn max_batch_size(&self) -> usize { 1 }

    fn name(&self) -> &'static str;
    fn model_id(&self) -> &'static str;
    fn policy_version(&self) -> &'static str;
    fn threshold(&self) -> f32;
}
```

- **Outer `Result::Err`** = request-level: transport/5xx-exhausted →
  `ClassifierTransientError`, 4xx → permanent, cost → `CostCeilingExceeded`. Same
  typed errors as today, just per-batch.
- **`ItemOutcome::Error`** = RunPod-only (job COMPLETED, one slot un-decodable).
- Zentropi/Stub keep `max_batch_size()=1`, so their behaviour is **byte-identical
  to today** (1-item batches, request-level errors only — the default loop never
  produces an `ItemOutcome::Error`).

### 2. RunPod client (`src/toxicity/runpod_cope_b.rs`)

- `classify_batch(contents)` becomes the **single HTTP chokepoint**:
  - body `{"input":{"contents":[...]}}`,
  - same retry / cost-guard / `/status` poll machinery as the current
    `classify_with_timeout` (refactored to take the list),
  - parse `{"output":{"verdicts":[ {ok:true,toxic,confidence,model,policy_version}
    | {ok:false,error} ]}}` → `Vec<ItemOutcome>`,
  - **strict length check:** `verdicts.len() == contents.len()` else outer `Err`
    (contract violation → abort; should never happen),
  - per-slot confidence still validated finite ∈ [0,1] (existing guard).
- `classify(content)` delegates: `classify_batch(&[content.to_string()])` →
  match `vec[0]`: `Verdict→Ok`, `Error(e)→Err(anyhow!(e))`. Keeps `warm_up`,
  the A/B compare gate, and `zentropi-check` single-call callers working.
- `max_batch_size()` reads `CHARCOAL_RUNPOD_BATCH_SIZE` (default 32, clamp 1..=128).

### 3. Handler (`gpu/cope-b-runpod/handler.py`) — batch-only

- Accepts `event["input"]["contents"]: list[str]`.
- Builds a prompt per content, fires N `engine.generate` coroutines (vLLM
  continuous batching), awaits in order.
- Per-item `try/except` around `decode_verdict`: a failed slot returns
  `{"ok": false, "error": "<detail>"}`; a good slot returns
  `{"ok": true, "toxic": ..., "confidence": ..., "model": ..., "policy_version": ...}`.
- Returns `{"verdicts": [...]}` (RunPod wraps it as `{"output": {"verdicts": ...}}`),
  **order-preserving**.
- The single-`content` path is removed.

### 4. Burst (`src/pipeline/scan_phases/burst.rs`)

- After fetching `burst_batch` pending rows and building envelopes, **chunk** the
  rows by `classifier.max_batch_size()`.
- `buffer_unordered(burst_concurrency)` over **chunks**; each future returns
  `(chunk_rows, Result<Vec<ItemOutcome>>)`.
- Per completed chunk:
  - **outer `Ok(vec)`**: assert `vec.len() == chunk_rows.len()`; zip by index:
    - `Verdict(v)` → push real `VerdictRow`,
    - `Error(_)` → push **benign sentinel** `VerdictRow{toxic_token:false,
      model_id:"decode-error", confidence:0.0, policy_version:<from classifier>}`,
      `warn!`, bump `classifier_decode_errors`, increment local `errored` count.
  - **outer `Err`**: existing handling — `CostCeilingExceeded` → cost-cap,
    `ClassifierTransientError` → interrupt, anything else → permanent abort
    (after persisting the batch's successes). The stop-accumulating-after-fire
    drain behaviour is preserved.
- Persist via the existing batched `record_classification_verdicts`.
- Return `BurstOutcome::Complete { errored }` (see §5).

### 5. Orchestrator (`src/pipeline/scan_phases/mod.rs`)

- `BurstOutcome::Complete` gains a field: `Complete { errored: usize }` (stays
  `Copy`). The `Complete` arm sets `summary.degraded = true` when `errored > 0`.
- `recover_account_inner` matches only `BurstOutcome::CostCapped`, so it is
  unaffected by the new field.
- **Known limitation (documented, accepted):** the `errored`-driven `degraded`
  flag is best-effort *within a single process run*. On a later resume the errored
  rows are already terminal (`done`), so that resume's `run_burst` returns
  `Complete { errored: 0 }` and won't re-raise `degraded`. The `warn!` logs and
  the metric remain the durable record. Acceptable for a ~never path.

### 6. Config

- `CHARCOAL_RUNPOD_BATCH_SIZE` — texts per RunPod request. Default 32
  (= handler `max_num_seqs`), clamp 1..=128.
- In-flight texts = `burst_concurrency × batch_size`. With `workersMax=10` the
  excess jobs queue. Existing `CHARCOAL_BURST_CONCURRENCY` (default 16) and
  `CHARCOAL_BURST_BATCH` (default 500) are unchanged; tune post-deploy via the
  #185 meter.

## Error handling summary

| Failure | Granularity | Result |
|---|---|---|
| Transport / 5xx, retries exhausted | whole batch | `ClassifierTransientError` → burst `Interrupted` (resumable) |
| HTTP 4xx | whole batch | permanent → burst aborts |
| Cost ceiling tripped | whole batch | `CostCeilingExceeded` → burst `CostCapped` (resumable) |
| Slot un-decodable (job COMPLETED) | per item | benign sentinel verdict + warn + metric + `degraded` |
| `verdicts.len() ≠ contents.len()` | whole batch | outer `Err` → abort (contract violation) |

## Rollout

The client always sends `contents`; an old handler reading `content` would
`KeyError`. The RunPod endpoint image is independent of the Railway app deploy,
so: **rebuild the handler image and update the endpoint before** deploying the app
revision that sends batches. (For #63 Phase-6 → prod, fold this into the existing
checklist.)

## Testing (TDD, RED → GREEN)

**Handler (`gpu/cope-b-runpod/`, pytest):**
- batch returns verdicts in input order;
- one un-decodable slot → that slot `ok:false`, siblings `ok:true` (isolation);
- `len(verdicts) == len(contents)`;
- empty `contents` → empty `verdicts`.

**Client (`src/toxicity/runpod_cope_b.rs`):**
- request body shape `{"input":{"contents":[...]}}`;
- parse a verdicts array → `Vec<ItemOutcome>` (mixed ok/error);
- positional alignment preserved;
- length mismatch → outer `Err`;
- per-slot `ok:false` → `ItemOutcome::Error`;
- `classify()` delegates to `classify_batch` and unwraps slot 0.

**Burst (`src/pipeline/scan_phases/burst.rs`):**
- chunking honours `max_batch_size`;
- positional zip back to `(account_did, post_uri)`;
- per-item error → benign sentinel recorded + `errored` count + `degraded`;
- request-level cost-cap / transient / permanent still honoured;
- returns `Complete { errored }`.

**Default trait impl (`classifier.rs`):**
- `classify_batch` loops `classify`; `max_batch_size()==1`; first error →
  outer `Err` (Zentropi/Stub parity with today).

## Out of scope

- GPU right-sizing / quantization / multi-DC failover — that's #187.
- Changing tier thresholds or the scoring formula.
- Any Zentropi batching (Zentropi has no batch endpoint; it stays 1-per-call).
