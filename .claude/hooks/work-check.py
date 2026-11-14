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
# Not a command: what blocks a commit/merge/push when the installed hooks
# are gone (see _hooks_missing).
HOOKS_MISSING = "hooks missing"
# Not a command: what blocks a commit/push from main in a cloud session.
CLOUD_ON_MAIN = "cloud commit on main"
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
    "nice": frozenset(("-n", "--adjustment")),
    "timeout": frozenset(("-s", "--signal", "-k", "--kill-after")),
    "ionice": frozenset(("-c", "--class", "-n", "--classdata", "-p", "--pid")),
    "stdbuf": frozenset(("-i", "--input", "-o", "--output", "-e", "--error")),
    "caffeinate": frozenset(("-t", "-w")),
    "xargs": frozenset(("-I", "-i", "-n", "-L", "-l", "-P", "-d", "-E", "-e",
                        "-s", "-a", "--arg-file", "--delimiter", "--max-args",
                        "--max-lines", "--max-procs", "--max-chars")),
    "sudo": frozenset(("-u", "--user", "-g", "--group", "-h", "--host",
                       "-p", "--prompt", "-C", "--close-from", "-D", "--chdir",
                       "-r", "--role", "-t", "--type", "-U", "--other-user")),
}
# Wrappers that take positional arguments before the command (`timeout 5 git …`).
_WRAPPER_POSITIONALS = {"timeout": 1}
# Words bash reads before a command in the same segment: subshell and group
# openers, negation, and the reserved words of if/while/until/for/case
# bodies (`then git reset`, `do git clean`).
_SUBSHELL_OPENERS = frozenset(("(", "{", "!", "if", "then", "else", "elif",
                               "do", "while", "until"))


def _git_invocations(command):
    """Each git invocation in a shell command as (subcommand, args). See
    _git_calls."""
    return [(sub, args) for _, _, sub, args in _git_calls(command)]


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


# Bash's ANSI-C quoting, $'…' (with \' inside), which shlex can't read.
_ANSI_C_STRING = re.compile(r"\$'(?:[^'\\]|\\.)*'")


def _command_lines(command):
    """The command's lines as token lists, the way bash reads them.
    Comments go first, quote-aware: quotes inside a `# comment` are just
    text to bash, but to shlex they'd pair up across lines and hide the
    commands between them. A line shlex still can't parse is retried with
    each $'…' string replaced by a placeholder; one that still won't parse
    is skipped."""
    out = []
    for line in _split_on_unquoted_newlines(_strip_shell_comment(command)):
        tokens = _shell_tokenize(line)
        if tokens is None:
            tokens = _shell_tokenize(_ANSI_C_STRING.sub("'_'", line))
        if tokens is not None:
            out.append(tokens)
    return out


# Builtins whose operands are assignments (`export GIT_DIR=…`).
_DECLARERS = frozenset(("export", "declare", "typeset", "local", "readonly"))


def _env_assignments(seg):
    """The `VAR=value` words in a segment that bash treats as assignments:
    leading ones (after subshell openers and reserved words), operands of
    export/declare/…, and those after `env` and its options. Not a commit
    message or grep pattern that merely looks like one."""
    assign = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")
    found = []
    i = 0
    while i < len(seg) and (seg[i] in _SUBSHELL_OPENERS or assign.match(seg[i])):
        if assign.match(seg[i]):
            found.append(seg[i])
        i += 1
    while i < len(seg) and seg[i] in ("builtin", "command"):
        i += 1
    if i < len(seg) and seg[i] in _DECLARERS:
        found.extend(w for w in seg[i + 1:] if assign.match(w))
    elif i < len(seg) and seg[i] == "env":
        i += 1
        while i < len(seg) and (seg[i].startswith("-") or assign.match(seg[i])):
            if assign.match(seg[i]):
                found.append(seg[i])
            i += 2 if seg[i] in _COMMAND_WRAPPERS["env"] else 1
    return found


