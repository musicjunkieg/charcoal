#!/bin/bash
# Install the daily RunPod balance check as a launchd job (#411).
#
#   tools/runpod-balance/install.sh [ntfy-topic]
#
# Runs check_balance.py every day at 9:00 AM Mac local time (PT). If the Mac
# is asleep then, launchd runs it on wake. Pass an ntfy.sh topic to also get
# a phone push; omit it for macOS notifications only.
#
# Prerequisite: the full-access RunPod key in the Keychain (prompts for the
# key, so it never lands in shell history):
#   security add-generic-password -s charcoal-runpod-billing -a runpod -w
set -euo pipefail

LABEL="watch.charcoal.runpod-balance"
HERE="$(cd "$(dirname "$0")" && pwd)"
SCRIPT="$HERE/check_balance.py"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
TOPIC="${1:-}"
PY="$(command -v python3)"

# ntfy accepts letters, digits, '_' and '-', up to 64 chars. Reject anything
# else now: a bad topic would fail every phone push while the Mac
# notification still "delivers", so nothing would flag it.
if [ -n "$TOPIC" ] && ! [[ "$TOPIC" =~ ^[A-Za-z0-9_-]{1,64}$ ]]; then
  echo "Invalid ntfy topic '$TOPIC': use only letters, digits, '_' or '-' (max 64)." >&2
  exit 1
fi

if ! security find-generic-password -s charcoal-runpod-billing >/dev/null 2>&1; then
  echo "No Keychain item 'charcoal-runpod-billing'. Add the key first:" >&2
  echo "  security add-generic-password -s charcoal-runpod-billing -a runpod -w" >&2
  exit 1
fi

mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"

# plistlib writes valid, escaped XML whatever the paths or topic contain.
"$PY" - "$PLIST" "$LABEL" "$PY" "$SCRIPT" "$HOME/Library/Logs/charcoal-runpod-balance.out" "$TOPIC" <<'PY_EOF'
import os, plistlib, sys
plist, label, py, script, out, topic = sys.argv[1:7]
job = {
    "Label": label,
    "ProgramArguments": [py, script],
    "StartCalendarInterval": {"Hour": 9, "Minute": 0},
    "StandardOutPath": out,
    "StandardErrorPath": out,
}
# launchd does not inherit this shell's environment, so a threshold override
# exported before install would apply to the first check below but not to the
# daily job. Copy any that are set into the job.
env = {}
if topic:
    env["CHARCOAL_ALERT_NTFY_TOPIC"] = topic
for name in ("CHARCOAL_MIN_BALANCE_USD", "CHARCOAL_MAX_DAILY_SPEND_USD"):
    if os.environ.get(name):  # skip unset and empty (NAME=) alike
        env[name] = os.environ[name]
if env:
    job["EnvironmentVariables"] = env
with open(plist, "wb") as f:
    plistlib.dump(job, f)
PY_EOF
plutil -lint "$PLIST" >/dev/null

launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"
echo "Installed $LABEL (daily 9:00 AM). Running the first check now:"

# Run it synchronously (not via kickstart) so the result shown is THIS run's.
set +e
CHARCOAL_ALERT_NTFY_TOPIC="$TOPIC" "$PY" "$SCRIPT"
rc=$?
set -e
case $rc in
  0) echo "First check OK." ;;
  1) echo "First check raised an alert (see above)." ;;
  *) echo "First check FAILED (exit $rc) — fix this before relying on the job." >&2; exit $rc ;;
esac
