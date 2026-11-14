#!/usr/bin/env bash
# Point a linked worktree's chainlink and deciduous databases at the
# primary checkout's, so every worktree (e.g. each Sesh session) reads and
# writes the same issues and decision graph instead of a throwaway copy.
#
# Usage (from inside a linked worktree; .sesh/config.json runs it per session):
#   ./scripts/link-shared-dbs.sh
#
# Each DB becomes a symlink to the primary checkout's file. SQLite resolves
# the link, so its lock and journal live with the real DB and concurrent
# writers from different worktrees coordinate. The primary's DB need not
# exist yet: the first write through the link creates it there.
#
# A worktree that already has a real DB file keeps it (with a warning) —
# it may hold work that isn't in the primary's DB. A DB whose folder is
# already a link to the primary's is shared as it is. In the primary
# checkout itself this is a no-op.

set -e

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Ask git about a directory, ignoring any GIT_DIR, GIT_COMMON_DIR etc.
# inherited from a calling git process (git lists them itself).
_git_in() { (cd "$1" && unset $(git rev-parse --local-env-vars) && git rev-parse --path-format=absolute "${@:2}") 2>/dev/null; }

GIT_DIR_ABS="$(_git_in "$REPO_ROOT" --git-dir)"
COMMON_DIR="$(_git_in "$REPO_ROOT" --git-common-dir)"
if [ "$GIT_DIR_ABS" = "$COMMON_DIR" ]; then
    echo "Primary checkout — nothing to link."
    exit 0
fi

# The primary checkout is the work tree whose own git dir is the common
# one. Usually that's the common dir's parent; with `git init
# --separate-git-dir` it may be elsewhere (git can't name it — Sesh's
# SESH_ROOT_PATH can) or the parent may be a subdirectory of it. Resolve
# each candidate to its work-tree root and use the first git confirms.
PRIMARY=""
for candidate in "${SESH_ROOT_PATH:-}" "$(dirname "$COMMON_DIR")"; do
    [ -n "$candidate" ] && [ -d "$candidate" ] || continue
    top="$(_git_in "$candidate" --show-toplevel)" || continue
    if [ -n "$top" ] && [ "$(_git_in "$top" --git-dir)" = "$COMMON_DIR" ]; then
        PRIMARY="$top"
        break
    fi
done
if [ -z "$PRIMARY" ]; then
    echo "⚠️  Can't find this repo's primary checkout (separate git dir?) — linking nothing. Set SESH_ROOT_PATH to it and rerun."
    exit 0
fi

# The path behind $1, through a linked file or a linked folder (as in
# install-hooks.sh and restore-dbs.sh).
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

for db in .chainlink/issues.db .deciduous/deciduous.db; do
    target="$PRIMARY/$db"
    link="$REPO_ROOT/$db"
    # A worktree whose whole DB folder links to the primary's (tandem's
    # Sesh setup) already shares the DB. Linking the file there would put a
    # link to itself in the primary.
    if [ "$(_real_path "$(dirname "$link")")" = "$(_real_path "$(dirname "$target")")" ]; then
        echo "✅ $db is already shared (its folder links to the primary's)"
        continue
    fi
    if [ -L "$link" ]; then
        ln -sfn "$target" "$link"
    elif [ -e "$link" ]; then
        echo "⚠️  $db is a real file in this worktree — leaving it; it is not shared or backed up."
        continue
    else
        mkdir -p "$(dirname "$link")"
        ln -s "$target" "$link"
    fi
    echo "✅ $db → $target"
done
