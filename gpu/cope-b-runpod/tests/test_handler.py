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


def _multi_engine(fake_engine, results):
    """Make fake_engine.generate return a fresh async-iterator per call, popping
    one prebuilt RequestOutput per call in order. Lets a batch test give each
    content its own token."""
    it = iter(results)
    fake_engine.generate = MagicMock(
        side_effect=lambda *a, **k: _async_iter(next(it))
    )


async def test_handler_batch_returns_verdicts_in_input_order(patched_engine):
    handler, fake_engine = patched_engine
    _multi_engine(
        fake_engine,
        [_mock_engine_result(token="1", logprob=-0.05),
         _mock_engine_result(token="0", logprob=-0.2)],
    )
    result = await handler.handler(
        {"id": "req-b1", "input": {"contents": ["hostile", "benign"]}}
    )
    verdicts = result["verdicts"]
    assert len(verdicts) == 2
    assert verdicts[0]["ok"] is True and verdicts[0]["toxic"] is True
    assert verdicts[1]["ok"] is True and verdicts[1]["toxic"] is False
    assert verdicts[0]["model"] == "cope-b-a4b"


async def test_handler_batch_isolates_undecodable_slot(patched_engine):
    # Middle slot emits a non-binary token → that slot is ok:false, its
    # siblings are unaffected (per-item isolation, no whole-batch failure).
    handler, fake_engine = patched_engine
    _multi_engine(
        fake_engine,
        [_mock_engine_result(token="1"),
         _mock_engine_result(token="maybe"),
         _mock_engine_result(token="0")],
    )
    result = await handler.handler(
        {"id": "req-b2", "input": {"contents": ["a", "b", "c"]}}
    )
    verdicts = result["verdicts"]
    assert len(verdicts) == 3
    assert verdicts[0]["ok"] is True
    assert verdicts[1]["ok"] is False and "error" in verdicts[1]
    assert verdicts[2]["ok"] is True and verdicts[2]["toxic"] is False


async def test_handler_batch_length_matches_input(patched_engine):
    handler, fake_engine = patched_engine
    _multi_engine(fake_engine, [_mock_engine_result(token="1") for _ in range(4)])
    result = await handler.handler(
        {"id": "req-b3", "input": {"contents": ["a", "b", "c", "d"]}}
    )
    assert len(result["verdicts"]) == 4


async def test_handler_batch_empty_contents(patched_engine):
    handler, _ = patched_engine
    result = await handler.handler({"id": "req-b4", "input": {"contents": []}})
    assert result["verdicts"] == []


async def test_handler_returns_policy_version_from_env(patched_engine, monkeypatch):
    handler, fake_engine = patched_engine
    monkeypatch.setenv("POLICY_VERSION", "policy-v3-2026-07-01")
    import importlib
    importlib.reload(handler)
    handler.build_prompt = lambda policy, content: f"<prompt>{content}</prompt>"
    _multi_engine(fake_engine, [_mock_engine_result(token="1")])
    handler._engine = fake_engine  # type: ignore[attr-defined]
    result = await handler.handler({"id": "req-b5", "input": {"contents": ["test"]}})
    assert result["verdicts"][0]["policy_version"] == "policy-v3-2026-07-01"


async def test_handler_raises_on_missing_contents(patched_engine):
    handler, _ = patched_engine
    with pytest.raises(KeyError):
        await handler.handler({"id": "req-b6", "input": {}})


def test_result_slot_ok_for_binary_token():
    # result_slot is pure: given a decoded output it returns an ok slot.
    import handler  # type: ignore
    out = _mock_engine_result(token="1", logprob=-0.05).outputs[0]
    slot = handler.result_slot(out)
    assert slot["ok"] is True and slot["toxic"] is True
    assert 0.9 < slot["confidence"] < 1.0


def test_result_slot_error_for_bad_token():
    import handler  # type: ignore
    out = _mock_engine_result(token="maybe", logprob=-1.0).outputs[0]
    slot = handler.result_slot(out)
    assert slot["ok"] is False and "error" in slot
