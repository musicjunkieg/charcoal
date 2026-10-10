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

## What a run needs

CoPE-B calls the paid RunPod endpoint, so it needs all of these set (in
`.env` or the shell):

- `RUNPOD_ENDPOINT_URL` and `RUNPOD_API_KEY` - where and who.
- `CHARCOAL_COPE_B_POLICY_VERSION` - the policy version baked into the deployed
  image. **The run refuses CoPE-B before the GPU wakes if this is unset or not
  a real version**, so a misconfigured run cannot spend money on verdicts that
  could not be trusted.
- Optional: `CHARCOAL_GPU_COST_CENTS_PER_HOUR` and `CHARCOAL_RUNPOD_WORKERS_MAX`
  tune the cost meter exactly as they do in production.

Detoxify needs the local ONNX models (`charcoal download-model`, or
`CHARCOAL_MODEL_DIR=./models`). A model that is missing its keys or files, or
that fails its identity check, is skipped - the rest of the run continues.

## Grow the pool before expecting a verdict

A comparison needs **20 holdout labels in each class** (toxic and clean). The
starting pool of 110 posts yields only 32 holdout posts, so the report will
say "not enough holdout labels" until the pool grows. Add PII-scrubbed posts
with `tools/find-fixtures` (see its README) and append them to `pool.jsonl` -
roughly 600 posts gives a holdout of ~200. If toxic posts are rare in what
you harvest, use `find-fixtures`' list mode on known-hostile accounts so the
toxic class can reach 20.

## How to read the report

- **Holdout** posts (a fixed third, chosen by hash - you cannot move a post
  into or out of it) are the only ones scored. **Diagnostic** posts - where
  models disagree - appear in a separate section; they are not rates.
- **better / worse**: an exact paired test (McNemar) on the posts where the
  two models disagreed, at the 5 % level. The Wilson intervals are context.
- **too close to call - label more**: the paired test is not significant.
- **not enough holdout labels**: fewer than 20 paired labels in that class.
- **no verdict - coverage below 90 %**: a model answered too few of that
  class to compare fairly.
- Every model must be in `pricing.json`. CoPE-B's spend is capped by the
  RunPod client's real cost meter at the run's cap.
- Known limit: after CoPE-B, the next model's cap is the cap minus the
  *estimated* spend so far (real provider-reported usage arrives in Part B).
- Verdicts live in `verdicts/` and are never committed (its `.gitignore`).
