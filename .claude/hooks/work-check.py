#!/usr/bin/env python3
"""
PreToolUse hook that blocks Write|Edit|Bash unless a chainlink issue
is being actively worked on. Forces issue creation before code changes.
"""

import json
import re
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

# Git commands that lose work or rewrite history. Blocked in every project:
# hook-config.json's blocked_git_commands adds to these and can't remove
# them, so the block message's "permanently" stays true.
CORE_BLOCKED_GIT = [
    "git reset", "git rebase", "git clean",
    "git checkout .", "git restore .", "git branch -D",
    "git push --force", "git push -f",
    # Skipping hooks: they're what stops commits, merges and pushes on main.
    "git commit --no-verify", "git commit -n",
    "git merge --no-verify", "git push --no-verify",
    # Pointing core.hooksPath elsewhere (git -c, git config, GIT_CONFIG_*)
    # disables every hook; matched by _disables_hooks, not as a prefix.
    "git config core.hooksPath",
]
HOOKS_PATH_BLOCK = "git config core.hooksPath"
# Core blocks whose harm is skipping the hooks rather than losing work.
_HOOK_SKIP_BLOCKS = frozenset((
    "git commit --no-verify", "git commit -n", "git merge --no-verify",
    "git push --no-verify", HOOKS_PATH_BLOCK,
))

