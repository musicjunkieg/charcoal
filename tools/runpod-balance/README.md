# RunPod balance check (#411)

A once-a-day check, run by launchd on the Mac mini, that warns before the
prepaid RunPod balance runs out and flags a runaway early.

RunPod auto-pay is **off**, so the balance is the hard ceiling on GPU spend.
RunPod's own low-balance email ($25) covers the same ground; this is the
second line, and it also catches a *spike* while the balance still looks fine.

## What it checks

| Alert | Default | Override |
|---|---|---|
| Balance below | $25 | `CHARCOAL_MIN_BALANCE_USD` |
| Yesterday's spend over | $10 (~3× the $100/month pace) | `CHARCOAL_MAX_DAILY_SPEND_USD` |

"Yesterday" is the last complete **UTC** day in RunPod's billing, which ends
at 5 PM PT (4 PM in winter).

If the check itself fails — key rejected, network down, a malformed reply —
it notifies too. Silence means healthy **only while notifications work**: if
every channel fails, the run exits 2 and the only trace is the log. Glance at
the log now and then; a missing or `CHECK FAILED` line is the tell.

**Known expiry:** the live balance comes from RunPod's GraphQL API, which
RunPod plans to retire in early 2027; REST v2 has no balance route yet. When
GraphQL goes away the check fails loudly (it never reads as $0). Move the
balance read to whatever replaces it then; the daily-spend half already
uses REST v2.

## Install

1. Put the full-access RunPod key in the Keychain (it prompts; nothing goes
   in shell history). **Never put this key in Railway** — the prod key there
   is endpoint-only on purpose (#406).

   ```
   security add-generic-password -s charcoal-runpod-billing -a runpod -w
   ```

2. Install the job (optionally with an [ntfy.sh](https://ntfy.sh) topic for
   phone pushes — pick an unguessable name and subscribe to it in the ntfy app):

   ```
   tools/runpod-balance/install.sh            # macOS notifications only
   tools/runpod-balance/install.sh <topic>    # plus phone push
   ```

It runs daily at 9:00 AM PT (or on wake, if the Mac was asleep) and once
immediately on install.

To change a threshold, export it when you run the installer
(`CHARCOAL_MIN_BALANCE_USD=40 tools/runpod-balance/install.sh <topic>`); the
installer copies it into the daily job and prints the overrides in effect.
Re-running the installer keeps any override you don't set again, so you can
change one threshold at a time. To go back to the defaults, uninstall (below)
and install again.

## Reading it

Every run appends one line to `~/Library/Logs/charcoal-runpod-balance.log`:

```
2026-10-26T09:00:02-07:00 balance=$58.00 yesterday(2026-10-25)=$0.41 now=$0.00/hr ok
```

Run by hand: `python3 tools/runpod-balance/check_balance.py`.
Tests: `python3 tools/runpod-balance/test_check_balance.py`.

## Uninstall

```
launchctl bootout gui/$(id -u)/watch.charcoal.runpod-balance
rm ~/Library/LaunchAgents/watch.charcoal.runpod-balance.plist
```
