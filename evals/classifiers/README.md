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

**Always run `label --merge` before running `label` again.** `label`
rewrites `to-label.jsonl`, so it now refuses to run while that file holds
answers you typed but have not merged yet - it tells you to run
`label --merge` first. Answers that are already merged, or rows you left
blank, are safe to replace.

**A label belongs to the exact text you labelled.** `label --merge` records a
fingerprint of the post's text (`content_hash`) next to your answer. If a
post's text in `pool.jsonl` changes later, its old label is no longer used:
`label` and `report` print "N labels ignored - post text changed since
labelling; relabel", and `label` offers that post again. Your new answer is
added as a new line and replaces the old one.

## What the spending cap does (and does not) cover

- **The GPU is only woken when there is work and money for it.** Before
  CoPE-B's first request, the run checks which posts still need an answer
  (posts already answered by the same model and policy are reused for free)
  and whether the first batch fits under the cap. If nothing needs judging,
  or the cap cannot cover even one batch, the run stops without contacting
  RunPod at all - so it never pays a cold start for nothing.
- **The $1 cap (`--cap-usd`) is checked before each request, not during it.**
  A request that is already running is allowed to finish, and RunPod keeps a
  worker alive for about 60 seconds of billed idle time after it. So real
  spend can go over the cap by roughly one batch plus that ~60 s of idle time
  for each worker.
- **A request the eval gives up on may keep running on RunPod.** If a batch
  hits the eval's timeout, the eval records those posts as errors and moves
  on, but RunPod may still finish the job, and that time is not counted
  against the cap.
- **The eval's 600-second timeout assumes the default RunPod client
  settings.** It is set above the client's own limits (60 s per request,
  180 s for the first warm-up request, plus its retries). If you raise
  `CHARCOAL_CLASSIFIER_TIMEOUT_MS`, `CHARCOAL_CLASSIFIER_WARMUP_TIMEOUT_MS` or
  `CHARCOAL_CLASSIFIER_MAX_RETRIES`, raise `--timeout-secs` to match, or the
  eval may give up on a request the client is still waiting for.

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
toxic class can reach 20. Every post needs its own `id`: if you append a
post that is already in the pool, every command refuses to load it and names
the two lines with the same id.

## How to read the report

- **Holdout** posts (a fixed third, chosen by hash - you cannot move a post
  into or out of it) are the only ones scored. **Diagnostic** posts - where
  models disagree - appear in a separate section; they are not rates.
- **better / worse**: an exact paired test (McNemar) on the posts where the
  two models disagreed, at the 5 % level. McNemar is a test of whether two
  models' disagreements lean one way more than chance would explain.
- **Wilson intervals** (the ranges shown next to each rate) are the range the
  true rate probably lies in, given how few posts were labelled. They are
  context only; the paired test decides better / worse.
- **too close to call - label more**: the paired test is not significant.
- **not enough holdout labels**: fewer than 20 paired labels in that class.
- **no verdict - coverage below 90 %**: a model answered too few of that
  class to compare fairly.
- **$/1k posts** is an estimate from the list prices in `pricing.json` times
  the average post length - not what the GPU actually cost. Check the
  RunPod billing page for real spend.
- Every model must be in `pricing.json`. CoPE-B's spend is also checked by
  the RunPod client's real cost meter (GPU time used), set to the run's cap -
  with the limits described under "What the spending cap does".
- Known limit: after CoPE-B, the next model's cap is the cap minus the
  *estimated* spend so far (real provider-reported usage arrives in Part B).
- Verdicts live in `verdicts/` and are never committed (its `.gitignore`).
