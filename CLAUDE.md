# CLAUDE.md — Charcoal Project Context

## What is this project?

Charcoal is a predictive threat detection tool for Bluesky. It identifies
accounts likely to engage with a protected user's content in a toxic or
bad-faith manner, before that engagement happens. See SPEC.md for full
requirements and README.md for usage instructions.

## Current status

The MVP is functional. All 7 implementation phases are complete:
1. Project skeleton, config, and database
2. Bluesky auth + post fetching
3. Topic fingerprint with TF-IDF
4. Toxicity scoring (ONNX local model, Perspective API fallback)
5. Amplification detection pipeline
6. Profile scoring and threat tiers
7. Reports, markdown output, and polish

Post-MVP improvements applied:
- Sentence embeddings for semantic topic overlap (all-MiniLM-L6-v2, 384-dim)
- Multiplicative threat scoring: `tox * 70 * (1 + overlap * 1.5)` — overlap
  amplifies toxicity instead of contributing independently, so allies with high
  overlap but low toxicity stay Low tier
- Cosine similarity for topic overlap (replaced weighted Jaccard)
- Weighted toxicity categories (identity_attack/insult/threat elevated)
- Crash-resilient pipelines (incremental DB writes + panic catching)
- Mode 2 background sweep for second-degree network
- UTF-8 safe string truncation (prevents panics on emoji/CJK text)
- LazyLock regex compilation (avoids redundant compilations in TF-IDF)
- ONNX inference offloaded to `spawn_blocking` (keeps async runtime responsive)
- Git hooks for pre-commit (fmt + clippy + tests) and pre-push (tests + clippy)

132 tests passing, clippy clean, all CLI commands wired and tested end-to-end.

### External contributions

PR #1 by Bobby Grayson ([@notactuallytreyanastasio](https://github.com/notactuallytreyanastasio)):
- Correctness fixes: evidence threshold, weighted sorting, float comparison
- Performance: LazyLock regex, spawn_blocking for ONNX inference
- UTF-8 safety: `truncate_chars()` helper replacing byte-slice truncation
- 77 integration tests covering pure functions and composition chains
- Git hooks installer script (`scripts/install-hooks.sh`)

## Who am I?

I'm Bryan (@chaosgreml.in on Bluesky). I'm not a software developer — I'm an
IT consultant and community builder who is learning to build software with AI
assistance. When you explain decisions or ask me questions, use plain language
rather than assuming I know framework-specific jargon. I can learn quickly,
but I need context for unfamiliar concepts.

I do maintain one other Rust application, so I'm familiar with cargo, basic
Rust project structure, and the general development workflow. I'm not fluent
in Rust, but I can read it and follow along when things are well-commented.

## CRITICAL: System Context

ALWAYS read /.sprite/llm.txt when getting started. This provides you crucial
information on the capabilities you have on this system.

## Development workflow

