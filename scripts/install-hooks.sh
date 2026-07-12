#!/usr/bin/env bash
# Install git hooks for this project.
#
# Run this once after `new-project` (or after cloning):
#   ./scripts/install-hooks.sh
#
# Hooks installed:
#   pre-commit  — blocks main commits; runs format check; auto-exports
#                 chainlink issues + deciduous graph; backs up DBs to S3
#   pre-push    — blocks main pushes; runs full lint + tests
#
# This file is a TEMPLATE. Customize the language-specific quality gates
# (search "CUSTOMIZE" in the heredocs below) for your project's stack.

set -e

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOOKS_DIR="$REPO_ROOT/.git/hooks"

if [ ! -d "$HOOKS_DIR" ]; then
    echo "❌ $HOOKS_DIR does not exist. Run 'git init' first."
    exit 1
fi

PROJECT_NAME="$(basename "$REPO_ROOT")"
echo "Installing $PROJECT_NAME git hooks..."

# ── pre-commit ───────────────────────────────────────────────────────

cat > "$HOOKS_DIR/pre-commit" << 'HOOK'
#!/usr/bin/env bash
# pre-commit hook
#
# Rules:
#   1. Block commits directly to main (use a feature branch + PR)
#   2. Run language-specific format check on staged files
#   3. Always: export chainlink issues + sync deciduous graph into the commit
#   4. Always: upload both .db files to S3-compatible storage (if configured)
#
# Bypass (emergency only): git commit --no-verify

set -e

REPO_ROOT="$(git rev-parse --show-toplevel)"

# Load .env safely — handles quoted values, export prefixes, special chars.
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

# ── 2. Language-specific format gate ─────────────────────────────────
# CUSTOMIZE: add/remove language gates as your project's stack requires.
# Each gate is fast (format-check only, no clippy/tsc/etc).

# Rust: cargo fmt --check
if [ -f "$REPO_ROOT/Cargo.toml" ]; then
    RUST_FILES=$(git diff --cached --name-only --diff-filter=ACMR | grep -E '\.(rs|toml)$' || true)
    if [ -n "$RUST_FILES" ]; then
        echo "🔍 Pre-commit: cargo fmt --check..."
        if ! (cd "$REPO_ROOT" && cargo fmt --check 2>/dev/null); then
            echo ""
            echo "❌ Rust code is not formatted. Run: cargo fmt"
            echo ""
            exit 1
        fi
        echo "✅ Rust formatting OK"
    fi
fi

# Node/TS: prettier --check (if .prettierrc* present)
if [ -f "$REPO_ROOT/package.json" ] && ls "$REPO_ROOT"/.prettierrc* >/dev/null 2>&1; then
    JS_FILES=$(git diff --cached --name-only --diff-filter=ACMR | grep -E '\.(js|ts|tsx|jsx|mjs|cjs|json|md|yml|yaml)$' || true)
    if [ -n "$JS_FILES" ]; then
        echo "🔍 Pre-commit: prettier --check..."
        if ! (cd "$REPO_ROOT" && npx prettier --check $JS_FILES 2>/dev/null); then
            echo ""
            echo "❌ Files not formatted. Run: npx prettier --write ."
            echo ""
            exit 1
        fi
        echo "✅ JS/TS formatting OK"
    fi
fi

# Python: ruff format --check (if pyproject.toml or ruff.toml present)
if [ -f "$REPO_ROOT/pyproject.toml" ] || [ -f "$REPO_ROOT/ruff.toml" ]; then
    PY_FILES=$(git diff --cached --name-only --diff-filter=ACMR | grep -E '\.py$' || true)
    if [ -n "$PY_FILES" ] && command -v ruff &>/dev/null; then
        echo "🔍 Pre-commit: ruff format --check..."
        if ! (cd "$REPO_ROOT" && ruff format --check $PY_FILES 2>/dev/null); then
            echo ""
            echo "❌ Python code is not formatted. Run: ruff format ."
            echo ""
            exit 1
        fi
        echo "✅ Python formatting OK"
    fi
fi