def _exported(seg):
    """The assignments a segment makes for later commands: operands of
    export/declare/… (after any builtin/command), or a segment that is only
    assignments (`HOME=/x;` changes an already-exported HOME for every later
    command). Not ones leading a command (`HOME=/x npm test`)."""
    assign = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")
    if seg and all(assign.match(w) or w in _SUBSHELL_OPENERS for w in seg):
        return [w for w in seg if assign.match(w)]
    i = 0
    while i < len(seg) and (seg[i] in _SUBSHELL_OPENERS or seg[i] in ("builtin", "command")
                            or assign.match(seg[i])):
        i += 1
    if i < len(seg) and seg[i] in _DECLARERS:
        return [w for w in seg[i + 1:] if assign.match(w)]
    return []


def _git_calls(command):
    """Each git invocation bash would run directly, as (assignments, git
    options, subcommand, args): found in every line and separator-delimited
    segment, after any leading `VAR=value` assignments, wrappers like `env`
    or `nice`, redirections and subshell openers. Heredoc bodies are data,
    not commands. Git hidden in `$(…)`, backticks, `bash -c '…'`, an alias
    or another program isn't seen: auto mode and the runtime core.hooksPath
    check cover those. A line shlex can't parse is retried without a
    trailing `# comment` (which bash ignores), then skipped."""
    found = []
    for tokens in _command_lines(command):
        for seg in _split_segments(tokens):
            assigns = []
            i = 0
            while i < len(seg):
                word = seg[i]
                if word in _SUBSHELL_OPENERS:
                    i += 1
                elif re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", word):
                    assigns.append(word)
                    i += 1
                elif word in _REDIRECTS or (
                    word.isdigit() and i + 1 < len(seg) and seg[i + 1] in _REDIRECTS
                ):
                    # `>/dev/null git …`, `2>&1 git …`: skip the redirection
                    # (fd number, operator, target) before the command.
                    i += 1 if word in _REDIRECTS else 2
                    i += 1  # the redirection's target
                elif word in _COMMAND_WRAPPERS:
                    # Skip the wrapper and its own options, up to `--`, then
                    # any positionals it takes (timeout's duration).
                    takes_value = _COMMAND_WRAPPERS[word]
                    i += 1
                    while i < len(seg) and seg[i].startswith("-") and seg[i] != "-":
                        if seg[i] == "--":
                            i += 1
                            break
                        i += 2 if seg[i] in takes_value else 1
                    i += _WRAPPER_POSITIONALS.get(word, 0)
                else:
                    break
            if i >= len(seg) or os.path.basename(seg[i]) != "git":
                continue
            i += 1
            opts = []
            while i < len(seg) and seg[i].startswith("-"):
                step = 2 if seg[i] in _GIT_VALUE_OPTIONS else 1
                opts.extend(seg[i:i + step])
                i += step
            if i < len(seg):
                args = [a for a in seg[i + 1:] if a not in (")", "}")]
                found.append((assigns, opts, seg[i], args))
    return found


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
            # `git checkout ./`, `-- ./.`, `:/` (the repo root): all of `.`.
            if word == "." and any(
                    a in (":/", ":/.", ":(top)") or (not a.startswith("-") and os.path.normpath(a) == ".")
                    for a in args):
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


# Environment that points git at other config or another repo, which could
# turn hooksPath on for one command: on git itself, or exported for later.
_CONFIG_ENV = re.compile(r"^(GIT_CONFIG\w*|GIT_DIR|GIT_COMMON_DIR)=")
_GIT_OWN_ENV = re.compile(r"^(HOME|XDG_CONFIG_HOME)=")
# Config keys that switch hooks off or pull in another config file.
_HOOK_KEYS = ("hookspath", "include.path", "includeif.")
# --show-origin/--show-scope only decorate a read; with a key and a value
# git still writes, so they don't count.
_CONFIG_READS = frozenset(("--get", "--get-all", "--get-regexp", "--list", "-l",
                           "get", "list"))