# Defaults — overridden by .chainlink/hook-config.json if present
# (blocked_git_commands is then added to CORE_BLOCKED_GIT, not replacing it).
# `git merge` isn't blocked: merging main into a feature branch is how a PR
# with conflicts in the generated docs exports (resolved by the keep-ours
# driver) gets back to mergeable, and the hooks already refuse commits and
# pushes on main (pre-commit, pre-merge-commit, pre-push).
DEFAULT_BLOCKED_GIT = [
    "git push", "git commit", "git rebase", "git cherry-pick",
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


def _is_str_list(value):
    return isinstance(value, list) and all(isinstance(v, str) for v in value)


def blocked_git_configured(chainlink_dir):
    """Whether the project's hook-config.json sets a valid
    blocked_git_commands (which then replaces DEFAULT_BLOCKED_GIT)."""
    if not chainlink_dir:
        return False
    try:
        with open(os.path.join(chainlink_dir, "hook-config.json"), "r", encoding="utf-8") as f:
            config = json.load(f)
    except (json.JSONDecodeError, OSError, UnicodeDecodeError):
        return False
    return isinstance(config, dict) and _is_str_list(config.get("blocked_git_commands"))


def load_config(chainlink_dir):
    """Load hook config from .chainlink/hook-config.json, falling back to defaults.

    Returns (tracking_mode, blocked_git, allowed_bash).
    tracking_mode is one of: "strict", "normal", "relaxed".
      strict  — block Write/Edit/Bash without an active issue
      normal  — remind (print warning) but don't block
      relaxed — no issue-tracking enforcement, only git blocks

    In a claude.ai cloud session (CLAUDE_CODE_REMOTE=true) the mode is
    always "relaxed": the sandbox tracker is a fresh, throwaway copy, so
    an issue created there would never reach the real tracker and could
    reuse an id already taken locally. Git blocks still apply.
    """
    blocked = list(DEFAULT_BLOCKED_GIT)
    allowed = list(DEFAULT_ALLOWED_BASH)
    mode = "strict"

    if chainlink_dir:
        config_path = os.path.join(chainlink_dir, "hook-config.json")
        try:
            with open(config_path, "r", encoding="utf-8") as f:
                config = json.load(f)

            if not isinstance(config, dict):
                config = {}
            if config.get("tracking_mode") in ("strict", "normal", "relaxed"):
                mode = config["tracking_mode"]
            # A malformed value (null, a string, a list with non-strings)
            # must not crash the hook, which would block every tool call.
            # Both fail closed: blocks fall back to the stricter defaults,
            # and the allowlist to nothing (every command gets the issue
            # check) rather than to the permissive defaults.
            if _is_str_list(config.get("blocked_git_commands")):
                blocked = config["blocked_git_commands"]
            if "allowed_bash_prefixes" in config:
                value = config["allowed_bash_prefixes"]
                allowed = value if _is_str_list(value) else []
        except (json.JSONDecodeError, OSError, UnicodeDecodeError):
            pass

    if is_cloud_session():
        mode = "relaxed"

    # Core blocks first, then the project's, without duplicates.
    blocked = list(dict.fromkeys(CORE_BLOCKED_GIT + list(blocked)))
    return mode, blocked, allowed


def is_cloud_session():
    """True inside a claude.ai cloud session, which sets CLAUDE_CODE_REMOTE."""
    return os.environ.get("CLAUDE_CODE_REMOTE") == "true"


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


# git's global options that take a separate value (`git -C dir reset`).
_GIT_VALUE_OPTIONS = frozenset((
    "-C", "-c", "--git-dir", "--work-tree", "--namespace",
    "--config-env", "--super-prefix", "--exec-path",
))
# Words that can run a command without being it (`env git reset`), with
# their options that take a separate value (`env -u NAME git …`).
_COMMAND_WRAPPERS = {
    "env": frozenset(("-u", "--unset", "-C", "--chdir", "-S", "--split-string")),
    "command": frozenset(),
    "exec": frozenset(("-a",)),
    "nohup": frozenset(),
    "time": frozenset(("-f", "--format", "-o", "--output")),
    "builtin": frozenset(),
    "xargs": frozenset(("-I", "-i", "-n", "-L", "-l", "-P", "-d", "-E", "-e",
                        "-s", "-a", "--arg-file", "--delimiter", "--max-args",
                        "--max-lines", "--max-procs", "--max-chars")),
    "sudo": frozenset(("-u", "--user", "-g", "--group", "-h", "--host",
                       "-p", "--prompt", "-C", "--close-from", "-D", "--chdir",
                       "-r", "--role", "-t", "--type", "-U", "--other-user")),
}
_SUBSHELL_OPENERS = frozenset(("(", "{", "!"))


def _git_invocations(command):
    """Each git invocation in a shell command as (subcommand, args), found
    in every line and separator-delimited segment, after any leading
    `VAR=value` assignments, wrappers like `env`, subshell openers and git
    global options (`-C dir`, `--git-dir=…`, `--no-pager`). Commands hidden
    in `$(…)`, backticks or `bash -c '…'` are not seen."""
    return _git_invocations_parsed(command)[0]


def _strip_shell_comment(line):
    """`line` without a bash comment: from an unquoted `#` that starts a
    word to the end. A word starts at the beginning of the line or after
    unquoted, unescaped whitespace or one of ;&|() — an escaped space
    (`x\\ #`) doesn't start one. Inside `$'…'` a backslash escapes."""
    quote = None
    word_start = True
    i = 0
    while i < len(line):
        c = line[i]
        if quote:
            if c == "\\" and quote in ('"', "$'"):
                i += 2
                continue
            if c == quote[-1]:
                quote = None
            i += 1
            continue
        if c == "\\":
            word_start = False
            i += 2
            continue
        if c == "$" and line.startswith("$'", i):
            quote = "$'"
            word_start = False
            i += 2
            continue
        if c in "'\"":
            quote = c
            word_start = False
        elif c == "#" and word_start:
            # A comment runs to the end of its physical line only.
            nl = line.find("\n", i)
            return line[:i] if nl == -1 else line[:i] + _strip_shell_comment(line[nl:])
        else:
            word_start = c.isspace() or c in ";&|()"
        i += 1
    return line


def _git_invocations_parsed(command):
    """_git_invocations plus whether every line tokenized. A line shlex
    can't parse (e.g. an apostrophe in a trailing `# don't` comment, which
    bash ignores but shlex sees as an open quote) falls back to splitting
    on whitespace, so its git commands are still found."""
    found = []
    clean = True
    for line in _split_on_unquoted_newlines(command):
        tokens = _shell_tokenize(line)
        token_lists = [tokens] if tokens is not None else []
        if tokens is None:
            clean = False
            # Read it two ways and keep what either finds, so a wrong guess
            # in one can't hide a git command (fail closed):
            #  - without a trailing `# comment`, as bash would, re-tokenized;
            #  - the whole line split on whitespace and separators, with
            #    quote and backslash characters dropped, so `'git' reset`
            #    or `g\it reset` still read as `git reset`.
            stripped = _shell_tokenize(_strip_shell_comment(line))
            if stripped is not None:
                token_lists.append(stripped)
            token_lists.append([
                t for t in (re.sub(r"[\"'\\]", "", w) for w in
                            re.findall(r"&&|\|\||\|&|[;|&()]|[^\s;|&()]+", line))
                if t
            ])
        for seg in (seg for toks in token_lists for seg in _split_segments(toks)):
            i = 0
            while i < len(seg):
                word = seg[i]
                if word in _SUBSHELL_OPENERS or re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", word):
                    i += 1
                elif word in _REDIRECTS or (
                    word.isdigit() and i + 1 < len(seg) and seg[i + 1] in _REDIRECTS
                ):
                    # `>/dev/null git …`, `2>&1 git …`: skip the redirection
                    # (fd number, operator, target) before the command.
                    i += 1 if word in _REDIRECTS else 2
                    i += 1  # the redirection's target
                elif word in _COMMAND_WRAPPERS:
                    # Skip the wrapper and its own options, up to `--`.
                    takes_value = _COMMAND_WRAPPERS[word]
                    i += 1
                    while i < len(seg) and seg[i].startswith("-") and seg[i] != "-":
                        if seg[i] == "--":
                            i += 1
                            break
                        i += 2 if seg[i] in takes_value else 1
                else:
                    break
            if i >= len(seg) or os.path.basename(seg[i]) != "git":
                continue
            i += 1
            while i < len(seg) and seg[i].startswith("-"):
                i += 2 if seg[i] in _GIT_VALUE_OPTIONS else 1
            if i < len(seg):
                args = [a for a in seg[i + 1:] if a not in (")", "}")]
                found.append((seg[i], args))
    return found, clean


def _is_force_push(args):
    """Whether `git push` args overwrite remote history: --force (any
    form), --mirror, a short-flag cluster with f (`-fu`), or a `+refspec`."""
    for a in args:
        # --force-if-includes alone is a no-op (it only refines a lease).
        if a in ("--force", "--mirror") or a.startswith("--force-with-lease"):
            return True
        if _long_prefix(a, "--force") or _long_prefix(a, "--force-with-lease") or _long_prefix(a, "--mirror"):
            return True
        if a.startswith("-") and not a.startswith("--") and "f" in a[1:]:
            return True
        if a.startswith("+") and len(a) > 1:
            return True
    return False


# Short options that take a value, per subcommand: in a cluster like
# `-am"msg"`, everything after one of these is its value, not more flags.
_SHORT_VALUE_OPTIONS = {"commit": set("mFcCtSu")}


def _long_prefix(arg, option):
    """Whether `arg` is an abbreviation git could expand to `option`
    (`--no-ve` for `--no-verify`). Any prefix counts, down to `--` + one
    character: if it's ambiguous, git refuses it anyway."""
    return (arg.startswith("--") and "=" not in arg and len(arg) >= 3
            and option.startswith(arg))


def _cluster_has(arg, flag, sub):
    """Whether a short-option cluster (`-Df`, `-anm`) sets `flag`. Reading
    stops at a value-taking option, so `-m"notes"` doesn't set -n."""
    if not arg.startswith("-") or arg.startswith("--"):
        return False
    takes_value = _SHORT_VALUE_OPTIONS.get(sub, set())
    for ch in arg[1:]:
        if ch == flag:
            return True
        if ch in takes_value or not ch.isalpha():
            return False
    return False


def _invocation_hits(pattern, invocations):
    """Whether a blocked entry like "git branch -D" matches any invocation:
    same subcommand, and every further word of the entry among its args
    (in any position; a short flag also inside a cluster like `-Df`)."""
    words = pattern.split()
    if len(words) < 2 or words[0] != "git":
        return False
    sub, required = words[1], words[2:]
    for inv_sub, args in invocations:
        if inv_sub != sub:
            continue
        if sub == "push" and required and required[0] in ("--force", "-f"):
            if _is_force_push(args):
                return True
            continue
        def has(word):
            if word in args:
                return True
            # git takes an unambiguous prefix of a long option (--no-ve).
            if word.startswith("--") and any(_long_prefix(a, word) for a in args):
                return True
            if len(word) == 2 and word[0] == "-" and word[1] != "-":
                return any(_cluster_has(a, word[1], sub) for a in args)
            return False
        if all(has(w) for w in required):
            return True
    return False


# git commit's short flags that can share a cluster with -m (`-am`), and
# alone: none opens an editor or reads config that could run the message.
_COMMIT_CLUSTER_FLAGS = set("avsq")  # not n: --no-verify is core-blocked
# The exact long flags allowed beside an exempt message. Git also takes
# abbreviations (`--ed` for `--edit`), so anything not listed exactly —
# including any abbreviation — means the whole command is scanned.
_COMMIT_SAFE_LONG_FLAGS = frozenset((
    "--all", "--quiet", "--signoff", "--verbose",
    "--no-edit", "--allow-empty", "--amend", "--",
))


def _message_flag(word):
    """'next' if `word` is a git commit flag taking the next word as the
    message (-m, --message, a cluster like -am), 'self' if it carries the
    message itself (-mTEXT, --message=TEXT), else None."""
    if word in ("-m", "--message"):
        return "next"
    if word.startswith("--message="):
        return "self"
    if word.startswith("-") and not word.startswith("--") and len(word) > 2:
        # A cluster: allowed flags, then m, then (optionally) the message
        # attached: -am, -mTEXT, -amTEXT, -sam"TEXT".
        flags = word[1:]
        m = flags.find("m")
        if m != -1 and set(flags[:m]) <= _COMMIT_CLUSTER_FLAGS:
            return "next" if m == len(flags) - 1 else "self"
    return None


def _runs_nothing(text):
    """Whether this (already unquoted) message can't expand into anything:
    no `$` at all (parameter, arithmetic, `$(`…) and no backtick."""
    return "$" not in text and "`" not in text


def _scan_text(command):
    """The command as text for the coarse scan, minus a git commit message
    bash won't execute: `git commit -m 'never git reset'` mentions a core
    command but runs only `git commit`.

    Only when the whole command is one plain `git commit`: a single line
    and segment, `git commit` as its first two words (no git options
    between them: `-c core.editor=sh` could run the message), no
    redirection, no `-e`/`--edit` (an editor gets the text), and a
    message with no `$` or backtick. Anything else — a pipe or
    redirection that could feed the text to a shell, another command,
    `find -exec`, `xargs`, `sh -c` — is scanned whole."""
    # Bash's $'…' and $"…" quoting has rules shlex doesn't follow (`\'`
    # inside $'…'), so word boundaries can't be trusted: no exemption.
    if "$'" in command or '$"' in command:
        return command
    lines = [ln for ln in _split_on_unquoted_newlines(command) if ln.strip()]
    if len(lines) != 1:
        return command
    tokens = _shell_tokenize(lines[0])
    if not tokens or any(t in _SEPARATORS or t in _REDIRECTS for t in tokens):
        return command
    if len(tokens) < 2 or os.path.basename(tokens[0]) != "git" or tokens[1] != "commit":
        return command
    # Every word after `git commit` must be a message flag (with its
    # message), an allowed flag, or a path; otherwise scan it all.
    drop = set()
    n = 2
    while n < len(tokens):
        t = tokens[n]
        kind = _message_flag(t)
        if kind == "next" and n + 1 < len(tokens):
            if _runs_nothing(tokens[n + 1]):
                drop.add(n + 1)
            n += 2
            continue
        if kind == "self":
            if _runs_nothing(t):
                drop.add(n)
        elif t.startswith("--"):
            if t not in _COMMIT_SAFE_LONG_FLAGS:
                return command
        elif t.startswith("-") and len(t) > 1:
            if not set(t[1:]) <= _COMMIT_CLUSTER_FLAGS:
                return command
        n += 1
    return " ".join(w for i, w in enumerate(tokens) if i not in drop)


def _coarse_git_invocations(command):
    """Every `git` word in the command, with what follows it up to a
    separator, after dropping quotes, backslashes, brackets and `$(`. A
    blunt reading run on every command (fail closed): git inside a string,
    a substitution, `sh -c`, a pipe into a shell, `find -exec`, `timeout`…
    is found whatever runs it. The cost is that a command merely
    mentioning a core command (`grep 'git reset' docs/`) is blocked too,
    except in a git commit message (see _scan_text)."""
    # `!` too: a git alias runs `!git …` as a shell command.
    text = re.sub(r"\$\(|[`'\"\\(){}<>!]", " ", command)
    found = []
    for seg in re.split(r"&&|\|\||[;&|\n]", text):
        words = seg.split()
        for j, w in enumerate(words):
            if os.path.basename(w) != "git":
                continue
            k = j + 1
            while k < len(words) and words[k].startswith("-"):
                k += 2 if words[k] in _GIT_VALUE_OPTIONS else 1
            if k < len(words):
                found.append((words[k], words[k + 1:]))
    return found


def _shell_words_text(text):
    """`text` with quotes, backslashes and `$(` removed, the way bash would
    join `'core.hooks''Path'` or `core.hooks\\Path` into one word."""
    return re.sub(r"\$\(|[`'\"\\()]", "", text)


def _disables_hooks(command):
    """Whether a command could turn git hooks off, read fail-closed on the
    quote-normalized text (minus an exempt commit message): any mention of
    core.hooksPath, git config includes (include.path, includeIf.*.path),
    git config set through GIT_CONFIG* environment variables, or .git/hooks
    itself (deleting or replacing a hook)."""
    words = _shell_words_text(_scan_text(command))
    text = words.lower()
    paths = re.sub(r"/(\./)+", "/", re.sub(r"/{2,}", "/", text))
    has_git = re.search(r"(^|[\s;&|()!])(\S*/)?git(\s|$)", text)
    git_config = re.search(r"(^|\s)(-c|--config-env)(\s|=)|\bgit\s+config\b", words)
    return bool(
        "hookspath" in text
        or "include.path" in text
        or "includeif." in text
        or re.search(r"\bgit_config\w*\s*=", text)
        or ".git/hooks" in paths
        or "--git-path hooks" in paths
        # Where git reads config or the repo from, set inline.
        or (has_git and re.search(
            r"\b(home|xdg_config_home|git_dir|git_common_dir|git_config_nosystem)\s*=", text))
        # A variable or substitution can spell out a config key the text
        # never shows: with git config in play, any $ or backtick blocks,
        # as does one in git's own option or subcommand position.
        or (git_config and re.search(r"[$`]", command))
        or _expansion_before_subcommand(command)
    )


def _expansion_before_subcommand(command):
    """Whether any git invocation has a $ or backtick among git's own
    options or in its subcommand position (`git "$OPT" commit`), where it
    could become -c core.hooksPath=…. After the subcommand is fine
    (`git checkout "$BRANCH"`). An untokenizable line counts as yes."""
    for line in _split_on_unquoted_newlines(command):
        tokens = _shell_tokenize(line)
        if tokens is None:
            if re.search(r"\bgit\b", line) and re.search(r"[$`]", line):
                return True
            continue
        for seg in _split_segments(tokens):
            for i, word in enumerate(seg):
                if os.path.basename(word) != "git":
                    continue
                j = i + 1
                while j < len(seg) and seg[j].startswith("-"):
                    if re.search(r"[$`]", seg[j]):
                        return True
                    if seg[j] in _GIT_VALUE_OPTIONS and j + 1 < len(seg):
                        if re.search(r"[$`]", seg[j + 1]):
                            return True
                        j += 2
                    else:
                        j += 1
                if j < len(seg) and re.search(r"[$`]", seg[j]):
                    return True
    return False


# git subcommands that only read, for the merge-word backstop.
_READ_ONLY_GIT = frozenset((
    "log", "diff", "show", "status", "grep", "blame", "shortlog",
    "describe", "rev-parse", "ls-files", "ls-tree", "cat-file", "reflog",
))


# Anything that can run a git merge somewhere other than the cwd, whose
# branch is the one checked: changing directory, pointing git at another
# repo, or a nested shell/wrapper the parser can't follow.
_OTHER_CHECKOUT = re.compile(
    r"(^|[\s;&|(])(cd|pushd|xargs|eval)(\s|$)"
    r"|\bGIT_(DIR|WORK_TREE)\s*="
    r"|(^|\s)(-C|--git-dir|--work-tree|--chdir)"
    r"|\b(sh|bash|zsh|dash|ksh|fish)\s+(-\w+\s+)*-\w*c\b"
    r"|\s-exec(dir)?\s"
)


def merge_on_main(input_data, current_branch):
    """Whether this command may run `git merge` on main. Merging main into
    a feature branch is allowed; merging into main (even a fast-forward,
    which no hook sees) is not. current_branch is the cwd's, so a merge
    that changes directory or targets another repo (cd, pushd, -C,
    --git-dir, GIT_DIR) or runs where the branch is unknown fails closed.
    `git pull` isn't counted: updating main from upstream is normal."""
    command = input_data.get("tool_input", {}).get("command", "").strip()
    # Scan the text minus an exempt commit message: a message may say
    # "git merge origin/main" without merging anything.
    scanned = _scan_text(command)
    coarse = sum(1 for sub, _ in _coarse_git_invocations(scanned) if sub == "merge")
    parsed = sum(1 for sub, _ in _git_invocations(command) if sub == "merge")
    plain = _plain_merge_count(command)
    # Backstop for forms neither parser reads: the word "merge" after a
    # "git" anywhere (commit messages aside) counts as one more merge,
    # unless every git command found is read-only, so "merge" is just an
    # argument (git log --grep=merge, git diff -- merge.py).
    # Brackets become spaces here (not deleted), so `q(git)` stays a word.
    words = re.sub(r"[()]", " ", re.sub(r"\$\(|[`'\"\\]", " ", _scan_text(command)))
    coarse_invocations = _coarse_git_invocations(scanned)
    subs = {sub for sub, _ in _git_invocations(command) + coarse_invocations}
    # Exempt only when every "git" word is one of those read-only commands:
    # an unaccounted-for git (perl -e 'exec q(git), q(merge)') keeps it.
    explained = (bool(subs) and subs <= _READ_ONLY_GIT
                 and len(re.findall(r"\bgit\b", words)) == len(coarse_invocations)
                 and _git_invocations_parsed(command)[1]
                 and not re.search(r"[$`]|\beval\b|\b(sh|bash|zsh|dash|ksh)\s+(-\w+\s+)*-\w*c\b",
                                   command))
    mentioned = 1 if not explained and re.search(r"\bgit\b.*\bmerge\b", words, re.S) else 0
    if not (coarse or parsed or plain or mentioned):
        return False
    return (current_branch in ("main", "")
            # Every merge either parser sees must be a plain one: its
            # segment's own command, with no git global options (-c
            # core.worktree=…, -C, --git-dir can retarget it). Anything else
            # (nice, an interpreter's -c, an alias, globals) fails closed.
            or max(coarse, parsed, mentioned) > plain
            # A branch switch earlier in the command means the merge lands
            # on a branch other than the one checked (`git checkout main &&
            # git merge --ff-only feat`).
            or bool({sub for sub, _ in _git_invocations(command) + _coarse_git_invocations(command)}
                    & {"checkout", "switch"})
            or bool(_OTHER_CHECKOUT.search(_shell_words_text(command))))


def _plain_merge_count(command):
    """`git merge` invocations that are their segment's own command (after
    any VAR=value) with no git global options between `git` and `merge`:
    the only ones whose target checkout is surely the cwd. Lines that don't
    tokenize count none, so the caller fails closed."""
    count = 0
    for line in _split_on_unquoted_newlines(command):
        tokens = _shell_tokenize(line)
        if tokens is None:
            continue
        for seg in _split_segments(tokens):
            i = 0
            while i < len(seg) and re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", seg[i]):
                i += 1
            if i + 1 < len(seg) and os.path.basename(seg[i]) == "git" and seg[i + 1] == "merge":
                count += 1
    return count


def _git_target_dirs(command):
    """The directories a command's git invocations will run in, following
    `cd`/`pushd` and `git -C` step by step from the cwd. None when a target
    can't be worked out (--git-dir/--work-tree, GIT_DIR/GIT_WORK_TREE, a $ or
    ~ in a path, eval, a nested shell, a line that doesn't tokenize)."""
    # Text checks read the command minus an exempt commit message, which
    # can say "eval" or "GIT_DIR=" without running anything.
    scanned = _scan_text(command)
    if re.search(r"\beval\b|\b(sh|bash|zsh|dash|ksh)\s+(-\w+\s+)*-\w*c\b", scanned):
        return None
    # Repo or config set by assignment anywhere (export, declare, env, …).
    if re.search(r"\bGIT_(DIR|WORK_TREE|CONFIG\w*)\s*=", scanned):
        return None
    cwd = "."
    dirs = []
    for line in _split_on_unquoted_newlines(command):
        tokens = _shell_tokenize(line)
        if tokens is None:
            return None
        for seg in _split_segments(tokens):
            i = 0
            while i < len(seg) and re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", seg[i]):
                if re.match(r"^GIT_(DIR|WORK_TREE)=", seg[i]):
                    return None
                i += 1
            if i >= len(seg):
                continue
            # Only two segment shapes are understood: a plain `cd DIR` /
            # `pushd DIR`, and one whose first word is git. A directory change
            # anywhere else (`(cd`, `{ cd`, `then cd`, popd) or git behind any
            # wrapper (env -C, sudo -D, xargs, find -exec, nice…) fails closed.
            if any(w in ("cd", "pushd", "popd") for w in seg[i + 1:]):
                return None
            if seg[i] in ("env", "builtin", "command", "exec", "popd"):
                return None
            if (seg[i] not in ("cd", "pushd") and os.path.basename(seg[i]) != "git"
                    and any(os.path.basename(w) == "git" for w in seg[i + 1:])):
                return None
            if seg[i] in ("cd", "pushd"):
                if (i + 1 >= len(seg) or seg[i + 1].startswith("-")
                        or re.search(r"[$`~]", seg[i + 1])):
                    return None
                cwd = os.path.join(cwd, seg[i + 1])
                continue
            here = cwd
            for j, word in enumerate(seg):
                if os.path.basename(word) != "git":
                    continue
                k = j + 1
                while k < len(seg) and seg[k].startswith("-"):
                    opt = seg[k]
                    if opt in ("--git-dir", "--work-tree") or opt.startswith(("--git-dir=", "--work-tree=")):
                        return None
                    if opt == "-C" and k + 1 < len(seg):
                        if re.search(r"[$`~]", seg[k + 1]):
                            return None
                        here = os.path.join(here, seg[k + 1])
                    k += 2 if opt in _GIT_VALUE_OPTIONS else 1
            dirs.append(here)
    return dirs or ["."]


def _hooks_path_set(dirs=(".",)):
    """Whether core.hooksPath is set (any config scope) for any of these
    directories: with it set, git ignores .git/hooks entirely. Fails closed
    when a target can't be worked out (dirs is None) or git can't answer."""
    if dirs is None:
        return True
    for d in dict.fromkeys(dirs):
        try:
            out = subprocess.run(
                ["git", "-C", d, "config", "--get", "core.hooksPath"],
                capture_output=True, text=True, timeout=5,
            )
        except (OSError, subprocess.SubprocessError):
            return True  # can't tell: fail closed
        if out.returncode != 1:  # 0: set; anything else: an error
            return True
    return False


def _current_branch():
    try:
        out = subprocess.run(
            ["git", "branch", "--show-current"],
            capture_output=True, text=True, timeout=5,
        )
        return out.stdout.strip() if out.returncode == 0 else ""
    except (OSError, subprocess.SubprocessError):
        return ""


def blocked_git_match(input_data, blocked_list):
    """The entry of blocked_list a Bash command hits, or None. Entries are
    tried in order (core blocks come first; see load_config). Each matches
    on the literal command text, as before, or on any git invocation in it,
    by subcommand and flags in any position — so `git -C . reset`,
    `git push origin main --force` or `env git rebase` don't slip past a
    prefix match."""
    return blocked_git_hit(input_data, blocked_list)[0]


def blocked_git_hit(input_data, blocked_list):
    """(entry hit, mention_only): mention_only when only the coarse scan
    found it — the command may just mention the git command (in a grep
    pattern, an echo) rather than run it. (None, False) if nothing hit."""
    command = input_data.get("tool_input", {}).get("command", "").strip()
    invocations, clean = _git_invocations_parsed(command)
    # Text to scan: the command minus commit messages bash won't run; an
    # unparseable line stays as written (see _scan_text).
    scan = _scan_text(command)
    coarse = _coarse_git_invocations(scan)
    if _disables_hooks(command):
        return HOOKS_PATH_BLOCK, False
    # Entries in order, so core blocks (first) win over the project's.
    for blocked in blocked_list:
        # The word-boundary refinement in _text_hits only applies when the
        # command parsed cleanly; otherwise keep the plain prefix match.
        if _text_hits(blocked, scan, word_boundary=clean) or _invocation_hits(blocked, invocations):
            return blocked, False
        if _invocation_hits(blocked, coarse):
            return blocked, True
    return None, False


def _text_hits(blocked, command, word_boundary=True):
    """The literal check: `blocked` at the start of the command or after
    `&& `, `; ` or `| `. An entry ending in a word character must end at a
    word boundary, so "git push --force" doesn't match
    "git push --force-if-includes"; one ending otherwise ("git checkout .")
    still matches as a prefix ("git checkout ./src")."""
    if not blocked:
        return False
    starts = [0] + [m.end() for m in re.finditer(r"(?:&&|;|\|)\s*", command)]
    for start in starts:
        if not command.startswith(blocked, start):
            continue
        end = start + len(blocked)
        if not word_boundary or not (blocked[-1].isalnum() or blocked[-1] == "_"):
            return True
        if end == len(command) or not (command[end].isalnum() or command[end] in "-_"):
            return True
    return False


def block_reason(hit, configured, mention_only=False):
    """Why `hit` is blocked: a core block (permanent, every project), the
    project's own blocked_git_commands, or the template default. With
    mention_only, the command may only mention it — say that instead."""
    if mention_only:
        return (
            f"This command mentions `{hit}` in a form the hook can't tell "
            "apart from running it (inside a string, a substitution, or as "
            "an argument to another program), so it's blocked to be safe. "
            "Git commit messages are exempt. If the command only mentions "
            "it (a search, an echo), ask the human to run it.\n\n"
        )
    if hit in _HOOK_SKIP_BLOCKS:
        return (
            f"`{hit}` skips the git hooks that keep commits, merges and pushes "
            "off main, so it is PERMANENTLY FORBIDDEN in every project; no "
            "project config can lift it. The human runs it if it's needed.\n\n"
        )
    if hit in CORE_BLOCKED_GIT:
        return (
            f"`{hit}` loses work or rewrites history, so it is PERMANENTLY "
            "FORBIDDEN in every project; no project config can lift it. "
            "The human runs it if it's needed.\n\n"
        )
    if configured:
        return (
            f"`{hit}` is blocked by this project's policy "
            "(`blocked_git_commands` in .chainlink/hook-config.json). "
            "The human performs this git operation.\n\n"
        )
    return (
        f"`{hit}` is blocked by the template's default git policy (this "
        "project sets no `blocked_git_commands` in .chainlink/hook-config.json, "
        "which is where to change it). The human performs this git operation.\n\n"
    )


def is_blocked_git(input_data, blocked_list):
    """Check if a Bash command is a blocked git mutation. Always denied."""
    return blocked_git_match(input_data, blocked_list) is not None


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
        # shlex strips "#..." as a comment by default, but bash only treats
        # "#" as a comment at the start of a word — "status#x" is one word.
        # Left enabled, the tokenizer sees a truncated command while bash
        # would execute the full line ("git status#x || rm -rf target"
        # reduces to "git status" and sails through the allowlist). Parse
        # "#" as ordinary text; any resulting mismatch fails conservative
        # (refuse to auto-allow) instead of permissive.
        lex.commenters = ""
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

    # Git blocks apply in all modes. Core blocks (work-losing / history-
    # rewriting) hold in every project; the rest are this project's policy.
    if tool_name == 'Bash' and merge_on_main(input_data, _current_branch()):
        print(
            "This git merge may land on main: the checkout is on main, its "
            "branch can't be determined, or the command merges in another "
            "checkout (cd, pushd, -C, --git-dir, GIT_DIR). Merging into main "
            "locally isn't allowed; main changes only through merged PRs. To "
            "bring main into your work, run `git merge origin/main` from your "
            "feature branch's own checkout.",
            file=sys.stderr,
        )
        sys.exit(2)

    hit, mention_only = (blocked_git_hit(input_data, blocked_git)
                         if tool_name == 'Bash' else (None, False))
    # Checked at runtime, not from the text: if core.hooksPath is already set
    # (any scope, however it got there), the hooks aren't running, so refuse
    # any git command that writes until a human puts it back.
    if not hit and tool_name == 'Bash':
        command = input_data.get("tool_input", {}).get("command", "")
        subs = {sub for sub, _ in _git_invocations(command) + _coarse_git_invocations(command)}
        if (subs & {"commit", "merge", "push", "rebase", "cherry-pick", "am"}
                and _hooks_path_set(_git_target_dirs(command))):
            hit, mention_only = HOOKS_PATH_BLOCK, False
    if hit:
        why = block_reason(hit, configured=blocked_git_configured(chainlink_dir),
                           mention_only=mention_only)
        # exit 2 → Claude Code reads the block reason from stderr; stdout
        # is invisible in that path, so print via sys.stderr.
        print(
            "MANDATORY COMPLIANCE — DO NOT ATTEMPT TO WORK AROUND THIS BLOCK.\n\n"
            + why +
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
        "  - Proceed without creating an issue first\n\n"
        "If the user told you not to create chainlink issues, stop and tell "
        "them this hook requires one.\n\n"
        "Otherwise, do exactly this, right now, before anything else:\n\n"
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
