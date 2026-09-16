#!/usr/bin/env bash
# Install git hooks for this project.
#
# Run this once after `new-project` (or after cloning):
#   ./scripts/install-hooks.sh
#
# Hooks installed:
#   pre-commit  — blocks main commits; runs format check; auto-exports
#                 chainlink issues + deciduous graph; backs up DBs to S3
#   pre-push    — blocks main pushes; runs lint + only the tests the push
#                 affects (the full suite runs in CI — #367)
#
# This file is a TEMPLATE. Customize the language-specific quality gates
# (search "CUSTOMIZE" in the heredocs below) for your project's stack.

set -e

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Resolve the shared hooks directory. In a regular repo `--git-common-dir`
# returns `.git` (relative to REPO_ROOT); in a git worktree it returns the
# absolute path to the primary checkout's `.git` (worktrees share hooks
# with the primary). Either way, `cd + pwd -P` normalizes to an absolute
# path so the rest of the script works regardless of cwd.
#
# The assignment lives inside an `if` conditional so `set -e` doesn't
# abort us on a failed `git rev-parse` before we get a chance to print
# the friendly "not a git repo" error. Both the exit-nonzero case and
# the succeeded-but-empty case land in the same branch.
if ! _common_gitdir="$(cd "$REPO_ROOT" && git rev-parse --git-common-dir 2>/dev/null)" || [ -z "$_common_gitdir" ]; then
    echo "❌ Not inside a git repository ($REPO_ROOT). Run 'git init' first."
    exit 1
fi
HOOKS_DIR="$(cd "$REPO_ROOT" && cd "$_common_gitdir" && pwd -P)/hooks"

if [ ! -d "$HOOKS_DIR" ]; then
    echo "❌ $HOOKS_DIR does not exist. Corrupted git repo?"
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
    #
    # fresh_count semantics:
    #   >0  → fresh export has content, safe to add
    #    0  → fresh export is a valid-shape but empty JSON (worktree may
    #         lack the .db source) → check HEAD before deciding
    #   -1  → fresh file is unreadable, malformed, truncated, or otherwise
    #         invalid — the `except Exception` catches any load / decode /
    #         type failure. Treat the same as 0: check HEAD; if HEAD has
    #         real content, preserve it rather than committing garbage on
    #         top of it.
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

    # Fresh has verifiable content → safe to add.
    if [ "$fresh_count" != "0" ] && [ "$fresh_count" != "-1" ]; then
        return 1
    fi

    committed_count=$(git show "HEAD:$path" 2>/dev/null | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    v = d.get(sys.argv[1], [])
    print(len(v) if isinstance(v, list) else 0)
except Exception:
    print(-1)
' "$key" 2>/dev/null || echo -1)

    # HEAD empty, missing, or itself unparseable → nothing to preserve.
    # (`committed_count -le 0` matches both 0 and -1, and -eq'ing against a
    #  potentially-empty string is why the `${committed_count:-0}` default.)
    if ! [ "${committed_count:-0}" -gt 0 ] 2>/dev/null; then
        return 1
    fi

    # Would clobber (either fresh is empty-but-valid, or fresh is
    # invalid/unreadable and we refuse to overwrite HEAD with
    # unverifiable data). Restore working tree so the file that lands
    # matches HEAD.
    git checkout HEAD -- "$path" 2>/dev/null || true
    return 0
}