# git config options that take the next word as their value.
_CONFIG_VALUE_OPTIONS = frozenset(("-f", "--file", "--blob", "--type", "--default",
                                   "--comment", "--value", "--url", "-t"))
_CONFIG_WRITES = frozenset(("set", "unset", "edit", "--unset", "--unset-all", "--add",
                            "--replace-all", "--edit", "-e", "--rename-section",
                            "--remove-section"))


def _disables_hooks(command):
    """Whether a git command here could turn git hooks off: core.hooksPath
    or a config include set with `-c`/`--config-env` or written with `git
    config`, GIT_CONFIG*/GIT_DIR (or HOME/XDG_CONFIG_HOME on git itself)
    pointing it at other config, or a $ or backtick among git's options or
    in its subcommand slot (`git $OPT commit`), which could become any of
    those. Read from the git commands bash runs, so a message or a grep that
    only mentions core.hooksPath isn't one. A hooksPath set some other way
    is caught at runtime (_hooks_path_set)."""
    calls = _git_calls(command)
    if not calls:
        return False
    for tokens in _command_lines(command):
        for seg in _split_segments(tokens):
            if any(_CONFIG_ENV.match(t) for t in _env_assignments(seg)):
                return True
            # `export HOME=…; git commit`: the later git reads that config.
            if any(_GIT_OWN_ENV.match(t) for t in _exported(seg)):
                return True
    for assigns, opts, sub, args in calls:
        if any(_CONFIG_ENV.match(a) or _GIT_OWN_ENV.match(a) for a in assigns):
            return True
        if re.search(r"[$`]", sub) or any(re.search(r"[$`]", o) for o in opts):
            return True
        for j, opt in enumerate(opts):
            value = None
            if opt in ("-c", "--config-env") and j + 1 < len(opts):
                value = opts[j + 1]
            elif opt.startswith("--config-env="):
                value = opt.split("=", 1)[1]
            if value is not None and any(k in value.lower() for k in _HOOK_KEYS):
                return True
        if sub == "config":
            # A read names the operation (`--get`, `--list`, or the `get`/
            # `list` subcommand first); a value that happens to say "list"
            # (`git config core.hooksPath list`) is a write.
            positional, k = [], 0
            while k < len(args):
                if args[k] in _CONFIG_VALUE_OPTIONS:
                    k += 2  # its value isn't the operation or the key
                    continue
                if not args[k].startswith("-"):
                    positional.append(args[k])
                k += 1
            reads = (any(a in _CONFIG_READS and a.startswith("-") for a in args)
                     or (positional[:1] and positional[0] in ("get", "list")))
            writes = (any(a in _CONFIG_WRITES for a in args)
                      or (not reads and len(positional) >= 2))
            # A key spelled through a variable (`git config "$K" /x`) could
            # be core.hooksPath: refuse any such write.
            touches = any(k in a.lower() for a in args for k in _HOOK_KEYS) or any(
                re.search(r"[$`]", a) for a in args)
            if touches and (writes or not reads):
                return True
    return False


def merge_on_main(input_data, current_branch):
    """Whether this command may run `git merge` on main. Merging main into
    a feature branch is allowed; merging into main (even a fast-forward,
    which no hook sees) is not. current_branch is the cwd's, so a merge
    that switches branch first, changes directory or targets another repo
    (cd, pushd, -C, --git-dir, GIT_DIR), comes with git options or a
    wrapper, or runs where the branch is unknown is refused too. Only
    merges bash runs directly count: a commit message or grep that says
    "git merge" isn't one. `git pull` isn't counted: updating main from
    upstream is normal."""
    command = input_data.get("tool_input", {}).get("command", "").strip()
    calls = _git_calls(command)
    merges = sum(1 for _, _, sub, _ in calls if sub == "merge")
    if not merges:
        return False
    dirs = _git_target_dirs(command)
    return (current_branch in ("main", "")
            # Every merge must be a plain one: its segment's own command,
            # with no git global options (-c core.worktree=…, -C,
            # --git-dir can retarget it) or wrapper.
            or merges > _plain_merge_count(command)
            # A branch switch in the same command means the merge lands on
            # a branch other than the one checked.
            or any(sub in ("checkout", "switch") for _, _, sub, _ in calls)
            or dirs is None
            or any(os.path.normpath(d) != "." for d in dirs))


