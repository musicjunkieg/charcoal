# CLAUDE.md — Charcoal Project Context

## What is this project?

Charcoal is a predictive threat detection tool for Bluesky. It identifies
accounts likely to engage with a protected user's content in a toxic or
bad-faith manner, before that engagement happens. See SPEC.md for full
requirements.

## Who am I?

I'm Bryan. I'm not a software developer — I'm an IT consultant and community
builder who is learning to build software with AI assistance. When you explain
decisions or ask me questions, use plain language rather than assuming I know
framework-specific jargon. I can learn quickly, but I need context for
unfamiliar concepts.

I do maintain one other Rust application, so I'm familiar with cargo, basic
Rust project structure, and the general development workflow. I'm not fluent
in Rust, but I can read it and follow along when things are well-commented.

## CRITICAL: System Context

ALWAYS read /.sprite/llm.txt when getting started. This provides you crucial information on the capabilities you have on this system. 
## Development workflow

This project uses Chainlink (https://github.com/dollspace-gay/chainlink)
for issue tracking, session management, and coding guardrails. At the start
of each work session, run `chainlink session start` to load previous context.
At the end, run `chainlink session end --notes "..."` to preserve state for
next time. Break large tasks into Chainlink issues with subissues.

Use Deciduous (https://crates.io/crates/deciduous) for decision documentation.
When making meaningful technical choices (crate selection, architecture
patterns, API design, scoring approaches), log the decision with Deciduous
including the choice made, alternatives considered, and reasoning.

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
- Authentication will be via app password (provided as env var)

### Google Perspective API
- Used for toxicity scoring of post content
- Docs: https://developers.perspectiveapi.com/
- Free tier has rate limits — design the pipeline to respect them
- API key provided as env var

## Decision Graph Workflow

**THIS IS MANDATORY. Log decisions IN REAL-TIME, not retroactively.**

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
| Choosing between approaches | `decision` | "Choose state management" |
| About to write/edit code | `action` | "Implementing Redux store" |
| Something worked or failed | `outcome` | "Redux integration successful" |
| Notice something interesting | `observation` | "Existing code uses hooks" |

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
| `outcome` | The action/goal it resolves |
| `action` | The goal/decision that spawned it |
| `option` | Its parent decision |
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

### Git Staging Rules - CRITICAL

**NEVER use broad git add commands that stage everything:**
- ❌ `git add -A` - stages ALL changes including untracked files
- ❌ `git add .` - stages everything in current directory
- ❌ `git add -a` or `git commit -am` - auto-stages all tracked changes
- ❌ `git add *` - glob patterns can catch unintended files

**ALWAYS stage files explicitly by name:**
- ✅ `git add src/main.rs src/lib.rs`
- ✅ `git add Cargo.toml Cargo.lock`
- ✅ `git add .claude/commands/decision.md`

**Why this matters:**
- Prevents accidentally committing sensitive files (.env, credentials)
- Prevents committing large binaries or build artifacts
- Forces you to review exactly what you're committing
- Catches unintended changes before they enter git history

### Session Start Checklist

```bash
deciduous check-update    # Update needed? Run 'deciduous update' if yes
deciduous nodes           # What decisions exist?
deciduous edges           # How are they connected? Any gaps?
git status                # Current state
```

### Multi-User Sync

Share decisions across teammates:

```bash
# Export your branch's decisions
deciduous diff export --branch feature-x -o .deciduous/patches/my-feature.json

# Apply patches from teammates (idempotent)
deciduous diff apply .deciduous/patches/*.json

# Preview before applying
deciduous diff apply --dry-run .deciduous/patches/teammate.json
```

PR workflow: Export patch -> commit patch file -> PR -> teammates apply.
