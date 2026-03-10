#!/usr/bin/env bash
# Service wrapper: sets working directory and loads .env before starting charcoal serve.
set -euo pipefail
cd /home/sprite/charcoal
set -a
source .env
set +a
exec ./target/release/charcoal serve --port 8080 --bind 0.0.0.0
