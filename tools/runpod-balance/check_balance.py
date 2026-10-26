#!/usr/bin/env python3
"""Daily RunPod balance and spend check, run from the Mac mini (#411).

Why this exists: RunPod auto-pay is OFF, so the prepaid balance is the hard
ceiling on any runaway GPU spend. This check warns before that ceiling is hit,
and catches a runaway early — the 2026-10-02 leak billed every day for 16 days
before anyone noticed.

Why on the Mac and not in the app: reading the balance needs a full-access
RunPod key. Railway is where secrets leaked on 2026-10-21, so the prod key
there is deliberately endpoint-only. This key lives only in the macOS Keychain.

It alerts when:
  - the balance is below $25 (CHARCOAL_MIN_BALANCE_USD), or
  - yesterday's spend was over $10 (CHARCOAL_MAX_DAILY_SPEND_USD) — about 3x
    the $100/month budget pace.

Every run appends one line to ~/Library/Logs/charcoal-runpod-balance.log.
Alerts go to a macOS notification and, if CHARCOAL_ALERT_NTFY_TOPIC is set,
to that ntfy.sh topic as a phone push.

Stdlib only. Exit status: 0 ok, 1 alert raised, 2 the check itself failed
(which also notifies: a silent failure would look like a healthy account).
"""

import json
import os
import subprocess
import sys
import urllib.request
from datetime import datetime, timezone

GRAPHQL_URL = "https://api.runpod.io/graphql"
BILLING_URL = "https://api.runpod.io/v2/billing?bucketSize=day&lastN=2"
KEYCHAIN_SERVICE = "charcoal-runpod-billing"
LOG_PATH = os.path.expanduser("~/Library/Logs/charcoal-runpod-balance.log")
DEFAULT_MIN_BALANCE = 25.0
DEFAULT_MAX_DAILY_SPEND = 10.0
TIMEOUT_SECS = 30


class CheckError(Exception):
    """The check could not produce a trustworthy reading."""


def parse_myself(body):
    """GraphQL `myself` response -> (balance_usd, spend_per_hr_usd)."""
    if body.get("errors"):
        raise CheckError(f"RunPod GraphQL error: {body['errors'][0].get('message', body['errors'])}")
    me = (body.get("data") or {}).get("myself")
    if not me or me.get("clientBalance") is None:
        raise CheckError("RunPod returned no balance")
    return float(me["clientBalance"]), float(me.get("currentSpendPerHr") or 0.0)


def parse_yesterday_spend(body):
    """`/v2/billing?bucketSize=day&lastN=2` -> (yesterday's UTC date, total USD).

    The last bucket is today, still in progress; the one before it is the
    most recent complete day.
    """
    records = body.get("records") or []
    if len(records) < 2:
        raise CheckError(f"expected 2 daily billing buckets, got {len(records)}")
    day = records[-2]
    return day["startTime"][:10], float(day.get("totalAmount") or 0.0)


def evaluate(balance, yesterday_spend, spend_per_hr,
             min_balance=DEFAULT_MIN_BALANCE, max_daily_spend=DEFAULT_MAX_DAILY_SPEND):
    """Pure decision: a list of alert messages (empty = healthy)."""
    alerts = []
    if balance < min_balance:
        alerts.append(
            f"RunPod balance is ${balance:.2f}, below ${min_balance:.2f}. "
            f"Top up, or check /admin for a runaway (now spending ${spend_per_hr:.2f}/hr)."
        )
    if yesterday_spend > max_daily_spend:
        alerts.append(
            f"RunPod billed ${yesterday_spend:.2f} yesterday, over ${max_daily_spend:.2f}. "
            "Possible runaway: compare with the /admin GPU cost section."
        )
    return alerts


def summary_line(balance, day, yesterday_spend, spend_per_hr):
    return (f"balance=${balance:.2f} yesterday({day})=${yesterday_spend:.2f} "
            f"now=${spend_per_hr:.2f}/hr")


def read_api_key():
    """Keychain first; RUNPOD_BILLING_API_KEY env as a fallback for manual runs."""
    try:
        out = subprocess.run(
            ["security", "find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"],
            capture_output=True, text=True, check=True, timeout=TIMEOUT_SECS,
        )
        key = out.stdout.strip()
        if key:
            return key
    except (subprocess.SubprocessError, FileNotFoundError):
        pass
    key = os.environ.get("RUNPOD_BILLING_API_KEY", "").strip()
    if not key:
        raise CheckError(f"no RunPod key in Keychain service '{KEYCHAIN_SERVICE}' or RUNPOD_BILLING_API_KEY")
    return key


def _request(url, key, payload=None):
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(url, data=data, headers={
        "Authorization": f"Bearer {key}",
        "Content-Type": "application/json",
        "User-Agent": "charcoal-balance-check/1",
    })
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT_SECS) as resp:
            return json.load(resp)
    except Exception as e:  # network, HTTP status, or bad JSON: all mean "no reading"
        raise CheckError(f"{url.split('?')[0]}: {e}") from e


def notify(title, message):
    """macOS notification, plus an ntfy.sh push if a topic is configured."""
    script = f"display notification {json.dumps(message)} with title {json.dumps(title)}"
    subprocess.run(["osascript", "-e", script], capture_output=True, check=False)
    topic = os.environ.get("CHARCOAL_ALERT_NTFY_TOPIC", "").strip()
    if topic:
        req = urllib.request.Request(f"https://ntfy.sh/{topic}", data=message.encode(),
                                     headers={"Title": title, "Priority": "high"})
        try:
            urllib.request.urlopen(req, timeout=TIMEOUT_SECS).close()
        except Exception as e:
            log(f"ntfy push failed: {e}")


def log(line):
    stamp = datetime.now(timezone.utc).astimezone().isoformat(timespec="seconds")
    os.makedirs(os.path.dirname(LOG_PATH), exist_ok=True)
    with open(LOG_PATH, "a", encoding="utf-8") as f:
        f.write(f"{stamp} {line}\n")
    print(f"{stamp} {line}")


def main():
    try:
        min_balance = float(os.environ.get("CHARCOAL_MIN_BALANCE_USD", DEFAULT_MIN_BALANCE))
        max_daily = float(os.environ.get("CHARCOAL_MAX_DAILY_SPEND_USD", DEFAULT_MAX_DAILY_SPEND))
        key = read_api_key()
        balance, per_hr = parse_myself(_request(
            GRAPHQL_URL, key, {"query": "query { myself { clientBalance currentSpendPerHr } }"}))
        day, spent = parse_yesterday_spend(_request(BILLING_URL, key))
    except (CheckError, ValueError) as e:
        log(f"CHECK FAILED: {e}")
        notify("Charcoal: RunPod check failed", str(e))
        return 2

    alerts = evaluate(balance, spent, per_hr, min_balance, max_daily)
    log(summary_line(balance, day, spent, per_hr) + (" ALERT" if alerts else " ok"))
    for a in alerts:
        notify("Charcoal: RunPod alert", a)
    return 1 if alerts else 0


if __name__ == "__main__":
    sys.exit(main())
