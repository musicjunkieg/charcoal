"""Handler tests — RunPod request/response shape, verdict + confidence
calculation, and error handling. The vLLM engine is mocked so tests run
on CPU machines without GPU."""

import sys
from unittest.mock import MagicMock, patch
import pytest

# Stub heavy GPU-only deps before any handler import so collection works on CPU.
#
# Deviation from the plan: the plan used `pytest.importorskip("transformers")`,
# which skips the entire handler suite on machines without transformers (i.e.
# Bryan's Mac mini, and any CPU-only dev box). Since these tests verify pure
# handler *logic* (token->bool mapping, exp(logprob) confidence, decoded-token
# normalization, response shape, error handling) and feed a fully-mocked
# engine whose output is independent of the prompt, neither vLLM nor the real
# tokenizer matters here. We stub `vllm`/`runpod` in sys.modules (those are
# never imported by other test modules) and — crucially — patch
# `prompt.build_prompt` directly in the fixture rather than stubbing the
# `transformers` module globally. Stubbing sys.modules["transformers"] would
# leak into test_prompt.py (run in the same process), defeating its
# importorskip guard and breaking the tokenizer-dependent prompt tests.
sys.modules.setdefault("vllm", MagicMock())
sys.modules.setdefault("runpod", MagicMock())

pytestmark = pytest.mark.asyncio


async def _async_iter(*items):
    """Wrap a sequence of values as an async iterator (matches vLLM's
    AsyncLLMEngine.generate, which is an async generator yielding partial
    RequestOutputs)."""
    for item in items:
        yield item


def _mock_engine_result(
    token: str,
    logprob: float = -0.1,
    other_logprob: float = -3.0,
    decoded_prefix: str = "",
):
    """Build a MagicMock that looks like vllm's RequestOutput.outputs[0].
    `decoded_prefix` lets tests simulate Gemma's SentencePiece behavior
    where decoded_token may carry a leading space or ▁ marker."""
    other_token = "0" if token == "1" else "1"
    logprobs_map = {
        1: MagicMock(logprob=logprob, decoded_token=f"{decoded_prefix}{token}"),
        2: MagicMock(logprob=other_logprob, decoded_token=f"{decoded_prefix}{other_token}"),
    }
    out = MagicMock()
    out.text = token
    out.logprobs = [logprobs_map]
    result = MagicMock()
    result.outputs = [out]
    return result


@pytest.fixture
def patched_engine(monkeypatch, tmp_path):
    """Patch AsyncLLMEngine.from_engine_args at the import boundary so handler
    sees a mock instead of trying to load a real model."""
    policy_file = tmp_path / "policy.txt"
    policy_file.write_text("Test policy.")
    monkeypatch.setenv("MODEL_PATH", "zentropi-ai/cope-b-a4b")
    monkeypatch.setenv("POLICY_PATH", str(policy_file))

    fake_engine = MagicMock()
    # AsyncLLMEngine.generate is an async generator; replace with a callable
    # that returns an async iterator on every call. Tests set
    # fake_engine.generate_result on the returned MagicMock to control what
    # _async_iter yields.
    fake_engine.generate_result = _mock_engine_result(token="1")
    fake_engine.generate = MagicMock(
        side_effect=lambda *args, **kwargs: _async_iter(fake_engine.generate_result)
    )

    with patch("vllm.AsyncLLMEngine.from_engine_args", return_value=fake_engine):
        import importlib
        import handler  # type: ignore
        importlib.reload(handler)
        # build_prompt loads a real tokenizer (transformers) which we don't have
        # on CPU and don't need — the mocked engine ignores the prompt. Patch it
        # to a no-op string builder so the handler logic runs end-to-end. We
        # patch the name bound into handler's namespace (it did
        # `from prompt import build_prompt`).
        handler.build_prompt = lambda policy, content: f"<prompt>{content}</prompt>"
        yield handler, fake_engine


async def test_handler_returns_toxic_true_when_model_emits_1(patched_engine):
    handler, fake_engine = patched_engine
    fake_engine.generate_result = _mock_engine_result(token="1", logprob=-0.05)
    result = await handler.handler({"id": "req-1", "input": {"content": "test"}})
    out = result["output"]
    assert out["toxic"] is True
    assert out["model"] == "cope-b-a4b"
    # Confidence is exp(logprob), so exp(-0.05) ≈ 0.95
    assert 0.9 < out["confidence"] < 1.0


async def test_handler_returns_toxic_false_when_model_emits_0(patched_engine):
    handler, fake_engine = patched_engine
    fake_engine.generate_result = _mock_engine_result(token="0", logprob=-0.2)
    result = await handler.handler({"id": "req-2", "input": {"content": "test"}})
    out = result["output"]
    assert out["toxic"] is False
    assert 0.7 < out["confidence"] < 0.9   # exp(-0.2) ≈ 0.819


async def test_handler_normalizes_decoded_token_with_sentinel_prefix(patched_engine):
    """Gemma's SentencePiece tokenizer may return decoded_token with a leading
    space or ▁ marker (`'▁1'` or `' 1'`). out.text.strip() is the bare token;
    the logprobs lookup must normalize both sides before comparing or the
    confidence calculation silently falls through to ValueError."""
    handler, fake_engine = patched_engine
    fake_engine.generate_result = _mock_engine_result(
        token="1", logprob=-0.05, decoded_prefix="▁"
    )
    result = await handler.handler({"id": "req-norm-1", "input": {"content": "test"}})
    assert result["output"]["toxic"] is True
    assert 0.9 < result["output"]["confidence"] < 1.0

    fake_engine.generate_result = _mock_engine_result(
        token="0", logprob=-0.1, decoded_prefix=" "
    )
    result = await handler.handler({"id": "req-norm-2", "input": {"content": "test"}})
    assert result["output"]["toxic"] is False


async def test_handler_returns_policy_version_from_env(patched_engine, monkeypatch):
    handler, fake_engine = patched_engine
    monkeypatch.setenv("POLICY_VERSION", "policy-v3-2026-07-01")
    import importlib
    importlib.reload(handler)
    # Reload reset the fake_engine reference and restored the real build_prompt;
    # re-patch both so the module reads the new POLICY_VERSION but still runs
    # against the mock engine and the no-op prompt builder.
    handler.build_prompt = lambda policy, content: f"<prompt>{content}</prompt>"
    fake_engine.generate_result = _mock_engine_result(token="1")
    handler._engine = fake_engine  # type: ignore[attr-defined]
    result = await handler.handler({"id": "req-3", "input": {"content": "test"}})
    assert result["output"]["policy_version"] == "policy-v3-2026-07-01"


async def test_handler_raises_on_missing_input(patched_engine):
    handler, _ = patched_engine
    with pytest.raises(KeyError):
        await handler.handler({"id": "req-4", "input": {}})


async def test_handler_raises_on_unexpected_model_output(patched_engine):
    """If the model emits something other than "0" or "1", surface the failure
    rather than silently falling back. Spec: "No silent fallbacks."""
    handler, fake_engine = patched_engine
    fake_engine.generate_result = _mock_engine_result(token="maybe", logprob=-1.0)
    with pytest.raises(ValueError, match="unexpected"):
        await handler.handler({"id": "req-5", "input": {"content": "test"}})
