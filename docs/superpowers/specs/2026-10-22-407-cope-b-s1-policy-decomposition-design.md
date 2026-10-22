# #407 — CoPE-B-S1 in the classifier eval, with explainable reasons

Status: design approved in brainstorming 2026-10-22; awaiting spec review.
Builds on: #404 classifier eval (`docs/superpowers/specs/2026-10-08-404-classifier-eval-design.md`,
`evals/classifiers/README.md`).
Research: chainlink #407 comment (model card verified 2026-10-22).

## Goal

Find out whether Zentropi's CoPE-B-S1 can (1) judge posts at least as well as
today's CoPE-B against Bryan's master policy, and (2) tell the protected user
*why* a post was flagged, by naming which kind of hostility it contained.

Audience for reasons: **the protected user, inside Charcoal's UI.** Reasons may
be fine-grained and are private. They are never published as AT Protocol labels
in this work.

Out of scope: any production change. Everything here lives in the eval
harness (`src/cli/eval_classifiers/`). Production adoption is a later decision
that this eval informs.

## What S1 is (facts this design depends on)

- A multi-question model: up to **26 yes/no questions per request**, each with
  its own policy text, answered together.
- It returns probabilities only — **no written rationale**. So an explanation
  can only come from *which* questions said yes.
- Zentropi's model card: bundle **distinct** concepts. Overlapping shades of
  one concept in the same request cost accuracy. Most accurate mode is one
  question per request.
- Hosted API: `POST /v1/multilabel`, `model: "cope-b-s1"`, `content_text`, and
  1–26 `labelers` (`key` + `criteria_text`). Returns a 0/1 label plus a
  confidence (0.5–1.0) per labeler, not the raw probability. Marked "specs may
  change". **Free for development use** (Bryan, 2026-10-22).
- Self-host option: container `zentropiai/cope-s1-sglang:v0.1.6` (no `latest`
  tag), System One format (`POST /v1/systemone`), needs an 80 GB GPU. Same
  format as Jev/Clef, so a System One client would serve #404 Part B too.

## Design

### 1. The master policy decides; six reasons explain

Each post gets **one request with seven questions**:

| Key | Question | Role |
|---|---|---|
| `master` | `gpu/cope-b-runpod/policy.txt`, unchanged | **Sole decider** of toxic / clean |
| `personal_attack` | Personal attack | Reason only |
| `identity_hate` | Identity-based hate | Reason only |
| `mockery` | Mockery & condescension | Reason only |
| `threat` | Threats & wishing harm | Reason only |
| `pile_on` | Rallying a pile-on | Reason only |
| `bad_faith` | Bad-faith engagement | Reason only |

How the master policy's ten "output 1" bullets map to the families:

| Master bullet | Family |
|---|---|
| Personal attack | `personal_attack` |
| Identity attack or hate | `identity_hate` |
| Belittling or mocking; contemptuous dismissal; patronizing; passive-aggressive | `mockery` |
| Threats or intimidation; endorsing/wishing violence or harm | `threat` |
| Inciting or rallying others | `pile_on` |
| Bad-faith concern trolling / sea-lioning | `bad_faith` |

Why six families, not ten: the four "mockery" bullets are the "overlapping
shades of one concept" the model card warns against bundling. Merged, they are
one distinct concept. Separately, they would blur each other and could pull
down the master question in the same request.

Why the master stays whole rather than "toxic = any reason fired": the master
policy's exclusions (criticising ideas, non-participant targets, quoting,
venting) and its "decisive defaults" are where most of its accuracy lives.
Rebuilding them from parts would change what counts as toxic. The reasons
only explain a decision the master already made.

### 2. Reason policies (draft — Bryan edits the wording)

They only answer "is *this kind* of hostility present?" They do **not** repeat
the master policy's exclusions. Each non-identity family keeps a one-line
target clause, so that a reason attaches to the right thing in a post that
both attacks a participant and insults a politician.

All six use S1's standard policy instructions ("Does the content meet the
criteria described in the following policy?").

- **personal_attack** — The content insults, name-calls, or attacks the
  character, honesty, motives, worth, or intelligence of a person taking part
  in the conversation (someone writing or being addressed). Criticising only
  their ideas or claims does not meet this.
- **identity_hate** — The content uses slurs, dehumanizing language, mockery,
  or accusations of criminality, predation, or perversion against people
  because of their gender, trans status, race, body size, sexuality,
  disability, or religion. This applies whether it targets one person or the
  whole group.
- **mockery** — The content belittles, mocks, patronizes, or shows contempt
  for a person taking part in the conversation. This includes sarcastic
  trivialization, condescension, dismissive disdain, and insincere praise
  hiding a barb.
- **threat** — The content threatens or intimidates a person taking part in
  the conversation (directly, conditionally, or in veiled terms), or endorses,
  celebrates, or wishes violence or harm on them.
- **pile_on** — The content encourages or rallies others to pile onto,
  harass, or target a person taking part in the conversation.
- **bad_faith** — The content uses concern trolling, "just asking questions",
  or sea-lioning whose real purpose is to demean or harass a person taking
  part in the conversation.

Each reason policy is stored in its own file under
`evals/classifiers/reasons/<key>.txt`. Its version is a hash of its text (see
§4.6 for what an edit re-asks).

### 3. Where S1 runs: hosted first

- **Phase 1 (this work):** the hosted `/v1/multilabel` API with
  `ZENTROPI_API_KEY`. No deployment and no GPU. It is free for development.
  Its 0/1 + confidence output is enough to compare models; it is not enough
  to tune thresholds.
