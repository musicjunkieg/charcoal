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
install_cf() {
    local src dest bindir
    src="$(cd "$(dirname "$0")" && pwd)/cloud-tools"
    dest="${XDG_CACHE_HOME:-$HOME/.cache}/claude-cloud-tools"
    [ -f "$src/package-lock.json" ] || { echo "no cloud-tools/package-lock.json"; return 1; }
    command -v npm >/dev/null 2>&1 || { echo "npm not found"; return 1; }
    mkdir -p "$dest" && cp "$src/package.json" "$src/package-lock.json" "$dest/" || { echo "could not stage $dest"; return 1; }
    (cd "$dest" && npm ci --ignore-scripts --no-audit --no-fund >/dev/null 2>&1) || { echo "'npm ci' failed (lockfile mismatch or registry unreachable)"; return 1; }
    [ -x "$dest/node_modules/.bin/cf" ] || { echo "cf binary missing after install"; return 1; }
    # Put it on PATH: link into a bin dir that is already there (next to
    # npm, or ~/.local/bin), else extend PATH through the session env file.
    for bindir in "$(dirname "$(command -v npm)")" "$HOME/.local/bin"; do
        if [ -d "$bindir" ] && [ -w "$bindir" ] \
            && ln -sf "$dest/node_modules/.bin/cf" "$bindir/cf" 2>/dev/null \
            && command -v cf >/dev/null 2>&1; then
            return 0
        fi
    done
    if [ -n "$CLAUDE_ENV_FILE" ]; then
        echo "export PATH=\"$dest/node_modules/.bin:\$PATH\"" >> "$CLAUDE_ENV_FILE"
        return 0
    fi
    echo "installed to $dest/node_modules/.bin but could not add it to PATH"
    return 1
}

if ! command -v cf >/dev/null 2>&1; then
    if reason=$(install_cf); then
        # install_cf ran in a subshell; mirror its env-file PATH fallback here.
        command -v cf >/dev/null 2>&1 || PATH="${XDG_CACHE_HOME:-$HOME/.cache}/claude-cloud-tools/node_modules/.bin:$PATH"
        echo "cloud-setup: installed cf (Cloudflare CLI) — $(cf --version 2>/dev/null | grep -oE 'v[0-9][^ ]*' | head -1)"
    else
        echo "cloud-setup: cf (Cloudflare CLI) is unavailable this session — $reason. Do not fall back to wrangler or an unpinned 'npm install -g cf'; tell Bryan."
    fi
fi

if command -v cf >/dev/null 2>&1 && [ -z "$CLOUDFLARE_API_TOKEN" ]; then
    echo "cloud-setup: CLOUDFLARE_API_TOKEN is not set in this environment — cf commands that touch the account will fail until Bryan adds it to the cloud environment."
fi

exit 0
