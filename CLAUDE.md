# CLAUDE.md — Charcoal Project Context

## What is this project?

Charcoal is a predictive threat detection tool for Bluesky. It identifies
accounts likely to engage with a protected user's content in a toxic or
bad-faith manner, before that engagement happens. See SPEC.md for full
requirements and README.md for usage instructions.

## Current status

The MVP is functional. All 7 implementation phases are complete:
1. Project skeleton, config, and database
2. Public AT Protocol API client (unauthenticated, read-only)
3. Topic fingerprint with TF-IDF
4. Toxicity scoring (ONNX local model, Perspective API fallback)
5. Amplification detection pipeline (Constellation-primary)
6. Profile scoring and threat tiers
7. Reports, markdown output, and polish

Post-MVP improvements applied:
- **Public API refactor**: removed `bsky-sdk` and authentication — all read
  operations use the public AT Protocol API via `PublicAtpClient` (reqwest).
  Only `BLUESKY_HANDLE` needed, no app password required.
- **Constellation-primary**: amplification detection now always uses the
  Constellation backlink index (1+ year of data). Notification polling removed.
  The `--constellation` flag is gone — it's always on.
- **Behavioral signals** (PR #5): quote ratio, reply ratio, pile-on detection,
  and engagement metrics feed a Gate + Multiplier Hybrid scoring modifier.
  Benign gate caps allies at Watch tier (12.0); hostile multiplier boosts
  threat scores by 1.0–1.5x. Pile-on detection uses 24-hour sliding window
  with 5+ distinct amplifiers threshold. DB schema v3 stores behavioral
  signals as JSON on `account_scores`.
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
- Batch DID→handle resolution via `app.bsky.actor.getProfiles`
- Validate command: scores blocked accounts via PDS repo access to verify
  pipeline accuracy (resolves DIDs via plc.directory, discovers PDS endpoints)
- DB migrations run automatically on `db::open()` (not just `charcoal init`)
- **PostgreSQL backend**: trait-based dual-backend architecture (`Database` async
  trait with `SqliteDatabase` and `PgDatabase` implementations). SQLite remains
  the default; PostgreSQL is available via `--features postgres` and activated at
  runtime when `DATABASE_URL` is set. Uses pgvector for embeddings, JSONB for
  structured data. `charcoal migrate` command transfers all data from SQLite to
  PostgreSQL.

186 tests passing, clippy clean. CLI commands: `init`, `fingerprint`, `download-model`,
`scan`, `sweep`, `score`, `report`, `status`, `validate`, `migrate` (postgres feature).

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

The project has 186 tests across six categories:

- **Unit tests** (`tests/unit_scoring.rs`) — threat tiers, score computation,
  truncation, boundary conditions
- **Topic tests** (`tests/unit_topics.rs`) — cosine similarity, keyword
  weights, TF-IDF invariants, edge cases
- **Behavioral tests** (`tests/unit_behavioral.rs`) — boost computation,
  benign gate, quote/reply ratios, pile-on detection, real-world persona
  scenarios (quote-dunker, supportive ally, pile-on participant, etc.)
- **Composition tests** (`tests/composition.rs`) — end-to-end pipelines
  (TF-IDF → fingerprint → overlap → score → tier), report generation,
  ally/hostile/irrelevant account scenarios
- **Constellation tests** (`tests/unit_constellation.rs`) — serde
  deserialization, AT-URI construction, dedup logic
- **PostgreSQL tests** (`tests/db_postgres.rs`) — integration tests for the
  Postgres backend, gated on `--features postgres` + `DATABASE_URL` env var.
  8 tests covering scan state, fingerprint, embedding, scores, events, etc.

Run all tests with `cargo test --all-targets`. The default `cargo test` only
runs library tests — integration tests live in the `tests/` directory and
need `--all-targets` to be included.

To run PostgreSQL integration tests against a live instance:
```
DATABASE_URL=postgres://charcoal:charcoal@localhost/charcoal_test \
  cargo test --all-targets --features postgres
```

### Git hooks

After cloning, run `./scripts/install-hooks.sh` to install quality gates:
- **pre-commit**: blocks commits with formatting errors, clippy warnings,
  or failing tests (skipped for docs-only commits — markdown/text files)
- **pre-push**: blocks pushes with failing tests or clippy warnings
  (skipped for docs-only pushes)

### Keep it runnable

Every feature should be testable with a simple command. If I can't run
`cargo run` and see meaningful output within a few minutes of pulling
the code, something has gone wrong.

### Database architecture

The project uses a trait-based dual-backend database layer:

- **`Database` trait** (`src/db/traits.rs`): async trait with 14 methods
  covering all DB operations. All pipeline code operates on `Arc<dyn Database>`.
- **`SqliteDatabase`** (`src/db/sqlite.rs`): wraps `rusqlite::Connection` in
  `tokio::sync::Mutex`. Default backend, no external dependencies.
- **`PgDatabase`** (`src/db/postgres.rs`): native async via sqlx. Uses pgvector
  for 384-dim embeddings, JSONB for structured data. Gated on `postgres` feature.
- **Feature flags** in `Cargo.toml`: `sqlite` (default), `postgres` (optional).
  Uses `sqlx-core`/`sqlx-postgres` as split deps to avoid `libsqlite3-sys` link
  conflict with rusqlite's bundled SQLite.
- **Runtime selection**: when `DATABASE_URL` env var is set and starts with
  `postgres://`, the app uses PostgreSQL. Otherwise, SQLite.
- **Migrations**: SQLite uses `src/db/schema.rs`; Postgres uses numbered SQL
  files in `migrations/postgres/` embedded via `include_str!`.
- **`charcoal migrate`**: one-time data transfer from SQLite→PostgreSQL.
  Only available with `--features postgres`.

Building with PostgreSQL support:
```
cargo build --features postgres
```

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

### Bluesky / AT Protocol API (public, no auth)
- Used for fetching posts, followers, and resolving DIDs to handles
- All read endpoints are public — no authentication needed
- Docs: https://docs.bsky.app/
- Public API endpoint: `https://public.api.bsky.app`
- Crate: `atrium-api` 0.25.7 (response types only), `reqwest` (HTTP client)

### ONNX models (local, no API keys needed)
- **Toxicity**: Detoxify `unbiased-toxic-roberta` (~126 MB) — 7 toxicity categories,
  trained to reduce bias around identity mentions
- **Embeddings**: `all-MiniLM-L6-v2` (~90 MB) — 384-dim sentence embeddings for
  semantic topic overlap (captures "fatphobia" ≈ "obesity" without exact keywords)
- Both run locally via `ort` crate, no rate limits
- Download both with `charcoal download-model` (one-time, ~216 MB total)
- See `docs/toxicity-alternatives-report.md` for the toxicity model evaluation

### Constellation backlink index (primary amplification detection)
- Primary source for detecting quotes/reposts of the protected user's content
- Indexes all AT Protocol amplification events — 1+ year of data
- Catches engagement from blocked/muted accounts that other methods miss
- API: `GET /xrpc/blue.microcosm.links.getBacklinks` with `subject` (AT-URI)
  and `source` (`collection:json_path`, e.g. `app.bsky.feed.post:embed.record.uri`)
- Public instance at `https://constellation.microcosm.blue`
- No auth required, no published Rust client crate — hand-rolled reqwest client
- Set `CONSTELLATION_URL` env var to override the default instance
- Always-on — no flag needed (replaced the old `--constellation` opt-in)

### Google Perspective API (fallback scorer)
- Optional fallback, enabled with `CHARCOAL_SCORER=perspective`
- Docs: https://developers.perspectiveapi.com/
- Requires `PERSPECTIVE_API_KEY` env var
- Sunsetting December 2026 — ONNX is the recommended path forward

### PostgreSQL (optional server backend)
- Optional alternative to SQLite for server deployments
- Requires pgvector extension for 384-dim embedding storage
- Crates: `sqlx-core` + `sqlx-postgres` (split to avoid libsqlite3-sys conflict
  with rusqlite), `pgvector` 0.4
- Activated by: `cargo build --features postgres` + `DATABASE_URL` env var
- Migrations in `migrations/postgres/` (3 files, embedded via `include_str!`)
- `charcoal migrate --database-url <url>` transfers SQLite data to Postgres

## Git Staging Rules - CRITICAL

**NEVER use broad git add commands that stage everything:**
- `git add -A` / `git add .` / `git add -a` / `git commit -am` / `git add *`

**ALWAYS stage files explicitly by name:**
- `git add src/main.rs src/lib.rs`
- `git add Cargo.toml Cargo.lock`

**NEVER use heredoc syntax (`<<EOF` / `<<'EOF'`) in commit commands.**
Heredocs break in zsh on this system. Use single-quoted multi-line strings
instead:
```
git commit -m 'first line

Body text here.'
```

**NEVER use `EnterWorktree` or git worktrees.**
Worktrees crash this machine. Always use a plain branch:
```
git checkout -b feat/my-feature
```

**Atomic commits — push regularly.**
Commit after each logical unit of work. Push the feature branch frequently
so work is never sitting only locally.

<!-- deciduous:start -->
## Decision Graph Workflow

**THIS IS MANDATORY. Log decisions IN REAL-TIME, not retroactively.**

### Available Slash Commands

| Command | Purpose |
|---------|---------|
| `/decision` | Manage decision graph - add nodes, link edges, sync |
| `/recover` | Recover context from decision graph on session start |
| `/work` | Start a work transaction - creates goal node before implementation |
| `/document` | Generate comprehensive documentation for a file or directory |
| `/build-test` | Build the project and run the test suite |
| `/serve-ui` | Start the decision graph web viewer |
| `/sync-graph` | Export decision graph to GitHub Pages |
| `/decision-graph` | Build a decision graph from commit history |
| `/sync` | Multi-user sync - pull events, rebuild, push |

### Available Skills

| Skill | Purpose |
|-------|---------|
| `/pulse` | Map current design as decisions (Now mode) |
| `/narratives` | Understand how the system evolved (History mode) |
| `/archaeology` | Transform narratives into queryable graph |

### The Node Flow Rule - CRITICAL

The canonical flow through the decision graph is:

```
goal -> options -> decision -> actions -> outcomes
```

- **Goals** lead to **options** (possible approaches to explore)
- **Options** lead to a **decision** (choosing which option to pursue)
- **Decisions** lead to **actions** (implementing the chosen approach)
- **Actions** lead to **outcomes** (results of the implementation)
- **Observations** attach anywhere relevant
- Goals do NOT lead directly to decisions -- there must be options first
- Options do NOT come after decisions -- options come BEFORE decisions
- Decision nodes should only be created when an option is actually chosen, not prematurely

### The Core Rule

```
BEFORE you do something -> Log what you're ABOUT to do
AFTER it succeeds/fails -> Log the outcome
CONNECT immediately -> Link every node to its parent
AUDIT regularly -> Check for missing connections
```

### Behavioral Triggers - MUST LOG WHEN:

| Trigger | Log Type | Example |
|---------|----------|---------|
| User asks for a new feature | `goal` **with -p** | "Add dark mode" |
| Exploring possible approaches | `option` | "Use Redux for state" |
| Choosing between approaches | `decision` | "Choose state management" |
| About to write/edit code | `action` | "Implementing Redux store" |
| Something worked or failed | `outcome` | "Redux integration successful" |
| Notice something interesting | `observation` | "Existing code uses hooks" |

### Document Attachments

Attach files (images, PDFs, diagrams, specs, screenshots) to decision graph nodes for rich context.

```bash
# Attach a file to a node
deciduous doc attach <node_id> <file_path>
deciduous doc attach <node_id> <file_path> -d "Architecture diagram"
deciduous doc attach <node_id> <file_path> --ai-describe

# List documents
deciduous doc list              # All documents
deciduous doc list <node_id>    # Documents for a specific node

# Manage documents
deciduous doc show <doc_id>     # Show document details
deciduous doc describe <doc_id> "Updated description"
deciduous doc describe <doc_id> --ai   # AI-generate description
deciduous doc open <doc_id>     # Open in default application
deciduous doc detach <doc_id>   # Soft-delete (recoverable)
deciduous doc gc                # Remove orphaned files from disk
```

**When to suggest document attachment:**

| Situation | Action |
|-----------|--------|
| User shares an image or screenshot | Ask: "Want me to attach this to the current goal/action node?" |
| User references an external document | Ask: "Should I attach a copy to the decision graph?" |
| Architecture diagram is discussed | Suggest attaching it to the relevant goal node |
| Files not in the project are dropped in | Attach to the most relevant active node |

**Do NOT aggressively prompt for documents.** Only suggest when files are directly relevant to a decision node. Files are stored in `.deciduous/documents/` with content-hash naming for deduplication.

### CRITICAL: Capture VERBATIM User Prompts

**Prompts must be the EXACT user message, not a summary.** When a user request triggers new work, capture their full message word-for-word.

**BAD - summaries are useless for context recovery:**
```bash
# DON'T DO THIS - this is a summary, not a prompt
deciduous add goal "Add auth" -p "User asked: add login to the app"
```

**GOOD - verbatim prompts enable full context recovery:**
```bash
# Use --prompt-stdin for multi-line prompts
deciduous add goal "Add auth" -c 90 --prompt-stdin << 'EOF'
I need to add user authentication to the app. Users should be able to sign up
with email/password, and we need OAuth support for Google and GitHub. The auth
should use JWT tokens with refresh token rotation.
EOF

# Or use the prompt command to update existing nodes
deciduous prompt 42 << 'EOF'
The full verbatim user message goes here...
EOF
```

**When to capture prompts:**
- Root `goal` nodes: YES - the FULL original request
- Major direction changes: YES - when user redirects the work
- Routine downstream nodes: NO - they inherit context via edges

**Updating prompts on existing nodes:**
```bash
deciduous prompt <node_id> "full verbatim prompt here"
cat prompt.txt | deciduous prompt <node_id>  # Multi-line from stdin
```

Prompts are viewable in the web viewer.

### CRITICAL: Maintain Connections

**The graph's value is in its CONNECTIONS, not just nodes.**

| When you create... | IMMEDIATELY link to... |
|-------------------|------------------------|
| `outcome` | The action that produced it |
| `action` | The decision that spawned it |
| `decision` | The option(s) it chose between |
| `option` | Its parent goal |
| `observation` | Related goal/action |
| `revisit` | The decision/outcome being reconsidered |

**Root `goal` nodes are the ONLY valid orphans.**

### Quick Commands

```bash
deciduous add goal "Title" -c 90 -p "User's original request"
deciduous add action "Title" -c 85
deciduous link FROM TO -r "reason"  # DO THIS IMMEDIATELY!
deciduous serve   # View live (auto-refreshes every 30s)
deciduous sync    # Export for static hosting

# Metadata flags
# -c, --confidence 0-100   Confidence level
# -p, --prompt "..."       Store the user prompt (use when semantically meaningful)
# -f, --files "a.rs,b.rs"  Associate files
# -b, --branch <name>      Git branch (auto-detected)
# --commit <hash|HEAD>     Link to git commit (use HEAD for current commit)
# --date "YYYY-MM-DD"      Backdate node (for archaeology)

# Branch filtering
deciduous nodes --branch main
deciduous nodes -b feature-auth
```

### CRITICAL: Link Commits to Actions/Outcomes

**After every git commit, link it to the decision graph!**

```bash
git commit -m "feat: add auth"
deciduous add action "Implemented auth" -c 90 --commit HEAD
deciduous link <goal_id> <action_id> -r "Implementation"
```

The `--commit HEAD` flag captures the commit hash and links it to the node. The web viewer will show commit messages, authors, and dates.

### Git History & Deployment

```bash
# Export graph AND git history for web viewer
deciduous sync

# This creates:
# - docs/graph-data.json (decision graph)
# - docs/git-history.json (commit info for linked nodes)
```

To deploy to GitHub Pages:
1. `deciduous sync` to export
2. Push to GitHub
3. Settings > Pages > Deploy from branch > /docs folder

Your graph will be live at `https://<user>.github.io/<repo>/`

### Branch-Based Grouping

Nodes are auto-tagged with the current git branch. Configure in `.deciduous/config.toml`:
```toml
[branch]
main_branches = ["main", "master"]
auto_detect = true
```

### Audit Checklist (Before Every Sync)

1. Does every **outcome** link back to what caused it?
2. Does every **action** link to why you did it?
3. Any **dangling outcomes** without parents?

### Session Start Checklist

```bash
deciduous check-update    # Update needed? Run 'deciduous update' if yes
deciduous nodes           # What decisions exist?
deciduous edges           # How are they connected? Any gaps?
deciduous doc list        # Any attached documents to review?
git status                # Current state
```

### Multi-User Sync

Sync decisions with teammates via event logs:

```bash
# Check sync status
deciduous events status

# Apply teammate events (after git pull)
deciduous events rebuild

# Compact old events periodically
deciduous events checkpoint --clear-events
```

Events auto-emit on add/link/status commands. Git merges event files automatically.
<!-- deciduous:end -->
