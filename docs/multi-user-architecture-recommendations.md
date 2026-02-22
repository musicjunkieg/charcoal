# Multi-User Architecture Recommendations

**Date:** 2026-02-18
**Status:** Active — review before starting each milestone
**Deciduous nodes:** 86-90 (decisions), edges 81-84
**Related issues:** #35, #47-#56

## Context

Charcoal's MVP is a single-user CLI tool. The next phase moves it toward a
multi-user web service where Bluesky users sign up via OAuth and get their own
threat analysis. This document captures architectural decisions and
recommendations made before starting that work, based on research into
Constellation API, AT Protocol OAuth, and the original architecture seed
(`docs/charcoal-architecture-seed.md`).

---

## 1. Platform: Railway + Axum + PostgreSQL

**Decision:** Use Railway for hosting, Axum for the web framework, and
PostgreSQL for the database. This replaces the architecture seed's assumption
of Cloudflare Workers + D1 + KV.

**Why this is better for Charcoal:**

| Concern | Workers + D1 | Railway + Axum |
|---------|-------------|----------------|
| ONNX models (~300MB) | Cold start penalty, no persistent memory | Load once at startup, stay resident |
| Background jobs | Cron triggers, limited runtime | Native async tasks, no time limits |
| Concurrency | Isolate-per-request, no shared state | Long-running process, shared state |
| Rust ecosystem | Limited Workers Rust support | Full Rust async ecosystem |
| Database | D1 (SQLite-at-edge), limited | Managed PostgreSQL, full SQL |

**Action items:**
- Keep SQLite for local development/CLI mode (useful for contributors)
- Use PostgreSQL via `sqlx` for the server deployment
- Railway's cheapest tier may be tight with models loaded; plan for 1GB+ RAM

## 2. Database Migration: SQLite to PostgreSQL

**Decision:** Replace `rusqlite` with `sqlx` when building the web service.
Don't try to support both simultaneously in production.

**Key changes:**
- `rusqlite` → `sqlx` with compile-time checked queries
- `i64` timestamps → `TIMESTAMPTZ` native Postgres type
- `TEXT` JSON blobs → `JSONB` for queryable structured data
- `BLOB` embedding vectors → `bytea` (or consider `pgvector` extension for
  native vector operations if Railway supports it)
- All migrations via `sqlx migrate` instead of hand-written SQL

