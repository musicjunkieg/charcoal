# Charcoal

Predictive threat detection for Bluesky. Charcoal identifies accounts likely
to engage with your content in a toxic or bad-faith manner — before that
engagement happens.

## How it works

When someone quotes or reposts your content on Bluesky, their followers are
suddenly exposed to your posts. Charcoal monitors these **amplification
events**, then scores the amplifier's followers on two axes:

- **Toxicity** — does this account have a pattern of hostile language?
- **Topic overlap** — does this account post about the same subjects you do?

Neither signal alone is a threat. An account that's hostile but posts about
unrelated topics is unlikely to find you. An account that shares your topics
but isn't hostile is probably an ally. The **combination** of high toxicity
and high topic overlap is what Charcoal flags.

The output is a ranked threat list with evidence (the toxic posts that drove
each score), so you can review and decide what action to take.

## Quick start

### 1. Build

```bash
git clone https://github.com/musicjunkieg/charcoal.git
cd charcoal
cargo build --release
```

### 2. Configure

Copy the example environment file and fill in your credentials:

```bash
cp .env.example .env
```

You need:
- **BLUESKY_HANDLE** — your Bluesky handle (e.g. `yourname.bsky.social`)

No app password or authentication is needed — Charcoal uses the public AT
Protocol API for all read operations.

Optional settings (see `.env.example` for details):
- `PUBLIC_API_URL` — custom public API endpoint (default: `https://public.api.bsky.app`)
- `CONSTELLATION_URL` — Constellation backlink index URL
- `CHARCOAL_SCORER` — toxicity backend: `onnx` (default) or `perspective`
- `CHARCOAL_MODEL_DIR` — custom path for ONNX model files
- `CHARCOAL_DB_PATH` — custom path for the SQLite database
- `RUST_LOG` — log level (default: `charcoal=info`)
- `CHARCOAL_ONNX_SESSIONS` — number of ONNX toxicity sessions to pool
  (default `1`, clamped to 1–8; each is a separate ~126 MB model load).
  Only worth raising if the #343 Phase 0 runbook shows a gain.
- `CHARCOAL_COPE_B_POLICY_VERSION` — the `POLICY_VERSION` the hosted CoPE-B
  endpoint is serving (only read when `CHARCOAL_CLASSIFIER=runpod`). It must
  equal the value the endpoint reports on its verdicts, **verbatim**; the
  default is `policy-unknown`. See "Stage-2 classifier policy" below.
- `CHARCOAL_REFRESH_INTERVAL_HOURS` — how often the background score refresh
  runs (default `24`; `0` or `off` disables it; clamped to 1–168). See
  "Score expiry and refresh" below.

### 3. Initialize

```bash
cargo run -- init
```

Creates the SQLite database and tables.

### 4. Download ONNX models

```bash
cargo run -- download-model
```

Downloads three ONNX models (~500 MB total) to your local machine:
- **Toxicity model** — Detoxify unbiased-toxic-roberta (~126 MB) for toxicity scoring
- **Embedding model** — all-MiniLM-L6-v2 (~90 MB) for semantic topic overlap
- **NLI cross-encoder** — nli-deberta-v3-xsmall (~284 MB) for contextual hostility

This is a one-time step. All three models run entirely locally — no API key needed,
no rate limits. Files are stored in `~/.local/share/charcoal/models/` (macOS:
`~/Library/Application Support/charcoal/models/`).

### 5. Build your topic fingerprint

```bash
cargo run -- fingerprint
```

Fetches your recent posts and extracts a topic fingerprint using TF-IDF
analysis. The fingerprint shows what subjects you post about and how much.
Review the output to confirm it looks accurate. Rebuild anytime with
`--refresh`.

### 6. Scan for threats

```bash
cargo run -- scan --analyze
```

This is the main pipeline:
1. Queries the Constellation backlink index for quote/repost events on your posts
2. Fetches the follower list of each amplifier
3. Scores each follower for toxicity and topic overlap
4. Stores results in the database

Options:
- `--analyze` — actually score followers (without this, only events are recorded)
- `--max-followers N` — limit followers analyzed per amplifier (default: 50)
- `--concurrency N` — parallel scoring workers (default: 8)

### 7. Sweep second-degree network (optional)

```bash
cargo run -- sweep
```

Scans your followers-of-followers — the accounts one hop removed from your
direct audience. These are people who haven't encountered your content yet
but may if an amplification event occurs.

Options:
- `--max-followers N` — first-degree followers to scan (default: 200)
- `--depth N` — second-degree followers per first-degree (default: 50)
- `--concurrency N` — parallel scoring workers (default: 8)

This is slower than `scan` (potentially thousands of API calls) and is
designed for periodic use rather than continuous monitoring.

### 8. View results

**Score a single account:**
```bash
cargo run -- score @someone.bsky.social
```

**Generate a threat report:**
```bash
cargo run -- report
```

