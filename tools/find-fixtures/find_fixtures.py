#!/usr/bin/env python3
"""Find candidate Bluesky posts for CoPE-B policy fixture curation.

Two modes; both emit JSONL in the fixture schema from
docs/superpowers/plans/2026-06-05-cope-b-self-host.md (Chunk 2):

    {"id": "...", "label": "", "category": "", "content": "...", "note": ""}

**Mode 1 — author** (default): walks a Bluesky user's `getAuthorFeed`. Target
known-hostile accounts for toxic candidates, known-supportive accounts for
clean candidates, accounts in your topic space for edge cases.

**Mode 2 — backlinks** (`--from-post <at-uri>`): uses Microcosm's
[Constellation backlink index](https://constellation.microcosm.blue) to find
every reply/quote pointing at a specific parent post, then fetches each
amplifier's record and builds the `[Parent post] / [Reply]` envelope. This is
exactly how Charcoal's amplification pipeline discovers candidates in prod,
so the resulting fixtures mirror the runtime distribution.

`label` is left blank for the reviewer to fill in (toxic | clean | uncertain).
`category` is a best-effort guess from a small keyword list — overwrite freely.
`content` uses the EXACT `[Parent post]: <p>\\n\\n[Reply]: <r>` envelope that
`src/toxicity/mod.rs::format_parent_reply` produces, so candidates are drop-in
inputs for the classifier.

**Why not searchPosts?**
`app.bsky.feed.searchPosts` requires authentication (CDN returns 403 on the
public host, bsky.social returns 401). The author + backlinks paths are
unauthenticated and produce stronger curation signal anyway.

Usage:
    # Mode 1 — toxic candidates from a known-hostile account
    python3 find_fixtures.py author hostile.bsky.social --count 30 > t.jsonl

    # Mode 1 — clean candidates from a known-supportive account
    python3 find_fixtures.py author supportive.bsky.social --count 30 > c.jsonl

    # Mode 2 — replies/quotes pointing at a specific seed post (URL or at:// URI)
    python3 find_fixtures.py backlinks "at://did:plc:.../app.bsky.feed.post/..." > r.jsonl
    python3 find_fixtures.py backlinks "https://bsky.app/profile/handle.bsky.social/post/3kabc..." > r.jsonl

    # Tune length and count
    python3 find_fixtures.py author handle --min-len 40 --count 50 > out.jsonl

stdlib only — no `pip install`. Python 3.10+.

PII scrubbing applies per the Chunk 2 PII checklist:
    @handle.bsky.social    -> <user>
    at://did:plc:...       -> <at-uri>
    https://bsky.app/...   -> <post-url>
    did:plc:...            -> <did>

Paraphrasing distinctive multi-word phrases (Chunk 2 PII checklist item 5)
is NOT automated — the reviewer must rewrite identifying phrasing before
committing.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Iterable, Optional

API_BASE = "https://public.api.bsky.app"
CONSTELLATION_BASE = "https://constellation.microcosm.blue"
USER_AGENT = "charcoal-fixture-finder/0.1 (+https://github.com/musicjunkieg/charcoal)"

# Constellation source identifier — `collection:json_path` (no leading dot).
# See src/constellation/client.rs in this repo for the canonical shape.
REPLY_SOURCE = "app.bsky.feed.post:reply.parent.uri"
QUOTE_SOURCE = "app.bsky.feed.post:embed.record.uri"

# PII scrubbers. Order matters: handle AT-URIs and post URLs before bare handles
# so the inner @handles inside those don't get double-substituted.
AT_URI_RE = re.compile(r"at://[a-zA-Z0-9:._/\-]+")
POST_URL_RE = re.compile(r"https?://bsky\.app/profile/[^\s)]+")
DID_RE = re.compile(r"did:plc:[a-z0-9]+")
HANDLE_RE = re.compile(r"@[a-zA-Z0-9][\w\-]*(?:\.[a-zA-Z0-9][\w\-]*)+")


def scrub_pii(text: str) -> str:
    text = AT_URI_RE.sub("<at-uri>", text)
    text = POST_URL_RE.sub("<post-url>", text)
    text = DID_RE.sub("<did>", text)
    text = HANDLE_RE.sub("<user>", text)
    return text


# Best-effort category guess against the allowed-values set from Chunk 2.
# Reviewer should overwrite — this is just a hint to triage faster.
_CATEGORY_HINTS: list[tuple[str, list[str]]] = [
    ("identity-attack", ["tranny", "faggot", " fag ", "retard", "kike",
                         "bitch", "whore", "slut", "groomer"]),
    ("dehumanization", ["subhuman", "vermin", "parasite", "infestation",
                        "scum", "filth", "roach"]),
    ("counter-speech", ["punch a nazi", "punch nazis", "fuck nazis",
                        "fuck terfs", "TERF"]),
    ("reclamation", ["queer pride", "trans rights", "fat liberation",
                     "fat positive", "my fat body", "we queer"]),
    ("news-commentary", ["shooting", "stabbing", "murdered", "attack at",
                         "police killed", "school shooter"]),
    ("concern-troll", ["just asking", "just sayin", "have you considered",
                       "for your own good", "i worry that", "I worry",
                       "not gonna lie"]),
    ("coded-sarcasm", ["oh sure", "yeah right", "totally normal", "very cool"]),
    ("dogpile", ["ratio", "we all", "everyone agrees", "consensus"]),
    ("disagreement", ["i disagree", "actually,", "wrong because",
                      "respectfully disagree"]),
    ("support", ["love you", "proud of", "you matter", "we got you",
                 "with you", "rooting for"]),
]


def guess_category(text: str) -> Optional[str]:
    lc = text.lower()
    for cat, keywords in _CATEGORY_HINTS:
        if any(k in lc for k in keywords):
            return cat
    return None


def _fetch(path: str, params: dict, timeout: float = 15.0, base: str = API_BASE) -> dict:
    qs = urllib.parse.urlencode(params)
    url = f"{base}{path}?{qs}"
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode("utf-8"))


# ─── At-URI / bsky.app URL helpers ─────────────────────────────────────────

_BSKY_POST_RE = re.compile(
    r"^https?://bsky\.app/profile/([^/]+)/post/([a-z0-9]+)/?$"
)


def bsky_url_to_at_uri(url_or_uri: str) -> str:
    """Accept either a bsky.app post URL or an at:// URI; return the at:// URI.

    `bsky.app/profile/<handle-or-did>/post/<rkey>` -> `at://<did>/app.bsky.feed.post/<rkey>`.
    Handles in the URL are resolved to DIDs via com.atproto.identity.resolveHandle.
    """
    s = url_or_uri.strip()
    if s.startswith("at://"):
        return s
    m = _BSKY_POST_RE.match(s)
    if not m:
        raise ValueError(f"not an at:// URI or recognizable bsky.app URL: {s!r}")
    actor, rkey = m.group(1), m.group(2)
    if actor.startswith("did:"):
        did = actor
    else:
        # resolveHandle returns {"did": "..."}
        resp = _fetch("/xrpc/com.atproto.identity.resolveHandle", {"handle": actor})
        did = resp.get("did")
        if not did:
            raise ValueError(f"could not resolve handle {actor!r} to a DID")
    return f"at://{did}/app.bsky.feed.post/{rkey}"


# ─── Constellation backlinks mode ──────────────────────────────────────────

def constellation_backlinks(parent_uri: str, sources: list[str], limit: int,
                             page_pause: float = 0.3) -> Iterable[str]:
    """Yield reply/quote AT-URIs pointing at `parent_uri` across the given sources.

    Wraps `blue.microcosm.links.getBacklinks`. Response shape is
    `{"records": [{"did", "collection", "rkey"}], "cursor"?}` — we reconstruct
    the at:// URI from the triple.
    """
    remaining = limit
    for source in sources:
        cursor: Optional[str] = None
        emitted_for_source = 0
        # Spread the budget roughly evenly across sources, with a small buffer.
        target_for_source = remaining // max(1, len(sources)) + 1
        while emitted_for_source < target_for_source and remaining > 0:
            params: dict = {
                "subject": parent_uri,
                "source": source,
                "limit": min(100, target_for_source - emitted_for_source),
            }
            if cursor:
                params["cursor"] = cursor
            try:
                resp = _fetch(
                    "/xrpc/blue.microcosm.links.getBacklinks",
                    params,
                    base=CONSTELLATION_BASE,
                )
            except urllib.error.HTTPError as e:
                print(f"# constellation HTTP {e.code} on source {source}", file=sys.stderr)
                break
            except (urllib.error.URLError, json.JSONDecodeError) as e:
                print(f"# constellation failed: {e}", file=sys.stderr)
                break
            records = resp.get("records") or []
            if not records:
                break
            for rec in records:
                did = rec.get("did")
                collection = rec.get("collection")
                rkey = rec.get("rkey")
                if not (did and collection and rkey):
                    continue
                yield f"at://{did}/{collection}/{rkey}"
                emitted_for_source += 1
                remaining -= 1
                if remaining <= 0:
                    return
                if emitted_for_source >= target_for_source:
                    break
            cursor = resp.get("cursor")
            if not cursor:
                break
            time.sleep(page_pause)


def get_record_by_uri(at_uri: str) -> Optional[dict]:
    """Fetch a single record (e.g. a post) by its at:// URI via getRecord.

    at://<did>/<collection>/<rkey> -> /xrpc/com.atproto.repo.getRecord?repo=<did>&collection=<col>&rkey=<rkey>
    Returns the record dict (value of `.value`) or None on failure.
    """
    if not at_uri.startswith("at://"):
        return None
    body = at_uri[len("at://"):]
    parts = body.split("/", 2)
    if len(parts) != 3:
        return None
    repo, collection, rkey = parts
    try:
        resp = _fetch(
            "/xrpc/com.atproto.repo.getRecord",
            {"repo": repo, "collection": collection, "rkey": rkey},
        )
    except (urllib.error.URLError, json.JSONDecodeError):
        return None
    return resp.get("value")


def normalize_actor(actor: str) -> str:
    """Accept '@handle.bsky.social', 'handle.bsky.social', or 'did:plc:...'.
    Returns the form getAuthorFeed wants in its `actor` parameter."""
    actor = actor.strip()
    if actor.startswith("@"):
        actor = actor[1:]
    return actor


def get_author_feed(actor: str, limit: int, page_pause: float = 0.3) -> Iterable[dict]:
    """Yield up to `limit` feed items from app.bsky.feed.getAuthorFeed.

    Each yielded dict is a `FeedViewPost` — has `.post`, optional `.reply`
    (which contains the parent record when this feed item is a reply), and
    optional `.reason` (repost marker — we skip those).
    """
    remaining = limit
    cursor: Optional[str] = None
    while remaining > 0:
        page_size = min(100, remaining)
        params: dict = {"actor": actor, "limit": page_size, "filter": "posts_with_replies"}
        if cursor:
            params["cursor"] = cursor
        try:
            resp = _fetch("/xrpc/app.bsky.feed.getAuthorFeed", params)
        except urllib.error.HTTPError as e:
            print(f"# getAuthorFeed HTTP {e.code}: {actor}", file=sys.stderr)
            return
        except (urllib.error.URLError, json.JSONDecodeError) as e:
            print(f"# getAuthorFeed failed: {e}", file=sys.stderr)
            return
        items = resp.get("feed", []) or []
        if not items:
            return
        for item in items:
            yield item
            remaining -= 1
            if remaining <= 0:
                return
        cursor = resp.get("cursor")
        if not cursor:
            return
        time.sleep(page_pause)


def extract_text_from_post(post: dict) -> Optional[str]:
    record = post.get("record") or {}
    text = record.get("text")
    if not isinstance(text, str):
        return None
    return text.strip()


def build_envelope(item: dict, fetch_parent: bool) -> Optional[dict]:
    # Skip reposts — `reason` present means this is a repost, not an authored post
    if item.get("reason"):
        return None

    post = item.get("post") or {}
    body = extract_text_from_post(post)
    if not body:
        return None

    # `item.reply` is the AppView-decorated parent (already inlined — no second fetch needed)
    reply_view = item.get("reply") or {}
    parent_view = reply_view.get("parent") if reply_view else None
    parent_body: Optional[str] = None
    if parent_view and fetch_parent:
        # parent_view.record.text on the AppView, or fall back to nested .post.record.text
        parent_record = parent_view.get("record") or parent_view.get("post", {}).get("record") or {}
        pt = parent_record.get("text")
        if isinstance(pt, str):
            parent_body = pt.strip()

    if parent_body:
        content = (
            f"[Parent post]: {scrub_pii(parent_body)}"
            f"\n\n[Reply]: {scrub_pii(body)}"
        )
        category_input = f"{body} {parent_body}"
    else:
        content = scrub_pii(body)
        category_input = body

    cid = (post.get("cid") or "")[:8] or (post.get("uri") or "")[-12:]
    return {
        "id": f"cand-{cid}",
        "label": "",
        "category": guess_category(category_input) or "",
        "content": content,
        "note": "",
    }


def build_envelope_from_backlink(
    reply_at_uri: str, parent_text: str
) -> Optional[dict]:
    """Given a reply's at:// URI (from Constellation) and the cached parent text,
    fetch the reply's record and build the fixture envelope."""
    record = get_record_by_uri(reply_at_uri)
    if not record:
        return None
    body = (record.get("text") or "").strip()
    if not body:
        return None
    content = (
        f"[Parent post]: {scrub_pii(parent_text)}"
        f"\n\n[Reply]: {scrub_pii(body)}"
    )
    # Use last 12 chars of the rkey-bearing URI as a stable id
    suffix = reply_at_uri.split("/")[-1][:12] or "unk"
    return {
        "id": f"cand-{suffix}",
        "label": "",
        "category": guess_category(f"{body} {parent_text}") or "",
        "content": content,
        "note": "",
    }


