# Chainlink Hook CWD Resolution Bug

## Problem

Chainlink hooks configured in `.claude/settings.json` use relative paths:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "command": "python3 .claude/hooks/work-check.py"
      }
    ]
  }
}
```

These paths resolve relative to the Bash tool's **current working directory**, not the project root. If any Bash command changes the CWD (e.g., `cd web && npm run build`), the CWD persists across subsequent Bash calls. The next time the hook fires, it resolves `.claude/hooks/work-check.py` from the new CWD — which doesn't contain a `.claude/` directory — and fails with "No such file or directory."

This is a silent, confusing failure. The hook blocks all subsequent Bash tool calls until the user manually resets the CWD.

## Reproduction

1. Configure a PreToolUse hook with a relative path (default Chainlink setup)
2. Run any Bash command that changes CWD: `cd subdir && some_command`
3. Run any subsequent Bash command
4. Hook fails: `can't open file 'subdir/.claude/hooks/work-check.py': No such file or directory`

## Root Cause

Claude Code's Bash tool persists the working directory between invocations, but the hook runner does not normalize paths before invoking hook commands. The hook command is passed directly to the shell, inheriting whatever CWD the Bash tool currently has.

## Proposed Fix

The hook runner should resolve relative paths in hook commands against the **project root** (the directory containing `.claude/`), not the shell's CWD.

### Option A: Resolve in the hook runner (preferred)

When the hook runner encounters a relative path in a hook command, prepend the project root:

```
# Before executing a hook command:
# 1. Detect project root (walk up from initial CWD looking for .claude/)
# 2. If the command contains a relative path to a file under .claude/, resolve it against project root
# 3. Execute the resolved command
```

This is transparent to users — existing configs continue to work regardless of CWD.

### Option B: Resolve in the hook script itself

Each hook script can resolve its own project root:

```python
#!/usr/bin/env python3
import os

# Resolve project root from this script's location
# .claude/hooks/work-check.py → project root is ../../
script_dir = os.path.dirname(os.path.abspath(__file__))
project_root = os.path.dirname(os.path.dirname(script_dir))
os.chdir(project_root)

# ... rest of the hook
```

This works but requires every hook script to include boilerplate, and doesn't help hooks that are simple one-liners.

### Option C: Document the limitation

Add a note to the hook documentation that CWD may not be the project root, and recommend using absolute paths or the `__file__`-based resolution pattern.

## Recommendation

Option A is the cleanest fix. The hook runner already knows the project root (it found `.claude/settings.json` there). It should use that root when resolving hook command paths, similar to how `git` resolves hook paths relative to `.git/` regardless of CWD.

If Option A is too invasive, Option B should be applied to the default hook templates that Chainlink generates, so new projects get the fix automatically.

## Impact

This affects any project where:
- Chainlink hooks use relative paths (the default)
- The AI agent runs Bash commands that change CWD (common in monorepos, projects with frontend subdirectories, or any multi-step build process)

The workaround is to manually reset CWD or use absolute paths in settings.json, but both are fragile and user-hostile.
