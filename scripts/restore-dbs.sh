#!/usr/bin/env bash
# Restore chainlink issues and deciduous decision graph from Tigris blob storage.
#
# Usage:
#   ./scripts/restore-dbs.sh
#
# Requires TIGRIS_BUCKET, TIGRIS_ACCESS_KEY_ID, TIGRIS_SECRET_ACCESS_KEY,
# and TIGRIS_ENDPOINT to be set — either in .env or the environment.
#
# The restored files:
#   .chainlink/issues.db    — chainlink issue tracker database
#   .deciduous/deciduous.db — deciduous decision graph database

set -e

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Pre-flight: require aws CLI
if ! command -v aws &>/dev/null; then
    echo "❌ aws CLI not found. Install it: sudo apt install awscli"
    exit 1
fi

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

# Validate required vars
for var in TIGRIS_BUCKET TIGRIS_ACCESS_KEY_ID TIGRIS_SECRET_ACCESS_KEY TIGRIS_ENDPOINT; do
    if [ -z "${!var}" ]; then
        echo "❌ Missing required variable: $var"
        echo "   Add it to .env (see .env.example for format)"
        exit 1
    fi
done

export AWS_ACCESS_KEY_ID="$TIGRIS_ACCESS_KEY_ID"
export AWS_SECRET_ACCESS_KEY="$TIGRIS_SECRET_ACCESS_KEY"

S3="s3://$TIGRIS_BUCKET"
ENDPOINT="--endpoint-url=$TIGRIS_ENDPOINT --region=auto"

echo "🗄️  Restoring databases from Tigris ($TIGRIS_BUCKET)..."

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
