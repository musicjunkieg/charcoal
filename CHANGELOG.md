# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Added
- Multi-interest topic fingerprint (#297, spike #295 BETTER tier): the
  protected user's posts are now clustered in embedding space into up to 12
  weighted topic centroids (greedy agglomerative, centroid linkage,
  deterministic), each labeled by TF-IDF over only its own posts — replacing
  the single mean-pooled centroid that smeared multi-topic users into a
  vector in nobody's topic region. Live overlap is now max-over-topics
  ("is this account near ANY of my topics?"); candidates still get one
  centroid, so scan-time cost is unchanged. Real-data validation forced one
  algorithm correction: MiniLM embeddings are anisotropic, so merge
  decisions run on mean-centered vectors (raw-space cosines collapsed 487
  of 500 posts into one hub cluster); stored centroids stay in the original
  space. Discovery search keywords now come one-per-semantic-topic for free.
- Shadow compare for #135 recalibration: every embedding-scale score also
  records the pre-#297 mean-centroid overlap in a new nullable
  `account_scores.overlap_legacy` column (migration 0013, both backends),
  so gate/tier thresholds can later be retuned from real paired data. The
  live score, gates, and tiers use only the new overlap; thresholds are
  numerically unchanged at ship time. Note for #135: at 500 posts the
  12-cluster cap, not `merge_threshold`, determines cluster count — the
  0.60 default is uncalibrated. A read path for the column is #305.

### Fixed
- Fingerprint generations can no longer diverge (#302, CodeRabbit Major
  deferred off PR #101): fingerprint JSON, mean embedding, and the new
  per-topic centroid rows persist in ONE transaction per backend via
  `save_fingerprint_bundle`, replacing the two-statement sequence whose
  warn-only embedding failure could leave a fresh fingerprint beside a
  stale embedding for up to 14 days. `charcoal migrate` transfers the whole
  generation atomically too, and user deletion clears centroid rows (FK
  cascade on Postgres, explicit delete on SQLite). A fingerprint whose
  centroid rows don't match its JSON (pre-#297 legacy, or pre-#302
  divergence) is treated as stale and rebuilt on the next scan — the same
  amortized one-rebuild-per-user rollout as #296. An unreadable stored
  fingerprint JSON now also rebuilds instead of failing that user's scans
  forever (#303 precedent).
- The web scan, admin pre-seed, and CLI `charcoal fingerprint` now share
  one build path (`src/topics/build.rs`) instead of maintaining a
  hand-duplicated copy in `main.rs`.
- Repaired eight defects in the topic-overlap math (#296, spike #295 GOOD
  tier): per-post embeddings are L2-normalized before averaging so long
  posts no longer dominate the centroid; cosine similarity preserves its
  sign instead of clamping opposition to zero; URLs and @mentions are
  stripped before embedding on both sides; Stage 1 and the Stage-2 keyword
  fallback gate on a keyword-scale threshold (0.05, provisional) instead of
  the embedding-scale 0.15 that was silently early-exiting relevant
  accounts; keyword weights are rank-weighted by TF-IDF score; extractor
  configs are unified at 60 keywords / 10 clusters on both sides;
  `Unreliable` candidate fingerprints can no longer authorize the Stage-1
  early exit; and the protected fingerprint is rebuilt when older than 14
  days instead of living forever.

### Changed
- Marked the deciduous graph exports (`docs/graph-data.json`,
  `docs/git-history.json`) as generated in a new `.gitattributes` (#300).
  They regenerate on every commit via the pre-commit hook; GitHub now
  collapses them in PR diffs and `git diff` shows a single line instead of
  hundreds of churned JSON lines. They remain committed — GitHub Pages
  serves the graph viewer from `/docs`.
- Completed a research spike on topic-fingerprint quality (#295): mapped the
  current single-centroid pipeline against 2026 state of the art and produced
  a good/better/best implementation plan (report in `docs/research/`,
  follow-up issues #296–#299). No code changed.
- The authed app now consumes the design tokens instead of ~292 hard-coded
  colour values (#250, #255). `tokens.css` defined the Charcoal palette and the
  marketing pages used it; the product did not. This was bypass rather than
  drift — the hard-coded values were overwhelmingly palette-*correct*, so
  nothing looked broken, and a token change would propagate to the landing page
  while silently skipping every authed surface. The failure only shows up the
  day someone changes a colour and half the app ignores them.

  The original audit counted 161 literal hex values. Measured before starting,
  it was ~175 hex plus 117 `rgba()` literals the hex-only grep could not see —
  the same palette at varying alpha. Those now resolve through channel tokens
  (`--copper-rgb: 201 149 108`, consumed as `rgb(var(--copper-rgb) / 0.2)`).
  Because a hex token and its channel triplet are independent declarations that
  nothing in CSS relates, a test asserts every `--x-rgb` matches its `--x`.

  Threat-tier colours had been duplicated four times: three TypeScript
  `TIER_COLORS` maps plus a fourth copy in `LabelButtons` that the audit missed
  because it is named `TIERS`. All four are gone, replaced by semantic tokens
  selected through one global `tiers.css` — the pattern the dashboard already
  used. That also removed a latent bug: the accounts filter pill built its
  border with `` `${TIER_COLORS[tier]}40` ``, string-concatenating a hex-alpha
  suffix, which no `var()`-based approach could have survived.

  A `tierClass()` helper preserves the old `?? '#a8a29e'` fallback exactly, so
  abstained accounts (`NotAssessed`, `Insufficient Data`) keep the colour they
  had; interpolating the tier straight into a class name would have silently
  recoloured them. Two files needed same-file overrides where moving a rule
  from Svelte-scoped (0-2-0) to global (0-1-0) would otherwise have lost the
  cascade; two others correctly needed none.

  No colour changed. A resolved-value diff — which resolves every `var()` back
  to a literal and compares the result per file — reports zero deltas, and was
  negative-controlled before any clean run was trusted.

### Fixed
- Give the scan queue ONE total order for display, position, and admission
  (#271, #288). `enqueued_at` alone is a partial order — SQLite stores it as
  RFC3339 text and Postgres stamps `NOW()`, so two requests landing in the same
  tick tie — and the three places that consumed it disagreed about what to do
  with a tie. `list_scan_queue` sorted on `(enqueued_at, user_did)` but counted
  position as `enqueued_at <=`, so tied rows rendered 1st, 2nd, 3rd while every
  one of them was told it was 3rd; `scan_queue_entry` repeated the same count;
  and `claim_next_scan` ordered on `enqueued_at` alone, so the row admitted next
  was not necessarily the row the dashboard showed as next. All six queries
  across both backends now use `(enqueued_at, user_did)` — a row-value
  comparison for the position counts, the same pair in both `ORDER BY`s. The
  regression tests seed rows sharing one timestamp, which `enqueue_scan`'s
  wall-clock stamp cannot produce, and assert distinct positions plus that
  admission takes the row listed first.
- Show the reason a scan failed in the admin dashboard's Last Scan cell (#288).
  It was carried only by a `title` attribute on a non-focusable `<span>` —
  unreachable by keyboard, unannounced by screen readers, and simply absent on
  touch, so the failure detail this feature exists to surface was visible to
  nobody who could not hover. The cell now renders the reason inline, shortened
  to keep the column narrow, with the full untruncated string in a
  visually-hidden sibling so assistive tech gets all of it.
- Show the admin dashboard the real scan queue (#288). `GET /api/admin/users`
  built its only scan column from the process-local `ScanManager`, so it knew
  only about scans *this* process launched: the column was empty after a
  redeploy, blind to a second replica, had no concept of `queued` — meaning an
  admin-triggered scan, which since #257 is enqueued rather than launched,
  showed nothing at all — and could not tell a failed scan from one that never
  ran. Every other admission decision moved to the durable `scan_queue` row in
  #257/#278; this was the last consumer left behind. A new
  `Database::list_scan_queue` returns every row with its queue position, and
  the handler uses that one query twice: to enrich each user with status,
  position, start, finish and last error, and to render a `queue` panel of the
  active rows against `CHARCOAL_SCAN_CONCURRENCY`. `fingerprint_building` stays
  on `ScanManager`, which is the correct scope for it — it tracks a
  `tokio::spawn` this process owns and has no durable row anywhere. A database
  error now fails the request rather than degrading to "nobody has ever
  scanned", because an operator acts on that.
- Point the git hooks at the checked-out models (#284). The pre-push hook runs
  `cargo test` without `CHARCOAL_MODEL_DIR`, and test binaries never load `.env`
  (dotenvy runs in `main.rs` only) — so once the #257 queue tests started failing
  loudly instead of passing silently, the hook blocked every push and the
  variable had to be set by hand. It is now exported when a `models/` directory
  exists and the caller has not set one.
- Give `PublicAtpClient` a request timeout — 30s per request, 10s to connect
  (#257). Async `reqwest` has **no** default timeout, so a Bluesky host that
  accepted the connection and then never answered held its gather task forever.
  The #182 retries above do not help: a request that never returns never
  becomes an error to retry. This is the same defect already fixed for
  Constellation in #235, and the same constants; under scan concurrency a
  permanently-parked task costs a queue slot that never comes back. The
  regression test asserts a stalled response gives up on its own clock —
  without the timeout it takes the mock's full 30 seconds.
- Make `eta_seconds` identical across the two database backends (#257). The
  SQLite side measured completed scans with `num_seconds()`, which truncates,
  while Postgres used `EXTRACT(EPOCH FROM ...)`, which keeps fractional
  seconds — so the same scan history quoted a different wait either side of a
  backend switch. SQLite now keeps the fraction rather than Postgres losing it:
  `eta_seconds` multiplies the median by the batch count, so truncation
  compounds (a 90.5s median two batches out is 181s, not 180s), and rounding
  both would mean discarding precision Postgres already has. A cross-backend
  parity test seeds both with the same hand-written timestamps and asserts they
  answer the same.
- Record the panic message when a background scan unwinds (#257). The
  `catch_unwind` around the scan future dropped the payload and stored a fixed
  `"Background scan panicked"`, and that string is what reaches *both*
  `ScanStatus::last_error` and the durable `scan_queue` row — so a panicked
  scan left no clue about its cause in either place. It now reuses
  `scan_phases::panic_message`, the extractor the gather path already uses for
  exactly this.
- Stop the #257 HTTP queue tests from passing when the ONNX models are absent.
  Each guarded on model availability and returned early, printing a skip line
  that libtest discards for passing tests — five tests on this branch have
  already asserted guarantees they never exercised. They now fail loudly with
  the command that fixes the environment. (The wider cleanup of the older
  model-gated tests is #269.)
- Untrack `web/build/index.html` (#279). `web/build` is gitignored, but this
  one file had been force-added, so it sat in the tree while the twelve hashed
  assets it references did not. It was inert — CI and the Dockerfile both run
  `npm run build` before any `--features web` cargo build, so the committed copy
  was never the one deployed — but it had to be hand-refreshed on every frontend
  change and misled anyone reading the tree. Removed from the index only; the
  file stays on disk, where `include_dir!` needs it.
- Make two `.impeccable/design.json` examples do what they claim. The brand
  mark's description says the rings pulse and the core breathes, but its CSS
  defined no animation at all — so the `prefers-reduced-motion` rule was
  disabling nothing. Both animations are now present (4.5s cycle, rings
  staggered 0.4s, per DESIGN.md), and the reduced-motion override has something
  real to turn off. The navigation example described itself as fixed over a
  gradient fade while sitting in normal flow; it now carries the
  `position: fixed` / viewport offsets / `z-index: 100` the shipped nav uses.
  These are copyable reference examples — the point is that they match the
  system they document.
- Add schema **v12**, which backfills `scan_queue.claim_id` (#257). v11 was
  amended in place to add the fencing token while #257 was still on its branch —
  reasonable, since v11 had never shipped, but not sufficient: a database created
  from the *pre-amendment* v11 has a `scan_queue` with no `claim_id` **and**
  version 11 already recorded, so both migration runners skip 0011 entirely and
  the column is never added. Every claim, heartbeat and finish then fails with a
  missing-column error. Amending 0011 cannot reach those databases; only a new
  version can. No deployed environment is affected (staging and production both
  stopped at v10 with no `scan_queue` table) — the exposed population is
  developer machines that ran this branch mid-stream. The migration is a clean
  no-op on a fresh database, where v11 already creates the column.
- Report a failed queue read-back as `position: null` rather than `0` on both
  scan-trigger endpoints (#257). The enqueue still succeeds and the answer is
  still `202`, but `0` is a *real* position — it is what a `running` scan
  reports — so falling back to it made "the database read failed" indistinguish-
  able from "your scan is already running", with the error discarded on top. The
  error is now logged and the unknown case is a value no successful read ever
  produces. Same silent-failure shape already fixed in `delete_user` (#278).
- Retry public Bluesky reads on 429 and 5xx (#182). Every read through
  `PublicAtpClient` now makes up to 4 attempts, backing off exponentially from
  250ms to 8s with jitter. A typed `XrpcAttemptError` decides what is
  retryable rather than string-matching an `anyhow` chain: transport failures,
  429 and 5xx back off, while other 4xx and deserialization failures return on
  the first attempt instead of being retried into a guaranteed-identical
  answer. Discovery had been capped at ~8 in-flight requests explicitly "until
  #182 lands"; scan concurrency (#257) pushes in-flight requests past that
  ceiling, and with no backoff a rate-limited read surfaced as a plain fetch
  failure — which #236 shows can cost an entire account.
- Gate `delete_user` on the durable `scan_queue` row rather than the
  process-local `ScanManager` (#278). The old guard only knew about scans *this
  process* launched, so it missed a user who was merely queued, one running on
  another replica, and anything surviving a restart — every other admission
  decision had already moved to the queue row and this one was left behind. A
  database error in the new lookup is a `500` rather than an implicit yes:
  the guard it replaced was an in-memory read that could not fail, so treating
  "we cannot tell" as "go ahead" would delete a user out from under a live
  scan, which is the one thing the check exists to prevent.
- **`POST /api/scan` queues instead of refusing (#257).** Scans were globally
  single-flight — one per *server* — so a second user got
  `409 "Another scan is already in progress on this server"`. With open signup
  (#256) and scans that run 22 minutes to 2 hours, that was the second user's
  entire experience of Charcoal, all day. Both trigger endpoints now enqueue and
  return `202` with a queue position and ETA; the background admitter is the one
  and only thing that starts a scan, under `CHARCOAL_SCAN_CONCURRENCY`. The
  admin trigger enqueues like everyone else rather than jumping the queue — one
  admission path, so the cap (and the GPU spend behind it) holds absolutely.
  `GET /api/status` now prefers the durable `scan_queue` row over the
  process-local `ScanManager`, gaining a `queued` phase and a `queue` block that
  is omitted entirely when the user is not waiting. The process-global
  `any_running` flag is deleted: with the queue as the admission authority it
  could only disagree with it, and it disagreed in both directions — refusing
  admin triggers whenever any admitted scan ran, and letting an extra scan past
  the cap when the first of two concurrent scans cleared it globally.
- Refuse to start a second pipeline for a user whose first is still executing
  (#273). `run_admitter` reclaims lapsed leases and admits in the *same* pass
  with no delay between them, and `claim_next_scan` takes the oldest queued row
  — which is very often the row the reclaim just re-queued, for a user whose
  pipeline is still running. Two pipelines then wrote the same `scan_state`
  resume markers (#208) and both drained `classification_queue` against RunPod,
  where the `$2` cost ceiling is *per scan* and was therefore paid twice, with
  real concurrency exceeding the cap. The fencing token alone did not cover
  this: the successor starts long before the predecessor's next beat, and if the
  lease lapsed *because* `heartbeat_scan` was erroring, `heartbeat_until_lost`
  retries through `Err` and may never notice. Admission now also consults an
  in-process registry of live claims, released on every exit by a guard. It is
  deliberately not keyed on `ScanManager`, whose entry still says "running" on
  purpose — that guard would refuse the successor forever.
- Fence every scan-status write on the claim that owns it (#274). `run_scan`
  writes its terminal status from *inside* the scan future, before
  `run_under_slot` ever classifies the exit, so a superseded worker whose
  pipeline finished inside the window had already stamped `Done` over its
  successor's live entry by the time the `Completed` arm ran and correctly did
  nothing; every `set_progress` call in that window landed there too. A previous
  fix covered only the `Abandoned` exit arm, which is the one path that write
  never takes. Ownership now lives on the status entry itself and a stale write
  is a no-op, which also lets the abandonment path report honestly instead of
  leaving a "running" label that would never change.
- Drop the `AtCapacity` admission log from WARN to INFO (#275). It fires on
  every 30s tick for as long as any backlog exists — ~120 lines an hour — and
  under open signup a standing backlog is the expected steady state, not an
  incident. At WARN it trains an operator to filter WARN, which costs them the
  `Wedged` ERROR that actually means something.
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
- Give the R2 backups history, a sanity gate, and a 30-day expiry (#286).
  Cloudflare R2 does **not** implement bucket versioning — `PutBucketVersioning`,
  `GetBucketVersioning` and `ListObjectVersions` are all unimplemented — so
  `s3://charcoal-backups/issues.db` was a single object with no history that
  every commit overwrote. On 2026-08-07 that came within one commit of being
  unrecoverable: checking out an old branch that *tracks* `.chainlink/issues.db`
  destroyed the live database, chainlink silently created an empty one, and a
  commit in that state would have uploaded it over the only good copy. Since the
  storage layer offers no versioning, the hook now creates the history itself —
  each upload also writes `history/<name>-<UTC>.db` — and refuses to upload a
  database with implausibly few rows, which is the guard that actually blocks
  that failure. A `expire-backup-history-30d` lifecycle rule expires the
  `history/` prefix after 30 days; the canonical pair is deliberately outside it.
- Animate the classification progress bar with `transform: scaleX()` instead of
  `width` (#280). Transitioning `width` relayouts on every frame of the 0.5s
  animation; `transform` is composited. The gradient is unaffected — it spans
  the element either way, so compressing it by scale matches compressing it by
  width — and the only real difference is that `scaleX` squashes the 3px radius
  horizontally on the growing edge, which is sub-pixel on a 6px-tall bar.
- Load the three ONNX models once at boot into shared state instead of per
  scan, and refuse to start the server when any of them is missing (#257).
  Concurrent scans would each pay ~500MB otherwise — the fp32 NLI export (#231)
  is 284MB of it — which is what made concurrency a memory problem rather than
  merely an untidy one. Two operational consequences to know before deploying:
  **the server now fails to boot on a broken model volume** rather than
  degrading scan by scan, so a bad volume surfaces as a failed deploy instead of
  as user-visible scan failures hours later; and **idle memory rises from
  ~0.78GB to ~1.28GB**, which Railway meters continuously (#188) even on days
  nobody scans.
- Cover the six `scan_queue` methods on SQLite, which is the default backend and had none. Closes a regression gap rather than a bug — a reviewer hand-verified no divergence from Postgres exists today, but the #257 fix wave had changed SQLite's transaction behaviour to `BEGIN IMMEDIATE`, added `UPDATE .. RETURNING`, and added the `claim_id` fencing guard, none of it exercised. The two ETA cases are split deliberately: both the status gate and the median branch return `None`, so a single test would pass for the wrong reason (#270)
- Verify danabra.mov re-scan 2026-07-20 (post-#224) (#229)
- Railway drops scan logs at 500/sec — observability gap during scans (#226)
- Diagnose degraded=true on the 8174-account staging scan (2026-07-19) (#220)
- Pre-commit hook no longer stages the gitignored `.chainlink/issues-export.json`. The file is also untracked, so the `.gitignore` entry can finally apply — a gitignore rule has no effect on an already-tracked file, which is why it kept conflicting on every branch integration. (`--no-verify` remains an emergency bypass, unrelated to this.) (#181)
- Batch the 5 NLI hypotheses into one padded `[5, max_len]` forward pass instead of 5 sequential single-item inferences — ~5× fewer NLI ONNX runs, biggest in the amplification event loop (NLI per event). NOTE: the quantized `nli-deberta-v3-xsmall` export is not perfectly padding-invariant, so batching shifts `context_score` by a small, systematic amount — **measured on macOS ARM64 only** (≈0.006 on the final hostility, ≈0.002–0.008 per hypothesis), and accepted *on that platform* as within the model's own quantization noise and immaterial to threat tiers (bands 8/15/35). A model-gated unit test at a 0.02 tolerance was intended to pin the batch-vs-single equivalence, but it never actually executed in CI (it read `default_model_dir()` while CI sets `CHARCOAL_MODEL_DIR`), so **nothing has ever enforced this bound** (#213). **CORRECTION (#231):** on Linux x86_64 — the platform production runs on — the same model bytes diverge by **0.14** on hypothesis 0 (batched 0.031 vs single 0.172), far outside that tolerance. The equivalence claim was therefore never verified where it matters. **RESOLVED (#231):** the cause was the quantized export's runtime per-tensor activation scale, not padding; the fp32 export makes batching exact on both platforms, and the equivalence test is un-quarantined at a `1e-4` tolerance. The batching speedup here was also overstated — measured 1.69x, not ~5x (#213)

### Added
- Phase A now logs where its time actually goes — Bluesky fetch vs the Stage-1
  ONNX pass vs the Stage-2 clean pass, with an `inference_pct` (#264). The #257
  concurrency default rested on an estimate that Phase A was "~100% Bluesky
  I/O"; it is not, because `gather.rs` runs ONNX inference in the same phase, on
  the shared model mutex. Stage 1 and Stage 2 are separate buckets on purpose —
  Stage 1 runs for every account, Stage 2 only for survivors, so folding them
  together would blur two different questions. This is the number that decides
  whether raising `CHARCOAL_SCAN_CONCURRENCY` helps: under ~10% inference,
  concurrency 2 should approach 2x; at 30%+ the mutex is the ceiling and the
  lever is a session pool instead of more concurrency.
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
