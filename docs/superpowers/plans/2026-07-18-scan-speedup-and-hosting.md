# Scan Speedup + Hosting Strategy Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement Part 2 task-by-task. Steps use checkbox (`- [ ]`) syntax. **Read the target file before editing** — this plan cites `staging` line numbers that will drift.

**Goal:** Cut onboarding-scan wall-clock by ~2 hours through code changes, and settle the Railway→Hetzner question with evidence instead of assumption.

**Headline:** The two tasks do *not* reinforce each other. The research inverted the premise. Part 1 explains why; Part 2 is the work that actually pays.

**Chainlink issues:** #188 (hosting — **closed 2026-07-19 as evaluated-declined**) · #178 (**closed** — already shipped) · **#213** (the real speedup work, successor to #178; was #189 before the 2026-07-19 DB reconciliation) · related: #179, #180, #182, #184.

> **Branch note:** this plan is written against `staging`, which is where the live chainlink DB and the current code both live. An earlier draft was written on `main` and got two things wrong as a result — see "Corrections" at the end.

**Tech Stack:** Rust/Axum, tokio, `ort` (ONNX), Postgres+pgvector, Docker on Railway.

## Global Constraints

- All work on `feat/*` branches off `staging`, never direct to `staging` or `main`.
- TDD: tests before implementation. Never weaken a test to make it pass.
- `cargo test --features web` must stay green; clippy clean on web/default/postgres.
- Explicit `git add <file>` only. No `git add -A`/`.`/`-am`. No heredocs.
- Log to deciduous in real time (goal → options → decision → action → outcome).

---

# Part 1 — Findings: the premise doesn't survive contact with the data

The request was: speed up the CPU side of scanning, and tie it to a Railway→Hetzner move for more cores at lower cost. Three findings independently break that chain.

**And #188 never claimed otherwise.** Reading the issue's own body settles this before the research does. #188 is scoped explicitly as a *billing-model* migration — "usage-based -> flat-rate" — and it says in as many words that the infra spike "concluded I/O-bound (not CPU-bound), DB is the crux," and that it "does NOT include the GPU/RunPod classifier spend." The performance framing is not in the backlog; it was an inference layered on top of it. The scan-speed work and the hosting work are two separate tracks that happen to both touch cost.

## 1.1 Railway does not penalize parallelization

