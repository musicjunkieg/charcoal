# How Charcoal's Classifier & Scorer Work, and How We Size Zentropi Usage

> Audience: engineering/product at Zentropi. Assumes familiarity with ML
> classification but not with Charcoal's codebase. Written to explain where
> Zentropi sits in our pipeline and how much traffic a single protected
> account generates.

## 1. What Charcoal is doing at a high level

Charcoal protects a single Bluesky user (the "protected account") by finding
other accounts likely to harass them. For each candidate account we produce a
**threat score** (0–100) and a **tier** (Low / Watch / Elevated / High). That
score is a blend of three signals:

- **Toxicity** — how often this account is actually hostile (this is where
  Zentropi comes in)
- **Topic overlap** — how semantically close their interests are to the
  protected user's
- **Behavioral/graph signals** — quote/reply ratios, pile-on participation,
  engagement, and social-graph distance

The key design principle: topic proximity alone is *not* a threat. An ally who
posts supportively about the same topics should score Low. The threat signal is
the **combination** of topical proximity and a **pattern of hostile behavior**.
Zentropi is what lets us measure "hostile behavior" accurately — which is why
it sits at the center of the system.

## 2. Why we use Zentropi at all (the two-stage classifier)

Our local toxicity model (an ONNX Detoxify model, `unbiased-toxic-roberta`) has
a well-known failure mode: it keyword-triggers on identity terms. "Fuck yeah,
fat liberation!" and "fat people are disgusting" both score ~0.95. Trusting
that model's high scores would systematically false-positive on exactly the
communities we're trying to protect.

So we treat the two models asymmetrically:

- **ONNX is trustworthy only for *low* scores.** A post scoring < 0.10
  genuinely contains no hostile language or identity terms — that's a reliable
  "obviously clean" signal.
- **ONNX high scores are not trusted at all.** Anything that isn't obviously
  clean gets handed to Zentropi, whose conversation-scoped policy can tell ally
  use of identity language from hostile use, third-party venting from a direct
  attack, and legitimate disagreement from harassment.

This gives us a **two-stage scorer**:

- **Stage 1 — ONNX clean-pass filter (free, local, no API calls).** Every post
  is scored locally. Posts below the 0.10 clean threshold are cleared and never
  sent to Zentropi.
- **Stage 2 — Zentropi binary classification.** Every post at or above 0.10 is
  sent to Zentropi's `/v1/label` endpoint, which returns a binary verdict
  (1 = toxic, 0 = safe) plus a confidence. **ONNX never contributes to the
  toxic count** — it only *subtracts* from the pool Zentropi has to look at.
  All actual toxicity decisions are Zentropi's.

A few operational details on our side of the integration:

- We call your pre-built **labeler** (`labeler_id`, optionally pinned to a
  `labeler_version_id`) rather than sending the full policy text per request.
- **Replies are sent as `[Parent post] / [Reply]` pairs** so the
  conversation-scoped policy can judge whether a reply is hostile toward the
  person it's answering. Originals and quote-posts are sent as solo text.
- Concurrency is capped at **4 in-flight requests** per account batch, with up
  to **3 retries** (exponential backoff) on 5xx/429/network errors.
- If no Zentropi key is configured, the whole thing degrades to ONNX-only with
  a 0.50 threshold — **zero calls**. Zentropi is the production path, not a
  hard dependency.

## 3. How one account gets scored (the per-account flow)

For each candidate account we run an **adaptive, staged** profile build so we
don't pay full cost on accounts that are obviously irrelevant:

**Stage 1 — cheap triage (25 posts, 0 Zentropi calls).**
We fetch ~25 recent posts (replies *and* originals), ONNX-score them locally,
and compute a quick topic-overlap estimate. If the account is **both** clean
(all first-person posts below 0.10) **and** topically irrelevant (overlap below
the gate), we stop here and classify it Low. This early-exits an estimated
**50–60% of accounts** at zero Zentropi cost.