**Timing:** Do this when the Axum skeleton is ready (#51), not before.
The current SQLite schema works fine for CLI mode and Constellation integration.

**Risk:** `pgvector` availability on Railway is unconfirmed as of 2026-02-18.
If not available, `bytea` with application-level cosine similarity works fine
(we already do this). Research pgvector on Railway before committing to it.

## 3. Multi-User Schema Design

**Decision:** Split data into shared (global) and per-user tables.

```
-- Shared across all users (an account's toxicity doesn't depend on who's asking)
profiles:        did, handle, toxicity_score, embedding_vector, last_scored_at
topic_keywords:  did, keywords_json, scored_at

-- Per protected user (threat depends on topic overlap with THIS user)
protected_users: did, handle, oauth_tokens_encrypted, settings_json, created_at
threat_assessments: protected_user_did, account_did, topic_overlap,
                    threat_score, threat_tier, scored_at
actions:         protected_user_did, account_did, action_type, user_verdict,
                 executed_at

-- Shared intelligence (derived from multiple users' actions)
network_flags:   account_did, flag_type, contributor_count, confidence, updated_at
```

**Key insight:** Toxicity and embeddings are properties of the *account*, not
the relationship. An account's toxicity score is the same regardless of who's
evaluating them. But threat scores depend on topic overlap with a *specific*
protected user — so they're per-user.

**Timing:** Design this schema when starting multi-user (#49). The current
single-user schema doesn't need to change for Constellation integration.

## 4. Constellation API Integration

**Decision:** Use official Bluesky APIs as primary, Constellation as
supplementary enrichment.

**Research findings (2026-02-18):**

- **Constellation** (microcosm.blue): Third-party backlink index. Live but
  early/beta. Provides "who engaged with this post?" queries. No auth required,
  no documented rate limits. Historical backfill incomplete.
- **Official Bluesky APIs:**
  - `app.bsky.feed.getQuotes` — full historical quote data
  - `app.bsky.feed.getRepostedBy` — full historical repost data
  - Already authenticated via bsky-sdk in our codebase

**Recommendation:** Start with official APIs for reliable historical data.
Add Constellation as a secondary source for discovering engagement we wouldn't
see through follows (the "exposure graph" from the architecture seed). Don't
depend on Constellation as the sole data source given its beta status.

**Risk:** Constellation's backfill may never be complete, or the service may
change. Keep it as an enrichment layer, not a dependency.

## 5. AT Protocol OAuth

**Decision:** Use `atrium-oauth` v0.1.6 + `atproto-oauth-axum` for web
authentication.

**Research findings (2026-02-18):**

- AT Protocol uses OAuth 2.1 with mandatory PKCE, DPoP (proof-of-possession
  tokens), and PAR (pushed authorization requests)
- `atrium-oauth` v0.1.6 handles the protocol-level OAuth flow in Rust
- `atproto-oauth-axum` provides ready-made Axum route handlers for login
- bsky-sdk 0.1.23 does NOT support OAuth natively (uses app passwords)
- Each user's OAuth tokens need encrypted storage in Postgres

**Implementation path:**
1. Add Axum server with `atproto-oauth-axum` routes
2. Store encrypted tokens in `protected_users` table
3. Use tokens to call Bluesky APIs on behalf of each user
4. Token refresh handling (AT Protocol OAuth tokens expire)

**Risk:** `atrium-oauth` is v0.1.x — API may change. Pin the version and
monitor for breaking changes. The crate is from the same team as `atrium-api`
(which we already depend on), so it's likely to stay maintained.

## 6. ONNX Models in Server Context

**Current state:** ~300MB total (126MB toxicity + 90MB embeddings). Models
load once, inference uses `Mutex<ort::Session>` with `spawn_blocking`.

**Multi-user concerns:**
- `Mutex` serializes inference — only one request at a time. For multi-user,
  consider a pool of sessions or `tokio::sync::Semaphore` to control concurrency
- Memory: both models resident = ~300MB baseline. Add Postgres connections,
  web server overhead, and per-request allocations — 1GB RAM minimum
- Cold start: models take 2-3 seconds to load. Fine for a long-running server,
  but handle gracefully (health check endpoint, readiness probe)

**Recommendation:** Keep the current `Mutex` approach until we see actual
contention. Premature optimization here isn't worth it — a semaphore with
N=2 or N=4 concurrent sessions is the obvious next step if needed.

## 7. Scoring Formula Evolution

**Current multiplicative formula:**
```
score = toxicity * 70 * (1 + overlap * 1.5)
```

**Architecture seed's vision (4-component):**
```
35% toxicity + 20% topic overlap + 20% behavioral signals + 25% network flags
```

**Recommendation:** When adding behavioral signals and network flags, keep
the multiplicative principle. These new signals should amplify the toxicity
signal, not contribute independently. An account with suspicious behavior
patterns but zero toxicity is not a threat — they're just active.

**Proposed evolution:**
```
base = toxicity * toxicity_weight
overlap_amp = 1 + overlap * overlap_multiplier
behavior_amp = 1 + behavior_signal * behavior_multiplier
network_amp = 1 + network_flag * network_multiplier
score = base * overlap_amp * behavior_amp * network_amp
```

This preserves the key insight: toxicity is the necessary condition. Everything
else amplifies or attenuates it.

**Timing:** Don't implement this until behavioral signals (#54) and shared
intelligence (#55) exist. The current 2-component formula is correct for
what we have today.

## 8. Implementation Order

**Recommended sequence (each builds on the previous):**

1. **Constellation API integration** (#53, #35) — add as a data source,
   still single-user CLI. Low risk, immediate value.
2. **Axum web skeleton** (#51) — basic server, health check, serve existing
   report as HTML. Railway deployment.
3. **OAuth integration** (#50) — user sign-up via Bluesky account.
4. **Database migration** (#48) — SQLite → PostgreSQL for server mode.
5. **Multi-user schema** (#49) — per-user threat assessments, shared profiles.
6. **ONNX concurrency** (#52) — optimize inference for multiple users.
7. **Behavioral signals** (#54) — reply ratio, quote ratio, pile-on detection.
8. **Scoring formula update** (#56) — incorporate new signal types.
9. **Shared intelligence** (#55) — cross-user network flags. Needs multiple
   active users first.

**Why this order:**
- Steps 1-2 are independently valuable and don't require schema changes
- OAuth and database migration are prerequisites for multi-user
- Behavioral signals and shared intelligence need the multi-user foundation
- Each step is testable and deployable independently

---

## Appendix: Crate Versions Researched

As of 2026-02-18:
- `atrium-oauth` v0.1.6 — AT Protocol OAuth client
- `atproto-oauth-axum` — Axum integration for AT Protocol OAuth
- `axum` — latest stable for web framework
- `sqlx` — async database driver with compile-time query checking
- `ort` 2.0.0-rc.11 — ONNX Runtime (already in use)
- `bsky-sdk` 0.1.23 — does NOT support OAuth (app password only)

## Appendix: Architecture Seed Divergences

The original architecture seed (`docs/charcoal-architecture-seed.md`) was
written before the MVP was built. Key divergences to note:

| Seed assumption | Current reality |
|----------------|-----------------|
| Cloudflare Workers | Railway + Axum |
| D1 database | PostgreSQL |
| KV cache | In-memory or Redis (TBD) |
| Additive scoring (4 components) | Multiplicative (toxicity-gated) |
| Perspective API primary scorer | ONNX local models primary |
| TF-IDF topic overlap | Sentence embeddings (all-MiniLM-L6-v2) |
| Spacedust firehose | Deferred to later phase |
| Durable Objects | Not needed (persistent server) |

The seed's *concepts* (exposure graph, shared intelligence, behavioral signals,
action execution with user review) remain valid. The *implementation details*
have evolved based on what we learned building the MVP.