- **Phase 2 (separate issue, only if Phase 1 says S1 earns it):** self-host
  on RunPod through a System One client, shared with #404 Part B. That gives
  raw probabilities and privacy. It needs a thin queue-handler wrapper,
  because the container is a plain web server.

### 4. Harness changes

1. **Two new eval models** in `build_eval_model` (`models.rs`):
   - `s1-master` — one labeler (the master policy). Model card's
     most-accurate mode.
   - `s1-bundle` — seven labelers, master + six reasons, one request per post.

   Both answer toxic/clean from the `master` labeler. Running both on the same
   posts measures what bundling costs.
2. **A hosted multilabel client** (`src/toxicity/zentropi_multilabel.rs`), eval
   use only: builds the request, sends one post per request, and parses
   strictly (see §5).
3. **Reasons travel with each verdict.** The eval's saved verdict gains an
   optional `reasons: { key: { fired: bool, confidence: f32 } }`, plus
   `reasons_complete: bool`. Existing models leave it absent; old verdict
   files still load (`#[serde(default)]`). The production `ClassifierVerdict`
   is **not** changed. The S1 client implements `ToxicityClassifier` for the
   master answer and exposes reasons through an eval-side method.
4. **Labels gain optional reasons.** `Label` gets
   `#[serde(default)] reasons: Vec<String>`. These are the family keys Bryan
   ticks, and they are only allowed on `toxic` labels. A reason on a `clean`
   label, or an unknown key, is refused at `label --merge` with the line
   named. `to-label.jsonl` shows the six keys as a hint.
5. **`pricing.json`** gets `s1-master` and `s1-bundle` at `0.0` (free for
   development). The report notes that this is development pricing, not a
   production cost.
6. **Cache key** includes each labeler's policy version: master policy hash
   plus each reason file's hash. Editing a reason invalidates `s1-bundle`
   answers only. Because the hosted API asks all labelers in one request,
   `s1-bundle` re-sends the whole request. That is free, so it is
   acceptable. The `s1-master` answers are reused untouched.

### 5. Error handling

- **Rate limits / transient failures:** retry with backoff (the existing
  `backon` pattern). Once retries are exhausted, the post is recorded as an
  **error, never as clean**. The existing 90 % coverage rule keeps a model
  with too many errors from getting a verdict.
- **Unexpected response shape** (the preview API may change): a missing or
  malformed field is an error, never a silent "no". If `master` parses but a
  reason does not, keep the master verdict and set `reasons_complete: false`.
- **Response for the wrong model** (`model` echoed is not `cope-b-s1`): treat
  it like the existing identity mismatch. If every post in a batch disagrees,
  stop the model.
- **Master clean, reasons fired:** the post is not flagged. The report counts
  it as "reasons on clean posts", per family. A high count means that
  reason's policy is too broad.
- **Master toxic, no reason fired:** the report counts it as "unexplained
  flags". In a future UI it would read "flagged under your overall policy; no
  specific reason matched". A high count means a family is missing or too
  narrow.
- **Privacy:** posts already go to Zentropi through the existing Zentropi
  classifier. The hosted test sends nothing anywhere new.

### 6. How we judge it (the report)

Three questions, all on **holdout** posts only:

1. **Is S1 as good a judge?** `s1-master` vs `cope-b` (and `detoxify`) on
   toxic/clean, using the existing McNemar comparison and the 20-per-class
   minimum.
2. **Does bundling hurt?** `s1-bundle` vs `s1-master` on the master answer,
   same paired test. If bundling is worse, the fallback is the model card's
   "screen broadly, then confirm" pattern: master alone, then reasons asked
   only for flagged posts.
3. **Are the reasons right?** On holdout posts Bryan labelled toxic *with*
   reasons:
   - **top-reason agreement**: S1's highest-confidence fired reason is one
     Bryan ticked;
   - **per family**: fired-when-ticked rate and fired-when-not-ticked rate,
     with Wilson intervals;
   - **unexplained flags** and **reasons on clean posts** (§5).

No pass/fail threshold is set for question 3 yet. With ~200 holdout posts, a
rare family (e.g. `pile_on`) may have too few examples to judge. The report
says "too few examples" below 10 ticked posts for a family, rather than
printing a rate.

### 7. Testing (TDD)

Unit tests, written before code:
- Request builder: correct `model`, keys, and criteria text; 1 labeler for
  `s1-master`, 7 for `s1-bundle`.
- Strict parser: a good response; a missing `master` (error); a missing
  reason (master kept, `reasons_complete=false`); a wrong model; a
  non-0/1 label (error).
- `Label` reasons: a round-trip; old lines without `reasons` load; reasons on
  a `clean` label are refused; an unknown key is refused.
- Cache key: editing one reason file changes only that reason's version.
- Report: top-reason agreement, per-family rates, unexplained-flag and
  reasons-on-clean counts, and "too few examples", on hand-built fixtures.

Integration: a mock HTTP server (the pattern the existing Zentropi tests use)
for retry-then-error and a 429 sequence. No test calls the real API.

## Rollout

1. Build the harness changes (this spec's plan).
2. Run `s1-master` + `s1-bundle` on the current pool. This is free, so it can
   run before the pool grows, to shake out errors.
3. Pool growth to ~600 (already pending for #404). Bryan labels, ticking
   reasons on toxic posts.
4. Read the report. Then decide on Phase 2 self-hosting / production in a new
   issue.

## Open risks

- The hosted API is a preview; its shape or free tier may change mid-eval.
  Strict parsing makes a change loud rather than silent.
- Hosted 0/1 + confidence hides the raw probability, so threshold tuning
  waits for Phase 2.
- Labelling reasons adds effort per toxic post. It is optional per post, and
  only labelled posts count toward question 3.
