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

**An answer that can no longer be saved is skipped, not blocked.** If you
answer a post and then, before merging, remove it from `pool.jsonl` or
change its text, that answer cannot be saved: it would describe a post that
is gone, or text that is not the post's any more. This holds even if the
text later changes back to something you labelled earlier: the answer is
about the text you saw, not the text that is there now. `label --merge`
skips such answers and prints "N answers not saved — post removed from the
pool or its text changed since you labelled it; holdout and seed posts still
in the pool are offered again by `label`". (A diagnostic post comes back
only if the models' verdicts pick it again.) `label` does not treat skipped
answers as unsaved work, so it can write a fresh file. An answer is never
saved against text other than the text you saw when you answered.

**Changing a saved answer.** If `to-label.jsonl` gives a post a different
answer from the one `labels.jsonl` already holds for the same text, both
`label` and `label --merge` stop with "that post is already in labels.jsonl
as …, but this row answers it differently". Merge stops before saving
anything. To change a saved answer: edit `labels.jsonl` directly, then make
that row's label in `to-label.jsonl` blank (or the same as `labels.jsonl`).
Both steps matter — after editing `labels.jsonl` alone, the old answer still
in `to-label.jsonl` now disagrees with it, and both commands keep stopping
until that row is cleared. An old label for text the post no longer has does
not count: the new answer is saved as a relabel.

**A label belongs to the exact text you labelled.** `label --merge` records a
fingerprint of the post's text (`content_hash`) next to your answer. If a
post's text in `pool.jsonl` changes later, its old label is no longer used:
`label` and `report` print "N labels ignored — post text changed since
labelling; relabel (run `charcoal eval-classifiers label`)", and `label`
offers that post again. Your new answer is
added as a new line and replaces the old one. A post that started as a
hand-picked **seed** label stays a seed when you relabel it, so it never
moves into the scored holdout. `label` always offers such a post again,
shows its source as `seed` in `to-label.jsonl`, and counts it as a seed
(not holdout) in the "N posts to label (… holdout, … seed, … diagnostic)"
line.

Old lines are never thrown away: if a post's text later changes back to text
you labelled before, the newest label you gave for that exact text counts
again, and `label` does not offer the post. A post that was ever a seed is
still scored as a seed.

**A label for a post you removed from `pool.jsonl` is not used either.**
`label` and `report` print "N labels ignored — post no longer in the
pool". The line stays in `labels.jsonl` (nothing is deleted), so if the
post comes back with the same text, its label counts again.

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
- **Reaching the cap stops paid models, not free ones.** When one model hits
  the cap - whether the eval's own estimate or CoPE-B's real cost meter
  stopped it - the whole cap counts as used, and the run moves on to the next
  model. A model that costs nothing (Detoxify runs on this machine) still
  runs; every later paid model stops before waking anything. (The eval's
  running total is only an estimate, and the real meter can reach the cap
  well before it, so the remaining "headroom" cannot be trusted.) The
  "estimated spend this run" line still shows the eval's own estimate of
  what it asked for; it does not jump to the cap when the cap is reached.
- **A paid model that fails its identity check also ends paid spending.**
  That check may already have woken (and billed) the GPU, so the run treats
  it like reaching the cap: free models still run, every later paid model is
  refused before it wakes anything. A free model failing its check changes
  nothing.
- **Each model may be named once.** `--models cope-b,cope-b` is refused
  before anything runs: running a model twice in one run has no use, and the
  second pass could wake the GPU again.
- **A request the eval gives up on may keep running on RunPod.** If a batch
  hits the eval's timeout, the eval records those posts as errors and moves
  on, but RunPod may still finish the job, and that time is not counted
  against the cap.
- **The eval's 600-second timeout assumes the default RunPod client
  settings.** It is set above the client's own limits (60 s per request,
  180 s for the first warm-up request, plus its retries). If you raise
  `CHARCOAL_CLASSIFIER_TIMEOUT_MS`, `CHARCOAL_CLASSIFIER_WARMUP_TIMEOUT_MS` or
  `CHARCOAL_CLASSIFIER_MAX_RETRIES`, raise `--timeout-secs` to match, or the
  eval may give up on a request the client is still waiting for. The same
  timeout bounds CoPE-B's identity check: one that takes longer counts as a
  failed identity check (see below).

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
that fails its identity check, is skipped - no verdicts were bought from it,
so the rest of the run continues. (A missing key or file is caught before
anything is contacted; a failed CoPE-B identity check, though, has already
woken the GPU, so it may have cost one cold start plus the ~60 s idle tail.)
A model is also stopped and skipped when **every post in a batch** comes back
from another model or policy than the one declared - the deployment itself is
wrong, so more batches would only buy unusable answers. That batch's answers
are kept as errors. A single stray post in an otherwise good batch is just that
post's error, and the run goes on. Either kind of skip of a paid model ends
paid spending for the rest of the run; free models still run. **Any other error stops the whole run** with a
non-zero exit - for example a verdict file in `verdicts/` that cannot be read
or written. That can happen right after a paid batch, so carrying on would
keep spending while losing what was paid for. Fix the file problem, then run
again: verdicts already saved are reused.

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
  RunPod billing page for real spend. Failed calls count too (a failed call
  is still paid for), so a model whose calls all failed still shows a cost;
  the p50/p95 latency columns use successful calls only.
- Every model must be in `pricing.json`. CoPE-B's spend is also checked by
  the RunPod client's real cost meter (GPU time used), set to the run's cap -
  with the limits described under "What the spending cap does".
- Known limit: after CoPE-B, the next model's cap is the cap minus the
  *estimated* spend so far (real provider-reported usage arrives in Part B).
- Verdicts live in `verdicts/` and are never committed (its `.gitignore`).

## CoPE-B-S1 and reasons (#407)

Two more models call Zentropi's hosted CoPE-B-S1 with `ZENTROPI_API_KEY`.
The hosted API is free for development, so they are priced $0 and keep running
after a paid model reaches the cap.

- `s1-master` asks only your master policy (`gpu/cope-b-runpod/policy.txt`).
- `s1-bundle` asks the master policy plus six reason policies in one request.
  The master alone decides toxic or clean; the reasons only explain.

Compare `s1-bundle` with `s1-master` in the report to see whether bundling hurts.

**Ticking reasons.** In `to-label.jsonl`, a toxic post may list the reasons
that apply: `"reasons": ["mockery", "threat"]`. Valid reasons are
`personal_attack`, `identity_hate`, `mockery`, `threat`, `pile_on`, and
`bad_faith`. Reasons on a clean post, or an unknown name, stop `--merge`
before anything is saved. To add reasons to a post that is already in
`labels.jsonl`, edit that line directly (`--merge` skips answers it already has).

**Editing a reason policy.** The policies live in `evals/classifiers/reasons/`.
Editing one changes `s1-bundle`'s policy version, so the next run re-asks every
post for `s1-bundle` only. `s1-master` answers are reused.

**The REASONS section** of the report (holdout only):
- flagged with no reason: a high count means a family is missing or too narrow;
- clean calls where a reason still fired: a high count means a reason policy is too broad;
- top reason is one you ticked, and per family how often it fired when you did and did not tick it.
  A family with fewer than 10 ticked posts shows "too few examples".