# ── 3. Export chainlink issues ───────────────────────────────────────
echo "📦 Pre-commit: exporting chainlink issues..."
if (cd "$REPO_ROOT" && chainlink export --format json -o .chainlink/issues-export.json 2>/dev/null); then
    # The export is a regenerated artifact and .gitignore lists it. It is
    # deliberately NOT committed (ADR 0005): the authoritative issue records
    # are .chainlink/issues.db, which step 5 below backs up to R2 on every
    # commit. Committing the export made every branch integration conflict
    # on churned JSON, and the record-level merge driver that used to paper
    # over that was one more moving part downstream projects had to carry.
    #
    # `git check-ignore --no-index` asks "do the ignore rules cover this?"
    # rather than "is it tracked?", so the answer stays correct even if the
    # file is re-added to the index by accident. No _would_clobber guard
    # here, unlike the graph-data.json branch below — that guard protects a
    # COMMITTED file from being overwritten by an empty export, and this
    # file is not committed.
    if (cd "$REPO_ROOT" && git check-ignore -q --no-index .chainlink/issues-export.json); then
        # A project generated before ADR 0005 still has the export in its
        # index; the ignore rule alone does not untrack it, so it would sit
        # there with stale content forever. Say so once per commit rather
        # than silently editing the index — the fix is a one-line command.
        if (cd "$REPO_ROOT" && git ls-files --error-unmatch .chainlink/issues-export.json >/dev/null 2>&1); then
            echo "⚠️  .chainlink/issues-export.json is gitignored but still tracked — untrack it once with:"
            echo "      git rm --cached .chainlink/issues-export.json"
        fi
        echo "✅ Chainlink issues exported (gitignored — not staged)"
    else
        # The template-managed .gitignore always carries this rule, so
        # reaching here means the project's ignore rules were edited or
        # the file was renamed. Never stage regardless — staging is the
        # behaviour ADR 0005 removed — and say why. Non-blocking, like
        # every other step in this hook.
        echo "⚠️  .chainlink/issues-export.json is NOT gitignored — not staging it (ADR 0005)."
        echo "      Restore the '.chainlink/issues-export.json' line in .gitignore."
    fi
else
    echo "⚠️  Chainlink export failed (non-blocking)"
fi