def run_author_mode(actor: str, count: int, min_len: int, fetch_parent: bool) -> int:
    actor = normalize_actor(actor)
    emitted = 0
    seen = set()
    for item in get_author_feed(actor, count * 3):
        if emitted >= count:
            break
        row = build_envelope(item, fetch_parent=fetch_parent)
        if row is None:
            continue
        if len(row["content"]) < min_len:
            continue
        if row["content"] in seen:
            continue
        seen.add(row["content"])
        print(json.dumps(row, ensure_ascii=False))
        emitted += 1
    print(f"# emitted {emitted} candidates from {actor}", file=sys.stderr)
    return 0


def run_backlinks_mode(post: str, count: int, min_len: int,
                       include_quotes: bool) -> int:
    parent_uri = bsky_url_to_at_uri(post)
    parent_record = get_record_by_uri(parent_uri)
    if not parent_record:
        print(f"# could not fetch parent record for {parent_uri}", file=sys.stderr)
        return 1
    parent_text = (parent_record.get("text") or "").strip()
    if not parent_text:
        print(f"# parent record has no text: {parent_uri}", file=sys.stderr)
        return 1

    sources = [REPLY_SOURCE]
    if include_quotes:
        sources.append(QUOTE_SOURCE)

    emitted = 0
    seen = set()
    # Over-fetch — failed record fetches + dupes + short bodies are skipped
    for reply_uri in constellation_backlinks(parent_uri, sources, count * 3):
        if emitted >= count:
            break
        row = build_envelope_from_backlink(reply_uri, parent_text)
        if row is None:
            continue
        if len(row["content"]) < min_len:
            continue
        if row["content"] in seen:
            continue
        seen.add(row["content"])
        print(json.dumps(row, ensure_ascii=False))
        emitted += 1

    print(f"# emitted {emitted} candidates backlinking to {parent_uri}", file=sys.stderr)
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    sub = ap.add_subparsers(dest="mode", required=True)

    p_author = sub.add_parser(
        "author",
        help="Walk a Bluesky user's authored posts via getAuthorFeed.",
    )
    p_author.add_argument(
        "actor",
        help="Bluesky handle or DID — e.g. @hostile.bsky.social, supportive.bsky.social, did:plc:...",
    )
    p_author.add_argument("--count", type=int, default=30)
    p_author.add_argument("--min-len", type=int, default=20)
    p_author.add_argument(
        "--no-parent-fetch", action="store_true",
        help="emit solo content even for replies (no envelope; useful for originals)",
    )

    p_back = sub.add_parser(
        "backlinks",
        help="Use Constellation to find replies/quotes pointing at a specific seed post.",
    )
    p_back.add_argument(
        "post",
        help="Seed post — at:// URI or bsky.app post URL.",
    )
    p_back.add_argument("--count", type=int, default=30)
    p_back.add_argument("--min-len", type=int, default=20)
    p_back.add_argument(
        "--include-quotes", action="store_true",
        help="also include quote-posts (in addition to replies)",
    )

    args = ap.parse_args()

    if args.mode == "author":
        return run_author_mode(
            args.actor, args.count, args.min_len,
            fetch_parent=not args.no_parent_fetch,
        )
    if args.mode == "backlinks":
        return run_backlinks_mode(
            args.post, args.count, args.min_len,
            include_quotes=args.include_quotes,
        )
    return 1


if __name__ == "__main__":
    sys.exit(main())