Outputs a ranked threat list to the terminal and saves a markdown report to
`output/charcoal-report.md`. Use `--min-score N` to filter by minimum threat score.

**Check system status:**
```bash
cargo run -- status
```

Shows last scan time, database stats, fingerprint age, and scorer config.

## Threat tiers

Charcoal assigns each scored account a threat tier based on their combined
toxicity + topic overlap score (0-100):

| Tier | Score | Meaning |
|------|-------|---------|
| **Low** | 0-7 | No significant threat signal |
| **Watch** | 8-14 | Some overlap or toxicity — worth monitoring |
| **Elevated** | 15-24 | Notable combination of hostility and topic proximity |
| **High** | 25+ | Strong threat signal — both toxic and topically close |

## Score expiry and refresh

A threat score is a statement about how an account was behaving *when it was
scored*. People change, and so do Charcoal's models — so every score now has
an expiry date, and old scores stop being shown instead of quietly ageing into
fiction.

**How long a score lasts.** When a score is written, Charcoal records how
confident it was and stamps an expiry on the row:

| Scoring confidence | What it means | Valid for |
|---|---|---|
| High | 50+ posts analysed, full pipeline including context pairs | 14 days |
| Standard | 25–50 posts, standard sampling | 7 days |
| Low | fewer than 25 posts, the scorer exited early | 3 days |

The less Charcoal had to look at, the sooner it wants to look again.

**The scoring revision.** Alongside the expiry, each row records the *revision*
it was scored under. That is one opaque string combining a hand-maintained
version (`SCORING_GENERATION`) with the identity of every model the binary
loads — toxicity, embeddings, NLI. Scores from two different revisions are not
comparable, so swapping any model automatically retires every stored score;
nobody has to remember to do it. (The database column is called
`scoring_generation` and some UI copy still says "generation" — same thing.)

**What "Expired" means on the dashboard.** A score is shown only if it has not
expired **and** it was produced by the revision now running. Everything else is
hidden — not deleted. The dashboard, `GET /api/status` (`tier_counts.expired`)
and `charcoal status` all report how many rows are hidden, so a list that
suddenly shrinks has a visible explanation rather than looking like data loss.
Nothing is ever removed; an expired row comes back the moment it is re-scored.

**The nightly refresh.** Scores return to the lists two ways. The normal way is
re-engagement: someone quotes or reposts your post again, and a scan re-scores
them. The other is a background job that rides on the existing scan admitter —
no extra process, no extra database lock. Roughly once a day per user, it
re-scores only the accounts that matter most: the ones already rated **High or
Elevated** whose scores expire within the next two days, or that were scored
under an older revision. Low and Watch accounts are left to expire; they will
be re-scored if they come back.

The admitter ticks every 30 seconds and each tick queues at most 25 users, so
the work is spread out rather than arriving as one nightly stampede — which
matters most right after a revision change, when *everybody* is due at once.

```bash
CHARCOAL_REFRESH_INTERVAL_HOURS=24   # default. 0 or "off" disables the job.
                                     # Clamped to 1–168 (one week).
```

**A refresh never replaces a scan you asked for.** If you click Scan while a
refresh is queued for you, that queued job becomes your full scan and keeps its
place in line. If a refresh is already *running*, your request is written down
and becomes a queued full scan the moment the refresh finishes — you do not
have to click again, and it survives a restart. The 24-hour scan cooldown is
measured from your last full scan that actually finished; a nightly refresh
never resets it, and a scan that was interrupted never starts one.

**When a refresh cannot do the job honestly, it does not do it.** A refresh
cannot rebuild a topic fingerprint, so if yours is missing or was built by a
different embedding model, the refresh stands down and asks for a full scan
instead. If it cannot load the stored evidence it needs, it fails and retries
in an hour rather than writing a score computed without it. Failed, deferred
and interrupted refreshes retry hourly; only a clean run earns the next nightly
slot.

## Toxicity scoring