# ── 4. Sync deciduous decision graph ────────────────────────────────
echo "📦 Pre-commit: syncing decision graph..."
if (cd "$REPO_ROOT" && deciduous sync 2>/dev/null); then
    if [ -f "$REPO_ROOT/docs/graph-data.json" ]; then
        if _would_clobber "docs/graph-data.json" "nodes"; then
            echo "⚠️  Skipping graph-data.json: fresh export empty or invalid but HEAD has content (worktree may lack .deciduous/deciduous.db, or the export was truncated/corrupt). Committed version preserved."
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

    # ┌─ LOCAL EXCEPTION TO THE TEMPLATE ────────────────────────────────┐
    # │ Dated history + a sanity gate. NOT in project-template; a sync   │
    # │ will overwrite this. See #286.                                   │
    # └──────────────────────────────────────────────────────────────────┘
    # Cloudflare R2 does NOT implement bucket versioning — PutBucketVersioning,
    # GetBucketVersioning and ListObjectVersions are all unimplemented in its
    # S3 API. So `$S3/issues.db` is a single object with no history, and every
    # commit overwrites it.
    #
    # On 2026-08-07 that came within one commit of being unrecoverable: checking
    # out an old branch that TRACKS .chainlink/issues.db destroyed the live
    # database, chainlink silently created an empty one, and any commit made in
    # that state would have uploaded the empty DB over the only good copy.
    #
    # Two guards, since R2 gives us neither:
    #   1. Every upload also writes a dated key, so history exists.
    #   2. A degenerate DB is not uploaded at all. `sqlite3` is already a
    #      project dependency; if it is missing we upload anyway rather than
    #      silently stop backing up.
    BACKUP_STAMP=$(date -u +%Y%m%dT%H%M%SZ)

    # Refuse to back up a database with implausibly few rows. Returns 0 (sane)
    # when sqlite3 is unavailable or the table is unknown — fail open, because
    # a missed guard is better than a missed backup.
    backup_row_sanity() {  # $1 = db path, $2 = table, $3 = floor
        command -v sqlite3 &>/dev/null || return 0
        local n
        n=$(sqlite3 "$1" "SELECT COUNT(*) FROM $2;" 2>/dev/null) || return 0
        [ -z "$n" ] && return 0
        [ "$n" -ge "$3" ]
    }

    # History object uploads BEFORE the mutable backup object in both blocks
    # below: R2 gives `$S3/issues.db` / `$S3/deciduous.db` no version history
    # (see the R2-versioning note above), so it is the ONLY recovery point.
    # Writing history first means a failed history upload aborts before the
    # mutable object is overwritten, instead of after — the mutable copy is
    # the non-critical step now, since a history snapshot already exists to
    # fall back to if it fails. (#307, CodeRabbit PR #103)
    if [ -f "$REPO_ROOT/.chainlink/issues.db" ]; then
        if ! backup_row_sanity "$REPO_ROOT/.chainlink/issues.db" issues 25; then
            echo "  🛑 issues.db looks EMPTY or reset — refusing to overwrite the backup."
            echo "     A fresh chainlink DB (numbering restarts at #1) means the live one"
            echo "     was destroyed. Restore first; uploading now would clobber the copy"
            echo "     you would restore FROM. See the recovery notes on #286."
            BACKUP_OK=false
        elif aws s3 cp "$REPO_ROOT/.chainlink/issues.db" \
                "$S3/history/issues-$BACKUP_STAMP.db" $ENDPOINT --quiet 2>&1; then
            aws s3 cp "$REPO_ROOT/.chainlink/issues.db" "$S3/issues.db" $ENDPOINT --quiet 2>&1 || true
            echo "  ✅ issues.db → $BACKUP_S3_BUCKET (+ history/issues-$BACKUP_STAMP.db)"
        else
            echo "  ⚠️  issues.db history upload failed — recovery point not written, skipping backup"
            BACKUP_OK=false
        fi
    fi

    if [ -f "$REPO_ROOT/.deciduous/deciduous.db" ]; then
        if ! backup_row_sanity "$REPO_ROOT/.deciduous/deciduous.db" decision_nodes 25; then
            echo "  🛑 deciduous.db looks EMPTY or reset — refusing to overwrite the backup."
            BACKUP_OK=false
        elif aws s3 cp "$REPO_ROOT/.deciduous/deciduous.db" \
                "$S3/history/deciduous-$BACKUP_STAMP.db" $ENDPOINT --quiet 2>&1; then
            aws s3 cp "$REPO_ROOT/.deciduous/deciduous.db" "$S3/deciduous.db" $ENDPOINT --quiet 2>&1 || true
            echo "  ✅ deciduous.db → $BACKUP_S3_BUCKET (+ history/deciduous-$BACKUP_STAMP.db)"
        else
            echo "  ⚠️  deciduous.db history upload failed — recovery point not written, skipping backup"
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
#   2. Lint, and run ONLY the tests affected by the commits being pushed.
#      The full suite runs in CI on every pull request (#367).
#
# Bypass (emergency only): git push --no-verify

set -e

REMOTE="$1"
REPO_ROOT="$(git rev-parse --show-toplevel)"
ZERO_SHA=0000000000000000000000000000000000000000

# Reduce a remote URL to host/owner/repo, so the SSH and HTTPS spellings of one
# repository compare equal: `git@github.com:o/r.git` and
# `https://github.com/o/r` both become `github.com/o/r`.
canonical_repo() {
    echo "$1" | sed -E 's#^[a-z+]+://##; s#^[^@/]+@##; s#^([^/:]+):([^0-9/])#\1/\2#; s#\.git/?$##; s#/+$##' |
        tr '[:upper:]' '[:lower:]'
}

# Git passes the remote as given on the command line. A push to a URL passes
# the URL, not a remote name, so `$REMOTE/<branch>` would never resolve. Map it
# back to a named remote only when that remote's fetch or push URL is the SAME
# repository. Guessing `origin` is not safe: a push to some other repository
# would then be measured against origin's branches, and commits the push really
# sends could drop out of the change set. Unresolved stays empty.
REMOTE_NAME=""
if git remote | grep -qxF -- "$REMOTE"; then
    REMOTE_NAME="$REMOTE"
