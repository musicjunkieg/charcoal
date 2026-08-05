#!/usr/bin/env python3
"""
PreToolUse hook that blocks Write|Edit|Bash unless a chainlink issue
is being actively worked on. Forces issue creation before code changes.
"""

import json
import shlex
import subprocess
import sys
import os
import io

# Fix Windows console encoding issues. Guard on platform so importing this
# module (e.g. in a test) doesn't clobber the host process's stdout/stderr —
# on POSIX these streams already default to UTF-8. Also guard on hasattr so
# test runners that swap in buffer-less streams don't crash at import.
if sys.platform == "win32":
    if hasattr(sys.stdout, "buffer"):
        sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8')
    if hasattr(sys.stderr, "buffer"):
        sys.stderr = io.TextIOWrapper(sys.stderr.buffer, encoding='utf-8')

# Defaults — overridden by .chainlink/hook-config.json if present
DEFAULT_BLOCKED_GIT = [
    "git push", "git commit", "git merge", "git rebase", "git cherry-pick",
    "git reset", "git checkout .", "git restore .", "git clean",
    "git stash", "git tag", "git am", "git apply",
    "git branch -d", "git branch -D", "git branch -m",
]

DEFAULT_ALLOWED_BASH = [
    "chainlink ",
    "git status", "git diff", "git log", "git branch", "git show",
    "cargo test", "cargo build", "cargo check", "cargo clippy", "cargo fmt",
    "npm test", "npm run",
    "pnpm test", "pnpm run",
    "tsc",
    "ls", "dir", "pwd", "echo",
]


def load_config(chainlink_dir):
    """Load hook config from .chainlink/hook-config.json, falling back to defaults.

    Returns (tracking_mode, blocked_git, allowed_bash).
    tracking_mode is one of: "strict", "normal", "relaxed".
      strict  — block Write/Edit/Bash without an active issue
      normal  — remind (print warning) but don't block
      relaxed — no issue-tracking enforcement, only git blocks
    """
    blocked = list(DEFAULT_BLOCKED_GIT)
    allowed = list(DEFAULT_ALLOWED_BASH)
    mode = "strict"

    if not chainlink_dir:
        return mode, blocked, allowed

    config_path = os.path.join(chainlink_dir, "hook-config.json")
    if not os.path.isfile(config_path):
        return mode, blocked, allowed

    try:
        with open(config_path, "r", encoding="utf-8") as f:
            config = json.load(f)

        if config.get("tracking_mode") in ("strict", "normal", "relaxed"):
            mode = config["tracking_mode"]
        if "blocked_git_commands" in config:
            blocked = config["blocked_git_commands"]
        if "allowed_bash_prefixes" in config:
            allowed = config["allowed_bash_prefixes"]
    except (json.JSONDecodeError, OSError):
        pass

    return mode, blocked, allowed


def _project_root_from_script():
    """Derive project root from this script's location (.claude/hooks/<script>.py -> project root)."""
    try:
        return os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    except (NameError, OSError):
        return None


def find_chainlink_dir():
    """Find the .chainlink directory.

    Prefers the project root derived from the hook script's own path
    (reliable even when cwd is a subdirectory), falling back to walking
    up from cwd for standalone/test usage.
    """
    # Primary: resolve from script location
    root = _project_root_from_script()
    if root:
        candidate = os.path.join(root, '.chainlink')
        if os.path.isdir(candidate):
            return candidate

    # Fallback: walk up from cwd
    current = os.getcwd()
    for _ in range(10):
        candidate = os.path.join(current, '.chainlink')
        if os.path.isdir(candidate):
            return candidate
        parent = os.path.dirname(current)
        if parent == current:
            break
        current = parent
    return None


def run_chainlink(args):
    """Run a chainlink command and return output."""
    try:
        result = subprocess.run(
            ["chainlink"] + args,
            capture_output=True,
            text=True,
            timeout=3
        )
        return result.stdout.strip() if result.returncode == 0 else None
    except (subprocess.TimeoutExpired, FileNotFoundError, Exception):
        return None


