#!/usr/bin/env bash
# Restore chainlink issues and deciduous decision graph from S3-compatible
# blob storage (Cloudflare R2, Backblaze B2, etc.).
#
# Usage:
#   ./scripts/restore-dbs.sh
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

S3="s3://$BACKUP_S3_BUCKET"
ENDPOINT="--endpoint-url=$BACKUP_S3_ENDPOINT --region=${BACKUP_S3_REGION:-auto}"

echo "🗄️  Restoring databases from $BACKUP_S3_BUCKET..."

# ── chainlink issues ─────────────────────────────────────────────────
mkdir -p "$REPO_ROOT/.chainlink"
if aws s3 cp "$S3/issues.db" "$REPO_ROOT/.chainlink/issues.db" $ENDPOINT --quiet; then
    echo "✅ .chainlink/issues.db restored"
else
    echo "❌ Failed to restore issues.db — check bucket name and credentials"
fi

# ── deciduous decision graph ─────────────────────────────────────────
mkdir -p "$REPO_ROOT/.deciduous"
if aws s3 cp "$S3/deciduous.db" "$REPO_ROOT/.deciduous/deciduous.db" $ENDPOINT --quiet; then
    echo "✅ .deciduous/deciduous.db restored"
else
    echo "❌ Failed to restore deciduous.db — check bucket name and credentials"
fi

echo ""
echo "Done. Run 'chainlink list' and 'deciduous nodes' to verify."
