"""Prompt assembly for the CoPE-B-A4B classifier.

The model expects two layers:
1. A POLICY/CONTENT body matching the structure on the HF model card
   (https://huggingface.co/zentropi-ai/cope-b-a4b — "Usage" section).
2. The body wrapped in Gemma-4's chat template via `apply_chat_template`,
   with `add_generation_prompt=True` so the model emits the next token
   (which will be the "1" or "0" verdict).

We expose `build_body` separately so unit tests can golden-file the body
without instantiating a tokenizer.
"""

from __future__ import annotations

# Body template — keep formatting EXACTLY as on the model card.
# Two blank lines before POLICY, '=' underline, blank line, slot, two blank
# lines before CONTENT, '=' underline, blank line, slot. Changing whitespace
# breaks the model's expected prefix.
#
# Sentinels `__POLICY__` / `__CONTENT__` are used instead of str.format so
# policy text can contain literal `{` or `}` (common in policies that
# discuss handle placeholders, JSON examples, etc.) without exploding.
_BODY_TEMPLATE = (
    'Examine the given POLICY and determine if the given CONTENT meets the '
    'criteria for ANY of the LABELS. Answer "1" if yes, and "0" if no.\n'
    '\n'
    '\n'
    'POLICY\n'
    '======\n'
    '\n'
    '__POLICY__\n'
    '\n'
    '\n'
    'CONTENT\n'
    '=======\n'
    '\n'
    '__CONTENT__'
)


def build_body(policy: str, content: str) -> str:
    """Return the POLICY/CONTENT body text, before chat-template wrapping."""
    return _BODY_TEMPLATE.replace("__POLICY__", policy).replace("__CONTENT__", content)


_TOKENIZER = None


def _get_tokenizer():
    global _TOKENIZER
    if _TOKENIZER is None:
        # Lazy-import to keep test files from forcing transformers on every
        # collection pass; tokenizer load is ~50 MB of metadata.
        from transformers import AutoTokenizer
        import os
        model_path = os.environ.get("MODEL_PATH", "zentropi-ai/cope-b-a4b")
        _TOKENIZER = AutoTokenizer.from_pretrained(model_path)
    return _TOKENIZER


def build_prompt(policy: str, content: str) -> str:
    """Build the full prompt for vLLM: body wrapped in the Gemma chat template,
    with an assistant generation prompt at the end so the model emits "1"/"0"."""
    body = build_body(policy=policy, content=content)
    tokenizer = _get_tokenizer()
    return tokenizer.apply_chat_template(
        [{"role": "user", "content": body}],
        tokenize=False,
        add_generation_prompt=True,
    )