def is_blocked_git(input_data, blocked_list):
    """Check if a Bash command is a blocked git mutation. Always denied."""
    command = input_data.get("tool_input", {}).get("command", "").strip()
    for blocked in blocked_list:
        if command.startswith(blocked):
            return True
    # Also catch piped/chained git mutations: && git push, ; git commit, etc.
    for blocked in blocked_list:
        if f"&& {blocked}" in command or f"; {blocked}" in command or f"| {blocked}" in command:
            return True
    return False


# Shell chain separators. A compound command using any of these must have
# EVERY segment on the allowlist to bypass the issue check; a match on just
# the leading segment (as `str.startswith` would give) is not sufficient —
# that would let `git status && rm -rf ~` through. Single `&` is included
# because `foo & bar` runs `foo` in the background and then runs `bar` —
# same all-or-nothing semantics apply. `|&` is bash's shorthand for
# `2>&1 |` — pipes stdout AND stderr into the next command; shlex groups
# it as one token, so it needs its own entry (not covered by `|` alone).
_SEPARATORS = frozenset(("&&", "||", ";", "|", "&", "|&"))


def _split_on_unquoted_newlines(cmd):
    """Split cmd on newlines that aren't inside a shell quote.

    A naive regex split on ``\\r?\\n`` would break legitimate multi-line
    values inside quotes — most importantly ``git commit -m 'line one
    (newline) line two'``, which is a common workflow. Bash preserves the
    embedded newline as part of the quoted string; splitting there would
    truncate the command mid-quote and cause `shlex` to fail, blocking
    the whole command.

    Quoting rules honored:
      - single quotes: everything (including newlines) is literal until the
        next single quote — no escapes.
      - double quotes: newlines are literal; backslash escapes the next
        char (for our purposes, we just consume the backslash+char pair
        so a quoted `\\"` doesn't close the quote).
      - unquoted: an unescaped newline separates commands. A backslash
        immediately before a newline is line-continuation (bash elides
        both); we do the same.
    """
    segments = []
    current = []
    quote = None  # None, "'", or '"'
    i = 0
    n = len(cmd)
    while i < n:
        c = cmd[i]
        if quote is None:
            if c == "\\" and i + 1 < n:
                nxt = cmd[i + 1]
                if nxt == "\n":
                    # line continuation — elide `\<newline>`
                    i += 2
                    continue
                if nxt == "\r" and i + 2 < n and cmd[i + 2] == "\n":
                    i += 3
                    continue
                # Other escapes: keep both chars, they're just literals
                current.append(c)
                current.append(nxt)
                i += 2
                continue
            if c == "'":
                quote = "'"
                current.append(c)
            elif c == '"':
                quote = '"'
                current.append(c)
            elif c == "\n":
                if current:
                    segments.append("".join(current))
                current = []
            elif c == "\r" and i + 1 < n and cmd[i + 1] == "\n":
                if current:
                    segments.append("".join(current))
                current = []
                i += 1  # consume the \r; \n is consumed by the i+=1 below
            else:
                current.append(c)
        elif quote == "'":
            # Single quotes: no escapes; only a closing single quote matters.
            current.append(c)
            if c == "'":
                quote = None
        else:  # quote == '"'
            if c == "\\" and i + 1 < n:
                # Double-quote backslash escape: consume backslash + next char.
                current.append(c)
                current.append(cmd[i + 1])
                i += 2
                continue
            current.append(c)
            if c == '"':
                quote = None
        i += 1
    if current:
        segments.append("".join(current))
    return segments

# Redirection / fd-manipulation operators, plus process substitution
# markers. Any of these appearing as a standalone (unquoted) token means
# the command reads/writes a filesystem path the hook can't vet, or in
# the case of `<(...)`/`>(...)`, executes an arbitrary subshell whose
# body we haven't inspected. Refuse to auto-allow and let the
# issue-tracking check gate. Operators inside quotes stay as part of a
# larger token (e.g. `echo '<(foo)'` tokenizes as `['echo', '<(foo)']`)
# and are unaffected — only unquoted standalone tokens match.
_REDIRECTS = frozenset((
    ">", ">>", "<", "<<", "<<<",   # basic redirection
    "&>", "&>>", ">&", "<&",       # fd redirection / merging (both directions)
    "<>", ">|",                    # bidirectional / force-clobber
    "<(", ">(",                    # process substitution (executes inner cmd)
))