Railway meters **actual consumption**, not allocated capacity ([docs](https://docs.railway.com/reference/pricing)): CPU $0.000463/vCPU-min, RAM $0.000231/GB-min.

Parallelism rearranges CPU-seconds; it does not create them. A 16-way scan finishing in 15 min costs the *same* CPU as a 1-way scan taking 4 hours — and **less** RAM, because RAM is held for a shorter time. There is no metered penalty to parallelizing on Railway. The belief that billing was discouraging it is mistaken.

## 1.2 Your Railway bill is not scan compute — it's near-zero compute

Pulled live from the Railway API for this plan:

| Service / env | Window | Avg CPU (vCPU) | Max CPU | Avg RAM |
|---|---|---|---|---|
| charcoal-web / **production** | 30 days | **0.0000** | 0.0005 | 0.10 GB |
| Postgres / production | 7 days | 0.0001 | 0.0001 | 0.04 GB |
| charcoal-web / **staging** | 7 days | 0.0041 | 0.68 | 0.08 GB |

Priced at Railway's published rates, total metered usage across both environments is roughly **$2–5/month** — comfortably inside the $20 that Pro already includes. Production is doing essentially nothing.

So the $30.32 May bill was a *busy development month*, not a structural cost. The floor you are paying is the $20 Pro subscription, and no hosting migration removes a subscription you'd be replacing with a bigger flat fee.

**Caveat, stated honestly:** this is a quiet-period sample. It proves prod idles at ~zero; it does not fully reconstruct May. Confirm against the actual May invoice line items (Task 0) before treating the hosting question as closed.

## 1.3 Hetzner tripled the prices of exactly the lines you'd want

Hetzner raised prices on **15 June 2026** ([official table](https://docs.hetzner.com/general/infrastructure-and-availability/price-adjustment/)), very unevenly. The dedicated-vCPU lines — the ones a "more cores" migration targets — took the worst of it:

| Plan | Old | New | Factor |
|---|---|---|---|
| CX43 (8 shared / 16 GB) | €11.99 | €15.99 | 1.33× |
| CX53 (16 shared / 32 GB) | €22.49 | €29.49 | 1.31× |
| CPX42 (8 / 16 GB) | €25.49 | **€69.49** | **2.73×** |
| CCX33 (8 **dedicated** / 32 GB) | €62.49 | **€138.49** | 2.22× |
| CCX43 (16 **dedicated** / 64 GB) | €124.99 | **€275.99** | 2.21× |

Driver is the DRAM price shock; netcup (+24%), Scaleway and OVH moved similarly. **The "Hetzner is 4× cheaper" framing is a pre-June-2026 artifact.**

Costed end-to-end, every realistic target with 8–16 genuinely dedicated cores lands at **$43–115/mo** against your current $30.32 — plus 12–25 hours of migration and 2–4 hours/month of ops forever, plus you personally owning Postgres backups. Hetzner has **no managed Postgres** (verified against their own product page and API changelog).

## 1.4 And more cores wouldn't fix the scan anyway

This is the finding that really settles it. The scan is not core-starved. From the code on `staging`:

- **The serial sweep loops the task assumed are already fixed.** `sweep.rs:95-107` and `amplification.rs:347-364` both use `buffer_unordered(concurrency)`. That was landed under the real #178. The serial `for` loop only survives on `main`, which is behind.
- **The real serialization is a global mutex, not a core shortage.** All three ONNX sessions are `Arc<Mutex<Session>>` (`toxicity/onnx.rs:45`, `topics/embeddings.rs:31`, `scoring/nli.rs:100`). Only one inference of a given model runs **process-wide** at a time, regardless of gather concurrency. Adding cores does not widen this.
- **`ort` already parallelizes intra-op across cores** (no `with_intra_threads` override anywhere), so a single forward pass already uses the box. Extra cores give sub-linear returns.
- **Finalize is one misplaced `embed_batch`.** `profile.rs:458` runs up to 50 posts through MiniLM per account, inside the serial `run_finalize` loop (`scan_phases/mod.rs:430`), holding that global mutex.
  **Verified against a real run** (2026-07-18 staging, Railway deploy logs): 473 accounts, finalize 03:12:24 → 03:21:14 = **8m50s**, at a median **1.56s/account** (p90 1.61s, max 1.98s). That distribution is far too tight for network or DB, which jitter — it is deterministic CPU.
  The **~74 min / 2900-account** figure quoted elsewhere in this plan comes from prior-session notes dated 2026-06-26, which fall outside Railway's log retention and could **not** be verified. The per-account cost is the number to trust — it reproduced exactly across both runs — so the mechanism is confirmed even though the totals are not.
- **DB round-trips are real but small.** Finalize does 3 queries/account; co-locating the DB would save single-digit minutes, an order of magnitude less than the ONNX and HTTP serialization.

**Conclusion:** the scan is bound by *where work is scheduled*, not by how much CPU is available. Part 2 fixes that for free. A hosting move would have cost money and changed nothing.

## 1.4a What #188 actually asks for, answered

The issue names six requirements and a deliverable format. Answering them directly:

| #188 requirement | Status on a flat-rate box |
|---|---|
| Postgres 16+ with pgvector | Fine. `pgvector/pgvector:pg17`/`pg18` images, extension pre-compiled. Current pgvector is 0.8.5. |
| Persistent volume for ONNX models | Fine — and don't rsync them; `charcoal download-model` re-fetches ~300 MB and is verifiably correct. |
| Docker deploy (Ubuntu 24.04, glibc 2.39 for ort-sys) | Fine on any x86-64 host. **Not** fine on Hetzner's cheap Arm (CAX) line without a `docker buildx --platform linux/arm64` trial first — `ort` prebuilt aarch64 coverage is unverified. CX53 is only €11 more than CAX41; the Arm risk isn't worth €11. |
| Custom domain + TLS | Solved. Caddy, ~4 lines, auto Let's Encrypt + renewal. Mount `/data` as a named volume or every restart re-requests certs into a rate limit. |
| Two envs on one box | Works via Compose project names (isolates containers/networks/volumes). But it merges the blast radius and the I/O budget — see §1.5. |
| Egress not metered/capped | Fine. Hetzner EU includes 20 TB; **US plans include only 1 TB** (cut ~88% in 2025). Irrelevant at your volume, but know it. |

**Ops burden, self-managed vs managed:** Hetzner has **no managed Postgres** — verified against their own product page and API changelog. Choosing Hetzner *is* choosing to self-manage. Realistic: 6–12h setup, 1–3h/month steady, 2–6h per major upgrade. The cliff is major version upgrades (`pg_upgrade` needs both versions' binaries and the extension `.so` for the *new* major); at a few GB, dump-and-restore sidesteps most of it.

**Backups — skip PITR initially.** Charcoal's data is derived and re-scannable. `pg_dump --format=custom` every 6h → Backblaze B2 (~$0.30/mo, first 10 GB free) → **monthly restore rehearsal**. The tripwire for revisiting: `user_labels`. Once that holds hours of irreplaceable human judgment, add WAL-G. (Note: pgBackRest was declared unmaintained 2026-04-27 and **rescued 2026-05-18** by an AWS/Supabase/Percona/pgEdge/Tiger/Eon coalition — posts declaring it dead are stale.)

**Highest-risk migration item is not TLS — it's OAuth.** `CHARCOAL_OAUTH_CLIENT_ID` is a URL serving client metadata on the public hostname with exact-match redirect URIs, and `CHARCOAL_SESSION_SECRET` derives the stable P-256 signing key. Carry the secret across unchanged, verify the full round-trip on a temporary hostname, *then* cut DNS.

**If we ever proceed, the PaaS layer is contested — and I'd now lean toward not using one.** Dokploy wins the feature comparison decisively: first-class environments and scheduled Postgres *and* volume backups to S3 with UI restore are exactly the two Railway capabilities you'd otherwise hand-roll, and CapRover has neither. But the security record is disqualifying for an app holding OAuth tokens:

- **Dokploy** published **11 advisories on 2026-05-11, 8 critical.** The worst, CVE-2026-45631, is a pre-auth admin takeover: a hardcoded `BETTER_AUTH_SECRET` fallback that the install script never overrides, so *every default self-hosted instance in 0.27.0–0.28.8 shared one signing secret* — forge a JWT, auto-sign-in as admin, run commands on the host. Published PoC. Remediation needed an out-of-band secret-rotation script, which itself shipped buggy. Add: a bus factor of one (4,259 commits vs 65 for the #2 human; a second engineer starts ~now), a ~75% commit slowdown since May 2026, an unresolved Traefik-on-Swarm bug class producing non-self-healing 502s, and 500–600 MB idle RAM against a documented "2 GB minimum" that is fiction.
- **Coolify** disclosed **11 critical flaws in Jan 2026** (seven at CVSS 10.0, ~52,890 exposed instances), and its maintainer publicly describes the codebase as "YOLO programming" with few tests.
- **CapRover** is maintained but deliberately feature-frozen — no scheduled/S3 backups (config-only, self-described "experimental"), no environment concept, and Docker Compose has sat merged-but-unreleased for ~10 months.

**Both leading options are Docker Swarm**, which is the top failure mode in both issue trackers, and neither absolves you of Postgres backups anyway. Given that a PaaS control plane is an internet-facing, Docker-socket-holding admin panel — a *new* attack surface layered on top of the ops burden it was supposed to remove — the honest call for a single app with one Postgres is **Docker Compose + Caddy, no control plane.** Caddy is also strictly more capable on TLS than the alternatives (DNS-01 and wildcards; kamal-proxy structurally cannot do either). Build images in CI and deploy prebuilt regardless — a Rust release build will make a small box unresponsive.

## 1.5 Recommendation

1. **Do Part 2 now.** ~2 hours of wall-clock recoverable in code, zero infrastructure change, zero marginal cost.
2. **Close #188 as "evaluated — declined for now,"** with §1.1–1.3 as the rationale. Revisit if the bill sustains >$100/mo or Charcoal takes on many users.
3. **If a flat-rate box is ever wanted, it should be for *batch scan work*, not for charcoal.watch.** Hetzner's Server Auction was explicitly exempt from the June increase, has no setup fee and unlimited traffic — a ~16-core box at €40–70/mo is the one genuinely cheap corner left. That's an architecture change (offload scans to a worker box), not a migration.

   **This is reinforced by something #188 mentions in passing:** the soot v2 spec already assumes a Hetzner box at ~$80/mo fixed. If soot v2 happens, the Hetzner account, the ops burden, and the self-managed-Postgres learning curve get paid for *anyway*. At that point moving scan work onto the same infrastructure is incremental rather than a fresh migration — and that, not a cost saving on charcoal.watch, is the strongest version of the "move to Hetzner" case. It's worth revisiting **when soot v2 is actually built**, not before.
4. **The one Railway lever worth pulling now:** staging idles 24/7 for a two-environment setup you use intermittently. Sleeping or tearing down staging between pushes is an afternoon's work with a better ROI than any migration.

---

# Part 2 — Implementation plan: scan CPU speedup

Ordered by (wall-clock win) ÷ (risk). Tasks 1–2 are the bulk of the win.

**Baseline to beat:** amplification ~37m + sweep ~10m + gather ~80m + finalize ~74m ≈ 3h20m.

## File structure

| File | Responsibility | Touched by |
|---|---|---|
| `src/pipeline/scan_phases/gather.rs` | Phase A per-account collection | T1, T4 |
| `src/scoring/profile.rs` | Scoring, embedding, NLI orchestration | T1 |
| `src/pipeline/scan_phases/finalize.rs` | Phase C per-account scoring | T1 |
| `src/constellation/client.rs` | Backlink queries | T2 |
| `src/web/scan_job.rs` | Reply fetching | T2 |
| `src/scoring/nli.rs` | NLI cross-encoder | T3 |
| `src/pipeline/sweep.rs`, `amplification.rs` | Candidate staleness | T5 |

---

### Task 1: Move topic-overlap embedding out of finalize into gather

**The single biggest win: ~60–70 min of the ~74 min finalize phase.**

Finalize is a serial loop; gather is I/O-bound at concurrency 8 with idle CPU. Gather *already* runs ONNX there (the clean-pass at `gather.rs:232`), so this follows an established pattern rather than inventing one.

**Files:**
- Modify: `src/pipeline/scan_phases/gather.rs` (~:252 — produce the vector)
- Modify: `src/scoring/profile.rs:458-462` (consume a precomputed vector instead of embedding)
- Modify: `src/pipeline/scan_phases/finalize.rs` (thread the vector through)
- Test: `tests/composition.rs`, `tests/unit_scoring.rs`

**Interfaces:**
- Produces: a 384-dim `Vec<f32>` target mean embedding stored on the staged `AccountInput` blob.
- Consumes: nothing from other tasks.

- [ ] **Step 1:** Read `gather.rs:140-270`, `profile.rs:350-470`, `finalize.rs:60-240` in full before editing. Confirm exactly how `fingerprint_posts` is selected from the Stage-2 sample at `profile.rs:358-369`.
- [ ] **Step 2:** Write a failing test asserting that a scored account whose `AccountInput` carries a precomputed target embedding produces the *same* topic-overlap value as the current embed-in-finalize path. Reuse an existing persona fixture from `tests/composition.rs`.
- [ ] **Step 3:** Run it, confirm it fails to compile / fails on the missing field.
- [ ] **Step 4:** Add the embedding field to the staged `AccountInput` and **bump `ACCOUNT_INPUT_SCHEMA_VERSION`**. The version-mismatch path at `finalize.rs:96-105` already clears staging and re-gathers, so an in-flight deploy self-heals.
- [ ] **Step 5:** Compute the mean embedding in gather, reproducing the `fingerprint_posts` selection **exactly**. This is the one place this task can silently go wrong — if the selection differs, overlap values shift for every account.
- [ ] **Step 6:** Make `profile.rs` use the precomputed vector; the cosine dot product then costs microseconds.
- [ ] **Step 7:** Run `cargo test --features web`. All green.
- [ ] **Step 8:** Commit: `perf(scan): precompute topic embedding in gather, not finalize`

**Risk: LOW.** **What could break:** divergent `fingerprint_posts` selection silently changing every overlap score. Guard with the equivalence test in Step 2.

---

### Task 2: Parallelize the three remaining serial discovery loops

~200 strictly sequential HTTP round-trips before the pipeline starts.

**Files:**
- Modify: `src/constellation/client.rs:100-155` (`find_amplification_events`, 2 awaited `get_backlinks` per URI, ~100 serial calls)
- Modify: `src/constellation/client.rs:171-193` (`find_likers`, ~50 serial)
- Modify: `src/web/scan_job.rs:511-539` (`for post in &posts { fetch_replies_to_post(...).await }`, ~50 serial)
- Test: `tests/unit_constellation.rs`

**Interfaces:**
- Consumes: nothing. Produces: unchanged public signatures — ordering must be preserved.

- [ ] **Step 1:** Read `sweep.rs:95-118` — it is the proven in-repo pattern (`stream::iter(..).map(..).buffer_unordered(n)` + `sort_by_key` to restore deterministic order). Copy its shape.
- [ ] **Step 2:** Write a failing test asserting `find_amplification_events` returns results in the **same order** as the serial version for a fixed input set.
- [ ] **Step 3:** Run it; confirm it fails or is trivially passing against the serial impl (record the baseline ordering).
- [ ] **Step 4:** Convert each of the three loops to `buffer_unordered`, then `sort_by_key` to restore order.
- [ ] **Step 5:** Set concurrency to **4–8, not 64.** Constellation has no documented rate limit and `PublicAtpClient` still has no backoff (open issue #182). Do not raise this until #182 lands.
- [ ] **Step 6:** `cargo test --features web`; clippy clean.
- [ ] **Step 7:** Commit: `perf(discovery): parallelize constellation + reply fetches`

**Risk: LOW-MEDIUM.** **What could break:** tripping Bluesky/Constellation rate limits. Current failure mode is graceful (warn + skip → degraded scan), but it silently costs coverage. **Watch the staging logs for 429s after deploying this — if any appear, do #182 before going further.**

---

### Task 3: Batch the 5 NLI hypotheses into one forward pass

**Files:**
- Modify: `src/scoring/nli.rs:227-237`
- Test: `tests/unit_scoring.rs`

- [ ] **Step 1:** Read `nli.rs:170-240` and `topics/embeddings.rs:136-163` (the working padded-batch reference).
- [ ] **Step 2:** **Before writing any code, verify empirically that the batch dimension is accepted.** The comment at `nli.rs:181-184` warns this export **segfaults rather than errors** on unexpected input shapes. Run a one-off `[5, max_len]` inference and confirm it returns. If it segfaults, **stop and close this task as not-viable** — do not try to work around it.
- [ ] **Step 3:** Write a failing test asserting batched scoring returns the same 5 scores (within float tolerance) as 5 sequential calls.
- [ ] **Step 4:** Run it; confirm failure.
- [ ] **Step 5:** Replace the loop with a single padded `[5, max_len]` batch, mirroring `embeddings.rs`.
- [ ] **Step 6:** Tests green; commit `perf(nli): batch hypotheses into one forward pass`

**Risk: MEDIUM** — segfault-not-error is a real hazard. Step 2 is a hard gate, not a formality. **Win:** ~5× on all NLI, which matters most in the amplification event loop (`amplification.rs:133`).

---

### Task 4: Collapse the Stage-1/Stage-2 double fetch

`gather.rs:150` fetches 25 posts; `:173` re-fetches 50 for every proceeding account. Both are single-page `getAuthorFeed` calls — `page_size = max_posts.min(100)` (`bluesky/posts.rs:232`) — so fetching 50 costs the same as 25.

**Files:** Modify `src/pipeline/scan_phases/gather.rs:145-180`. Test: `tests/composition.rs`

- [ ] **Step 1:** Measure the Stage-1 terminal rate first (how many accounts early-exit at Stage 1). If most accounts terminate at Stage 1, this inverts the economics and is **not** worth doing — those accounts currently pay only the cheap fetch.
- [ ] **Step 2:** If the rate is low, write a failing test asserting Stage-1 outcome is unchanged when computed from the first 25 of a 50-post fetch.
- [ ] **Step 3:** Fetch 50 once; slice `[..25]` for `stage1_outcome`.
- [ ] **Step 4:** Tests green; commit `perf(gather): single fetch for both sampling stages`

**Risk: MEDIUM** — gated entirely on Step 1's measurement. **Win:** one Bluesky round-trip per proceeding account, ~⅓ of gather's I/O.

---

### Task 5: Kill the `is_score_stale` N+1

**Files:** Modify `src/pipeline/sweep.rs:130-134` and `src/pipeline/amplification.rs:383`. Test: `tests/composition.rs`

- [ ] **Step 1:** Read `sweep.rs:206` — the topic-first path already does this correctly (pull the fresh-DID set once, test in memory). Copy it.
- [ ] **Step 2:** Write a failing test asserting the bulk staleness query applies the **same 7-day cutoff** as the per-candidate call.
- [ ] **Step 3:** Replace both serial loops with one bulk query + in-memory set membership.
- [ ] **Step 4:** Tests green; commit `perf(scan): bulk staleness lookup instead of per-candidate query`

**Risk: LOW.** **What could break:** a mismatched staleness window silently re-scoring (or skipping) everything. Step 2 covers it. **Win:** ~9s at 2900 candidates, ~60s+ on a 20k second-degree pool.

---

## Deferred (identified, not scheduled)

- **ONNX session pool.** The global `Arc<Mutex<Session>>` is the deepest structural limit. Tasks 1–5 route around it; they do not remove it. If throughput still disappoints after Task 1, pool the sessions — but measure first, because `ort` already parallelizes intra-op and a pool may just contend for the same cores.
- **`AuditWriter::from_env` per NLI pair** (`profile.rs:517`, `amplification.rs:153`) — a file open/create syscall per pair instead of a held handle. Small, easy, unglamorous.
- **Per-row DB writes** in `enqueue_classifications` (`postgres.rs:1082`) and `record_classification_verdicts` (`:1172`) — one round-trip per row inside a transaction. Fix with `UNNEST` / `QueryBuilder::push_values`.
- **`PgPool` has no `PgPoolOptions`** (`postgres.rs:37`) — defaults to `max_connections = 10`. Fine at concurrency 8; a latent ceiling if raised.
- **#182 (429/backoff)** — promote to blocking if Task 2 surfaces any 429s.

---

## Self-review notes

- §1.4's claim that the sweep loops are already parallel is the single most load-bearing correction here; it is why Task 2 targets `constellation/client.rs` rather than `sweep.rs`. Verified against `git log staging -- src/pipeline/sweep.rs` (commit `cd6ae9c`).
- Tasks 3 and 4 both open with a **gate step that can cancel the task.** That is intentional, not indecision — both have a plausible failure mode that makes the work net-negative, and it's cheaper to check than to build and revert.
- Line numbers cite `staging` as of 2026-07-18 and will drift. Every task's Step 1 is "read the file."

## Corrections from re-reading the issues on `staging`

The first draft of this plan was written from `main`, whose `.chainlink/issues.db` is a stale snapshot ending at #177. Two things changed on re-reading the real issues via the CLI:

1. **#188's substance lives in a *comment*, not the description** — so the export's empty `description` field made it look like a bare title. The comment contains the trigger quote, six explicit requirements, the deliverable format, the soot-v2 note, and the "I/O-bound not CPU-bound, DB is the crux" line. §1.4a and §1.5 exist because of it. The conclusion did not change; it got better supported, and the "these two tasks reinforce each other" framing turned out to be contradicted by the issue itself.
2. **#178 was already implemented and has been closed.** Verified on `staging`: `sweep.rs:105` and `amplification.rs:361` both use `buffer_unordered(concurrency.clamp(1,64))` with `sort_by_key`, landed in `cd6ae9c` / `1d367e1` / `dd07ad5` (PR #62). The serial loop in its title only survives on `main`. The residual serial loops are in `constellation/client.rs` and `web/scan_job.rs`, which is now Task 2 under **#213**.

Also worth knowing for next time: `chainlink import` assigns IDs in file order, and the export is sorted **descending** — importing it as-is silently reverses every ID (#188→#1), breaking every cross-reference in the issue bodies. Sort ascending first. And `chainlink issue close` appends the raw issue title to `CHANGELOG.md`, which for an already-shipped issue produces a duplicate entry.
