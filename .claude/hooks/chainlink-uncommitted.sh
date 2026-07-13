#!/usr/bin/env bash
# Stop hook: warn if .chainlink/issues.jsonl has uncommitted changes
# when the Claude Code session is about to end.
#
# Particularly important in claude.ai cloud sessions, where the sandbox
# tears down and any unpushed updates to issues.jsonl are lost.

set -euo pipefail

[ -d .chainlink ] || exit 0
# Use -e (not -d): in git worktrees and submodules, `.git` is a file
# pointing at the real gitdir, not a directory. Both shapes are valid
# repos and both should get the dirty-file warning.
[ -e .git ] || exit 0

if ! git status --porcelain .chainlink/issues.jsonl 2>/dev/null | grep -q .; then
  exit 0
fi

cat >&2 <<MSG

WARNING: .chainlink/issues.jsonl has uncommitted changes.

  Chainlink issue updates live in this file; if the session ends
  without committing them, the updates are lost when the sandbox
  tears down. (Less catastrophic on a local Mac mini session — the
  file stays on disk — but still worth committing before context
  rolls over.)

  To commit and push:
    git add .chainlink/issues.jsonl
    git commit -m "chore(chainlink): update issues"
    git push

MSG

exit 0
