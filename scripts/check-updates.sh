#!/usr/bin/env bash
# check-updates.sh — Check for dependency updates across the Charcoal project.
#
# Usage:
#   ./scripts/check-updates.sh              # Check everything
#   ./scripts/check-updates.sh --tools      # Standalone cargo-installed tools only
#   ./scripts/check-updates.sh --deps       # Project dependencies only (Cargo + npm)
#   ./scripts/check-updates.sh --deps-rust  # Rust crate dependencies only
#   ./scripts/check-updates.sh --deps-npm   # npm dependencies only
#   ./scripts/check-updates.sh --deps-git   # Git-sourced dependencies only
#
# Requires: cargo, jq, npm (for --deps-npm)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# ── Colors ───────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
RESET='\033[0m'

# ── Helpers ──────────────────────────────────────────────────────────

# Get latest version from crates.io via cargo search
crate_latest() {
    local crate="$1"
    cargo search --limit 1 "$crate" 2>/dev/null \
        | grep "^${crate} " \
        | sed 's/.*= "\([^"]*\)".*/\1/'
}

# Get installed version of a cargo binary
tool_installed_version() {
    local crate="$1"
    cargo install --list 2>/dev/null \
        | grep "^${crate} " \
        | sed 's/.* v\([^ ]*\).*/\1/'
}

# Compare semver: returns "up-to-date", "outdated", or "unknown"
compare_versions() {
    local current="$1" latest="$2"
    if [ -z "$current" ] || [ -z "$latest" ]; then
        echo "unknown"
    elif [ "$current" = "$latest" ]; then
        echo "up-to-date"
    else
        echo "outdated"
    fi
}

# Print a dependency row
print_row() {
    local name="$1" current="$2" latest="$3" status="$4" notes="${5:-}"
    case "$status" in
        up-to-date) color="$GREEN" symbol="✓" ;;
        outdated)   color="$YELLOW" symbol="⬆" ;;
        pinned)     color="$CYAN" symbol="📌" ;;
        git)        color="$CYAN" symbol="🔗" ;;
        *)          color="$RED" symbol="?" ;;
    esac
    printf "  ${color}${symbol}${RESET}  %-28s %-14s %-14s %s\n" \
        "$name" "$current" "$latest" "$notes"
}

# Extract version from Cargo.toml for a given crate
# Handles both `crate = "version"` and `crate = { version = "version" }` formats
cargo_toml_version() {
    local crate="$1"
    local toml="$REPO_ROOT/Cargo.toml"

    # Try inline table format: crate = { version = "X.Y" ... }
    local ver
    ver=$(grep -E "^${crate} = \{" "$toml" 2>/dev/null \
        | sed 's/.*version *= *"\([^"]*\)".*/\1/' || true)
    if [ -n "$ver" ]; then echo "$ver"; return; fi

    # Try simple format: crate = "X.Y"
    ver=$(grep -E "^${crate} = \"" "$toml" 2>/dev/null \
        | sed 's/.*= *"\([^"]*\)".*/\1/' || true)
    if [ -n "$ver" ]; then echo "$ver"; return; fi

    # Try [dependencies.crate] table format
    ver=$(awk "/^\[dependencies\.${crate}\]/,/^\[/" "$toml" 2>/dev/null \
        | grep 'version' \
        | head -1 \
        | sed 's/.*= *"\([^"]*\)".*/\1/' || true)
    if [ -n "$ver" ]; then echo "$ver"; return; fi

    echo ""
}

# ── Tool Checks ──────────────────────────────────────────────────────