def _plain_merge_count(command):
    """`git merge` invocations that are their segment's own command (after
    any VAR=value) with no git global options between `git` and `merge`:
    the only ones whose target checkout is surely the cwd."""
    count = 0
    for tokens in _command_lines(command):
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
    ~ in a path, git behind a wrapper)."""
    cwd = "."
    dirs = []
    for tokens in _command_lines(command):
        for seg in _split_segments(tokens):
            # Repo or config set by assignment (leading, export, env, …).
            if any(re.match(r"^GIT_(DIR|WORK_TREE|CONFIG\w*)=", w) for w in _env_assignments(seg)):
                return None
            i = 0
            while i < len(seg) and re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", seg[i]):
                i += 1
            if i >= len(seg):
                continue
            # Only two segment shapes are understood: a plain `cd DIR` /
            # `pushd DIR`, and one whose first word is git. A directory change
            # anywhere else (`(cd`, `{ cd`, `then cd`, popd) or git behind any
            # wrapper (env -C, sudo -D, xargs, find -exec, nice…) fails closed.
            if any(w in ("cd", "pushd", "popd") for w in seg[i + 1:]):
                return None
            # eval/source run text in this shell, so a `cd` in it moves
            # every git command after it.
            if seg[i] in ("env", "builtin", "command", "exec", "popd", "eval", "source", "."):
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


def _hooks_missing(dirs=(".",), hook_names=("pre-commit",)):
    """Whether any of these repos uses the template's hooks
    (scripts/install-hooks.sh at its top level) but lacks one of these
    hooks, installed and executable, however it went missing. A cloud session
    never installs them, so it isn't asked. Unknown targets (dirs is None)
    are left to _hooks_path_set, which already refuses them."""
    if dirs is None or is_cloud_session():
        return False
    for d in dict.fromkeys(dirs):
        try:
            out = subprocess.run(
                ["git", "-C", d, "rev-parse", "--path-format=absolute",
                 "--show-toplevel", "--git-common-dir"],
                capture_output=True, text=True, timeout=5,
            )
        except (OSError, subprocess.SubprocessError):
            continue
        lines = out.stdout.split("\n")
        if out.returncode != 0 or len(lines) < 2:
            continue
        top, common = lines[0], lines[1]
        if not os.path.isfile(os.path.join(top, "scripts", "install-hooks.sh")):
            continue
        for name in hook_names:
            hook = os.path.join(common, "hooks", name)
            if not (os.path.isfile(hook) and os.access(hook, os.X_OK)):
                return True
    return False


def _switch_target(sub, args):
    """The branch a `git switch`/`git checkout` leaves the checkout on, or
    None: the new branch with -c/-C/-b/-B (`switch -c topic main` is on
    topic), nothing for a file checkout (`checkout main -- README.md`),
    else the first non-option word."""
    if sub not in ("checkout", "switch"):
        return None
    if "--" in args:
        k = args.index("--")
        if sub == "switch":
            # For switch, `--` only ends the options: `switch -- main`.
            args = args[:k] + args[k + 1:]
        elif args[k + 1:]:
            return None  # `checkout BRANCH -- FILE`: a file checkout, no switch
        else:
            args = args[:k]  # `checkout main --` still switches
    creates = ("-c", "-C", "--create", "--force-create", "-b", "-B", "--orphan")
    for i, a in enumerate(args):
        if a in creates and i + 1 < len(args):
            return args[i + 1]
        for opt in ("--create=", "--force-create=", "--orphan="):
            if a.startswith(opt):
                return a[len(opt):]
    # `--track origin/main` (no -b/-c) creates and switches to `main`.
    for i, a in enumerate(args):
        if a in ("-t", "--track") and i + 1 < len(args):
            return args[i + 1].split("/", 1)[-1]
    words = [a for a in args if not a.startswith("-")]
    return words[0] if words else None


def _pushes_to_main(command):
    """Whether a git push names main as a destination: `main`, `HEAD:main`,
    `feat:main`, `refs/heads/main` (with or without a leading `+`)."""
    for sub, args in _git_invocations(command):
        if sub != "push":
            continue
        refspecs = [a for a in args if not a.startswith("-")][1:]  # after the remote
        for ref in refspecs:
            dest = ref.lstrip("+").split(":")[-1]
            if dest in ("main", "refs/heads/main"):
                return True
    return False


def _current_branch(directory="."):
    try:
        out = subprocess.run(
            ["git", "-C", directory, "branch", "--show-current"],
            capture_output=True, text=True, timeout=5,
        )
        return out.stdout.strip() if out.returncode == 0 else ""
    except (OSError, subprocess.SubprocessError):
        return ""


def blocked_git_match(input_data, blocked_list):
    """The entry of blocked_list a Bash command hits, or None. Each matches
    any git invocation by subcommand and flags in any position, so
    `git -C . reset`, `git push origin main --force` or `env git rebase`
    don't slip past a prefix match."""
    return blocked_git_hit(input_data, blocked_list)


