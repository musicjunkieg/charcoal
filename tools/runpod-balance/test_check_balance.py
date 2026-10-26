#!/usr/bin/env python3
"""Tests for check_balance.py (#411). Run: python3 tools/runpod-balance/test_check_balance.py"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import check_balance as cb  # noqa: E402


class ParseTests(unittest.TestCase):
    def test_parses_balance_and_hourly_spend(self):
        body = {"data": {"myself": {"clientBalance": 58.123, "currentSpendPerHr": 0.0}}}
        self.assertEqual(cb.parse_myself(body), (58.123, 0.0))

    def test_graphql_errors_raise_instead_of_reading_as_zero(self):
        # A rejected key must never look like an empty balance (or a full one).
        with self.assertRaises(cb.CheckError):
            cb.parse_myself({"errors": [{"message": "Unauthorized"}]})
        with self.assertRaises(cb.CheckError):
            cb.parse_myself({"data": {"myself": None}})

    def test_yesterday_is_the_last_complete_day_bucket(self):
        # lastN=2 returns [yesterday, today-in-progress]; yesterday is records[0].
        body = {
            "records": [
                {"startTime": "2026-10-24T00:00:00Z", "serverlessGpuAmount": 1.4, "totalAmount": 1.5},
                {"startTime": "2026-10-25T00:00:00Z", "serverlessGpuAmount": 0.2, "totalAmount": 0.2},
            ]
        }
        self.assertEqual(cb.parse_yesterday_spend(body), ("2026-10-24", 1.5))

    def test_missing_billing_records_raise(self):
        with self.assertRaises(cb.CheckError):
            cb.parse_yesterday_spend({"records": []})

    def test_missing_null_or_non_finite_amounts_raise_instead_of_reading_as_zero(self):
        # Codex: a malformed response must never log "ok".
        today = {"startTime": "2026-10-25T00:00:00Z", "totalAmount": 0.0}
        for bad in ({}, {"totalAmount": None}, {"totalAmount": float("nan")},
                    {"totalAmount": "inf"}, {"totalAmount": "x"}):
            day = {"startTime": "2026-10-24T00:00:00Z", **bad}
            with self.assertRaises(cb.CheckError, msg=bad):
                cb.parse_yesterday_spend({"records": [day, today]})
        for bad in (None, float("nan"), "x"):
            with self.assertRaises(cb.CheckError, msg=bad):
                cb.parse_myself({"data": {"myself": {"clientBalance": bad, "currentSpendPerHr": 0}}})

    def test_unexpected_shapes_raise_check_error(self):
        for body in ([], "nope", {"errors": "boom"}, {"data": []}):
            with self.assertRaises(cb.CheckError, msg=body):
                cb.parse_myself(body)
        for body in ([], {"records": "x"}, {"records": [1, 2]}, {"records": [{}, {}]}):
            with self.assertRaises(cb.CheckError, msg=body):
                cb.parse_yesterday_spend(body)


class DeliveryTests(unittest.TestCase):
    def test_log_never_raises_when_the_file_cannot_be_written(self):
        # A logging failure must not stop the alert that follows it.
        orig = cb.LOG_PATH
        cb.LOG_PATH = "/nonexistent-root-dir/charcoal/x.log"
        try:
            cb.log("line")  # must not raise
        finally:
            cb.LOG_PATH = orig

    def test_notify_reports_failure_when_no_channel_delivers(self):
        orig_run = cb.subprocess.run
        cb.subprocess.run = lambda *a, **k: cb.subprocess.CompletedProcess(a, 1, "", "err")
        os.environ.pop("CHARCOAL_ALERT_NTFY_TOPIC", None)
        try:
            self.assertFalse(cb.notify("t", "m"))
        finally:
            cb.subprocess.run = orig_run

    def test_notify_succeeds_when_osascript_does(self):
        orig_run = cb.subprocess.run
        cb.subprocess.run = lambda *a, **k: cb.subprocess.CompletedProcess(a, 0, "", "")
        os.environ.pop("CHARCOAL_ALERT_NTFY_TOPIC", None)
        try:
            self.assertTrue(cb.notify("t", "m"))
        finally:
            cb.subprocess.run = orig_run


class EvaluateTests(unittest.TestCase):
    def test_healthy_account_raises_no_alerts(self):
        self.assertEqual(cb.evaluate(balance=58.0, yesterday_spend=1.5, spend_per_hr=0.0), [])

    def test_low_balance_alerts(self):
        alerts = cb.evaluate(balance=24.99, yesterday_spend=1.0, spend_per_hr=0.0)
        self.assertEqual(len(alerts), 1)
        self.assertIn("$24.99", alerts[0])

    def test_balance_exactly_at_threshold_does_not_alert(self):
        self.assertEqual(cb.evaluate(balance=25.0, yesterday_spend=0.0, spend_per_hr=0.0), [])

    def test_spend_spike_alerts_even_with_healthy_balance(self):
        # The 2026-10-02 leak billed every day; a day above $10 is ~3x the
        # $100/month budget pace and is the early sign of a runaway.
        alerts = cb.evaluate(balance=58.0, yesterday_spend=10.01, spend_per_hr=0.0)
        self.assertEqual(len(alerts), 1)
        self.assertIn("$10.01", alerts[0])

    def test_both_conditions_give_two_alerts(self):
        self.assertEqual(len(cb.evaluate(balance=5.0, yesterday_spend=20.0, spend_per_hr=3.29)), 2)

    def test_thresholds_are_overridable(self):
        alerts = cb.evaluate(balance=30.0, yesterday_spend=6.0, spend_per_hr=0.0, min_balance=40.0, max_daily_spend=5.0)
        self.assertEqual(len(alerts), 2)


class SummaryTests(unittest.TestCase):
    def test_summary_line_carries_all_figures(self):
        line = cb.summary_line(balance=58.0, day="2026-10-24", yesterday_spend=1.5, spend_per_hr=3.29)
        for part in ("$58.00", "2026-10-24", "$1.50", "$3.29/hr"):
            self.assertIn(part, line)


if __name__ == "__main__":
    unittest.main()