check_tools() {
    echo ""
    echo -e "${BOLD}═══ Standalone Tools (cargo install) ═══${RESET}"
    echo ""
    printf "       %-28s %-14s %-14s %s\n" "TOOL" "INSTALLED" "LATEST" "NOTES"
    printf "       %-28s %-14s %-14s %s\n" "----" "---------" "------" "-----"

    local tools=(
        "chainlink-tracker:chainlink"
        "cargo-watch:cargo-watch"
        "cargo-edit:cargo-add"
    )

    for entry in "${tools[@]}"; do
        local crate="${entry%%:*}"
        local binary="${entry##*:}"
        local installed latest status

        installed=$(tool_installed_version "$crate")
        if [ -z "$installed" ]; then
            installed="not found"
        fi
        latest=$(crate_latest "$crate")
        if [ -z "$latest" ]; then
            latest="?"
        fi

        status=$(compare_versions "$installed" "$latest")
        local notes=""
        if [ "$status" = "outdated" ]; then
            notes="https://crates.io/crates/${crate}"
        fi
        print_row "$crate" "$installed" "$latest" "$status" "$notes"
    done

    # Deciduous is installed via brew
    local dec_installed dec_latest
    dec_installed=$(deciduous --version 2>/dev/null | awk '{print $NF}' || echo "not found")
    dec_latest=$(brew info deciduous --json 2>/dev/null | jq -r '.[0].versions.stable // empty' 2>/dev/null || echo "?")
    if [ "$dec_latest" = "null" ] || [ -z "$dec_latest" ]; then
        # Fallback: check crates.io
        dec_latest=$(crate_latest "deciduous")
    fi
    local dec_status
    dec_status=$(compare_versions "$dec_installed" "$dec_latest")
    print_row "deciduous (brew)" "$dec_installed" "$dec_latest" "$dec_status" ""
}

# ── Rust Dependency Checks ───────────────────────────────────────────

check_deps_rust() {
    echo ""
    echo -e "${BOLD}═══ Rust Dependencies (Cargo.toml) ═══${RESET}"
    echo ""
    printf "       %-28s %-14s %-14s %s\n" "CRATE" "PINNED" "LATEST" "NOTES"
    printf "       %-28s %-14s %-14s %s\n" "-----" "------" "------" "-----"

    # All crates.io dependencies (not git-sourced)
    local crates=(
        clap tokio atrium-api reqwest rusqlite
        keyword_extraction stop-words regex-lite
        serde serde_json dotenvy async-trait
        ort tokenizers dirs futures anyhow
        tracing tracing-subscriber colored indicatif chrono
        sqlx-core sqlx-postgres pgvector
        axum tower-http include_dir
        hmac sha2 rand hex base64 percent-encoding
    )

    for crate in "${crates[@]}"; do
        local pinned latest status notes=""
        pinned=$(cargo_toml_version "$crate")
        if [ -z "$pinned" ]; then
            continue  # Not in Cargo.toml (transitive dep or typo)
        fi

        latest=$(crate_latest "$crate")
        if [ -z "$latest" ]; then
            latest="?"
        fi

        # Strip leading caret/tilde/= for comparison
        local pinned_clean="${pinned#[\^~=]}"

        # Check if pinned is a range (e.g. "0.12") vs exact
        # A range like "0.12" is compatible with "0.12.28"
        if [[ "$latest" == "${pinned_clean}"* ]]; then
            status="up-to-date"
        elif [ "$pinned_clean" = "$latest" ]; then
            status="up-to-date"
        else
            status="outdated"
            notes="https://crates.io/crates/${crate}"
        fi

        print_row "$crate" "$pinned" "$latest" "$status" "$notes"
    done
}

# ── Git Dependency Checks ────────────────────────────────────────────

