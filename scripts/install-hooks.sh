#!/usr/bin/env bash
# Install Charcoal git hooks.
#
# Run this once after cloning:
#   ./scripts/install-hooks.sh
#
# Hooks installed:
#   pre-commit  — blocks main commits; enforces fmt; auto-exports issues + graph
#   pre-push    — blocks main pushes; runs full clippy + tests before GitHub

set -e

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOOKS_DIR="$REPO_ROOT/.git/hooks"

echo "Installing Charcoal git hooks..."

# ── pre-commit ───────────────────────────────────────────────────────

cat > "$HOOKS_DIR/pre-commit" << 'HOOK'
#!/usr/bin/env bash
# Charcoal pre-commit hook
#
# Rules:
#   1. Block commits directly to main (use a feature branch + PR)
#   2. If Rust/TOML files staged: enforce cargo fmt (fast gate only)
#   3. Always: export chainlink issues + sync deciduous graph into the commit
#   4. Always: upload both .db files to Tigris for crash recovery (if configured)
#
# Bypass (emergency only): git commit --no-verify

set -e

REPO_ROOT="$(git rev-parse --show-toplevel)"

# Load .env safely — handles quoted values, export prefixes, and special chars
# Does NOT execute the file as bash; parses key=value line by line.
_load_env() {
    local line key value
    while IFS= read -r line; do
        [[ "$line" =~ ^[[:space:]]*# ]] && continue        # skip comments
        [[ -z "${line//[[:space:]]/}" ]] && continue        # skip blank lines
        line="${line#export }"                               # strip leading 'export '
        [[ "$line" =~ ^([A-Za-z_][A-Za-z0-9_]*)=(.*)$ ]] || continue
        key="${BASH_REMATCH[1]}"
        value="${BASH_REMATCH[2]}"
        value="${value%\"}" ; value="${value#\"}"            # strip surrounding double quotes
        value="${value%\'}" ; value="${value#\'}"            # strip surrounding single quotes
        export "$key=$value"
    done < "$1"
}
if [ -f "$REPO_ROOT/.env" ]; then
    _load_env "$REPO_ROOT/.env"
fi

# ── 1. Block commits to main ─────────────────────────────────────────
CURRENT_BRANCH=$(git branch --show-current)
if [ "$CURRENT_BRANCH" = "main" ]; then
    echo ""
    echo "❌ Direct commits to main are not allowed."
    echo "   Create a feature branch: git checkout -b feat/your-feature"
    echo ""
    exit 1
fi

# ── 2. Rust quality gate (fmt only — clippy/tests run at push) ───────
RUST_FILES=$(git diff --cached --name-only --diff-filter=ACMR | grep -E '\.(rs|toml)$' || true)

if [ -n "$RUST_FILES" ]; then
    echo "🔍 Pre-commit: checking formatting..."
    if ! cargo fmt --check 2>/dev/null; then
        echo ""
        echo "❌ Code is not formatted. Run: cargo fmt"
        echo ""
        exit 1
    fi
    echo "✅ Formatting OK"
fi

# ── 3. Export chainlink issues ───────────────────────────────────────
echo "📦 Pre-commit: exporting chainlink issues..."
if chainlink export --format json -o .chainlink/issues-export.json 2>/dev/null; then
    git add .chainlink/issues-export.json
    echo "✅ Chainlink issues exported"
else
    echo "⚠️  Chainlink export failed (non-blocking)"
fi

# ── 4. Sync deciduous decision graph ────────────────────────────────
echo "📦 Pre-commit: syncing decision graph..."
if deciduous sync 2>/dev/null; then
    [ -f docs/graph-data.json ] && git add docs/graph-data.json
    [ -f docs/git-history.json ] && git add docs/git-history.json
    echo "✅ Decision graph synced"
else
    echo "⚠️  Deciduous sync failed (non-blocking)"
fi

# ── 5. Upload DBs to Tigris blob storage ────────────────────────────
if [ -n "$TIGRIS_BUCKET" ] && [ -n "$TIGRIS_ACCESS_KEY_ID" ] && [ -n "$TIGRIS_SECRET_ACCESS_KEY" ]; then
    if ! command -v aws &>/dev/null; then
        echo "⚠️  Tigris configured but aws CLI not found — skipping backup (run: sudo apt install awscli)"
    else
    echo "☁️  Pre-commit: backing up databases to Tigris..."
    ENDPOINT="--endpoint-url=${TIGRIS_ENDPOINT:-https://fly.storage.tigris.dev} --region=auto"

    export AWS_ACCESS_KEY_ID="$TIGRIS_ACCESS_KEY_ID"
    export AWS_SECRET_ACCESS_KEY="$TIGRIS_SECRET_ACCESS_KEY"
    S3="s3://$TIGRIS_BUCKET"

    BACKUP_OK=true

    if [ -f "$REPO_ROOT/.chainlink/issues.db" ]; then
        if aws s3 cp "$REPO_ROOT/.chainlink/issues.db" "$S3/issues.db" $ENDPOINT --quiet 2>&1; then
            echo "  ✅ issues.db → Tigris"
        else
            echo "  ⚠️  issues.db upload failed (non-blocking)"
            BACKUP_OK=false
        fi
    fi

    if [ -f "$REPO_ROOT/.deciduous/deciduous.db" ]; then
        if aws s3 cp "$REPO_ROOT/.deciduous/deciduous.db" "$S3/deciduous.db" $ENDPOINT --quiet 2>&1; then
            echo "  ✅ deciduous.db → Tigris"
        else
            echo "  ⚠️  deciduous.db upload failed (non-blocking)"
            BACKUP_OK=false
        fi
    fi

    if [ "$BACKUP_OK" = true ]; then
        echo "✅ Tigris backup complete"
    fi
    fi  # end aws CLI check
else
    echo "⏭️  Tigris not configured (set TIGRIS_BUCKET/TIGRIS_ACCESS_KEY_ID/TIGRIS_SECRET_ACCESS_KEY in .env)"
fi

echo "✅ Pre-commit: all checks passed."
HOOK

chmod +x "$HOOKS_DIR/pre-commit"
echo "  ✓ pre-commit"

# ── pre-push ─────────────────────────────────────────────────────────

cat > "$HOOKS_DIR/pre-push" << 'HOOK'
#!/usr/bin/env bash
# Charcoal pre-push hook
#
# Rules:
#   1. Block pushes to main (PRs only — GitHub enforces this too, but belt+suspenders)
#   2. If Rust/TOML changes in commits being pushed: run full clippy + tests
#
# Bypass (emergency only): git push --no-verify

set -e

REMOTE="$1"

# ── 1. Block pushes to main ──────────────────────────────────────────
# Git passes push targets on stdin: <local_ref> <local_sha> <remote_ref> <remote_sha>
while read local_ref local_sha remote_ref remote_sha; do
    if [ "$remote_ref" = "refs/heads/main" ]; then
        echo ""
        echo "❌ Direct push to main is not allowed."
        echo "   Open a pull request instead."
        echo ""
        exit 1
    fi
done

# ── 2. Rust quality gate (full clippy + tests before code hits GitHub) ──
REMOTE_REF=$(git rev-parse "$REMOTE/$(git branch --show-current)" 2>/dev/null || echo "")

if [ -n "$REMOTE_REF" ]; then
    RUST_FILES=$(git diff --name-only "$REMOTE_REF"..HEAD | grep -E '\.(rs|toml)$' || true)
else
    # New branch — check all files vs main
    RUST_FILES=$(git diff --name-only main..HEAD 2>/dev/null | grep -E '\.(rs|toml)$' || true)
fi

if [ -z "$RUST_FILES" ]; then
    echo "📝 Pre-push: no Rust changes, skipping quality gate."
    exit 0
fi

echo "🔍 Pre-push: running clippy..."
if ! cargo clippy --all-targets --features web --quiet 2>&1; then
    echo ""
    echo "❌ Clippy warnings found. Fix them before pushing."
    echo ""
    exit 1
fi

echo "🔍 Pre-push: running tests..."
if ! cargo test --features web --quiet 2>&1; then
    echo ""
    echo "❌ Tests failed. Fix them before pushing."
    echo ""
    exit 1
fi

echo "✅ Pre-push: all checks passed."
HOOK

chmod +x "$HOOKS_DIR/pre-push"
echo "  ✓ pre-push"

echo ""
echo "Done. Both hooks are installed."
echo "Bypass any hook with --no-verify (emergency only)."