This project uses Chainlink (https://github.com/dollspace-gay/chainlink)
for issue tracking, session management, and coding guardrails. At the start
of each work session, run `chainlink session start` to load previous context.
At the end, run `chainlink session end --notes "..."` to preserve state for
next time. Break large tasks into Chainlink issues with subissues.

**CRITICAL: Deciduous decision logging is mandatory — not aspirational.**
Use Deciduous (https://crates.io/crates/deciduous) to log every meaningful
action and decision in real-time. This is NOT something to "catch up on later."

- **Before implementing**: `deciduous add action "..." --commit HEAD -f "files"`
- **After completing**: `deciduous add outcome "..." --commit HEAD`
- **Every action node MUST include `--commit`** to link it to the git history
- **Link nodes immediately** with `deciduous link FROM TO -r "reason"`
- Log decisions, alternatives considered, and reasoning

The full workflow reference is at
[docs/deciduous-workflow.md](docs/deciduous-workflow.md). If you find yourself
batching deciduous updates at the end of a session, you are doing it wrong.

Deciduous v0.12.0 is installed. Notable features beyond basic logging:
- `deciduous writeup` — generate PR writeups from graph nodes
- `deciduous audit --associate-commits` — auto-link nodes to commits
- `deciduous diff export/apply` — multi-user sync via patch files
- `deciduous roadmap` — sync ROADMAP.md with GitHub Issues
- `deciduous integration` — show Claude Code integration status

### Coding standards

This is a Rust project. Follow idiomatic Rust patterns:

- Use the `?` operator for error propagation, not `.unwrap()`
- Use `anyhow::Result` for application-level errors
- Use `thiserror` for library-level error types if needed
- Run `cargo clippy` and address warnings
- Prefer well-established crates over hand-rolling functionality
- Add comments that explain *why*, not just *what* — I'll be reading this
  code to learn from it

### Testing

The project has 132 tests across three categories:

- **Unit tests** (`tests/unit_scoring.rs`) — threat tiers, score computation,
  truncation, boundary conditions
- **Topic tests** (`tests/unit_topics.rs`) — cosine similarity, keyword
  weights, TF-IDF invariants, edge cases
- **Composition tests** (`tests/composition.rs`) — end-to-end pipelines
  (TF-IDF → fingerprint → overlap → score → tier), report generation,
  ally/hostile/irrelevant account scenarios

Run all tests with `cargo test --all-targets`. The default `cargo test` only
runs library tests — integration tests live in the `tests/` directory and
need `--all-targets` to be included.

### Git hooks

After cloning, run `./scripts/install-hooks.sh` to install quality gates:
- **pre-commit**: blocks commits with formatting errors, clippy warnings,
  or failing tests
- **pre-push**: blocks pushes with failing tests or clippy warnings

### Keep it runnable

Every feature should be testable with a simple command. If I can't run
`cargo run` and see meaningful output within a few minutes of pulling
the code, something has gone wrong.

## Domain knowledge you should know

### How harassment works on Bluesky

The primary harassment escalation vector on Bluesky is the quote-post. Someone
with a hostile audience quotes a vulnerable user's post with mocking or hostile
commentary, which broadcasts the original post to an audience that didn't
choose to see it. This is why Charcoal focuses on amplification events (quotes
and reposts) as the primary trigger for threat analysis.

Followers are the LEAST likely source of harassment — they opted in to seeing
the content. The danger comes from second-degree and third-degree exposure.

### Topic sensitivity

The protected user (Bryan) is publicly visible in several topic areas that
attract targeted hostility. These include (but are not limited to) fat
liberation and body politics, queer and trans identity, DEI and anti-racism,
AI/LLMs, community governance and cybernetics, a cappella music education, and
Atlassian developer community topics. However, Bryan cannot fully enumerate
their own topic areas — the system must extract a topic fingerprint dynamically
from their posting history rather than relying on a hardcoded list.

When scoring topic overlap, remember that topic proximity alone is not a threat
signal. An account that posts supportively about fat liberation is an ally.
The threat signal is the COMBINATION of topical proximity and behavioral
hostility — someone who is active in the same spaces AND has a pattern of
toxic engagement.

### The broader Charcoal vision

This MVP is the intelligence layer of a larger system. The eventual product
includes automated muting/blocking with user review, shared intelligence
across multiple protected users, real-time monitoring via AT Protocol event
streams, and deployment on a cloud platform (exact platform TBD — could be
Cloudflare Workers, Railway, or something else). None of that is in scope for
this MVP, but keep it in mind when making architectural decisions — don't
paint us into a corner that makes the future version harder to build.

## Key external services

### Bluesky / AT Protocol API
- Used for fetching posts, followers, and detecting amplification events
- Docs: https://docs.bsky.app/
- Authentication via app password (provided as env var)
- Crates: `bsky-sdk` 0.1.23, `atrium-api` 0.25.7

### ONNX models (local, no API keys needed)
- **Toxicity**: Detoxify `unbiased-toxic-roberta` (~126 MB) — 7 toxicity categories,
  trained to reduce bias around identity mentions
- **Embeddings**: `all-MiniLM-L6-v2` (~90 MB) — 384-dim sentence embeddings for
  semantic topic overlap (captures "fatphobia" ≈ "obesity" without exact keywords)
- Both run locally via `ort` crate, no rate limits
- Download both with `charcoal download-model` (one-time, ~216 MB total)
- See `docs/toxicity-alternatives-report.md` for the toxicity model evaluation

### Google Perspective API (fallback scorer)
- Optional fallback, enabled with `CHARCOAL_SCORER=perspective`
- Docs: https://developers.perspectiveapi.com/
- Requires `PERSPECTIVE_API_KEY` env var
- Sunsetting December 2026 — ONNX is the recommended path forward

## Git Staging Rules - CRITICAL

**NEVER use broad git add commands that stage everything:**
- `git add -A` / `git add .` / `git add -a` / `git commit -am` / `git add *`

**ALWAYS stage files explicitly by name:**
- `git add src/main.rs src/lib.rs`
- `git add Cargo.toml Cargo.lock`
