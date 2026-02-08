#!/usr/bin/env python3
"""
PreToolUse hook that nudges when no active working issue is set.
Runs before Write|Edit|Bash to remind about issue tracking.
"""

import json
import subprocess
import sys
import os
import io

# Fix Windows encoding issues
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8')


def find_chainlink_dir():
    """Find the .chainlink directory by walking up from cwd."""
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


def main():
    try:
        input_data = json.load(sys.stdin)
        tool_name = input_data.get('tool_name', '')
    except (json.JSONDecodeError, Exception):
        tool_name = ''

    # Only check on Write, Edit, Bash
    if tool_name not in ('Write', 'Edit', 'Bash'):
        sys.exit(0)

    chainlink_dir = find_chainlink_dir()
    if not chainlink_dir:
        sys.exit(0)

    # Check session status
    status = run_chainlink(["session", "status"])
    if not status:
        sys.exit(0)

    # If already working on something, no nudge needed
    if "Working on: #" in status:
        sys.exit(0)

    # Check if there are open issues to work on
    open_issues = run_chainlink(["list", "-s", "open"])
    if not open_issues or "No issues found" in open_issues:
        # No open issues - might need to create one, but don't block
        sys.exit(0)

    # Soft nudge: working on nothing but there are open issues
    print("Reminder: No active working issue. Run `chainlink session work <id>` or `chainlink quick \"title\"` to track your work.")
    sys.exit(0)


if __name__ == "__main__":
    main()
