"""Unit tests for prompt.py — Gemma chat template + POLICY/CONTENT body.

The build_prompt() function must produce the exact text the model expects.
We verify the body shape against the model card's verbatim template
(no INSTRUCTIONS or ANSWER headers; literal POLICY and CONTENT slots) and
verify the Gemma chat-template wrapping by checking the role markers.
"""

import pytest


EXPECTED_BODY = """Examine the given POLICY and determine if the given CONTENT meets the criteria for ANY of the LABELS. Answer "1" if yes, and "0" if no.


POLICY
======

This is a test policy.


CONTENT
=======

[Parent post]: Hello.

[Reply]: World."""


def test_build_body_matches_model_card_template():
    """The body text fed into the chat template must match the model card
    structure exactly: two blank lines before POLICY header, '=' underline,
    blank line, policy slot, two blank lines, CONTENT header, etc."""
    from prompt import build_body

    body = build_body(
        policy="This is a test policy.",
        content="[Parent post]: Hello.\n\n[Reply]: World.",
    )
    assert body == EXPECTED_BODY


def test_build_body_handles_literal_braces_in_policy_and_content():
    """Policies often contain `{handle}`-style placeholders or JSON examples.
    A str.format()-based template would crash on these; sentinel-replace
    must pass them through verbatim."""
    from prompt import build_body

    body = build_body(
        policy="Rule {1}: don't address users as {their_handle}.",
        content="Reply to {parent_handle}: hello {there}",
    )
    assert "{1}" in body
    assert "{their_handle}" in body
    assert "{parent_handle}" in body
    assert "{there}" in body


def test_build_body_not_corrupted_by_literal_sentinel_in_policy():
    """A policy that contains the literal `__CONTENT__` (or `__POLICY__`)
    sentinel must NOT be re-substituted. The split/join construction treats
    such occurrences as plain text — sequential `.replace()` would have
    corrupted them by injecting the content value into the policy."""
    from prompt import build_body

    body = build_body(
        policy="See the __CONTENT__ section and the __POLICY__ header below.",
        content="ACTUAL_CONTENT",
    )
    # The literal sentinel inside the policy survives verbatim...
    assert "See the __CONTENT__ section and the __POLICY__ header below." in body
    # ...and the real content appears exactly once (in its own slot).
    assert body.count("ACTUAL_CONTENT") == 1


# The remaining tests require the tokenizer (transformers) to wrap the body in
# the Gemma chat template. On machines without transformers they skip
# individually (not at module scope) so the two pure build_body tests above
# still run; CI runs everything inside the vLLM image where transformers is
# present.


def test_build_prompt_wraps_body_in_gemma_chat_template(policy_text, sample_content_pair):
    """build_prompt() runs tokenizer.apply_chat_template with role=user. The
    resulting prompt must include the user-role marker and end with the
    assistant-generation prompt suffix so the model emits a 0/1 token next."""
    pytest.importorskip("transformers")
    from prompt import build_prompt

    prompt = build_prompt(policy=policy_text, content=sample_content_pair)
    # Gemma chat-template markers (these are stable strings the template emits)
    assert "<start_of_turn>user" in prompt, "expected user-role start marker"
    assert "<start_of_turn>model" in prompt, "expected assistant-role generation prompt"
    # The body must appear inside the user turn (between user-start and end_of_turn)
    user_block = prompt.split("<start_of_turn>user")[1].split("<end_of_turn>")[0]
    assert "POLICY" in user_block
    assert "CONTENT" in user_block
    assert sample_content_pair in user_block


def test_build_prompt_handles_solo_content(policy_text, sample_content_solo):
    """Original posts (no parent) pass content through unchanged — no envelope.
    The model sees the bare body text in the CONTENT slot."""
    pytest.importorskip("transformers")
    from prompt import build_prompt

    prompt = build_prompt(policy=policy_text, content=sample_content_solo)
    assert sample_content_solo in prompt
    # We did NOT prepend a [Parent post] envelope:
    assert "[Parent post]:" not in prompt or sample_content_solo.startswith("[Parent post]:")


def test_build_prompt_is_deterministic(policy_text, sample_content_pair):
    """Same inputs must produce byte-identical output. Prefix caching relies
    on this — a non-deterministic prompt invalidates the policy KV cache
    every call."""
    pytest.importorskip("transformers")
    from prompt import build_prompt

    a = build_prompt(policy=policy_text, content=sample_content_pair)
    b = build_prompt(policy=policy_text, content=sample_content_pair)
    assert a == b


def test_build_prompt_policy_appears_before_content(policy_text):
    """Order matters for prefix caching: identical policy text must sit at
    the front so the same prefix is reused across calls with different
    content. Verify policy header precedes content header in the output."""
    pytest.importorskip("transformers")
    from prompt import build_prompt

    prompt = build_prompt(policy=policy_text, content="anything")
    p_idx = prompt.index("POLICY")
    c_idx = prompt.index("CONTENT")
    assert p_idx < c_idx, "POLICY must precede CONTENT for prefix caching"