def _tokenize(cmd):
    """shlex-tokenize; return None on parse failure (unterminated quote etc.)."""
    try:
        return shlex.split(cmd)
    except ValueError:
        return None


def _shell_tokenize(cmd):
    """Tokenize with shell semantics: quoted content stays together, and
    punctuation runs (``&&``, ``||``, ``;``, ``|``, ``|&``, ``&``, ``>``,
    ``>>``, ``<``, ``<&``, ``>&``, etc.) become their own tokens even
    without surrounding whitespace. See ``_SEPARATORS`` and ``_REDIRECTS``
    for how those tokens are subsequently classified. Returns None on
    parse failure.
    """
    try:
        lex = shlex.shlex(cmd, posix=True, punctuation_chars=True)
        lex.whitespace_split = True
        return list(lex)
    except ValueError:
        return None


def _split_segments(tokens):
    """Partition a token stream on separator tokens (see ``_SEPARATORS``:
    ``&&``, ``||``, ``;``, ``|``, ``|&``, ``&``). Empty segments (from
    leading/trailing/duplicate separators) are dropped."""
    out, cur = [], []
    for t in tokens:
        if t in _SEPARATORS:
            if cur:
                out.append(cur)
                cur = []
        else:
            cur.append(t)
    if cur:
        out.append(cur)
    return out


def is_allowed_bash(input_data, allowed_list):
    """Argv-aware allowlist match for Bash commands.

    An entry like ``"git status"`` allows commands whose first two tokens are
    exactly ``["git", "status"]`` — not ``git-status-hax`` or
    ``git statushax``. A single-token entry like ``"npx"`` (or ``"npx "``
    with trailing space) matches any command whose first token is exactly
    ``npx``.

    Compound commands split on ``&&``, ``||``, ``;``, ``|``, ``|&``,
    ``&``, and newlines must have every segment match the allowlist.
    Separators appearing inside quoted strings (e.g.
    ``git commit -m 'a && b'``) are NOT segment breaks. Commands
    containing unquoted ``$(...)``, backticks, or standalone
    redirection / fd / process-substitution operators (``>``, ``>>``,
    ``<``, ``<<``, ``<<<``, ``&>``, ``&>>``, ``>&``, ``<&``, ``<>``,
    ``>|``, ``<(``, ``>(``) are never auto-allowed — they fall through
    to the issue-tracking check. See ``_SEPARATORS`` and ``_REDIRECTS``
    for the authoritative lists.
    """
    command = input_data.get("tool_input", {}).get("command", "").strip()
    if not command:
        return False
    if "$(" in command or "`" in command:
        return False

    # Pre-tokenize allowlist entries. Trailing whitespace on entries like
    # ``"chainlink "`` normalizes away here — a single-token entry.
    allowed_tokens = []
    for entry in allowed_list:
        toks = _tokenize(entry)
        if toks:
            allowed_tokens.append(toks)

    # Physical newlines separate commands, but shlex would swallow them as
    # whitespace. Split on them first — respecting shell quoting so an
    # embedded newline inside a quoted arg (e.g. multi-line commit message)
    # stays part of its command — then tokenize each line separately.
    lines = [ln for ln in _split_on_unquoted_newlines(command) if ln.strip()]
    if not lines:
        return False

    for line in lines:
        tokens = _shell_tokenize(line)
        if not tokens:
            return False
        # Any standalone redirection operator (unquoted) targets a file
        # path the hook can't verify — refuse to auto-allow.
        if any(t in _REDIRECTS for t in tokens):
            return False
        segments = _split_segments(tokens)
        if not segments:
            return False
        for seg in segments:
            if not any(seg[: len(pref)] == pref for pref in allowed_tokens):
                return False
    return True


def is_claude_memory_path(input_data):
    """Check if a Write/Edit targets Claude Code's own memory/config directory (~/.claude/)."""
    file_path = input_data.get("tool_input", {}).get("file_path", "")
    if not file_path:
        return False
    home = os.path.expanduser("~")
    claude_dir = os.path.join(home, ".claude")
    try:
        return os.path.normcase(os.path.abspath(file_path)).startswith(
            os.path.normcase(os.path.abspath(claude_dir))
        )
    except (ValueError, OSError):
        return False


