"""Benchmark: assert that vLLMs prefix caching is actually firing.

We send N identical-policy requests with varying CONTENT and assert that
the median time-to-second-request is materially lower than time-to-first.
Without prefix caching, every call reprocesses the policy KV state — easy
~10x cost difference under our workload.

Requires a live vLLM endpoint (gpu/cope-b-runpod/tests/smoke_test.sh
must be running, or a deployed RunPod endpoint). Skip if neither is
available.
"""

import os
import statistics
import time
import urllib.request
import urllib.error
import json
import pytest


VLLM_URL = os.environ.get("VLLM_URL", "http://localhost:8000")


def _is_endpoint_up() -> bool:
    try:
        with urllib.request.urlopen(f"{VLLM_URL}/v1/models", timeout=2):
            return True
    except urllib.error.URLError:
        return False


pytestmark = pytest.mark.skipif(
    not _is_endpoint_up(),
    reason="vLLM endpoint not reachable; run smoke_test.sh or set VLLM_URL",
)


def _call(content: str) -> float:
    body = json.dumps({
        "model": os.environ.get("MODEL", "zentropi-ai/cope-b-a4b"),
        "prompt": "POLICY\n======\n\nshared policy text\n\nCONTENT\n=======\n\n" + content,
        "max_tokens": 1,
        "temperature": 0,
    }).encode()
    req = urllib.request.Request(
        f"{VLLM_URL}/v1/completions",
        data=body,
        headers={"content-type": "application/json"},
    )
    start = time.perf_counter()
    with urllib.request.urlopen(req, timeout=30) as r:
        r.read()
    return time.perf_counter() - start


def test_prefix_cache_warm_calls_are_materially_faster():
    # Warm the cache once
    first = _call("First content — establishes the prefix cache")
    # Now measure several warm calls
    warm = [_call(f"Warm content variant {i}") for i in range(5)]
    median_warm = statistics.median(warm)

    # Heuristic: warm median should be < 50% of cold first.
    # Tune this threshold once we have real numbers from Task 3.6 smoke runs.
    assert median_warm < first * 0.5, (
        f"Prefix caching not firing: first={first:.2f}s, median warm={median_warm:.2f}s. "
        f"Investigate --enable-prefix-caching flag."
    )
