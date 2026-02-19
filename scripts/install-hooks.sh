#!/usr/bin/env bash
# Install Charcoal git hooks.
#
# Run this once after cloning:
#   ./scripts/install-hooks.sh
#
# Hooks installed:
#   pre-commit  — blocks commits with formatting errors, clippy warnings, or failing tests
#   pre-push    — blocks pushes with failing tests or clippy warnings

set -e

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOOKS_DIR="$REPO_ROOT/.git/hooks"

echo "Installing Charcoal git hooks..."

# ── pre-commit ───────────────────────────────────────────────────────

cat > "$HOOKS_DIR/pre-commit" << 'HOOK'
#!/usr/bin/env bash
# Charcoal pre-commit hook
# Enforces: formatting, linting, and tests before every commit.
# Skips Rust checks when only docs/markdown files are staged.
#
# Bypass (emergency only): git commit --no-verify

set -e

# Check if any staged files are Rust/TOML source files
RUST_FILES=$(git diff --cached --name-only --diff-filter=ACMR | grep -E '\.(rs|toml)$' || true)

if [ -z "$RUST_FILES" ]; then
    echo "📝 Pre-commit: docs-only commit, skipping Rust checks."
    exit 0
fi

echo "🔍 Pre-commit: checking formatting..."
if ! cargo fmt --check 2>/dev/null; then
    echo ""
    echo "❌ Code is not formatted. Run: cargo fmt"
    echo ""
    exit 1
fi

echo "🔍 Pre-commit: running clippy..."
if ! cargo clippy --all-targets 2>&1 | grep -q "warning: " && cargo clippy --all-targets 2>/dev/null; then
    : # clippy passed
else
    # Run again to show the actual warnings
    echo ""
    cargo clippy --all-targets 2>&1
    echo ""
    echo "❌ Clippy warnings found. Fix them before committing."
    echo ""
    exit 1
fi

echo "🔍 Pre-commit: running tests..."
if ! cargo test --quiet 2>/dev/null; then
    echo ""
    echo "❌ Tests failed. Fix them before committing."
    echo ""
    exit 1
fi

echo "✅ Pre-commit: all checks passed."
HOOK

chmod +x "$HOOKS_DIR/pre-commit"
echo "  ✓ pre-commit"

# ── pre-push ─────────────────────────────────────────────────────────

cat > "$HOOKS_DIR/pre-push" << 'HOOK'
#!/usr/bin/env bash
# Charcoal pre-push hook
# Blocks pushes if tests fail or clippy has warnings.
# Skips Rust checks when only docs/markdown files changed since remote.
#
# Bypass (emergency only): git push --no-verify

set -e

# Check if any commits being pushed contain Rust/TOML changes
REMOTE="$1"
REMOTE_REF=$(git rev-parse "$REMOTE/$(git branch --show-current)" 2>/dev/null || echo "")

if [ -n "$REMOTE_REF" ]; then
    RUST_FILES=$(git diff --name-only "$REMOTE_REF"..HEAD | grep -E '\.(rs|toml)$' || true)
else
    # New branch — check all files vs main
    RUST_FILES=$(git diff --name-only main..HEAD 2>/dev/null | grep -E '\.(rs|toml)$' || true)
fi

if [ -z "$RUST_FILES" ]; then
    echo "📝 Pre-push: docs-only push, skipping Rust checks."
    exit 0
fi

echo "🔍 Pre-push: running tests..."
if ! cargo test --quiet 2>&1; then
    echo ""
    echo "❌ Tests failed. Fix them before pushing."
    echo ""
    exit 1
fi

echo "🔍 Pre-push: running clippy..."
if ! cargo clippy --all-targets --quiet 2>&1; then
    echo ""
    echo "❌ Clippy warnings found. Fix them before pushing."
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
