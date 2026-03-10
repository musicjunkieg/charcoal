# Charcoal Development Environment Setup

Guide for setting up the charcoal development environment on a new machine
(tested for Docker sandbox on M4 Mac mini with 64GB RAM).

## Prerequisites

- Docker sandbox with Ubuntu/Debian base
- Internet access (for git clone, cargo, npm)
- Tigris credentials (for restoring project databases)

## Step 1: Install System Dependencies

```bash
# Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
rustc --version  # should be 1.90+ (stable)

# Node.js 22.x (for SvelteKit frontend)
curl -fsSL https://deb.nodesource.com/setup_22.x | sudo bash -
sudo apt-get install -y nodejs
node --version  # should be 22.x
npm --version

# Python 3 (for Claude Code hooks and MCP server)
sudo apt-get install -y python3 python3-pip

# AWS CLI (for Tigris database restore)
sudo apt-get install -y awscli

# Build essentials (needed for native deps like ort/ONNX)
sudo apt-get install -y build-essential pkg-config libssl-dev
```

## Step 2: Install Development Tools

```bash
# Deciduous — decision graph tracker
cargo install deciduous
# Should install v0.13.8+

# Chainlink — issue tracker CLI
# Install from source (https://github.com/dollspace-gay/chainlink)
cargo install --git https://github.com/dollspace-gay/chainlink
```

## Step 3: Clone the Repository

```bash
cd ~
git clone https://github.com/musicjunkieg/charcoal.git
cd charcoal
```

## Step 4: Set Up Global Gitignore

Download from Tigris or create manually:

```bash
mkdir -p ~/.config/git
cat > ~/.config/git/ignore << 'GITIGNORE'
*.log
*.tmp
*.temp
node_modules/
.env
.env.*
dist/
build/
target/
*.o
*.so
.vscode/
.idea/
*.swp
*.swo
.DS_Store
Thumbs.db
/tmp/
/var/tmp/

**/.claude/settings.local.json
GITIGNORE
```

## Step 5: Configure Environment Variables

```bash
cp .env.example .env
```

Edit `.env` and fill in your values. Required for basic CLI usage:
- `BLUESKY_HANDLE` — your Bluesky handle

Required for web dashboard (`charcoal serve`):
- `CHARCOAL_ALLOWED_DID` — your Bluesky DID (did:plc:...)
- `CHARCOAL_OAUTH_CLIENT_ID` — your OAuth client metadata URL
- `CHARCOAL_SESSION_SECRET` — generate with `openssl rand -hex 32`

Required for database backup/restore:
- `TIGRIS_BUCKET`, `TIGRIS_ACCESS_KEY_ID`, `TIGRIS_SECRET_ACCESS_KEY`, `TIGRIS_ENDPOINT`

See `.env.example` for all options with documentation.

## Step 6: Install Git Hooks

```bash
./scripts/install-hooks.sh
```

This creates `pre-commit` and `pre-push` hooks that enforce:
- `cargo fmt` check
- `cargo clippy` (on Rust changes)
- `cargo test` (on push)
- Chainlink issue export
- Deciduous graph sync
- Tigris database backup

## Step 7: Restore Project Databases from Tigris

```bash
./scripts/restore-dbs.sh
```

This restores:
- `.chainlink/issues.db` — all project issues, history, session context
- `.deciduous/deciduous.db` — decision graph with 141 nodes, 133 edges

Verify:
```bash
chainlink list -s open
deciduous nodes | head -20
```

## Step 8: Restore Claude Code Memory

If using Claude Code, restore the project memory files:

```bash
# Create the memory directory (path depends on your project location)
MEMORY_DIR="$HOME/.claude/projects/$(pwd | tr '/' '-' | sed 's/^-//')/memory"
mkdir -p "$MEMORY_DIR"

# Download from Tigris
source .env
export AWS_ACCESS_KEY_ID="$TIGRIS_ACCESS_KEY_ID"
export AWS_SECRET_ACCESS_KEY="$TIGRIS_SECRET_ACCESS_KEY"

aws s3 cp "s3://$TIGRIS_BUCKET/claude-memory/MEMORY.md" "$MEMORY_DIR/MEMORY.md" \
    --endpoint-url="$TIGRIS_ENDPOINT" --region=auto
aws s3 cp "s3://$TIGRIS_BUCKET/claude-memory/patterns.md" "$MEMORY_DIR/patterns.md" \
    --endpoint-url="$TIGRIS_ENDPOINT" --region=auto
```

Note: The exact memory directory path is based on the absolute path to the
project. Claude Code will show you the path on first run — check
`~/.claude/projects/` for the auto-created directory and move the files there.

## Step 9: Build the Frontend

```bash
cd web
npm ci
npm run build
cd ..
```

## Step 10: Build and Test

```bash
# Basic build (CLI only, SQLite)
cargo build

# Full build with web dashboard
cargo build --features web

# Run all tests
cargo test --all-targets

# Run with web feature (includes OAuth tests)
cargo test --all-targets --features web

# Run clippy
cargo clippy --all-targets --features web
```

With 64GB RAM you can use the default parallelism — no need for
`CARGO_BUILD_JOBS=2` like the Sprite VM required.

## Step 11: Download ONNX Models

```bash
cargo run -- download-model
```

Downloads ~216MB total:
- Toxicity model (unbiased-toxic-roberta, ~126MB)
- Embedding model (all-MiniLM-L6-v2, ~90MB)

Models are stored in `~/.local/share/charcoal/models/` by default.

## Step 12: Initialize and Run

```bash
# Initialize the database
cargo run -- init

# Run a scan (requires BLUESKY_HANDLE in .env)
cargo run -- scan

# Start the web dashboard (requires web feature + OAuth env vars)
cargo run --features web -- serve
```

## What's Checked Into Git (already present after clone)

Everything in `.claude/` (settings, hooks, commands, skills, MCP server),
`.chainlink/` config (hook-config.json, rules/), `.deciduous/config.toml`,
`.mcp.json`, all scripts, `.env.example`, `railway.toml`.

## What's NOT in Git (must be restored/created)

| File | Source |
|------|--------|
| `.env` | Copy from `.env.example`, fill in secrets |
| `.chainlink/issues.db` | `./scripts/restore-dbs.sh` (from Tigris) |
| `.deciduous/deciduous.db` | `./scripts/restore-dbs.sh` (from Tigris) |
| `.git/hooks/*` | `./scripts/install-hooks.sh` |
| Claude memory files | Tigris `claude-memory/` prefix (see Step 8) |
| ONNX models | `cargo run -- download-model` |
| `~/.config/git/ignore` | See Step 4 |

## Tigris Bucket Contents

```
s3://your-bucket/
├── issues.db              # Chainlink issue database
├── deciduous.db           # Decision graph database
├── claude-memory/
│   ├── MEMORY.md          # Claude Code project memory
│   └── patterns.md        # Architecture patterns notes
└── config/
    └── global-gitignore   # Global gitignore template
```

## Differences from Sprite VM

| Sprite VM | New Machine |
|-----------|-------------|
| 1GB RAM — `CARGO_BUILD_JOBS=2` required | 64GB RAM — full parallelism |
| Kill server before building | Can build and run simultaneously |
| `EnterWorktree` crashes | Git worktrees should work fine |
| Heredoc `<<EOF` breaks in zsh | Test your shell; may work |
| Tigris backup via pre-commit hook | Same — hooks auto-backup |

## Session Workflow

```bash
# Start of session
chainlink session start
chainlink list -s open          # See what needs doing
deciduous nodes                 # Review decision graph

# During work
chainlink quick "Fix X" -p medium -l bug    # Create + start issue
# ... do the work ...
chainlink close <id>                         # Close when done

# End of session
chainlink session end --notes "Summary of what was done"
```