else
    target=$(canonical_repo "$REMOTE")
    for r in $(git remote); do
        if [ "$(canonical_repo "$(git remote get-url "$r" 2>/dev/null)")" = "$target" ] ||
            [ "$(canonical_repo "$(git remote get-url --push "$r" 2>/dev/null)")" = "$target" ]; then
            REMOTE_NAME="$r"
            break
        fi
    done
fi

# ── 1. Block pushes to main ──────────────────────────────────────────
# Git passes push targets on stdin: <local_ref> <local_sha> <remote_ref> <remote_sha>
# The remote_sha of the line pushing HEAD is the remote's current tip, reported
# by git itself, so it is the most reliable base for the change set below.
HEAD_SHA=$(git rev-parse HEAD)
PUSH_REMOTE_SHA=""
while read -r local_ref local_sha remote_ref remote_sha; do
    if [ "$remote_ref" = "refs/heads/main" ]; then
        echo ""
        echo "❌ Direct push to main is not allowed. Open a pull request instead."
        echo ""
        exit 1
    fi
    if [ "$local_sha" = "$HEAD_SHA" ] && [ -z "$PUSH_REMOTE_SHA" ]; then
        PUSH_REMOTE_SHA="$remote_sha"
    fi
done

# ── 2. Determine changed files vs the remote ─────────────────────────
# Every path below either measures the change set or leaves CHANGE_SET_KNOWN
# empty. An unknown change set is never treated as an empty one: that would
# skip every gate. It falls back to all tracked files instead (see below).
CURRENT_BRANCH=$(git branch --show-current)
CHANGED=""
CHANGE_SET_KNOWN=""

if [ -n "$PUSH_REMOTE_SHA" ] && [ "$PUSH_REMOTE_SHA" != "$ZERO_SHA" ] &&
    git cat-file -e "$PUSH_REMOTE_SHA^{commit}" 2>/dev/null; then
    # The branch exists on the remote and we have its tip: only what this
    # push adds.
    if CHANGED=$(git diff --name-only "$PUSH_REMOTE_SHA" HEAD); then
        CHANGE_SET_KNOWN=1
    fi
elif [ "$PUSH_REMOTE_SHA" != "$ZERO_SHA" ] && [ -n "$REMOTE_NAME" ] &&
    REMOTE_REF=$(git rev-parse --verify --quiet "$REMOTE_NAME/$CURRENT_BRANCH"); then
    # No usable stdin (a dry run reads /dev/null): the tracking ref instead.
    if CHANGED=$(git diff --name-only "$REMOTE_REF" HEAD); then
        CHANGE_SET_KNOWN=1
    fi
elif [ -n "$REMOTE_NAME" ]; then
    # New branch: compare against the branch it was cut FROM. Branches are cut
    # from staging, so the old `main..HEAD` counted every staging commit not yet
    # promoted to main as "changed" (#178). The three-dot form measures from the
    # point this branch forked, so only this branch's own commits count. It
    # fails when there is no fork point (a shallow clone, unrelated history).
    for candidate in "$REMOTE_NAME/staging" "$REMOTE_NAME/main"; do
        git rev-parse --verify --quiet "$candidate" >/dev/null || continue
        if CHANGED=$(git diff --name-only "$candidate"...HEAD 2>/dev/null); then
            CHANGE_SET_KNOWN=1
            break
        fi
    done
fi

if [ -z "$CHANGE_SET_KNOWN" ]; then
    echo "⚠️  Pre-push: could not tell what this push changes (remote '$REMOTE' unresolved, or no common history) — checking every tracked file"
    CHANGED=$(git ls-files)
fi