# ── 3. Export chainlink issues ───────────────────────────────────────
echo "📦 Pre-commit: exporting chainlink issues..."
if (cd "$REPO_ROOT" && chainlink export --format json -o .chainlink/issues-export.json 2>/dev/null); then
    git add -f .chainlink/issues-export.json
    echo "✅ Chainlink issues exported"
else
    echo "⚠️  Chainlink export failed (non-blocking)"
fi

# ── 4. Sync deciduous decision graph ────────────────────────────────
echo "📦 Pre-commit: syncing decision graph..."
if (cd "$REPO_ROOT" && deciduous sync 2>/dev/null); then
    [ -f "$REPO_ROOT/docs/graph-data.json" ] && git add docs/graph-data.json
    [ -f "$REPO_ROOT/docs/git-history.json" ] && git add docs/git-history.json
    echo "✅ Decision graph synced"
else
    echo "⚠️  Deciduous sync failed (non-blocking)"
fi

# ── 5. Upload DBs to S3-compatible blob storage ─────────────────────
if [ -n "$BACKUP_S3_BUCKET" ] && [ -n "$BACKUP_S3_ACCESS_KEY_ID" ] && [ -n "$BACKUP_S3_SECRET_ACCESS_KEY" ] && [ -n "$BACKUP_S3_ENDPOINT" ]; then
    if ! command -v aws &>/dev/null; then
        echo "⚠️  Backup configured but aws CLI not found — skipping (run: brew install awscli)"
    else
    echo "☁️  Pre-commit: backing up databases to $BACKUP_S3_BUCKET..."
    ENDPOINT="--endpoint-url=$BACKUP_S3_ENDPOINT --region=${BACKUP_S3_REGION:-auto}"

    export AWS_ACCESS_KEY_ID="$BACKUP_S3_ACCESS_KEY_ID"
    export AWS_SECRET_ACCESS_KEY="$BACKUP_S3_SECRET_ACCESS_KEY"
    S3="s3://$BACKUP_S3_BUCKET"

    BACKUP_OK=true

    if [ -f "$REPO_ROOT/.chainlink/issues.db" ]; then
        if aws s3 cp "$REPO_ROOT/.chainlink/issues.db" "$S3/issues.db" $ENDPOINT --quiet 2>&1; then
            echo "  ✅ issues.db → $BACKUP_S3_BUCKET"
        else
            echo "  ⚠️  issues.db upload failed (non-blocking)"
            BACKUP_OK=false
        fi
    fi

    if [ -f "$REPO_ROOT/.deciduous/deciduous.db" ]; then
        if aws s3 cp "$REPO_ROOT/.deciduous/deciduous.db" "$S3/deciduous.db" $ENDPOINT --quiet 2>&1; then
            echo "  ✅ deciduous.db → $BACKUP_S3_BUCKET"
        else
            echo "  ⚠️  deciduous.db upload failed (non-blocking)"
            BACKUP_OK=false
        fi
    fi

    if [ "$BACKUP_OK" = true ]; then
        echo "✅ Backup complete"
    fi
    fi  # end aws CLI check
else
    echo "⏭️  Backup not configured (set BACKUP_S3_* vars in .env)"
fi

echo "✅ Pre-commit: all checks passed."
HOOK

chmod +x "$HOOKS_DIR/pre-commit"
echo "  ✓ pre-commit"

# ── pre-push ─────────────────────────────────────────────────────────

cat > "$HOOKS_DIR/pre-push" << 'HOOK'
#!/usr/bin/env bash
# pre-push hook
#
# Rules:
#   1. Block pushes to main (PRs only — GitHub enforces this too, but belt+suspenders)
#   2. Run full lint + tests for changed files in commits being pushed
#
# Bypass (emergency only): git push --no-verify

set -e

REMOTE="$1"
REPO_ROOT="$(git rev-parse --show-toplevel)"

# ── 1. Block pushes to main ──────────────────────────────────────────
# Git passes push targets on stdin: <local_ref> <local_sha> <remote_ref> <remote_sha>
while read local_ref local_sha remote_ref remote_sha; do
    if [ "$remote_ref" = "refs/heads/main" ]; then
        echo ""
        echo "❌ Direct push to main is not allowed. Open a pull request instead."
        echo ""
        exit 1
    fi
