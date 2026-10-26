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

if ! security find-generic-password -s charcoal-runpod-billing >/dev/null 2>&1; then
  echo "No Keychain item 'charcoal-runpod-billing'. Add the key first:" >&2
  echo "  security add-generic-password -s charcoal-runpod-billing -a runpod -w" >&2
  exit 1
fi

ENV_BLOCK=""
if [ -n "$TOPIC" ]; then
  ENV_BLOCK="  <key>EnvironmentVariables</key>
  <dict><key>CHARCOAL_ALERT_NTFY_TOPIC</key><string>$TOPIC</string></dict>"
fi

mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
cat > "$PLIST" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array><string>$PY</string><string>$SCRIPT</string></array>
  <key>StartCalendarInterval</key>
  <dict><key>Hour</key><integer>9</integer><key>Minute</key><integer>0</integer></dict>
  <key>StandardOutPath</key><string>$HOME/Library/Logs/charcoal-runpod-balance.out</string>
  <key>StandardErrorPath</key><string>$HOME/Library/Logs/charcoal-runpod-balance.out</string>
$ENV_BLOCK
</dict>
</plist>
PLIST_EOF

launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"
echo "Installed $LABEL (daily 9:00 AM). Running once now:"
launchctl kickstart "gui/$(id -u)/$LABEL"
sleep 5
tail -1 "$HOME/Library/Logs/charcoal-runpod-balance.log" 2>/dev/null || echo "(no log line yet — check ~/Library/Logs/charcoal-runpod-balance.out)"