check_deps_git() {
    echo ""
    echo -e "${BOLD}═══ Git Dependencies ═══${RESET}"
    echo ""

    local git_url="https://github.com/musicjunkieg/atproto-crates"
    local pinned_rev="d2457eecc6814e8351486df3a3608542bae848a7"
    local short_rev="${pinned_rev:0:10}"

    printf "       %-28s %-14s %s\n" "REPO" "PINNED REV" "NOTES"
    printf "       %-28s %-14s %s\n" "----" "----------" "-----"

    # Check if there are newer commits on the default branch
    local latest_rev
    latest_rev=$(git ls-remote "$git_url" HEAD 2>/dev/null | awk '{print $1}' || echo "")

    if [ -z "$latest_rev" ]; then
        print_row "atproto-crates" "$short_rev" "fetch failed" "unknown" ""
    elif [ "$latest_rev" = "$pinned_rev" ]; then
        print_row "atproto-crates" "$short_rev" "${latest_rev:0:10}" "up-to-date" ""
    else
        print_row "atproto-crates" "$short_rev" "${latest_rev:0:10}" "outdated" \
            "${git_url}/compare/${short_rev}...${latest_rev:0:10}"
    fi

    echo ""
    echo "  Used by: atproto-oauth, atproto-oauth-axum, atproto-identity"
}

# ── npm Dependency Checks ────────────────────────────────────────────

check_deps_npm() {
    echo ""
    echo -e "${BOLD}═══ npm Dependencies (web/package.json) ═══${RESET}"
    echo ""

    if [ ! -d "$REPO_ROOT/web/node_modules" ]; then
        echo "  ⚠️  node_modules not found. Run: cd web && npm install"
        return
    fi

    cd "$REPO_ROOT/web"

    # npm outdated returns exit code 1 if anything is outdated, so ignore that
    local outdated_json
    outdated_json=$(npm outdated --json 2>/dev/null || true)

    if [ -z "$outdated_json" ] || [ "$outdated_json" = "{}" ]; then
        echo -e "  ${GREEN}✓${RESET}  All npm packages are up to date."
        return
    fi

    printf "       %-28s %-14s %-14s %-14s\n" "PACKAGE" "CURRENT" "WANTED" "LATEST"
    printf "       %-28s %-14s %-14s %-14s\n" "-------" "-------" "------" "------"

    echo "$outdated_json" | jq -r '
        to_entries[] |
        "\(.key)|\(.value.current // "?")|\(.value.wanted // "?")|\(.value.latest // "?")"
    ' | while IFS='|' read -r name current wanted latest; do
        local status="up-to-date"
        local notes=""
        if [ "$current" != "$latest" ]; then
            status="outdated"
            notes="https://www.npmjs.com/package/${name}"
        fi
        print_row "$name" "$current" "$latest" "$status" "$notes"
    done

    cd "$REPO_ROOT"
}

# ── Main ─────────────────────────────────────────────────────────────

show_help() {
    echo "Usage: $(basename "$0") [OPTIONS]"
    echo ""
    echo "Check for dependency updates across the Charcoal project."
    echo ""
    echo "Options:"
    echo "  --tools       Standalone cargo-installed tools only"
    echo "  --deps        All project dependencies (Rust + npm + git)"
    echo "  --deps-rust   Rust crate dependencies only"
    echo "  --deps-npm    npm dependencies only"
    echo "  --deps-git    Git-sourced dependencies only"
    echo "  --help        Show this help"
    echo ""
    echo "With no options, checks everything."
}

main() {
    local mode="${1:---all}"

    case "$mode" in
        --tools)
            check_tools
            ;;
        --deps)
            check_deps_rust
            check_deps_git
            check_deps_npm
            ;;
        --deps-rust)
            check_deps_rust
            ;;
        --deps-npm)
            check_deps_npm
            ;;
        --deps-git)
            check_deps_git
            ;;
        --all)
            check_tools
            check_deps_rust
            check_deps_git
            check_deps_npm
            ;;
        --help|-h)
            show_help
            exit 0
            ;;
        *)
            echo "Unknown option: $mode"
            show_help
            exit 1
            ;;
    esac

    echo ""
    echo -e "${BOLD}───────────────────────────────────────${RESET}"
    echo -e "  ${GREEN}✓${RESET} = up to date   ${YELLOW}⬆${RESET} = update available   ${CYAN}🔗${RESET} = git pinned"
    echo ""
}

main "$@"
