# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

Any publicly visible Bluesky user who posts about topics that attract targeted
hostility. They sign in with their own AT Protocol identity and get their own
threat picture — per-user topic fingerprint, per-user scores, isolated data.

Bryan (@chaosgreml.in) is the operator and one user among many, not the sole
audience. SPEC.md still says "Who is this for? Me (Bryan)"; that is historical.
Production is open to all Bluesky users (`CHARCOAL_ALLOWED_DID` empty = open
access), so multi-tenant concerns — onboarding a stranger who arrives cold,
per-user isolation, explaining results to someone with no context — are core
product truth rather than future work.

A second, narrower role exists: **admin** (`CHARCOAL_ADMIN_DIDS`), which can
view operational state and impersonate a user for support.

## Product Purpose

Charcoal identifies accounts likely to engage with a protected user's content in
a toxic or bad-faith manner, and surfaces them **before** that engagement
happens. Success is a user seeing a credible, evidence-backed picture of who is
likely to come at them, early enough to act.

## Positioning

Four claims a neighboring tool (blocklist, moderation labeler, mute list) could
not truthfully make. All four were confirmed as load-bearing:

- **Predictive, not reactive.** Blocklists and reports act after harm. Charcoal
  scores accounts that have not engaged with the user at all yet.
- **Amplification is the trigger.** It watches quote-posts and reposts — the
  actual escalation vector — and scores the audience that exposure reaches.
  Followers are the least likely source of harassment; they opted in. The danger
  is second- and third-degree exposure.
- **The combination is the signal.** Toxicity alone flags allies who swear.
  Topic overlap alone flags the user's own community. The threat is the
  intersection: active in the same spaces *and* a pattern of hostile engagement.
- **Topics are extracted, not assumed.** The fingerprint is derived from the
  user's actual posting history rather than a category list they pick from, so
  it covers areas they would not think to enumerate about themselves.

## Operating Context

- Users arrive from Bluesky and sign in via AT Protocol OAuth. No password.
- First run is a long job, not an instant result: a scan builds a topic
  fingerprint, discovers amplification events, gathers candidate accounts, and
  scores them. A recent production scan took ~22 minutes for 595 accounts.
  Progress must be legible while it runs.
- A requested scan does not necessarily start immediately. Scans are admitted
  from a queue under a concurrency cap, so a user may be **waiting** before any
  work begins — a real, user-visible state with a queue position and an ETA, not
  a loading spinner. Waiting must be described as waiting; a queued user is not
  being scanned yet, and telling them otherwise makes the first minutes of the
  product a lie. Design and onboarding cannot assume every requested scan begins
  on request.
- Scans are phased (collect → burst → score) and resumable across crashes and
  cost stops, so a user may return to a run in progress.
- Results are reviewed in a **triage queue** as well as browsed as an account
  list and per-account detail.
- Scans cost real money per run (GPU classification), and are metered with a
  per-scan ceiling.
- Deployed at charcoal.watch, with a separate staging environment.

## Capabilities and Constraints

Confirmed and shipped:

- Threat tiers **High ≥ 35 · Elevated ≥ 15 · Watch ≥ 8 · Low < 8**, plus two
  non-scores the UI must represent honestly: **NotAssessed** (the model could
  not assess this account's language) and **Insufficient Data**.
- Topic fingerprint via TF-IDF plus sentence embeddings over the user's posts.
- Amplification discovery via the Constellation backlink index (quotes, reposts,
  likes), covering engagement from accounts that would otherwise be invisible.
- Two-stage toxicity: a local ONNX clean-pass filter, then a self-hosted CoPE-B
  classifier for anything it cannot clear.
- Contextual hostility scoring via an NLI cross-encoder over interaction pairs,
  which distinguishes mockery and contempt from good-faith disagreement.
- Per-account evidence: the amplification events, the signals that fired, and
  graph distance.
- Surfaces today: marketing landing, login, dashboard, account list, account
  detail, triage queue, admin.

Constraints and open decisions:

- **Automated actions are planned, not built.** Charcoal will act on tier
  automatically, and every such action must be **reviewable and reversible**.
  This supersedes SPEC.md's "It does not automate any muting or blocking
  actions" — that line describes the MVP, not the direction.
- Non-English and low-signal accounts are surfaced as NotAssessed / Insufficient
  Data rather than scored, so any tier UI must treat "no verdict" as a
  first-class state and not a variant of Low.
- Scoring calibration is not final; tier thresholds are subject to revision.
- No pricing, plan, or licensing model has been decided.

## Brand Commitments

- Name: **Charcoal**. Domain: charcoal.watch.
- A companion publication exists at https://charcoal.leaflet.pub, linked from
  the landing page.
- No logo, wordmark, color palette, or typographic system was volunteered during
  init — none of it came from a brand the product arrived with.
- The binding design system is now **[DESIGN.md](DESIGN.md)** and its sidecar
  `.impeccable/design.json`, which declare the color system, type ramp, and
  component contracts. They were derived from the shipped interface rather than
  handed down at init, which is why the provenance above still matters: DESIGN.md
  documents what Charcoal became, and it is the source to change when the visual
  system changes.

## Evidence on Hand

Real, and usable in product work:

- Live production data: a scan of the operator's own account produced 595 scored
  accounts across Low / Insufficient Data / NotAssessed / Elevated.
- A written product spec (SPEC.md, partly historical) and a detailed CHANGELOG
  covering what shipped and why.
- An external contribution from Bobby Grayson (@notactuallytreyanastasio),
  PR #1 — correctness and UTF-8 fixes, the first integration-test suite.

Absent — future work must not fabricate these:

- No testimonials, named customers, user counts, or press.
- No published accuracy benchmark or labeled ground-truth evaluation.
- No pricing, uptime, or certification claims.

## Product Principles

1. **Plain language over jargon.** The primary user is not a software developer.
   Scores, tiers, and evidence must be explained in terms a non-technical person
   can act on — never a bare number or an internal metric name.
2. **Accusation-grade care.** The output names real people as likely harassers.
   False positives have a human cost, so evidence travels with every score and
   the design must never encourage acting on a tier alone.
3. **Automation stays reversible.** As automated actions land, each one must be
   reviewable after the fact and undoable. Speed never costs the user the
   ability to disagree with the system.
4. **Predict, then explain.** Surfacing someone before they engage is only
   defensible if the user can see why. The prediction and its reasoning are one
   deliverable, not two.
5. **A stranger must be able to start.** Any user can sign in, so first run has
   to carry someone with no prior context through a long job to a result they
   understand.

## Accessibility & Inclusion

No formal standard (WCAG level) has been established as a requirement.

One product-specific need is confirmed: users are people under targeted
harassment, often reading this while distressed. Comprehension under stress is
an accessibility concern here, not only a copy concern — which is why plain
language is a principle rather than a preference.
