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

### 3. Initialize

```bash
cargo run -- init
```

Creates the SQLite database and tables.

### 4. Download the toxicity model

```bash
cargo run -- download-model
```

Downloads the ONNX toxicity model (~126 MB) to your local machine. This is a
one-time step. The model runs entirely locally — no API key needed, no rate
limits.

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
  db/               SQLite schema, queries, and data models
```

## Development

```bash
# First-time setup: install git hooks (enforces fmt + clippy + tests)
./scripts/install-hooks.sh

cargo test --all-targets  # Run all 139 tests (unit + integration)
cargo clippy              # Lint
cargo run -- status       # Quick smoke test
```

## License

All rights reserved.