Charcoal uses a local ONNX model ([Detoxify unbiased-toxic-roberta](https://github.com/unitaryai/detoxify))
by default. This model:
- Runs on CPU with no API calls or rate limits
- Returns scores across 7 toxicity categories
- Was trained to reduce bias around identity mentions (important when your
  topics include things like fat liberation, queer identity, or trans rights)

Google's Perspective API is available as a fallback by setting
`CHARCOAL_SCORER=perspective` in your `.env` file (requires a
`PERSPECTIVE_API_KEY`). Note: Perspective API is sunsetting December 2026.

### Ensemble scoring (optional)

When `OPENAI_API_KEY` is set, Charcoal runs both the ONNX model and OpenAI's
free Moderation API concurrently. When both classifiers agree, their scores
are averaged. When they disagree (difference > 0.25), the lower score is used
by default — this reduces false positives from reclaimed language and cultural
context that a single model may misclassify. No env var = ONNX-only (same as
before).

### Stage-2 classifier policy

The Stage-2 classifier (`CHARCOAL_CLASSIFIER=runpod` — the self-hosted CoPE-B
endpoint) runs **outside** this binary, so Charcoal cannot read its identity
the way it reads the ONNX model versions it loads itself. The operator declares
it instead:

```bash
CHARCOAL_COPE_B_POLICY_VERSION=policy-v1   # the endpoint's own POLICY_VERSION
```

It must equal, character for character, the `policy_version` the endpoint
reports on its verdicts. Leading and trailing whitespace is trimmed (a
copy-pasted trailing newline would otherwise break everything below); unset or
blank means `policy-unknown`.

**What happens if it does not match.** A stored verdict records the policy that
actually produced it, and a score may only be published from verdicts this
binary recognises. So a mismatched value means every Stage-2 verdict is foreign
evidence: every account is re-gathered once and then skipped, and no scores are
written. To make that cheap to find rather than expensive to discover:

- every scan **probes the endpoint once before it gathers anything** and
  refuses to start, naming both values and this variable;
- if a scan is already running when the endpoint changes, the batch mapper logs
  one error per batch (not per post) saying the same thing.

The fix is one environment variable. If the endpoint's *policy itself* changed
(and not just its label), also bump `SCORING_GENERATION` — the stored scores
were produced under the old policy and should expire.

## PostgreSQL backend (optional)

Charcoal uses SQLite by default. For server deployments you can switch to
PostgreSQL:

```bash
# Build with Postgres support
cargo build --release --features postgres

# Point at your database
export DATABASE_URL=postgres://user:pass@host/dbname
```

**Prerequisite — pgvector extension:** Charcoal's first migration runs
`CREATE EXTENSION IF NOT EXISTS vector`, which requires superuser privileges
(or the extension to be pre-installed by your database provider). On managed
Postgres (Railway, Fly.io, Supabase, etc.) the `vector` extension is usually
available but you may need to enable it through their dashboard or with a
superuser connection before running Charcoal for the first time. On
self-hosted Postgres, run `CREATE EXTENSION vector` as a superuser once:

```sql
-- Connect as a superuser, then:
CREATE EXTENSION IF NOT EXISTS vector;
```

To transfer existing SQLite data to Postgres:

```bash
cargo run --features postgres -- migrate --database-url postgres://user:pass@host/dbname
```

## Architecture

```
src/
  main.rs           CLI entry point (clap)
  config.rs         Environment-based configuration
  lib.rs            Library root

  bluesky/          Public AT Protocol client, post fetching, amplification types
  topics/           TF-IDF topic extraction and fingerprinting
  toxicity/         Scorer trait + ONNX and Perspective backends
  scoring/          Profile building and threat score computation
  pipeline/         Amplification detection pipeline
  output/           Terminal display and markdown report generation
  db/               SQLite/PostgreSQL backends, schema, queries, and data models
```

## Web dashboard (optional)

Charcoal includes a web-based dashboard for browsing scored accounts and
triggering scans from a browser. It uses AT Protocol OAuth for authentication.

### Build with web support

```bash
cd web && npm ci && npm run build && cd ..
cargo build --release --features web
```

### Configure OAuth

You need three additional environment variables (see `.env.example`):

- `CHARCOAL_ALLOWED_DID` — your Bluesky DID (only this account can sign in)
- `CHARCOAL_OAUTH_CLIENT_ID` — URL of your OAuth client metadata document
- `CHARCOAL_SESSION_SECRET` — HMAC signing key for session cookies (generate
  with `openssl rand -hex 32`)
- `CHARCOAL_TOKEN_KEY` — optional; 64 hex chars (32 bytes). Encrypts stored
  OAuth write sessions for mute/block. Generate with `openssl rand -hex 32`.
  **Unset = mute/block disabled**; rotating it invalidates every stored
  session (people reconnect once).

For local development, use Tailscale Funnel to get a public HTTPS URL:

```bash
tailscale funnel 3000
```

Then register that URL as your OAuth client ID.

### Run the dashboard

```bash
cargo run --features web -- serve
```

The dashboard is available at `http://localhost:3000` (or your Tailscale Funnel URL).

## Development

```bash
# First-time setup: install git hooks (enforces fmt + clippy + tests)
./scripts/install-hooks.sh

cargo test --features web   # Run all 225 tests (unit + integration + OAuth)
cargo clippy                # Lint
cargo run -- status         # Quick smoke test
```

### macOS Tahoe / Xcode Beta workaround

On macOS Tahoe with Xcode Beta installed, the linker may fail to find
`clang_rt.osx`. Fix by setting the library path before build/test commands:

```bash
export LIBRARY_PATH="/Library/Developer/CommandLineTools/usr/lib/clang/17/lib/darwin:$LIBRARY_PATH"
```

Add this to your shell profile (`~/.zshrc`) to make it permanent.

### PostgreSQL tests

PostgreSQL integration tests require a live database:

```bash
DATABASE_URL=postgres://charcoal:charcoal@localhost/charcoal_test \
  cargo test --all-targets --features postgres
```

## License

All rights reserved.
