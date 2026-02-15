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

31 tests passing, clippy clean, all CLI commands wired and tested end-to-end.

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

Use Deciduous (https://crates.io/crates/deciduous) for decision documentation.
When making meaningful technical choices (crate selection, architecture
patterns, API design, scoring approaches), log the decision with Deciduous
including the choice made, alternatives considered, and reasoning. See
[docs/deciduous-workflow.md](docs/deciduous-workflow.md) for the full
Deciduous workflow reference.

### Coding standards

This is a Rust project. Follow idiomatic Rust patterns:

- Use the `?` operator for error propagation, not `.unwrap()`
- Use `anyhow::Result` for application-level errors
- Use `thiserror` for library-level error types if needed
- Run `cargo clippy` and address warnings
- Prefer well-established crates over hand-rolling functionality
- Add comments that explain *why*, not just *what* — I'll be reading this
  code to learn from it

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

### ONNX toxicity model (default scorer)
- Detoxify `unbiased-toxic-roberta` model, run locally via `ort` crate
- No API key needed, no rate limits
- Download with `charcoal download-model` (~126 MB, one-time)
- Trained to reduce bias around identity mentions
- See `docs/toxicity-alternatives-report.md` for the evaluation that led here

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
