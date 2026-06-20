"""RunPod Serverless worker entrypoint for the CoPE-B-A4B classifier.

vLLM AsyncLLMEngine handles the model + KV cache. Each request feeds a
prompt assembled via prompt.build_prompt, samples a single token greedily,
and returns the binary verdict + normalized confidence.

Spec: docs/superpowers/specs/2026-06-05-cope-b-self-hosted-design.md

Testability note (deviation from the plan's handler sketch):
    The plan imported vLLM at module top. That makes the module
    unimportable on CPU/Apple-Silicon machines and forces every handler
    test to fully stub vllm at import time. We instead:
      * keep `import runpod` / `from vllm import ...` *inside*
        `_build_engine()` (lazy), so importing this module never touches
        vLLM,
      * lazily build the engine on first `handler()` call,
      * factor the pure verdict/confidence math into `decode_verdict()` and
        `normalize_logprob()` helpers that tests can exercise directly
        without a GPU, a tokenizer, or vLLM.
    The wire contract and behaviour are unchanged.
"""

from __future__ import annotations

import math
import os
import uuid

from prompt import build_prompt


MODEL_PATH = os.environ.get("MODEL_PATH", "zentropi-ai/cope-b-a4b")
POLICY_PATH = os.environ.get("POLICY_PATH", "policy.txt")
POLICY_VERSION = os.environ.get("POLICY_VERSION", "policy-unversioned")

with open(POLICY_PATH, "r", encoding="utf-8") as fp:
    POLICY = fp.read()


# Module-level engine handle, built lazily on first request so that importing
# this module (for tests, tooling) never constructs a vLLM engine.
_engine = None


def _build_engine():
    """Construct the vLLM AsyncLLMEngine. vLLM is imported here (not at module
    top) so the module stays importable without CUDA/vLLM present."""
    from vllm import AsyncLLMEngine, AsyncEngineArgs

    return AsyncLLMEngine.from_engine_args(
        AsyncEngineArgs(
            model=MODEL_PATH,
            dtype="bfloat16",
            max_model_len=4096,          # 256K default is wasteful for ~300-tok inputs
            max_num_seqs=32,             # tune empirically post-deploy
            enable_prefix_caching=True,  # critical — policy text is identical per call
        )
    )


def _sampling_params():
    """Greedy single-token decode, top-2 logprobs so we can extract confidence.
    Imported lazily for the same reason as the engine."""
    from vllm import SamplingParams

    return SamplingParams(
        max_tokens=1,
        temperature=0.0,
        logprobs=2,
    )


def _get_engine():
    global _engine
    # Race-safe ONLY because _build_engine() is synchronous: there is no
    # `await` between the `if _engine is None` check and the assignment, so no
    # other coroutine can interleave. If _build_engine ever becomes async, this
    # check-and-set must be guarded by an asyncio.Lock to avoid double-build.
    if _engine is None:
        _engine = _build_engine()
    return _engine


def normalize_logprob(decoded_token: str) -> str:
    """Normalize a decoded token for comparison against the bare "0"/"1".

    Gemma's SentencePiece tokenizer may return decoded_token with a leading
    space or ▁ (U+2581) marker; strip whitespace and the SentencePiece
    underline so `'▁1'` and `' 1'` both compare equal to `'1'`."""
    return decoded_token.strip().lstrip("▁")


def decode_verdict(text: str, logprob_map) -> tuple[bool, float]:
    """Turn a single-token generation into (toxic, confidence).

    `text` is the raw emitted token; `logprob_map` is vLLM's
    `output.logprobs[0]` — a dict keyed by token_id whose values carry
    `.logprob` and `.decoded_token`.

    Raises:
        ValueError: emitted token is not "0"/"1", or it is missing from the
            logprobs map. Per the spec's no-silent-fallback rule.
    """
    token = text.strip()
    if token not in {"0", "1"}:
        raise ValueError(f"unexpected model token: {token!r}")

    emitted_logprob = next(
        (lp.logprob for lp in logprob_map.values() if normalize_logprob(lp.decoded_token) == token),
        None,
    )
    if emitted_logprob is None:
        raise ValueError(f"emitted token {token!r} missing from logprobs map")

    confidence = float(math.exp(emitted_logprob))
    return token == "1", confidence


async def handler(event):
    """Classify a single content string. event = {"id": ..., "input": {"content": ...}}.

    Returns the bare verdict dict {"toxic": bool, "confidence": float,
    "model": str, "policy_version": str}. RunPod Serverless wraps this return
    value in its own top-level "output" field, so the on-the-wire response is
    {"output": {"toxic": ...}} — which is exactly what RunPodCopeBClient expects.
    Returning {"output": {...}} here would double-nest it to
    {"output": {"output": {...}}} and break the Rust parser.

    Raises:
        KeyError: input missing "content"
        ValueError: model emitted a token other than "0" or "1"
    """
    inp = event["input"]
    content = inp["content"]   # raises KeyError if missing — surfaced to caller

    prompt = build_prompt(policy=POLICY, content=content)
    request_id = event.get("id") or uuid.uuid4().hex

    engine = _get_engine()
    sampling = _sampling_params()

    # AsyncLLMEngine.generate is an async iterator; the last yield contains the
    # finished output. For max_tokens=1 there's exactly one yield.
    final = None
    async for partial in engine.generate(prompt, sampling, request_id):
        final = partial
    if final is None:
        raise RuntimeError("vLLM engine produced no output")

    out = final.outputs[0]
    toxic, confidence = decode_verdict(out.text, out.logprobs[0])

    return {
        "toxic": toxic,
        "confidence": confidence,
        "model": "cope-b-a4b",
        "policy_version": POLICY_VERSION,
    }


if __name__ == "__main__":
    import runpod

    runpod.serverless.start({"handler": handler})
