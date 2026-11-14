#!/usr/bin/env bash
# Restore chainlink issues and deciduous decision graph from S3-compatible
# blob storage (Cloudflare R2, Backblaze B2, etc.).
#
# Usage:
#   ./scripts/restore-dbs.sh                 # the latest backup
#   ./scripts/restore-dbs.sh --list          # list dated history copies
#   ./scripts/restore-dbs.sh --from STAMP    # restore the copies from STAMP
#
# The pre-commit hook writes a dated copy under history/ on every upload,
# so --from can go back past a bad backup (STAMP as --list prints it).
#
# Requires BACKUP_S3_BUCKET, BACKUP_S3_ACCESS_KEY_ID, BACKUP_S3_SECRET_ACCESS_KEY,
# and BACKUP_S3_ENDPOINT to be set — either in .env or the environment.
#
# The restored files:
#   .chainlink/issues.db    — chainlink issue tracker database
#   .deciduous/deciduous.db — deciduous decision graph database

set -e

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Pre-flight: require aws CLI
if ! command -v aws &>/dev/null; then
    echo "❌ aws CLI not found. Install it: brew install awscli"
    exit 1
fi

# Load .env safely — handles quoted values, export prefixes, special chars.
# Does NOT execute the file as bash; parses key=value line by line.
_load_env() {
    local line key value
    while IFS= read -r line || [ -n "$line" ]; do
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

# Validate required vars
for var in BACKUP_S3_BUCKET BACKUP_S3_ACCESS_KEY_ID BACKUP_S3_SECRET_ACCESS_KEY BACKUP_S3_ENDPOINT; do
    if [ -z "${!var}" ]; then
        echo "❌ Missing required variable: $var"
        echo "   Add it to .env (see .env.example for format)"
        exit 1
    fi
done

export AWS_ACCESS_KEY_ID="$BACKUP_S3_ACCESS_KEY_ID"
export AWS_SECRET_ACCESS_KEY="$BACKUP_S3_SECRET_ACCESS_KEY"

# --prefix PREFIX restores another machine's backups ("" for the first
# machine's, which have no prefix); otherwise BACKUP_S3_PREFIX from .env.
if [ "${1:-}" = "--prefix" ]; then
    [ $# -ge 2 ] || { echo "❌ --prefix needs a value (\"\" for no prefix)"; exit 1; }
    BACKUP_S3_PREFIX="$2"
    shift 2
fi
S3="s3://$BACKUP_S3_BUCKET${BACKUP_S3_PREFIX:+/${BACKUP_S3_PREFIX%/}}"
ENDPOINT=(--endpoint-url "$BACKUP_S3_ENDPOINT" --region "${BACKUP_S3_REGION:-auto}")

MODE=latest
STAMP=""
case "${1:-}" in
    "") ;;
    --list) MODE=list ;;
    --from)
        MODE=from
        STAMP="${2:-}"
        if [ -z "$STAMP" ]; then echo "❌ --from needs a STAMP (see --list)"; exit 1; fi ;;
    *) echo "Usage: $0 [--prefix PREFIX] [--list | --from STAMP]"; exit 1 ;;
esac

if [ "$MODE" = list ]; then
    # `aws s3 ls` prints "<date> <time> <size> <name>"; group by stamp.
    if ! listing=$(aws s3 ls "$S3/history/" "${ENDPOINT[@]}"); then
        echo "❌ Couldn't list $S3/history/ — check bucket name and credentials"
        exit 1
    fi
    echo "$listing" | awk '{ print $NF }' \
        | sed -nE 's/^(issues|deciduous)-(.+)\.db$/\2 \1/p' | sort \
        | awk '{ dbs[$1] = dbs[$1] " " $2 } END { for (s in dbs) print s ":" dbs[s] }' | sort
    exit 0
fi

# Object names for each DB in this mode.
if [ "$MODE" = from ]; then
    ISSUES_KEY="history/issues-$STAMP.db"
    DECIDUOUS_KEY="history/deciduous-$STAMP.db"
    echo "🗄️  Restoring databases from ${S3#s3://} as of $STAMP..."
else
    ISSUES_KEY="issues.db"
    DECIDUOUS_KEY="deciduous.db"
    echo "🗄️  Restoring databases from ${S3#s3://}..."
fi

# In a worktree the DBs are often links to the primary checkout's (a
# linked file or folder). `aws s3 cp` renames its download onto the path,
# which would replace a linked file with a private copy and leave the
# shared DB stale, so write to the file behind the link instead.
_real_path() {
    local p="$1" t d n=0
    while [ -L "$p" ] && [ $n -lt 40 ]; do
        t="$(readlink "$p")"
        case "$t" in /*) p="$t" ;; *) p="$(dirname "$p")/$t" ;; esac
        n=$((n + 1))
    done
    d="$(cd -P "$(dirname "$p")" 2>/dev/null && pwd -P)" || { echo "$p"; return; }
    echo "$d/$(basename "$p")"
}

# ── chainlink issues ─────────────────────────────────────────────────
mkdir -p "$REPO_ROOT/.chainlink"
RESTORE_FAILED=0
ISSUES_DEST="$(_real_path "$REPO_ROOT/.chainlink/issues.db")"
if aws s3 cp "$S3/$ISSUES_KEY" "$ISSUES_DEST" "${ENDPOINT[@]}" --quiet; then
    echo "✅ .chainlink/issues.db restored ($ISSUES_DEST)"
else
    echo "❌ Failed to restore $ISSUES_KEY — check the name (see --list), bucket and credentials"
    RESTORE_FAILED=1
fi

# ── deciduous decision graph ─────────────────────────────────────────
mkdir -p "$REPO_ROOT/.deciduous"
DECIDUOUS_DEST="$(_real_path "$REPO_ROOT/.deciduous/deciduous.db")"
if aws s3 cp "$S3/$DECIDUOUS_KEY" "$DECIDUOUS_DEST" "${ENDPOINT[@]}" --quiet; then
    echo "✅ .deciduous/deciduous.db restored ($DECIDUOUS_DEST)"
else
    echo "❌ Failed to restore $DECIDUOUS_KEY — check the name (see --list), bucket and credentials"
    RESTORE_FAILED=1
fi

echo ""
if [ "$RESTORE_FAILED" -ne 0 ]; then
    # Both restores are attempted above so one failure doesn't mask the
    # other, but automation must not proceed on a missing/stale database.
    echo "Done with errors — at least one restore failed."
    exit 1
fi
echo "Done. Run 'chainlink list' and 'deciduous nodes' to verify."