def main():
    try:
        input_data = json.load(sys.stdin)
        tool_name = input_data.get('tool_name', '')
    except (json.JSONDecodeError, Exception):
        tool_name = ''

    # Only check on Write, Edit, Bash
    if tool_name not in ('Write', 'Edit', 'Bash'):
        sys.exit(0)

    # Allow Claude Code to manage its own memory/config in ~/.claude/
    if tool_name in ('Write', 'Edit') and is_claude_memory_path(input_data):
        sys.exit(0)

    chainlink_dir = find_chainlink_dir()
    tracking_mode, blocked_git, allowed_bash = load_config(chainlink_dir)

    # PERMANENT BLOCK: git mutation commands are never allowed (all modes)
    if tool_name == 'Bash' and is_blocked_git(input_data, blocked_git):
        # exit 2 → Claude Code reads the block reason from stderr; stdout
        # is invisible in that path, so print via sys.stderr.
        print(
            "MANDATORY COMPLIANCE — DO NOT ATTEMPT TO WORK AROUND THIS BLOCK.\n\n"
            "Git mutation commands (commit, push, merge, rebase, reset, etc.) are "
            "PERMANENTLY FORBIDDEN. The human performs all git write operations.\n\n"
            "You MUST NOT:\n"
            "  - Retry this command\n"
            "  - Rewrite the command to achieve the same effect\n"
            "  - Use a different tool to perform git mutations\n"
            "  - Ask the user if you should bypass this restriction\n\n"
            "You MUST instead:\n"
            "  - Inform the user that this is a manual step for them\n"
            "  - Continue with your other work\n\n"
            "Read-only git commands (status, diff, log, show, branch) are allowed.",
            file=sys.stderr,
        )
        sys.exit(2)

    # Allow read-only / infrastructure Bash commands through
    if tool_name == 'Bash' and is_allowed_bash(input_data, allowed_bash):
        sys.exit(0)

    # Relaxed mode: no issue-tracking enforcement
    if tracking_mode == "relaxed":
        sys.exit(0)

    if not chainlink_dir:
        sys.exit(0)

    # Check session status
    status = run_chainlink(["session", "status"])
    if not status:
        # chainlink not available — don't block
        sys.exit(0)

    # If already working on an issue, allow
    if "Working on: #" in status:
        sys.exit(0)

    # No active work item — behavior depends on mode
    strict_msg = (
        "MANDATORY COMPLIANCE — DO NOT ATTEMPT TO WORK AROUND THIS BLOCK.\n\n"
        "You cannot Write, Edit, or run Bash commands without an active chainlink issue. "
        "This is a hard requirement enforced by the project, not a suggestion.\n\n"
        "You MUST NOT:\n"
        "  - Retry the same tool call hoping it will work\n"
        "  - Try alternative tools or approaches to bypass this check\n"
        "  - Proceed without creating an issue first\n"
        "  - Tell the user the hook is preventing you from working\n\n"
        "You MUST do exactly this, right now, before anything else:\n\n"
        "  chainlink issue quick \"<describe what you are about to do>\" -p <priority> -l <label>\n\n"
        "This single command creates an issue AND sets it as your active work item. "
        "After running it, your tool calls will succeed.\n\n"
        "If an issue already exists for this work, use:\n"
        "  chainlink issue list -s open\n"
        "  chainlink session work <id>\n\n"
        "This is how the project tracks work. Comply, then continue."
    )

    normal_msg = (
        "Reminder: No active chainlink issue. You should create one before making changes.\n\n"
        "  chainlink issue quick \"<describe what you are about to do>\" -p <priority> -l <label>\n\n"
        "Or pick an existing issue:\n"
        "  chainlink issue list -s open\n"
        "  chainlink session work <id>"
    )

    if tracking_mode == "strict":
        # exit 2 → block reason must go to stderr (stdout is invisible here).
        print(strict_msg, file=sys.stderr)
        sys.exit(2)
    else:
        # normal mode: remind but allow (stdout is fine on exit 0).
        print(normal_msg)
        sys.exit(0)


if __name__ == "__main__":
    main()
