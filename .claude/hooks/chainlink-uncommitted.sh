#!/usr/bin/env bash
# Stop hook: warn if .chainlink/issues.jsonl has uncommitted changes
# when the Claude Code session is about to end.
#
# Particularly important in claude.ai cloud sessions, where the sandbox
# tears down and any unpushed updates to issues.jsonl are lost.

set -euo pipefail

# Anchor all path checks at the project root. Claude Code exports
# CLAUDE_PROJECT_DIR for hook invocations; fall back to cwd for standalone
# testing. Without this, running Claude from a subdirectory (e.g.
# frontend/) would make the .chainlink/.git checks silently no-op and skip
# the warning entirely.
cd "${CLAUDE_PROJECT_DIR:-.}" || exit 0

[ -d .chainlink ] || exit 0
# Use -e (not -d): in git worktrees and submodules, `.git` is a file
# pointing at the real gitdir, not a directory. Both shapes are valid
# repos and both should get the dirty-file warning.
[ -e .git ] || exit 0

if ! git status --porcelain .chainlink/issues.jsonl 2>/dev/null | grep -q .; then
  exit 0
fi

# Emit the warning as JSON on stdout — Claude Code surfaces the top-level
# `systemMessage` field to the user in the terminal. Previously this went
# to stderr, where it was invisible in claude.ai cloud sessions (the very
# case where losing uncommitted state hurts most) and easy to miss even
# locally. The Stop hook contract is documented at
# https://code.claude.com/docs/en/hooks.
cat <<'JSON'
{"systemMessage": ".chainlink/issues.jsonl has uncommitted changes.\n\nChainlink issue updates live in this file; if the session ends without committing them, the updates are lost when the sandbox tears down (less catastrophic on a local Mac session — the file stays on disk — but still worth committing before context rolls over).\n\nTo commit and push:\n  git add .chainlink/issues.jsonl\n  git commit -m 'chore(chainlink): update issues'\n  git push"}
JSON

exit 0
