#!/usr/bin/env bash
# Build the SvelteKit frontend, then compile the Rust binary with web support.
# The npm build must run first so web/build/ exists for include_dir! at compile time.
#
# Usage:
#   ./scripts/build-web.sh                 # SQLite backend (local dev)
#   ./scripts/build-web.sh --postgres      # PostgreSQL backend (server/Railway)
#
# Required env vars for Railway / server deploys:
#   DATABASE_URL          - Postgres connection string (auto-set by Railway addon)
#   BLUESKY_HANDLE        - Bryan's Bluesky handle
#   CHARCOAL_WEB_PASSWORD - Dashboard login password
#   CHARCOAL_SESSION_SECRET - 32+ byte random hex string for HMAC signing

set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> Building SvelteKit frontend..."
cd web
npm ci
npm run build
cd ..

if [[ "${1:-}" == "--postgres" ]]; then
    FEATURES="web,postgres"
else
    FEATURES="web"
fi

echo "==> Building Rust binary (features: $FEATURES)..."
cargo build --release --features "$FEATURES"

echo "==> Done. Run with:"
echo "    ./target/release/charcoal serve --port 3000"
