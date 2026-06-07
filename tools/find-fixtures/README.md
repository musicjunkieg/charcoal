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

# Mode 3: walk every member of a Bluesky moderation/curation list and harvest
# their authored posts (use this when you have a community-curated list naming
# accounts known for a specific behavior — e.g. transphobia, antisemitism)
python3 tools/find-fixtures/find_fixtures.py list \
    "https://bsky.app/profile/maintainer.bsky.social/lists/3xyz..." \
    --total 60 --per-account 5 > t.jsonl

# Any mode + --match: filter to candidates whose content matches a regex.
# Combined with `list` mode, this is the answer to "what words should we
# search for" — pick the language pattern, the tool does the harvesting.
python3 tools/find-fixtures/find_fixtures.py list <url> \
    --match "(tranny|groomer|woke mind virus)" --total 100 > t.jsonl

python3 tools/find-fixtures/find_fixtures.py --help   # full options
```

The script accepts both `bsky.app` URLs (post or list) and bare `at://` URIs;
it auto-resolves handles to DIDs as needed.

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
403 and `bsky.social` returns 401). The author / backlinks / list paths are
all unauthenticated and produce stronger curation signal anyway — each
targets a known account, post, or community-curated list of accounts rather
than a freeform keyword. The `--match` regex flag is the replacement for
keyword filtering when you DO want to narrow within those targets.

## Why list mode is usually the best path

- **Pre-curated**: mod-list maintainers have already done the per-account
  judgment about "this account engages in <behavior>" — we don't have to
  guess at handles
- **Themed**: lists usually come with a documented purpose (transphobia,
  antisemitism, a specific harassment crew), so the contents share a
  language pattern that `--match` can narrow further
- **Block-resilient**: many hostile accounts have likely already blocked the
  protected user(s), so they won't appear in Constellation backlinks of the
  protected user's posts. They DO still appear on a mod-list maintainer's
  list, and `getAuthorFeed` returns their authored posts regardless of who
  they've blocked (we're querying as an anonymous AppView visitor)

## API surfaces used

- `app.bsky.feed.getAuthorFeed` — public AT Protocol AppView, walks a user's
  authored posts (auth-free)
- `app.bsky.graph.getList` — list metadata + paginated members (auth-free)
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
# 1. Find a community mod list naming accounts known for transphobic harassment.
#    Walk every member, harvest up to 5 posts per account, filter to ones
#    that mention typical hostile language. This is the highest-signal path.
python3 tools/find-fixtures/find_fixtures.py list \
    "https://bsky.app/profile/maintainer.bsky.social/lists/3abc..." \
    --total 80 --per-account 5 \
    --match "(tranny|groomer|woke)" > /tmp/t-raw.jsonl

# 2. Fill out edge cases / clean cases with backlinks of your own posts
python3 tools/find-fixtures/find_fixtures.py backlinks \
    "https://bsky.app/profile/chaosgreml.in/post/3xyz..." \
    --count 40 --include-quotes >> /tmp/edge-raw.jsonl

# 3. Hand-review, label (toxic / clean / uncertain), paraphrase any
#    distinctive multi-word phrases, then drop into the real fixtures.
$EDITOR /tmp/t-raw.jsonl
mv /tmp/t-raw.jsonl tests/fixtures/cope_b/known_toxic.jsonl
```