done

# ── 2. Determine changed files vs the remote ─────────────────────────
CURRENT_BRANCH=$(git branch --show-current)
REMOTE_REF=$(git rev-parse "$REMOTE/$CURRENT_BRANCH" 2>/dev/null || echo "")

if [ -n "$REMOTE_REF" ]; then
    CHANGED=$(git diff --name-only "$REMOTE_REF"..HEAD || true)
else
    # New branch — check vs main
    CHANGED=$(git diff --name-only main..HEAD 2>/dev/null || true)
fi

# ── 3. Language-specific quality gates ───────────────────────────────
# CUSTOMIZE: add/remove gates as your project's stack requires.

# Rust: clippy + tests
if [ -f "$REPO_ROOT/Cargo.toml" ]; then
    if echo "$CHANGED" | grep -qE '\.(rs|toml)$'; then
        echo "🔍 Pre-push: cargo clippy..."
        if ! (cd "$REPO_ROOT" && cargo clippy --all-targets --quiet 2>&1); then
            echo ""
            echo "❌ Clippy warnings. Fix them before pushing."
            echo ""
            exit 1
        fi

        echo "🔍 Pre-push: cargo test..."
        if ! (cd "$REPO_ROOT" && cargo test --quiet 2>&1); then
            echo ""
            echo "❌ Tests failed. Fix them before pushing."
            echo ""
            exit 1
        fi
        echo "✅ Rust gates passed"
    fi
fi

# Node/TS: tsc + tests
if [ -f "$REPO_ROOT/package.json" ]; then
    if echo "$CHANGED" | grep -qE '\.(ts|tsx|js|jsx)$'; then
        if [ -f "$REPO_ROOT/tsconfig.json" ]; then
            echo "🔍 Pre-push: tsc --noEmit..."
            if ! (cd "$REPO_ROOT" && npx tsc --noEmit 2>&1); then
                echo ""
                echo "❌ TypeScript errors. Fix them before pushing."
                echo ""
                exit 1
            fi
        fi

        # Run npm test if a "test" script exists
        if grep -q '"test"' "$REPO_ROOT/package.json"; then
            echo "🔍 Pre-push: npm test..."
            TEST_CMD="npm test"
            command -v pnpm &>/dev/null && [ -f "$REPO_ROOT/pnpm-lock.yaml" ] && TEST_CMD="pnpm test"
            if ! (cd "$REPO_ROOT" && $TEST_CMD 2>&1); then
                echo ""
                echo "❌ Tests failed. Fix them before pushing."
                echo ""
                exit 1
            fi
        fi
        echo "✅ JS/TS gates passed"
    fi
fi

# Python: ruff check + pytest
if [ -f "$REPO_ROOT/pyproject.toml" ] || [ -f "$REPO_ROOT/ruff.toml" ]; then
    if echo "$CHANGED" | grep -qE '\.py$'; then
        if command -v ruff &>/dev/null; then
            echo "🔍 Pre-push: ruff check..."
            if ! (cd "$REPO_ROOT" && ruff check . 2>&1); then
                echo ""
                echo "❌ Ruff issues. Fix them before pushing."
                echo ""
                exit 1
            fi
        fi

        if command -v pytest &>/dev/null && [ -d "$REPO_ROOT/tests" ]; then
            echo "🔍 Pre-push: pytest..."
            if ! (cd "$REPO_ROOT" && pytest -q 2>&1); then
                echo ""
                echo "❌ Tests failed. Fix them before pushing."
                echo ""
                exit 1
            fi
        fi
        echo "✅ Python gates passed"
    fi
fi

echo "✅ Pre-push: all checks passed."
HOOK

chmod +x "$HOOKS_DIR/pre-push"
echo "  ✓ pre-push"

echo ""
echo "Done. Both hooks are installed."
echo "Bypass any hook with --no-verify (emergency only)."
