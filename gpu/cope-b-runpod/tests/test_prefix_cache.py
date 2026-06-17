"""Benchmark: assert that vLLM's prefix caching is actually firing.

We send N requests that share the SAME policy with varying CONTENT and assert
that the median time of warm requests is materially lower than the cold first
request. Without prefix caching, every call reprocesses the policy KV state —
easy ~10x cost difference under our workload.

This exercises the PRODUCTION prefix. Production builds the POLICY/CONTENT body
via prompt.build_body(...) and lets the model's Gemma chat template wrap it.
We reproduce that exactly by posting the body as a `user` message to the
chat-completions endpoint (`/v1/chat/completions`), which applies the Gemma
chat template server-side — so the cached prefix here is byte-identical to what
the handler sends in production. No local tokenizer/transformers needed: the
server owns the template.

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

# Import the production body builder so the shared prefix is the real one.
from prompt import build_body


VLLM_URL = os.environ.get("VLLM_URL", "http://localhost:8000")

# The shared policy is what we expect vLLM to cache as the common prefix.
# Use a realistic multi-line policy; only CONTENT varies between calls.
SHARED_POLICY = (
    "LABELS\n"
    "======\n"
    "toxic: content that harasses, demeans, or threatens a target.\n"
    "\n"
    "Apply the label when the CONTENT meets the criteria above."
)


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
    # Build the production POLICY/CONTENT body, then hand it to the
    # chat-completions endpoint as a user message. vLLM applies the Gemma chat
    # template server-side, reproducing the exact prefix the handler caches.
    body = build_body(policy=SHARED_POLICY, content=content)
    payload = json.dumps({
        "model": os.environ.get("MODEL", "zentropi-ai/cope-b-a4b"),
        "messages": [{"role": "user", "content": body}],
        "max_tokens": 1,
        "temperature": 0,
    }).encode()
    req = urllib.request.Request(
        f"{VLLM_URL}/v1/chat/completions",
        data=payload,
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