def blocked_git_hit(input_data, blocked_list):
    """The entry of blocked_list a Bash command runs, or None: hooks turned
    off (see _disables_hooks) first, then the entries in order, so core
    blocks (first; see load_config) win over the project's. Only git
    commands bash runs directly count (see _git_calls)."""
    command = input_data.get("tool_input", {}).get("command", "").strip()
    if _disables_hooks(command):
        return HOOKS_PATH_BLOCK
    invocations = _git_invocations(command)
    for blocked in blocked_list:
        # Hooks turned off are matched above, so `git config --get
        # core.hooksPath` (a read) isn't refused as a prefix match.
        if blocked != HOOKS_PATH_BLOCK and _invocation_hits(blocked, invocations):
            return blocked
    return None


def block_reason(hit, configured):
    """Why `hit` is blocked: a core block (permanent, every project), the
    project's own blocked_git_commands, or the template default."""
    if hit == CLOUD_ON_MAIN:
        return (
            "This cloud session's checkout is on main, and cloud sessions have no "
            "git hooks to keep commits and pushes off it. Create a branch first "
            "(`git switch -c <name>`), then commit and push that.\n\n"
        )
    if hit == HOOKS_MISSING:
        return (
            "This repo's git hooks aren't installed (.git/hooks/pre-commit, "
            "pre-push or pre-merge-commit is missing or not executable), so nothing keeps commits, merges and "
            "pushes off main. Reinstall them with `bash scripts/install-hooks.sh`, "
            "then retry.\n\n"
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


def _skip_heredoc_bodies(cmd, pos, line):
    """Where the next command starts after `line`, which ended just before
    `pos`: past the bodies of any heredocs `line` opens (`<<EOF`, `<<'EOF'`,
    `<<-EOF`, which strips leading tabs from the closing line), in order.
    A body with no closing line runs to the end of cmd, as in bash."""
    # A `<<` inside a comment or a quote opens nothing: bash runs the next
    # lines. So read the line with quotes kept (posix=False): a quoted "<<"
    # stays "'<<'" and doesn't count.
    try:
        lex = shlex.shlex(_strip_shell_comment(line), posix=False, punctuation_chars=True)
        lex.commenters = ""
        lex.whitespace_split = True
        tokens = [part for tok in lex for part in _split_operators(tok)]
    except ValueError:
        return pos
    delims = []
    for k, tok in enumerate(tokens[:-1]):
        if tok != "<<":
            continue
        # Inside `$(( … ))` or `(( … ))` it's a left shift, not a heredoc.
        # (Quoted text, kept with its quotes by this lexer, doesn't count.)
        before = "".join(t for t in tokens[:k] if t[:1] not in ("'", '"'))
        if before.count("((") > before.count("))"):
            continue
        d, strip_tabs = tokens[k + 1], False
        if d.startswith("-"):
            d, strip_tabs = d[1:], True
        d = re.sub(r"""["'\\]""", "", d)  # <<'EOF', <<"EOF", <<\EOF: delimiter EOF
        if d:
            delims.append((d, strip_tabs))
    for d, strip_tabs in delims:
        while pos < len(cmd):
            end = cmd.find("\n", pos)
            end = len(cmd) if end == -1 else end
            body_line = cmd[pos:end].rstrip("\r")
            pos = end + 1
            if (body_line.lstrip("\t") if strip_tabs else body_line) == d:
                break
    return min(pos, len(cmd))


def _split_on_unquoted_newlines(cmd):
    """Split cmd on newlines that aren't inside a shell quote.

    A heredoc's body (`<<EOF` … `EOF`) is input to its command, not lines
    bash runs, so it's left out: the line with the `<<` is kept, then the
    body is skipped up to the delimiter line.

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
                    i = _skip_heredoc_bodies(cmd, i + 1, segments[-1]) - 1
                current = []
            elif c == "\r" and i + 1 < n and cmd[i + 1] == "\n":
                if current:
                    segments.append("".join(current))
                    i = _skip_heredoc_bodies(cmd, i + 2, segments[-1]) - 2
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
        return [part for tok in lex for part in _split_operators(tok)]
    except ValueError:
        return None


# Shell operators, longest first, for splitting a run shlex keeps whole.
_OPERATORS = ("&>>", "<<<", "&&", "||", "|&", ">>", "<<", "&>", ">&", "<&", "<>",
              ">|", ";", "|", "&", "<", ">", "(", ")")


def _split_operators(tok):
    """shlex reads a run of operator characters as one token (`)&&`, `;(`);
    bash reads separate operators. Split such a run; leave other tokens."""
    if not tok or any(c not in "();<>|&" for c in tok) or tok in _OPERATORS:
        return [tok]
    parts, i = [], 0
    while i < len(tok):
        op = next((o for o in _OPERATORS if tok.startswith(o, i)), tok[i])
        parts.append(op)
        i += len(op)
    return parts


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

    hit = blocked_git_hit(input_data, blocked_git) if tool_name == 'Bash' else None
    # Checked at runtime, not from the text: if core.hooksPath is already set
    # (any scope, however it got there), the hooks aren't running, so refuse
    # any git command that writes until a human puts it back.
    if not hit and tool_name == 'Bash':
        command = input_data.get("tool_input", {}).get("command", "")
        subs = {sub for sub, _ in _git_invocations(command)}
        if subs & {"commit", "merge", "push", "rebase", "cherry-pick", "am"}:
            dirs = _git_target_dirs(command)
            if _hooks_path_set(dirs):
                hit = HOOKS_PATH_BLOCK
            elif _hooks_missing(dirs, ("pre-commit",)
                                + (("pre-push",) if "push" in subs else ())
                                + (("pre-merge-commit",) if "merge" in subs else ())):
                hit = HOOKS_MISSING
            # A cloud session never installs the hooks that keep commits and
            # pushes off main, so refuse them here when its checkout is on main.
            elif is_cloud_session() and (
                    (subs & {"commit", "push"}
                     and (any(_current_branch(d) == "main" for d in (dirs or ["."]))
                          # `git switch main && git commit`: on main by then.
                          or any(_switch_target(sub, args) == "main"
                                 for sub, args in _git_invocations(command))))
                    or _pushes_to_main(command)):
                hit = CLOUD_ON_MAIN
    if hit:
        why = block_reason(hit, configured=blocked_git_configured(chainlink_dir))
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
