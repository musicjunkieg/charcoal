# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Fixed
- Clear `scan_skips` when an account is deleted (#234). `delete_user_data`
  cleared every user-scoped table except `scan_skips` (added in v10, #226), so a
  deleted account left behind the user's DID, the DIDs of accounts scanned on
  their behalf, and raw error text. Charcoal's users are people being harassed;
  "delete my account" has to mean it. Fixed in both backends, inside the
  existing Postgres transaction, with coverage on **both** — the SQLite test
  alone would prove nothing about production, which runs Postgres.
- Bound Constellation requests with a 30s request timeout and a 10s connect
  timeout (#235). `ConstellationClient` set neither, so a server that accepts a
  connection and then never answers would hold one of the eight discovery
  concurrency slots for the life of the process, with no error to show for it.
  Discovery could quietly stop making progress. Regression test asserts a
  stalled request gives up on its own rather than hanging.
- Stream model downloads to disk instead of buffering them in memory (#233).
  `download_file` called `response.bytes()` — despite a comment claiming it
  streamed — holding the entire artifact in RAM before a blocking
  `std::fs::write`. With the fp32 NLI model (#231) that is ~284MB resident per
  download, on the async runtime. Worse, a crash mid-write left a **truncated
  file at the destination**, and `nli_files_present` only checks existence, so a
  partial `model.onnx` would look present and then fail ONNX parsing on every
  boot — a poison state needing manual cleanup on the Railway volume. Downloads
  now stream chunk-by-chunk to a sibling `.part` file and atomically rename into
  place, so the destination only ever holds a complete artifact. The sibling
  location keeps the rename on one filesystem, which matters because `rename(2)`
  does not fall back to copying across filesystems — it fails with `EXDEV`. A
  staging file under the system temp dir would therefore break the download
  outright wherever `/tmp` and the volume are separate mounts, which on Railway
  they are. The progress bar also advances during the transfer now rather than
  jumping to 100% at the end.
- Switch the NLI cross-encoder to the **fp32** export and fix corrupted
  contextual hostility scores in production (#231). The quantized
  `nli-deberta-v3-xsmall` export is *dynamically* quantized:
  `DynamicQuantizeLinear` derives a single per-tensor activation scale at
  runtime from the min/max of the whole `[batch, seq, hidden]` tensor. Batching
  the 5 hypotheses (#213) therefore put rows with different content into one
  tensor, changing that scale and shifting **every** row — including rows that
  were never padded. Attention masking keeps pad positions out of attention
  scores but not out of the tensor whose range sets the scale. On x86-64 without
  VNNI, ONNX Runtime's `u8s8 MatMulInteger` path amplified the perturbation
  roughly 20x over ARM, so **production was on the badly-affected side while the
  dev machine was not**. Measured on Linux x86_64: a mocking reply ("lol imagine
  being that big" → "fat people deserve healthcare too") scored `0.132`
  unbatched and `0.000` batched — the hypothesis ranking inverted so *support*
  outranked *attack*, erasing the signal. Error direction was suppression, on
  the exact harassment shape this tool exists to catch.

  The mechanism was isolated with a batch-of-2 of *identical* rows (no padding,
  no heterogeneity), which is bit-exact on both platforms, while any
  heterogeneous batch diverges. The fp32 export has no quantization ops, so
  batching is now **exact** (`+0.000000` on all five hypotheses on both
  platforms) and NLI is finally reproducible between a dev Mac and production —
  the quantized model gave `0.122` on ARM vs `0.172` on x86 for the same input,
  so dev numbers never predicted prod numbers, batching aside.

  Costs 284 MB instead of 87 MB (Railway volume 1.4/50 GB, staging scan-peak RAM
  3.8 GB against a 32 GB ceiling, ~$2/mo). It is nonetheless *faster* than the
  alternative fix: fp32-batched runs 92.7 ms/pair versus 112.3 ms/pair for
  reverting to unbatched-quantized. **Context scores will shift on redeploy** —
  the previous values were wrong, not merely noisy. `nli_files_present` now looks
  for `model.onnx`, so existing volumes re-download automatically and the stale
  quantized file is removed. The equivalence test is un-quarantined and its
  tolerance tightened from `0.02` to `1e-4`; at that bound it fails against the
  quantized model on macOS too, so this class of defect no longer needs x86 to
  surface.
- Abstain from NLI context scoring on unassessable-language pairs (#230) —
  the English-only MNLI cross-encoder returned noise on non-English text,
  which `context_multiplier` turned into up to a 1.5x threat-score inflation.
  The gate now lives inside `score_pair`, so all three NLI seams are covered,
  including the Mode B inferred-pair path. Amplifier toxicity scoring abstains
  on the same basis and the progress line reports `[tox: n/a — language]`
  instead of a misleading `[tox: 0.00]`.

### Changed
- Audit the authed app surfaces against DESIGN.md (#246)
- Integrate main into staging: template backfill commits #70-#75 conflict, blocking the promotion PR #63 (#239)
- Verify danabra.mov re-scan 2026-07-20 (post-#224) (#229)
- Railway drops scan logs at 500/sec — observability gap during scans (#226)
- Diagnose degraded=true on the 8174-account staging scan (2026-07-19) (#220)
- Pre-commit hook no longer stages the gitignored `.chainlink/issues-export.json`. The file is also untracked, so the `.gitignore` entry can finally apply — a gitignore rule has no effect on an already-tracked file, which is why it kept conflicting on every branch integration. (`--no-verify` remains an emergency bypass, unrelated to this.) (#181)
- Batch the 5 NLI hypotheses into one padded `[5, max_len]` forward pass instead of 5 sequential single-item inferences — ~5× fewer NLI ONNX runs, biggest in the amplification event loop (NLI per event). NOTE: the quantized `nli-deberta-v3-xsmall` export is not perfectly padding-invariant, so batching shifts `context_score` by a small, systematic amount — **measured on macOS ARM64 only** (≈0.006 on the final hostility, ≈0.002–0.008 per hypothesis), and accepted *on that platform* as within the model's own quantization noise and immaterial to threat tiers (bands 8/15/35). A model-gated unit test at a 0.02 tolerance was intended to pin the batch-vs-single equivalence, but it never actually executed in CI (it read `default_model_dir()` while CI sets `CHARCOAL_MODEL_DIR`), so **nothing has ever enforced this bound** (#213). **CORRECTION (#231):** on Linux x86_64 — the platform production runs on — the same model bytes diverge by **0.14** on hypothesis 0 (batched 0.031 vs single 0.172), far outside that tolerance. The equivalence claim was therefore never verified where it matters. **RESOLVED (#231):** the cause was the quantized export's runtime per-tensor activation scale, not padding; the fp32 export makes batching exact on both platforms, and the equivalence test is un-quarantined at a `1e-4` tolerance. The batching speedup here was also overstated — measured 1.69x, not ~5x (#213)

### Added
- Add handle typeahead to the login screen (proxied via backend) (#227)
- Onboarding scan progress + live threat visibility in web UI (#1)
- Batched RunPod classifier — the burst phase now sends **N post texts per `/runsync` request** instead of one, so vLLM's continuous batching (`max_num_seqs=32`) does the on-GPU parallelism and the queue-bound warm-idle waste (RunPod `delayTime` ~3-4s vs `executionTime` ~0.13s) collapses toward the compute floor — targeting ~$1/onboarding vs the prior ~$6-10. Handler and Rust client are batch-only (`{"input":{"contents":[…]}}` → `{"output":{"verdicts":[…]}}`); a post that fails to decode is recorded as an explicit benign `decode-error` sentinel (fail-open, logged + metered + scan `degraded`) rather than failing the batch or livelocking resume. Additive `classify_batch`/`max_batch_size` on the classifier trait keep Zentropi 1-per-call. New env: `CHARCOAL_RUNPOD_BATCH_SIZE` — texts per RunPod request (default 32 = handler `max_num_seqs`, clamped 1–128); in-flight texts ≈ `CHARCOAL_BURST_CONCURRENCY` × this (#186)
- Classification burst decouple — scans now run **collect → burst → score**: Phase A gathers posts and runs the ONNX clean-pass (no classifier), Phase B drains a DB-staged queue through the classifier in **one contiguous burst window** (making the `ScanCostMeter` measure real burst cost, not wall-clock), Phase C scores from stored verdicts. Backed by schema v9 (`classification_queue` + `scan_account_input`) and a `scan_phase` marker for crash-/402-resumability. The adaptive two-pass NLI gate (`raw_score >= 8.0`) moves into Phase C; behavior is locked by a golden test. Env: `CHARCOAL_BURST_CONCURRENCY` (16), `CHARCOAL_BURST_BATCH` (500) (#208)
- Per-scan RunPod cost backstop (`ScanCostMeter`) enforced at the per-call boundary — `elapsed × rate` metering hard-stops a runaway scan before disaster spend; on by default ($5 ceiling), only `CHARCOAL_SCAN_COST_CEILING_CENTS=0` disables (#206)
- Phase 6.7 — Staging gate (grimalkina re-scan) (#195)
- Phase 6.4 — A/B harness + shadow-agreement gate (#192)
- Phase 6.3 — Rust trait + RunPodCopeBClient + ZentropiClient refactor (#191)
- Phase 6.1 — A/B shadow-agreement sample + smoke set harvested for CoPE-B (#189)
- Phase 6.2 — RunPod GPU service (Dockerfile + handler + smoke) (#190)
- Integrate Zentropi CoPE as production toxicity classifier (#173)
- Wire topic-first discovery into sweep pipeline with --sweep-mode flag (#172)
- Wire adaptive sampling early exit into build_profile (#171)
- Add Zentropi CoPE API spike for binary toxicity validation (#159)
- Replace OpenAI Moderation with Groq GPT-OSS-Safeguard as ensemble secondary scorer (#154)
- Admin: match pre-seeded data to user on first OAuth login (#107)
- Admin: impersonation view — see any protected user's scored accounts (read-only) (#106)
- Admin: trigger scan for any protected account (#105)
- Admin: pre-seed protected account by handle (resolve DID, build fingerprint/embeddings) (#104)
- Phase 1.5: Admin dashboard — pre-seed protected accounts, trigger scans, impersonation view (#103)
- Wire NLI context scoring into scan pipeline (Task 17) (#102)
- Merge graph distance + ensemble branch to staging (#144)
- Add graph distance scoring and ensemble toxicity scorer (#136)
- Show context_score on account detail page (#111)
- Implement NLI scoring redesign (#109)
- v0.4 AT Protocol OAuth — Task 1: Add Cargo dependencies (#95)
- Wire NLI contextual scoring into profile building pipeline (#101)
- Add Postgres migration v5 and PgDatabase trait implementations (#100)
- Add frontend label components, review page, and accuracy dashboard (#99)
- Add label API endpoints (POST label, GET review, GET accuracy) (#98)
- Wire likes and replies into amplification scan pipeline (#96)
- Auto-fingerprint user on first scan via web UI (#122)
- Add onboarding tutorial for new users (#113)
- Add GitHub Actions CI and branch protection for main (#120)
- Update README documentation for new M4 Pro Mac Mini setup (#119)
- v0.4 AT Protocol OAuth: replace password auth with Bluesky sign-in (#50, #95)
  - PAR + PKCE + DPoP + private_key_jwt via `atproto-oauth` crate
  - DID-embedded session cookies with CHARCOAL_ALLOWED_DID gate
  - Stable P-256 signing key derived from CHARCOAL_SESSION_SECRET
  - AT Protocol tokens stored in-memory for future XRPC calls

### Changed
- Batch amplification event inserts into a single batched write instead of one round-trip per event — the amplification event loop was ~2m16s of a 28m24s scan at 359 sequential inserts. Postgres uses one `UNNEST` round-trip at any batch size; SQLite uses one transaction chunked at 100 rows per statement. New `Database::insert_amplification_events_batch` (#216, chainlink)
- Add vitest harness + TDD untested frontend logic (#3)
- Mutation pass: prove new backend tests can fail (#2)
- Production GPU capacity risk: US-GA-2 serverless is H100-only and very low capacity (#205)
- Merge PR #57 to staging + bump RunPod endpoint workersMax 0->3 (#204)
- Lower classify-gate AGREEMENT_THRESHOLD 0.90->0.85 (accept ~89% model-agreement ceiling) (#203)
- Set RunPod+Zentropi env vars on Railway staging before #54 merge (#202)
- Phase 6 cold-start opts: safetensors prefetch + persist compile cache + init timeout (#201)
- Triage CodeRabbit review on PR #54 (Phase 6 self-host) (#199)
- Phase 6.0 — audit_log generalization preflight (#188)
- Pull Zentropi call count from last full scan of grimalkina.bsky.social (#182)
- Investigate Zentropi API call concurrency pattern (#181)
- PR #46 CodeRabbit review: verify and fix findings (#175)
- Branch sync: ff staging, push topic-first-discovery, prune gone branches (#174)
- Write NLI scoring redesign implementation plan (#108)
- Axum web server skeleton (Railway deployment) (#51)
- AT Protocol OAuth integration for web UI (#50)
- Multi-user schema redesign (per-user vs shared data) (#49)
- Multi-user schema redesign (per-user vs shared data) (#49)

### Fixed
- Abstain from toxicity scoring on text our English-only models can't assess, instead of silently scoring it benign (#222). Charcoal's toxicity models (Detoxify ONNX gate + CoPE-B) are validated for English only; handed non-English text they emit a near-zero score the multiplicative threat formula reads as "safe", so every non-English account exited the pipeline as confidently Low — a harasser posting in Japanese was undetectable *by construction*. Measured: hostile Thai/Japanese/Cyrillic score ~0.0004, identical to random non-Latin noise; Latin-script non-English (Portuguese/German) gets partial, unreliable signal that clears the 0.10 gate on most genuine threats. Now `assess_language` (post `langs` tag + a Unicode script cross-check, no new dependency) partitions each account's posts before classification: unassessable posts are dropped (also saving wasted CoPE-B spend), and an account with fewer than 5 assessable posts where the unassessable ones dominate is assigned a new `ThreatTier::NotAssessed` — a state *outside* the ordered Low→High scale, stored with a NULL score and preserved (not recomputed to Low) on read — rather than a threat score. Surfaced as its own bucket in the markdown report, the `/api/status` `tier_counts`, and the dashboard. Guards all three scoring paths (Stage-1 early-exit, monolithic Stage-2, decoupled gather). Latin-script non-English misdeclared as `en` remains a documented limit (no language-ID dependency added). Amplification-event scoring gets the same treatment separately (#230) (#222)
- Sanitize NUL bytes in post text — PG 'unsupported Unicode escape sequence' (#220) (#224)
- Per-post isolation in toxicity batch scoring — one bad post kills the whole account (#220 follow-up) (#221)
- Cost meter now tracks **real-time real spend** across concurrent RunPod workers instead of assuming one. `ScanCostMeter` accumulates a worker-seconds integral — `∫ min(in_flight, workersMax) dt` — via an RAII `InFlightGuard` armed per RunPod call, so the $ estimate (and the disaster-brake trip) reflect actual concurrent worker billing ($3.29/hr **per** worker), not single-worker wall-clock. Previously a burst running ~5–10 workers under-counted real spend by that multiple (e.g. a ~$2 estimate vs ~$6–10 real). New env `CHARCOAL_RUNPOD_WORKERS_MAX` (default 10) sets the cap; trip predicate unchanged (#185)
- Burst resilience to transient classifier failures: a RunPod serverless blip (transport/5xx, retry budget exhausted) no longer hard-aborts the whole scan before finalize. The client surfaces a typed `ClassifierTransientError`; `run_burst` treats it like the cost cap — records successes, leaves the rest pending, returns `BurstOutcome::Interrupted` (degraded + resumable). Permanent errors (4xx/parse) still abort to avoid cross-resume livelock. Per-call retry budget widened (default 3→6, +8s max-delay cap, ~20s window). Surfaced by the #178 staging scan, which aborted in burst on one transport error (#183)
- Phase C finalize missing raw>=8.0 follower NLI gate (spec gap from Task 5.1, blocks 6.3 behavior-preservation) (#211)
- Fix chainlink-safe-fetch MCP: python -> python3 in .mcp.json (#197)
- Fix context score double-application in concern troll scoring (#163)
- Fix invalid model ID — drop explicit model param from OpenAI Moderation requests (#153)
- Switch OpenAI Moderation from omni-moderation to text-moderation-latest for higher rate limits (#152)
- Fix impersonation not persisting across nav links — as_user lost on page navigation (#151)
- Add global rate limiter to OpenAI Moderation scorer — backoff alone is insufficient (#150)
- Fix URL-encoded as_user DID in impersonation middleware (#149)
- Fix missing Admin nav link and impersonation banner in layout (#148)
- Fix OpenAI Moderation API rate limiting — 429 errors cause ensemble scorer to fail on every request (#147)
- Investigate NLI hypothesis saturation — all 5 scores near 0.9 (#113)
- Include context_score in account API response (#117)
- Deduplicate event pairs before NLI scoring in amplifier loop (#110)
- Fix duplicate SentenceEmbedder::load in scan job (#140)
- Address CodeRabbit review findings on PR #21 (#138)
- Fix OAuth callback to register user in database (#121)
- Move multi-user schema changelog entry from 0.3.0 to Unreleased (#105)
- Fix ambiguous test count wording and decision graph status typo (#104)
- Thread authenticated actor handle through web scan job (#102)
- Fix second round of CodeRabbit findings on PR #14 (#101)
- Fix CodeRabbit review findings on PR #14 (multi-user schema) (#100)
- Update CLAUDE.md test counts to note web feature gate (#98)
- Fix PR #13 review round 2 findings (4 items) (#97)
- Fix PR #13 review findings (7 items) (#96)
- Session cookies: startup fails with clear message if CHARCOAL_ALLOWED_DID, CHARCOAL_OAUTH_CLIENT_ID, or CHARCOAL_SESSION_SECRET are missing or too short

## [0.3.0] - 2026-03-07

### Security
- Fix inverted credential redaction in migrate command display (#78)
- Constant-time password comparison in login handler — prevents timing oracle on password length (#102)
- Reject future-dated session tokens using checked_sub (#101)
- Remove HMAC fallback to hardcoded key; panic on misconfiguration (#101)

### Added
- v0.3 web GUI: Axum API server + SvelteKit dashboard (login, dashboard, accounts, events, fingerprint, scan trigger) (#80–#86, #95, #97)
- Railway deployment configuration with Railpack (#87)
- Scan progress display with elapsed time counter (#95)
- Scan button disabled while scan is running (#97)

### Fixed
- Return 500 on corrupt fingerprint JSON instead of silently coercing to null (#102)
- Session cookies: startup fails with clear message if CHARCOAL_WEB_PASSWORD or CHARCOAL_SESSION_SECRET are missing or too short (#101)
- Lock held across DB await in status handler — snapshot fields before releasing the read guard (#101)
- ONNX and embedder model loads wrapped in spawn_blocking to avoid blocking async runtime (#101)

### Changed
- Update CLAUDE.md and CHANGELOG for v0.3 web GUI merge (#93)
- Allow git stash in hook-config (#94)

## [0.2.0] - 2026-02-20

### Added
- Display behavioral signals in threat reports (#67)
- Behavioral signals: reply ratio, quote ratio, pile-on detection (#54)
- Add validate command: score blocked accounts to verify pipeline accuracy (#63)
- Refactor Bluesky client to use public AT Protocol API without authentication (#62)
- Constellation backlink index for supplementary amplification detection (#35, #53)
- Batch DID→handle resolution via getProfiles for Constellation events (#58)
- Sentence embeddings for semantic topic overlap (all-MiniLM-L6-v2) (#34)
- Wire embedding-based overlap into profile scoring pipeline (#40)
- Store embedding vectors in DB and update fingerprint command (#39)
- Create SentenceEmbedder with ONNX inference + mean pooling (#38)
- Add sentence embedding model download (all-MiniLM-L6-v2) (#37)
- Reweight toxicity categories to reduce ally false positives (#31)
- Replace weighted Jaccard with cosine similarity for topic overlap (#30)
- Mode 2: Background sweep of followers-of-followers (#25)
- Surface quote text and toxicity in threat reports (#21)
- Tune threat tier thresholds for real-world score distribution (#8)

### Fixed
- post_count u32-to-i32 cast could overflow (#75)
- save_embedding silently fails if no fingerprint row exists (#71)
- Fix critical/high code review findings from PR #6 (#70)
- Recalibrate threat scoring for sentence embedding overlap scale (#44)
- Crash-resilient pipelines: incremental DB writes + panic catching (#33)
- Exclude protected user from their own threat report (#22)
- Support custom PDS endpoint for non-bsky.social accounts (#7)

### Changed
- Harden workflow: atomic commits, branch protections, issue and graph persistence (#88)
- sqlite feature flag now correctly gates sqlite-related code (#76)
- Postgres integration tests now clean up after themselves (#74)
- Document pgvector CREATE EXTENSION superuser requirement (#73)
- Add advisory lock for concurrent migration protection (#72)
- Optimize pile-on detection from O(n^2) to O(n) sliding window (#68)
- Test SQLite-to-PostgreSQL migration end-to-end (#69)
- Database migration: SQLite to PostgreSQL (#48)
- Adapt scoring formula for multi-component signals (#56)
- Organize generated files into gitignored directories (#65)
- Research AT Protocol public API authentication requirements (#61)
- Write architectural recommendations for multi-user migration (#47)
- Update docs and close session for sentence embeddings work (#42)
- Add tests for embedding DB queries, migration, and download helpers (#41)
- Update CLAUDE.md and docs to reflect contributor changes and new tests (#36)
- Increase posts per account from 20 to 50 for more stable fingerprints (#32)
- Skip follower analysis for repost events (#27)
- Stop tracking chainlink issues.db and .cache in git (#26)
- Replace dummy Perspective scorer with proper no-op (#24)
- Wire up --since flag or remove it (#23)
- Design repost scoring strategy — score all vs sample vs limit (#12)
- Cosmetic cleanup — update comments referencing Perspective as primary scorer (#14)
- Clean up CLAUDE.md and create README.md (#20)
- Add progress bar to parallel scoring (#18)
- Refactor scoring loop to use buffer_unordered (#17)
- Add --concurrency CLI flag to scan command (#16)
- Add futures dependency to Cargo.toml (#15)
- Scale scan pipeline — reduce per-account latency and support larger networks (#10)
- Close rate limiter issue as moot — ONNX scorer has no API rate limits (#9)
- Select and implement Perspective API replacement (#13)
- Research alternative toxicity scoring APIs (Perspective sunsetting Dec 2026) (#11)

## [0.1.0] - 2026-01-31

### Added
- Phase 7: Reports, markdown output, and polish (#6)
- Phase 6: Profile scoring and threat tiers
- Phase 5: Amplification detection pipeline (#5)
- Phase 4: Toxicity scoring with Perspective API (#4)
- Phase 3: Topic fingerprint with TF-IDF (#3)
- Phase 2: Bluesky auth + post fetching (#2)
- Phase 1: Project skeleton + config + database (#1)
