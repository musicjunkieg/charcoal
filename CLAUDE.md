# CLAUDE.md — Charcoal Project Context

## What is this project?

Charcoal is a predictive threat detection tool for Bluesky. It identifies
accounts likely to engage with a protected user's content in a toxic or
bad-faith manner, before that engagement happens. See SPEC.md for full
requirements and README.md for usage instructions.

## Response Style

Keep responses concise; avoid large single outputs. Break long explanations or
file dumps into smaller chunks to stay well under output token limits.

## Current status

The MVP and all post-MVP phases are shipped and deployed. **Do not trust a
status list in this file — it goes stale.** For what is actually true right now:

- `CHANGELOG.md` — what shipped, in order, with the reasoning
- `git log` / `chainlink issue list -s open` — current work and open issues
- `Cargo.toml` — feature flags (`web`, `postgres`) and dependency versions
- `src/db/schema.rs` + `migrations/postgres/` — the live schema version

Deployed at https://charcoal.watch (Railway, `main`), with a staging
environment at `charcoal-web-staging.up.railway.app` (`staging` branch, its own
Postgres + volume).

External contributions: PR #1 by Bobby Grayson
([@notactuallytreyanastasio](https://github.com/notactuallytreyanastasio)) —
correctness and UTF-8 safety fixes, the `truncate_chars()` helper, the first
integration-test suite, and `scripts/install-hooks.sh`.

## Who am I?

I'm Bryan (@chaosgreml.in on Bluesky). I'm not a software developer — I'm an
IT consultant and community builder who is learning to build software with AI
assistance. When you explain decisions or ask me questions, use plain language
rather than assuming I know framework-specific jargon. I can learn quickly,
but I need context for unfamiliar concepts.

I do maintain one other Rust application, so I'm familiar with cargo, basic
Rust project structure, and the general development workflow. I'm not fluent
in Rust, but I can read it and follow along when things are well-commented.


## Development workflow

This project uses Chainlink (https://github.com/dollspace-gay/chainlink)
for issue tracking, session management, and coding guardrails. At the start
of each work session, run `chainlink session start` to load previous context.
At the end, run `chainlink session end --notes "..."` to preserve state for
next time. Break large tasks into Chainlink issues with subissues.

**TDD workflow.** Write/finalize the spec, produce a test-driven implementation
plan, then implement. Always leave a handoff note at the end of planning
sessions.

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

Tests live in `tests/` (see the filenames — they're named by area) plus inline
`#[cfg(test)]` modules. `cargo test` runs unit, doc, and integration tests; use
`cargo test --all-targets` in CI so benches and examples also compile.

**⚠️ Model-gated tests silently skip and still print `ok`.** Every test that
loads a real ONNX model returns early when the model files aren't found, having
asserted nothing. Two traps stack here:

1. **Test binaries never load `.env`** (dotenvy runs in `main.rs` only), so they
   ignore `CHARCOAL_MODEL_DIR=./models` and look in the platform data dir. Always
   run: `CHARCOAL_MODEL_DIR=./models cargo test --features web`
2. **`cargo test | grep "^SKIP"` is a no-op** — libtest discards stderr from
   *passing* tests, so it returns empty regardless. You must pass
   `-- --show-output`.

The full check, which should report **zero** skips:
```
CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "^\s*SKIP:"
```

Note the trailing colon and the **absence of `-i`**. This check used to read
`grep -iE "^\s*SKIP"`, which false-positives on any test whose *name* begins
with "skip" — `skips_are_scoped_per_user` and friends match it, so a clean run
reports skips that do not exist. Match the `SKIP:` sentinel exactly.

To run OAuth tests (requires `--features web`):
```
cargo test --features web --test unit_oauth --test web_oauth
```

To run PostgreSQL integration tests against a live instance:
```
DATABASE_URL=postgres://$USER@localhost/charcoal_test \
  cargo test --all-targets --features postgres
```

⚠️ **This used to read `postgres://charcoal:charcoal@localhost/...`, which
fails on a Homebrew Postgres install** — `brew services` creates a superuser
role named after your OS account and no `charcoal` role, so that URL dies with
`FATAL: role "charcoal" does not exist` (#272). Use `$USER` as above, or create
the role once with `createuser -s charcoal`. If the database is missing,
`createdb charcoal_test` first.

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

Trait-based dual backend (`src/db/`): `Database` async trait, `SqliteDatabase`
(default) and `PgDatabase` (`--features postgres`). Read the trait for the
current method set — don't trust a list here.

Non-obvious constraints:

- **Runtime selection is implicit**: PostgreSQL activates when `DATABASE_URL` is
  set and starts with `postgres://`. Otherwise SQLite. No flag.
- **`sqlx-core`/`sqlx-postgres` are split deps on purpose** — pulling in full
  `sqlx` causes a `libsqlite3-sys` link conflict with rusqlite's bundled SQLite.
- **Postgres migrations must SELF-RECORD their version**
  (`INSERT INTO schema_version ... ON CONFLICT DO NOTHING`). The runner does not
  do it for you; a migration that skips this re-runs forever.
- Migrations auto-run on `db::open()`, not just `charcoal init`.

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
- **NLI cross-encoder**: `nli-deberta-v3-xsmall` (~284 MB) — contextual hostility.
  The **fp32** export, not the quantized one: the quantized export computes its
  activation scale per-tensor at runtime, so batching hypotheses corrupted every
  row on x86-64 (#231). Do not "optimize" this back to `model_quantized.onnx`.
- All run locally via `ort` crate, no rate limits
- Download all with `charcoal download-model` (one-time, ~500 MB total)
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

## Environment / Tooling

Use `python3` (not `python`) for all Python invocations and in MCP/`.mcp.json`
configs in this environment.

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

**Git worktrees are allowed.**
Previously this rule said worktrees crash the machine — that turned out to
be stale after the mid-2026 upgrade. Multiple recent sessions have used
worktrees against charcoal without issue. Prefer a plain branch for small
work (`git checkout -b feat/my-feature`); use a worktree when you need to
keep an in-flight branch untangled from other work.

**Project-specific ignores live in `.gitignore.local`, not `.gitignore`.**
`.gitignore` is template-managed and gets overwritten by
`update-project-from-template`; anything project-only (`refs/zentropi_info.txt`,
`/output/`, `/backups/`, `/docs/research/`) belongs in `.gitignore.local`.
`scripts/install-hooks.sh` symlinks `.git/info/exclude` at the shared gitdir
to that file so git honors it transparently — run install-hooks.sh once from
the primary checkout (not a worktree) after the first clone or after adding
`.gitignore.local` in a fresh clone.

**Atomic commits — push regularly.**
Commit after each logical unit of work. Push the feature branch frequently
so work is never sitting only locally.

<!-- deciduous:start -->
## Decision Graph Workflow

**THIS IS MANDATORY. Log decisions IN REAL-TIME, not retroactively.**

**Verify node IDs before wiring edges.** When editing decision/memory graph
edges, verify the target node ID against the spec before wiring — confirm the
node number explicitly.

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

Attach files (images, PDFs, diagrams, specs, screenshots) to decision graph nodes
for rich context — `deciduous doc --help` for the commands.

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
deciduous add action "Title" -c 85 --commit HEAD
deciduous link FROM TO -r "reason"  # DO THIS IMMEDIATELY!
```

Full flag reference: `deciduous add --help`.

### CRITICAL: Link Commits to Actions/Outcomes

**After every git commit, link it to the decision graph!**

```bash
git commit -m "feat: add auth"
deciduous add action "Implemented auth" -c 90 --commit HEAD
deciduous link <goal_id> <action_id> -r "Implementation"
```

The `--commit HEAD` flag captures the commit hash and links it to the node. The web viewer will show commit messages, authors, and dates.

### Git History & Deployment

`deciduous sync` exports `docs/graph-data.json` + `docs/git-history.json`, which
GitHub Pages serves from the `/docs` folder. Nodes are auto-tagged with the
current git branch (configurable in `.deciduous/config.toml`).

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

Decisions sync with teammates via event logs — see `deciduous events --help`.
Events auto-emit on add/link/status commands; git merges the event files
automatically.
<!-- deciduous:end -->
