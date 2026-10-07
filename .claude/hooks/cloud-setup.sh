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

# Cloudflare CLI (`cf`). Installed with `npm ci` from the committed
# cloud-tools/package-lock.json, scripts disabled: this runs unattended
# in a sandbox that may hold CLOUDFLARE_API_TOKEN, so every package in
# the tree — not just cf itself — must match a reviewed integrity hash.
# To bump: edit cloud-tools/package.json in project-template, regenerate
# the lock (`npm install --package-lock-only --ignore-scripts`), sync
# downstream. update-tools reports when the pin lags npm.
SRC="$(cd "$(dirname "$0")" && pwd)/cloud-tools"
DEST="${XDG_CACHE_HOME:-$HOME/.cache}/claude-cloud-tools"
CF_PIN=$(sed -n 's/.*"cf": *"\([^"]*\)".*/\1/p' "$SRC/package.json" 2>/dev/null | head -1)

# True only for Cloudflare's cf at exactly the pinned version. A bare
# `command -v cf` is not enough: Cloud Foundry's CLI is also called cf,
# and a sandbox can carry an older cf from before a pin bump.
# ARG: path or name of the executable to test (default: cf on PATH).
cf_is_pinned() {
    local bin="${1:-cf}"
    [ -n "$CF_PIN" ] && command -v "$bin" >/dev/null 2>&1 \
        && "$bin" --version 2>/dev/null | grep -F 'cf · v' \
            | awk -v want="v$CF_PIN" '$NF == want { ok = 1 } END { exit !ok }'
}

install_cf() {
    local bindir
    [ -n "$CF_PIN" ] && [ -f "$SRC/package-lock.json" ] || { echo "no cloud-tools/package.json + package-lock.json"; return 1; }
    if ! cf_is_pinned "$DEST/node_modules/.bin/cf"; then
        command -v npm >/dev/null 2>&1 || { echo "npm not found"; return 1; }
        mkdir -p "$DEST" && cp "$SRC/package.json" "$SRC/package-lock.json" "$DEST/" || { echo "could not stage $DEST"; return 1; }
        (cd "$DEST" && npm ci --ignore-scripts --no-audit --no-fund >/dev/null 2>&1) || { echo "'npm ci' failed (lockfile mismatch or registry unreachable)"; return 1; }
        cf_is_pinned "$DEST/node_modules/.bin/cf" || { echo "installed cf does not report v$CF_PIN"; return 1; }
    fi
    # Put it on PATH: link into a bin dir that is already there (next to
    # npm, or ~/.local/bin). That only counts if the pinned cf is what
    # then resolves — another cf earlier on PATH would still win.
    for bindir in "$(dirname "$(command -v npm 2>/dev/null)")" "$HOME/.local/bin"; do
        if [ -d "$bindir" ] && [ -w "$bindir" ] \
            && ln -sf "$DEST/node_modules/.bin/cf" "$bindir/cf" 2>/dev/null \
            && cf_is_pinned; then
            return 0
        fi
    done
    # Otherwise prepend through the session env file, which outranks
    # anything already on PATH.
    if [ -n "$CLAUDE_ENV_FILE" ] \
        && { echo "export PATH=\"$DEST/node_modules/.bin:\$PATH\"" >> "$CLAUDE_ENV_FILE"; } 2>/dev/null; then
        return 0
    fi
    echo "installed to $DEST/node_modules/.bin but could not put it on PATH (no writable bin dir; CLAUDE_ENV_FILE unset or unwritable)"
    return 1
}

if ! cf_is_pinned; then
    if reason=$(install_cf); then
        # install_cf ran in a subshell; mirror its env-file PATH fallback here.
        cf_is_pinned || PATH="$DEST/node_modules/.bin:$PATH"
        echo "cloud-setup: installed cf (Cloudflare CLI) v$CF_PIN"
    else
        echo "cloud-setup: cf (Cloudflare CLI) is unavailable this session — $reason. If this project already uses Wrangler (it has a wrangler config file), keep using Wrangler as usual. Otherwise do not fall back to wrangler or an unpinned 'npm install -g cf'; tell Bryan."
    fi
fi

if cf_is_pinned && [ -z "$CLOUDFLARE_API_TOKEN" ]; then
    echo "cloud-setup: CLOUDFLARE_API_TOKEN is not set in this environment — cf commands that touch the account will fail until Bryan adds it to the cloud environment."
fi

exit 0
