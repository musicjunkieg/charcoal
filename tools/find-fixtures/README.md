# find-fixtures

Standalone Python tool for harvesting candidate Bluesky posts to use as
labeled fixtures during CoPE-B policy testing.

Emits JSONL in the fixture schema from
[`docs/superpowers/plans/2026-06-05-cope-b-self-host.md`](../../docs/superpowers/plans/2026-06-05-cope-b-self-host.md)
Chunk 2 — the reviewer fills in `label` (and ideally `category` and `note`)
before committing to `tests/fixtures/cope_b/`.

## Quick start

stdlib only — no `pip install`. Python 3.10+.

```bash
# Mode 1: walk an account's authored posts (good for known-hostile or
# known-supportive accounts you've targeted by hand)
python3 tools/find-fixtures/find_fixtures.py author hostile.bsky.social --count 30 > t.jsonl

# Mode 2: harvest replies + quotes pointing at a specific seed post
# (good for taking a chaosgreml.in post that drew a pile-on and pulling
# every reply for review — mirrors how Charcoal's amplification pipeline
# actually finds candidates in production)
python3 tools/find-fixtures/find_fixtures.py backlinks \
    "https://bsky.app/profile/chaosgreml.in/post/3kabc..." \
    --count 30 --include-quotes > r.jsonl

python3 tools/find-fixtures/find_fixtures.py --help   # full options
```

The script accepts both `bsky.app` post URLs and bare `at://` URIs for the
backlinks-mode `post` argument; it auto-resolves handles to DIDs as needed.

## What you get

Each emitted JSONL line:

```json
{
  "id": "cand-bafyreid...",
  "label": "",
  "category": "identity-attack",
  "content": "[Parent post]: ...\n\n[Reply]: ...",
  "note": ""
}
```

- `label` — blank; the reviewer fills in `toxic` | `clean` | `uncertain`
- `category` — best-effort guess from a small keyword list, drawn from the
  allowed-values set in the Chunk 2 fixture contract; **overwrite freely**
- `content` — uses the exact `[Parent post]: <p>\n\n[Reply]: <r>` envelope
  produced by `src/toxicity/mod.rs::format_parent_reply`, so candidates are
  drop-in inputs for the classifier path
- `id` — derived from the source record's CID/rkey for stable identity

## PII scrubbing

These substitutions apply automatically (per Chunk 2's PII checklist):

| Pattern | Replacement |
|---|---|
| `@handle.bsky.social` | `<user>` |
| `at://did:plc:.../...` | `<at-uri>` |
| `https://bsky.app/profile/.../post/...` | `<post-url>` |
| `did:plc:...` | `<did>` |

**The reviewer must still paraphrase distinctive multi-word phrases** before
committing — that's Chunk 2 PII checklist item 5 and cannot be automated.

## Why not searchPosts?

`app.bsky.feed.searchPosts` requires authentication (the public CDN returns
403 and `bsky.social` returns 401). The author-feed + Constellation backlinks
paths are unauthenticated and produce stronger curation signal anyway — both
target a known account/post rather than a freeform keyword.

## API surfaces used

- `app.bsky.feed.getAuthorFeed` — public AT Protocol AppView, walks a user's
  authored posts (auth-free)
- `com.atproto.identity.resolveHandle` — handle → DID (auth-free)
- `com.atproto.repo.getRecord` — fetch a single record by AT-URI (auth-free)
- `blue.microcosm.links.getBacklinks` — Microcosm's [Constellation](https://constellation.microcosm.blue)
  backlink index; same service Charcoal uses for amplification discovery in
  production (`src/constellation/client.rs`)

## Limits and gotchas

- Constellation paginates with a cursor; the script over-fetches by 3× then
  filters short/duplicate bodies and parent-record fetch failures
- Default 0.3s sleep between paginated requests — bump if you see CDN throttling
- `--no-parent-fetch` (author mode only) drops the envelope and emits solo
  content; useful for harvesting originals
- The reviewer is expected to overwrite the heuristic `category` guess with a
  value from the allowed-values set defined in Chunk 2 of the plan

## Workflow with Chunk 2

```bash
# 1. Harvest 60 candidates from a known-hostile account
python3 tools/find-fixtures/find_fixtures.py author hostile.bsky.social \
    --count 60 > /tmp/t-raw.jsonl

# 2. Harvest 60 candidates from replies to a Bryan post known to draw pile-ons
python3 tools/find-fixtures/find_fixtures.py backlinks \
    "https://bsky.app/profile/chaosgreml.in/post/3xyz..." \
    --count 60 --include-quotes >> /tmp/t-raw.jsonl

# 3. Hand-review, label, paraphrase. Drop into the real fixtures.
$EDITOR /tmp/t-raw.jsonl
mv /tmp/t-raw.jsonl tests/fixtures/cope_b/known_toxic.jsonl
```