# Test seam: preview the gate for an arbitrary change set without committing
# it. Combine with CHARCOAL_HOOK_DRY_RUN to see what a push WOULD run.
if [ -n "$CHARCOAL_HOOK_CHANGED_FILES" ]; then
    CHANGED="$CHARCOAL_HOOK_CHANGED_FILES"
fi

# ── 3. Language-specific quality gates ───────────────────────────────
# CUSTOMIZE: add/remove gates as your project's stack requires.

# Rust: clippy + ONLY the tests this push affects
#
# ┌─ LOCAL EXCEPTION TO THE TEMPLATE (#367) ─────────────────────────────┐
# │ Charcoal-specific: feature names, the module→test mapping, and the   │
# │ Postgres hand-off to CI. `update-project-from-template` will replace │
# │ this block with the template's run-everything version. Re-apply it,  │
# │ or every push silently goes back to running the whole suite.         │
# └──────────────────────────────────────────────────────────────────────┘
#
# This gate is fast feedback on what THIS push touched — it is NOT the safety
# net. The full suite (every feature, every test binary, the Postgres suite,
# the model-gated tests) runs in CI on every pull request. The selection below
# is a heuristic: a change can break a test it does not obviously touch, and
# CI is what catches that.
#
# Preview what a push would run, without running it:
#   CHARCOAL_HOOK_DRY_RUN=1 .git/hooks/pre-push origin < /dev/null
# ...optionally for a hypothetical change set (newline-separated paths):
#   CHARCOAL_HOOK_CHANGED_FILES="src/web/refresh.rs" CHARCOAL_HOOK_DRY_RUN=1 \
#     .git/hooks/pre-push origin < /dev/null
if [ -f "$REPO_ROOT/Cargo.toml" ]; then
    RUST_CHANGED=$(echo "$CHANGED" | grep -E '\.rs$|(^|/)Cargo\.(toml|lock)$|^migrations/' || true)
    if [ -n "$RUST_CHANGED" ]; then
        # Run a gate, or in dry-run mode just print it. The command runs from
        # the repo root so a push from a subdirectory behaves the same.
        run_gate() {
            local label="$1"
            shift
            if [ -n "$CHARCOAL_HOOK_DRY_RUN" ]; then
                echo "   [dry run] $label: $*"
                return 0
            fi
            echo "🔍 Pre-push: $label..."
            (cd "$REPO_ROOT" && "$@" 2>&1)
        }

        # A test file gated `#![cfg(feature = "postgres")]` compiles to ZERO
        # tests without that feature and would "pass" having run nothing. Those
        # need a live database, so they belong to the Postgres suite in CI.
        is_postgres_test() {
            grep -q '^#!\[cfg(feature = "postgres")\]' "$1"
        }

        # Add `--test <name>` once.
        TEST_ARGS=""
        add_test() {
            case " $TEST_ARGS " in
                *" --test $1 "*) ;;
                *) TEST_ARGS="$TEST_ARGS --test $1" ;;
            esac
        }

        # Paths that the Postgres build compiles or depends on. Clippy still
        # type-checks the Postgres build locally, so a compile error cannot
        # wait for CI; only the Postgres TESTS are handed off.
        PG_TOUCHED=$(echo "$RUST_CHANGED" | grep -E '^src/db/|^migrations/postgres/|^tests/db_postgres\.rs$|(^|/)Cargo\.(toml|lock)$' || true)

        if ! run_gate "cargo clippy (web)" cargo clippy --all-targets --features web --quiet -- -D warnings; then
            echo ""
            echo "❌ Clippy warnings. Fix them before pushing."
            echo ""
            exit 1
        fi
        if [ -n "$PG_TOUCHED" ]; then
            if ! run_gate "cargo clippy (postgres)" cargo clippy --all-targets --features postgres --quiet -- -D warnings; then
                echo ""
                echo "❌ Clippy warnings in the Postgres build. Fix them before pushing."
                echo ""
                exit 1
            fi
        fi

        # 1. A changed integration-test file runs directly.
        PG_TESTS_CHANGED=""
        for f in $(echo "$RUST_CHANGED" | grep -E '^tests/[^/]+\.rs$' || true); do
            [ -f "$REPO_ROOT/$f" ] || continue # deleted in this push
            if is_postgres_test "$REPO_ROOT/$f"; then
                PG_TESTS_CHANGED="$PG_TESTS_CHANGED $(basename "$f" .rs)"
            else
                add_test "$(basename "$f" .rs)"
            fi
        done

        # 2. Changed library code runs its own inline unit tests (`--lib`), plus
        #    every integration test that names the changed module by its full
        #    path. `src/web/refresh.rs` → `charcoal::web::refresh`; `mod.rs`
        #    maps to its directory. A prefix can over-match a sibling (for
        #    example `…::refresh` also matches `…::refresh_scan`), which costs
        #    an extra test binary, never a missed one.
        SRC_CHANGED=$(echo "$RUST_CHANGED" | grep -E '^src/.+\.rs$' || true)
        if [ -n "$SRC_CHANGED" ]; then
            TEST_ARGS="--lib$TEST_ARGS"
            for f in $SRC_CHANGED; do
                mod=${f#src/}
                mod=${mod%.rs}
                mod=${mod%/mod}
                case "$mod" in
                    lib | main) continue ;; # crate roots: covered by --lib
                esac
                modpath="charcoal::$(echo "$mod" | sed 's#/#::#g')"
                for t in "$REPO_ROOT"/tests/*.rs; do
                    [ -f "$t" ] || continue
                    is_postgres_test "$t" && continue
                    if grep -qF "$modpath" "$t"; then
                        add_test "$(basename "$t" .rs)"
                    fi
                done
            done
        fi

        # ┌─ LOCAL EXCEPTION TO THE TEMPLATE ────────────────────────────────┐
        # │ This block is charcoal-specific and is NOT in project-template.  │
        # │ `update-project-from-template` will overwrite it — if a sync     │
        # │ drops these four lines, `git push` starts failing again with     │
        # │ model-gated test failures, which is a confusing symptom for a    │
        # │ missing env var. Re-apply, or upstream it. See #284.             │
        # └──────────────────────────────────────────────────────────────────┘
        # Point model-gated tests at the checked-out models. Test binaries do
        # not load .env (dotenvy runs in main.rs only), so without this they
        # look in the platform data dir, fail to find the ONNX files, and — now
        # that they fail loudly instead of silently passing (#257) — block every
        # push on a machine that has models but no exported variable (#284).
        if [ -d "$REPO_ROOT/models" ] && [ -z "$CHARCOAL_MODEL_DIR" ]; then
            export CHARCOAL_MODEL_DIR="$REPO_ROOT/models"
        fi

        if [ -n "$TEST_ARGS" ]; then
            # `--features web` rather than the default: web is where most of
            # the code lives, and the default build does not compile it at all,
            # so the old run-everything default gave web changes no local
            # coverage. Every non-Postgres test binary compiles under web.
            # shellcheck disable=SC2086 # TEST_ARGS is deliberately word-split
            if ! run_gate "cargo test (affected: ${TEST_ARGS# })" cargo test --features web $TEST_ARGS --quiet; then
                echo ""
                echo "❌ Tests failed. Fix them before pushing."
                echo ""
                exit 1
            fi
        else
            echo "⏭️  Pre-push: no local tests to run (dependency, migration or Postgres-only changes) — the full suite runs in CI"
        fi

        if [ -n "$PG_TOUCHED$PG_TESTS_CHANGED" ]; then
            echo "ℹ️  Pre-push: this push affects the Postgres build — the Postgres suite runs in CI on the pull request"
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

# Returns 0 iff $1 is Git's default `info/exclude` stub — i.e. contains
# only comment lines and blanks, no real ignore patterns. We can't
# byte-compare against a canonical stub because different git versions
# ship different stub text; instead, treat "no non-comment, non-blank
# lines" as the safe signature. This preserves user files that keep
# Git's header AND add patterns below.
_exclude_is_default_stub() {
    local file="$1"
    [ -f "$file" ] || return 1
    # Look for any line that has content and isn't a comment.
    if grep -qE '^[[:space:]]*[^[:space:]#]' "$file" 2>/dev/null; then
        return 1
    fi
    return 0
}

# ── Wire .gitignore.local into git's per-repo excludes ────────────────
#
# Git does NOT read `.gitignore.local` automatically — only the main
# `.gitignore` is read by default. To make the extension convention
# actually work, this script symlinks the per-clone `.git/info/exclude`
# (git's documented "in-repo but untracked" gitignore extension point)
# to the project's `.gitignore.local`. Git resolves the symlink
# transparently and applies its patterns on top of `.gitignore` +
# the user's global excludes.
#
# Per-clone by design: each clone runs `install-hooks.sh` once, so each
# clone gets the wiring. If a downstream user changes `.gitignore.local`
# later, the symlink means live edits propagate — no re-run needed.
#
# Detect linked worktrees. Git resolves `info/` from `$GIT_COMMON_DIR`,
# which is the primary checkout's `.git` — shared across all worktrees.
# `LOCAL_IGNORE` however points to the CURRENT worktree. If we run the
# wiring from a linked worktree we'd repoint the shared exclude at that
# worktree's local file, hijacking ignore behavior for every worktree.
# Skip and let the primary worktree handle it.
_git_dir_abs="$(cd "$REPO_ROOT" && cd "$(git rev-parse --git-dir)" && pwd -P)"
_common_dir_abs="$(cd "$REPO_ROOT" && cd "$_common_gitdir" && pwd -P)"
EXCLUDE_FILE="$_common_dir_abs/info/exclude"
LOCAL_IGNORE="$REPO_ROOT/.gitignore.local"

if [ "$_git_dir_abs" != "$_common_dir_abs" ]; then
    echo "  ⏭  Linked worktree — .gitignore.local wiring belongs to the primary"
    echo "     checkout; run install-hooks.sh there once. This worktree already"
    echo "     inherits whatever the primary set up (worktrees share info/exclude)."
elif [ -e "$LOCAL_IGNORE" ] || [ -h "$EXCLUDE_FILE" ]; then
    if [ -h "$EXCLUDE_FILE" ]; then
        # Only touch symlinks we can positively identify as ours (pointing
        # at $LOCAL_IGNORE). Any other symlink is user-managed — leave it.
        _current="$(readlink "$EXCLUDE_FILE" 2>/dev/null || true)"
        if [ "$_current" = "$LOCAL_IGNORE" ]; then
            echo "  ✓ .git/info/exclude -> .gitignore.local (already wired)"
        else
            echo "  ⚠️  .git/info/exclude is a symlink pointing elsewhere — leaving alone."
            echo "     Current target: $_current"
            echo "     To wire .gitignore.local, remove the symlink and re-run."
        fi
    elif [ ! -e "$EXCLUDE_FILE" ] || _exclude_is_default_stub "$EXCLUDE_FILE"; then
        # File is missing or is Git's default stub with zero real patterns.
        # `_exclude_is_default_stub` verifies the ENTIRE file is comment /
        # blank lines only — not just the first line — so a user who kept
        # the header and added their own patterns below is preserved.
        mkdir -p "$(dirname "$EXCLUDE_FILE")"
        ln -sf "$LOCAL_IGNORE" "$EXCLUDE_FILE"
        echo "  ✓ .git/info/exclude -> .gitignore.local (wiring project-local ignores)"
    else
        echo "  ⚠️  .git/info/exclude has custom content — leaving alone."
        echo "     To wire .gitignore.local, back up .git/info/exclude and re-run."
    fi
fi

echo ""
echo "Done. Both hooks are installed."
echo "Bypass any hook with --no-verify (emergency only)."
