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

# Load .env safely — parse key=value without executing as bash
if [ -f "$REPO_ROOT/.env" ]; then
    while IFS= read -r line; do
        [[ "$line" =~ ^[[:space:]]*# ]] && continue
        [[ -z "${line//[[:space:]]/}" ]] && continue
        [[ "$line" =~ ^([A-Za-z_][A-Za-z0-9_]*)=(.*)$ ]] && export "${BASH_REMATCH[1]}=${BASH_REMATCH[2]}"
    done < "$REPO_ROOT/.env"
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
if aws s3 cp "$S3/issues.db" "$REPO_ROOT/.chainlink/issues.db" $ENDPOINT --quiet 2>/dev/null; then
    echo "✅ .chainlink/issues.db restored"
else
    echo "⚠️  issues.db not found in bucket (first-time setup?)"
fi

# ── deciduous decision graph ─────────────────────────────────────────
mkdir -p "$REPO_ROOT/.deciduous"
if aws s3 cp "$S3/deciduous.db" "$REPO_ROOT/.deciduous/deciduous.db" $ENDPOINT --quiet 2>/dev/null; then
    echo "✅ .deciduous/deciduous.db restored"
else
    echo "⚠️  deciduous.db not found in bucket (first-time setup?)"
fi

echo ""
echo "Done. Run 'chainlink list' and 'deciduous nodes' to verify."