**Stage 2 — full analysis (up to 50 posts, this is where Zentropi is called).**
For accounts that survive triage, we fetch up to 50 posts, ONNX-score all of
them, and **send every post scoring ≥ 0.10 to Zentropi**. From Zentropi's
binary labels we compute a **reply-weighted toxicity rate** (replies weighted
70%, originals 30%, since harassment shows up in replies, not original posts).
That rate feeds the threat formula:

```
raw_score = toxicity_rate × 70 × (1 + topic_overlap × 1.5)
```

then modified by behavioral signals, an "ally gate," a context multiplier (from
a separate local NLI model), and graph distance.

**The number of Zentropi calls for one account = the number of its (≤50) posts
that clear the 0.10 ONNX filter.**

That count depends heavily on the account's topic space:

- **Off-topic, clean accounts** (tech, cooking, sports): most posts score
  < 0.10, so **5–15 calls** — or **0** if they early-exit at Stage 1.
- **Accounts in identity-adjacent spaces** (the ones we most need to evaluate):
  ONNX flags identity terms routinely, so **40–60% of 50 posts** go to Zentropi
  → roughly **20–30 calls**.

One cost wrinkle worth flagging: for **followers that score above the Watch
threshold (≥ 8.0)**, we re-run the full profile build a second time to add NLI
context scoring. That second pass independently re-runs Stage 1 + Stage 2, so
**borderline/hostile followers can cost ~2× their Zentropi calls.** This affects
only the minority of accounts that already look threatening.

## 4. How many calls a single protected account generates per scan

A scan for one protected account fans out across two populations:

**(a) Amplifiers.** We pull, from the Constellation backlink index plus reply
threads, everyone who **quoted, reposted, liked, or drive-by-replied** to the
protected user's ~50 most recent posts. Every unique amplifier gets a full
profile build.

**(b) Followers of hostile amplifiers.** For **quote and reply amplifiers
only** (reposts and likes are lower-signal and skipped), we fetch up to **50
followers each** and score them too. This is the predictive part — the people
in a quote-dunker's audience are the likely next harassers.

So, per scan:

```
accounts_scored ≈ (unique amplifiers)
                + 50 × (quote/reply amplifiers)
```

minus any account already scored within the last **7 days** (we skip fresh
scores). Concretely, a protected account with, say, 10 amplifiers (4 of them
quote/reply) fans out to roughly **10 + (4 × 50) = ~210 candidate accounts** in
one scan — though deduplication and the freshness skip typically bring the
scored set well below that.

Then, **per account**, Zentropi calls follow the Stage-1/Stage-2 logic above.
Putting it together for that example:

- ~210 candidates → ~50–60% early-exit at Stage 1 (**0 calls each**)
- ~90 reach Stage 2 → averaging, say, ~15–25 Zentropi calls each
- → on the order of **1,500–2,500 Zentropi calls for a single full scan** of
  one protected account, with a long tail from the 2× re-scoring on above-Watch
  accounts.

The two biggest levers on that number are entirely in our control and worth
calling out:

1. **The 0.10 ONNX clean-pass threshold** — raise it and fewer posts reach
   Zentropi (at some recall cost).
2. **`max_followers_per_amplifier` (default 50) and the 7-day freshness
   window** — these set the fan-out and how often we re-pay.

## 5. The one thing we'd want to validate with you

The architecture deliberately sends **more** traffic to Zentropi than a naive
"ambiguous band" design would, because we refuse to trust ONNX's high scores.
For accounts in identity-adjacent topic spaces that means 40–60% of their posts
hit your API. The open question for us is **throughput/rate limits on your side
at steady state** — a single active protected account is ~1,500–2,500 calls per
scan, and the product vision is many protected users scanning on a daily
cadence. We'd like to understand your free-tier and paid-tier ceilings so we
can size this correctly (our documented fallback if needed is self-hosting
CoPE, but we'd much rather stay on your API).
