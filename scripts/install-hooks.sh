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
    # Read staged files into an array (via while+append, portable to bash
    # 3.2 on macOS — no mapfile) then splat with "${JS_FILES[@]}". Using
    # $JS_FILES unquoted would word-split filenames with spaces or shell
    # metachars into bogus prettier args.
    JS_FILES=()
    while IFS= read -r _f; do
        [ -n "$_f" ] && JS_FILES+=("$_f")
    done < <(git diff --cached --name-only --diff-filter=ACMR | grep -E '\.(js|ts|tsx|jsx|mjs|cjs|json|md|yml|yaml)$' || true)
    if [ ${#JS_FILES[@]} -gt 0 ]; then
        echo "🔍 Pre-commit: prettier --check..."
        if ! (cd "$REPO_ROOT" && npx prettier --check "${JS_FILES[@]}" 2>/dev/null); then
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
    # See JS_FILES note above — same word-splitting hazard, same fix.
    PY_FILES=()
    while IFS= read -r _f; do
        [ -n "$_f" ] && PY_FILES+=("$_f")
    done < <(git diff --cached --name-only --diff-filter=ACMR | grep -E '\.py$' || true)
    if [ ${#PY_FILES[@]} -gt 0 ] && command -v ruff &>/dev/null; then
        echo "🔍 Pre-commit: ruff format --check..."
        if ! (cd "$REPO_ROOT" && ruff format --check "${PY_FILES[@]}" 2>/dev/null); then
            echo ""
            echo "❌ Python code is not formatted. Run: ruff format ."
            echo ""
            exit 1
        fi
        echo "✅ Python formatting OK"
    fi
fi

# ── Helper: skip an export add if it would clobber committed content ─
# When a worktree lacks the underlying .db source (e.g. a fresh clone,
# cloud sandbox, or ephemeral worktree), running `chainlink export` /
# `deciduous sync` produces a valid-shape but *empty* JSON. Adding that
# to the commit silently overwrites HEAD's rich version. This helper
# compares the item count at $2 (e.g. "issues", "nodes") between the
# fresh file and HEAD — if fresh has 0 but HEAD has some, we treat it
# as a worktree without state and preserve HEAD's version.
#
# Args: $1 = repo-relative path, $2 = top-level JSON key (list-typed)
# Returns 0 (true) if adding would clobber; 1 (false) if safe to add.
# Requires python3; degrades to always-safe-to-add if python3 missing.
_would_clobber() {
    local path="$1" key="$2"
    [ -f "$REPO_ROOT/$path" ] || return 1  # nothing to add anyway
    command -v python3 >/dev/null 2>&1 || return 1

    # NB: using `python3 -c '...'` (not `python3 - <<HEREDOC`) so that
    # stdin stays available for the piped `git show` in the second call.
    # A heredoc on `python3 -` would clobber the pipe.
    local fresh_count committed_count
    fresh_count=$(python3 -c '
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    v = d.get(sys.argv[2], [])
    print(len(v) if isinstance(v, list) else 0)
except Exception:
    print(-1)
' "$REPO_ROOT/$path" "$key" 2>/dev/null || echo -1)

    # Fresh has content, or parse failed → don't second-guess, safe to add.
    [ "$fresh_count" != "0" ] && return 1

    committed_count=$(git show "HEAD:$path" 2>/dev/null | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    v = d.get(sys.argv[1], [])
    print(len(v) if isinstance(v, list) else 0)
except Exception:
    print(-1)
' "$key" 2>/dev/null || echo -1)

    # HEAD empty or file not committed → nothing to preserve.
    { [ "${committed_count:-0}" -gt 0 ] 2>/dev/null; } || return 1

    # Would clobber. Restore working tree so the file that lands (if
    # anything else stages it) matches HEAD.
    git checkout HEAD -- "$path" 2>/dev/null || true
    return 0
}

# ── 3. Export chainlink issues ───────────────────────────────────────
echo "📦 Pre-commit: exporting chainlink issues..."
if (cd "$REPO_ROOT" && chainlink export --format json -o .chainlink/issues-export.json 2>/dev/null); then
    if _would_clobber ".chainlink/issues-export.json" "issues"; then
        echo "⚠️  Skipping issues-export.json: fresh export empty but HEAD has content (worktree may lack .chainlink/issues.db). Committed version preserved."
    else
        git add -f .chainlink/issues-export.json
        echo "✅ Chainlink issues exported"
    fi
else
    echo "⚠️  Chainlink export failed (non-blocking)"
fi

# ── 4. Sync deciduous decision graph ────────────────────────────────
echo "📦 Pre-commit: syncing decision graph..."
if (cd "$REPO_ROOT" && deciduous sync 2>/dev/null); then
    if [ -f "$REPO_ROOT/docs/graph-data.json" ]; then
        if _would_clobber "docs/graph-data.json" "nodes"; then
            echo "⚠️  Skipping graph-data.json: fresh export empty but HEAD has content (worktree may lack .deciduous/deciduous.db). Committed version preserved."
        else
            git add docs/graph-data.json
        fi
    fi
    # git-history.json is derived from git log, not from .deciduous state,
    # so it's not at clobber risk in an empty-state worktree.
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
