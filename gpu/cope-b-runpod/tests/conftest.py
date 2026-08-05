"""Shared fixtures for handler + prompt tests."""

from pathlib import Path
import pytest

ROOT = Path(__file__).parent.parent
POLICY_PATH = ROOT / "policy.txt"


@pytest.fixture
def policy_text() -> str:
    """Loads the live policy.txt so tests reflect what production sees."""
    return POLICY_PATH.read_text()


@pytest.fixture
def sample_content_pair() -> str:
    """An envelope identical to Charcoal's format_parent_reply output."""
    return (
        "[Parent post]: I just got home after a brutal commute.\n\n"
        "[Reply]: Yeah, same — those train delays are killing me."
    )


@pytest.fixture
def sample_content_solo() -> str:
    """An original post (no parent), as Charcoal would pass it."""
    return "Excited to share a piece I've been working on about labor unions."
