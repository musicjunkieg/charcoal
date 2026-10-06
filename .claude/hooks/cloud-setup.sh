#!/bin/bash
# cloud-setup.sh
# SessionStart hook: installs the CLIs CLAUDE.md tells Claude to use but
# that a claude.ai cloud sandbox doesn't ship with. Local sessions get
# them from the machine's own toolchain (kept current by update-tools),
# so this is a no-op outside the cloud.
#
# Never blocks a session: every failure is reported on stdout (which
# SessionStart feeds to Claude as context) and the hook still exits 0.

if [ "$CLAUDE_CODE_REMOTE" != "true" ]; then
    exit 0
fi

# Cloudflare CLI (`cf`). Pinned to an exact version and installed with
# --ignore-scripts: this runs unattended in a sandbox that may hold
# CLOUDFLARE_API_TOKEN, so it must never execute whatever npm "latest"
# happens to be. Bump the pin in project-template (update-tools reports
# when it lags npm) and sync it downstream.
CF_VERSION="1.0.0-beta.12"
if ! command -v cf >/dev/null 2>&1; then
    if ! command -v npm >/dev/null 2>&1; then
        echo "cloud-setup: npm not found — cf (Cloudflare CLI) was not installed."
    elif npm install -g --ignore-scripts "cf@$CF_VERSION" >/dev/null 2>&1 && command -v cf >/dev/null 2>&1; then
        echo "cloud-setup: installed cf (Cloudflare CLI) — $(cf --version 2>/dev/null | grep -oE 'v[0-9][^ ]*' | head -1)"
    else
        echo "cloud-setup: 'npm install -g --ignore-scripts cf@$CF_VERSION' failed — cf (Cloudflare CLI) is unavailable this session. Retry it by hand before any Cloudflare work; do not fall back to wrangler."
    fi
fi

if command -v cf >/dev/null 2>&1 && [ -z "$CLOUDFLARE_API_TOKEN" ]; then
    echo "cloud-setup: CLOUDFLARE_API_TOKEN is not set in this environment — cf commands that touch the account will fail until Bryan adds it to the cloud environment."
fi

exit 0
