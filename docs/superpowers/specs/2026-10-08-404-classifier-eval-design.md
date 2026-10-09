# Classifier eval harness — design (#404)

**Date:** 2026-10-08 · **Status:** approved in brainstorm, awaiting spec review
**Goal:** decide, with evidence, whether a new "decision model" (Jev, Clef,
Strands Decider…) is better and at least as affordable as CoPE-B for judging
whether a post is hostile — and keep that question cheap to re-ask whenever a
new model ships.

## Why

Charcoal's only paid model is CoPE-B on a RunPod GPU: it drives most of a
scan's cost (≈ $2.67 for a full staging scan) and every cold-start problem
(#385–#388, #402). In autumn 2026 a new class of cheap, fast classifiers
appeared — models that take text plus a fixed set of answers and return one
answer with probabilities. Today we cannot say whether any of them is better,
because **only 10 posts have human labels** (`tests/fixtures/cope_b/smoke.jsonl`,
5 toxic / 5 clean). The 100-post `ab_sample.jsonl` was harvested but never
labelled. The eval comes first; the model choice follows from it.

## Decisions made in the brainstorm

| Question | Decision |
|---|---|
| What should the eval answer first? | Build the eval before choosing a model |
| Level | **Post level** — "is this post hostile under our policy?" Account-level tier checks are out of scope. |
| Growing the labels | **Label the disagreements**: run every candidate over a pool, hand-label only posts where they disagree, plus a ~10 % random sample of agreed posts |
| Which error matters more | **Neither by default** — report false alarms and misses side by side, no blended score |
| Where it lives | **A Rust `charcoal eval-classifiers` command** built on the existing `ToxicityClassifier` trait, not a Python tool or an eval SaaS |

## Candidates (as of 2026-10-08)

| Candidate | Access | List price (input) | Note |
|---|---|---|---|
| CoPE-B (baseline) | RunPod, existing adapter | GPU-seconds | The model to beat |
| TypeSafe **Jev** | Hosted API, closed weights, 32K context | $0.042 / M tokens, output free | Price may be launch-subsidised |
| Cloudflare **Clef-flash** (9B) / **Clef** (27B) | Workers AI, open weights (Apache 2.0), 64K | $0.09 / $0.24 per M | Leaderboard scores self-reported |
| AWS **Strands Decider 2B** | Self-hosted only, open weights | Free (own hardware) | No auth on its local server |
| Detoxify (local) | Already in the binary | Free | Floor: a paid model must clearly beat it |

Figures are from third-party write-ups, not vendor docs; the first
implementation step verifies them.

## 1. Shape and data flow

```
pool.jsonl (unlabelled, PII-scrubbed)            labels.jsonl (human answers)
        │                                                 ▲
        ▼                                                 │
charcoal eval-classifiers run  ──►  verdicts/<model>.jsonl (cached)
        │                                                 │
        ├── disagreements + 10 % agreed sample ──► to-label.jsonl ──► label ─┘
        ▼
charcoal eval-classifiers report ──► per-model table
```

- **`run`** — every pool post through every selected model, in production's
  `format_parent_reply` envelope. Verdicts are cached per (model, post id):
  re-runs and newly added models never re-pay for a post already judged.
- **`label`** (a file, not a UI) — writes `to-label.jsonl`: posts the models
  disagree on, plus a random ~10 % of agreed posts so a blind spot every model
  shares still reaches a human. Blind: model votes are not shown. The 10
  existing smoke labels seed `labels.jsonl`.
- **`report`** — scores each model against the labels.

## 2. Adapters and fairness

Each candidate is a `ToxicityClassifier` implementation returning the existing
`ClassifierVerdict` (toxic yes/no, confidence 0–1, latency). CoPE-B reuses the
RunPod adapter unchanged; Detoxify gets a thin wrapper.

Fairness rules:

1. **Same input** — production's reply + parent envelope for every model.
2. **Same instructions** — decision models get the same policy CoPE-B judges
   against (`gpu/cope-b-runpod/policy.txt`) as their question.
3. **Same threshold** — toxic = confidence ≥ 0.5 for all; raw confidence is
   cached so another cut-off can be scored later without re-running.
4. **Real cost** — tokens actually sent × list price, from one pricing config
   file (`evals/classifiers/pricing.toml`), so a price change needs no code.

Safety:

- New adapters are reachable **only from the eval command**. Production keeps
  CoPE-B until a deliberate switch, which is then one new arm in
  `classifier::build_from_env`.
- **Spending cap per run**, default **$1**, configurable. Hitting it stops the
  run cleanly, keeps every paid verdict, and the next `run` resumes from the
  cache.

## 3. Report, errors, and proof the eval works

**Report, one row per model:**

- false-alarm rate (clean posts called toxic) and miss rate (toxic posts
  called clean), each with a **95 % Wilson interval**;
- error rate — calls that failed or timed out, recorded as errors, **never** as
  clean;
- cost per 1,000 posts; median and p95 latency;
- a verdict line: when two models' intervals overlap, it says **"too close to
  call — label more"** instead of naming a winner.

**Errors:** a failed or timed-out call is an `error` verdict and the run
continues. Missing API keys skip that model with a clear message rather than
failing the run.

**Proof the eval can fail:** the test suite includes two deliberately bad fake
models — always-clean and coin-flip. The report must rank them last and flag
them; if it cannot catch an obviously bad model, it cannot rank good ones.

**Tests** (all against fake classifiers; **CI never calls a paid API**):
metrics and Wilson intervals, the verdict cache, the disagreement picker and
the agreed-sample net, the spending cap and resume, error-as-error handling.

## Data and privacy

- `evals/classifiers/pool.jsonl` and `labels.jsonl` — PII-scrubbed (via
  `find-fixtures`), committed, the same practice as `tests/fixtures/cope_b/`.
- `evals/classifiers/verdicts/` and raw responses — local only, listed in
  `.gitignore.local`.
- Posts go only to the models under test, never to an eval service.

## Out of scope (YAGNI)

- Account-level / tier evaluation.
- A labelling UI — a JSONL file is enough at this volume.
- Replacing Detoxify, MiniLM or the NLI model; this harness can evaluate them
  later, but the first question is CoPE-B.
- Automatic switching of the production classifier.

## First implementation step

Verify from **official** docs, before writing adapters: request format,
whether a free-text policy can be the instruction, batch support, pricing,
rate limits and data-retention terms for Jev and Clef-flash. If either cannot
take a written policy as its instruction, stop and bring it back before
building that adapter.

## Open risks

- **Launch pricing** — Jev's price may not last; the pricing file makes the
  report easy to re-run, but a decision should note the date.
- **Policy length** — if the policy must be re-sent with every post it
  dominates tokens (≈ 60 M tokens per 20 k-post scan ≈ $2.50 on Jev, $5.40 on
  Clef-flash — comparable to RunPod). Native batching or a short label set
  changes this; step one checks.
- **Small label counts** — the Wilson intervals make this visible rather than
  hiding it; the expected outcome of the first rounds may well be "too close to
  call".
