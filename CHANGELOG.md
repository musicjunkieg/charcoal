# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Added
- v0.4 AT Protocol OAuth: replace password auth with Bluesky sign-in (#50, #95)
  - PAR + PKCE + DPoP + private_key_jwt via `atproto-oauth` crate
  - DID-embedded session cookies with CHARCOAL_ALLOWED_DID gate
  - Stable P-256 signing key derived from CHARCOAL_SESSION_SECRET
  - AT Protocol tokens stored in-memory for future XRPC calls

### Changed
- Multi-user schema redesign (per-user vs shared data) (#49)

### Fixed
- Fix ambiguous test count wording and decision graph status typo (#104)
- Thread authenticated actor handle through web scan job (#102)
- Fix second round of CodeRabbit findings on PR #14 (#101)
- Fix CodeRabbit review findings on PR #14 (multi-user schema) (#100)
- Update CLAUDE.md test counts to note web feature gate (#98)
- Fix PR #13 review round 2 findings (4 items) (#97)
- Fix PR #13 review findings (7 items) (#96)
- Session cookies: startup fails with clear message if CHARCOAL_ALLOWED_DID, CHARCOAL_OAUTH_CLIENT_ID, or CHARCOAL_SESSION_SECRET are missing or too short

## [0.3.0] - 2026-03-07

### Security
- Fix inverted credential redaction in migrate command display (#78)
- Constant-time password comparison in login handler — prevents timing oracle on password length (#102)
- Reject future-dated session tokens using checked_sub (#101)
- Remove HMAC fallback to hardcoded key; panic on misconfiguration (#101)

### Added
- v0.3 web GUI: Axum API server + SvelteKit dashboard (login, dashboard, accounts, events, fingerprint, scan trigger) (#80–#86, #95, #97)
- Railway deployment configuration with Railpack (#87)
- Scan progress display with elapsed time counter (#95)
- Scan button disabled while scan is running (#97)

### Fixed
- Return 500 on corrupt fingerprint JSON instead of silently coercing to null (#102)
- Session cookies: startup fails with clear message if CHARCOAL_WEB_PASSWORD or CHARCOAL_SESSION_SECRET are missing or too short (#101)
- Lock held across DB await in status handler — snapshot fields before releasing the read guard (#101)
- ONNX and embedder model loads wrapped in spawn_blocking to avoid blocking async runtime (#101)

### Changed
- Axum web server skeleton (Railway deployment) (#51)
- Update CLAUDE.md and CHANGELOG for v0.3 web GUI merge (#93)
- Allow git stash in hook-config (#94)

## [0.2.0] - 2026-02-20

### Added
- Display behavioral signals in threat reports (#67)
- Behavioral signals: reply ratio, quote ratio, pile-on detection (#54)
- Add validate command: score blocked accounts to verify pipeline accuracy (#63)
- Refactor Bluesky client to use public AT Protocol API without authentication (#62)
- Constellation backlink index for supplementary amplification detection (#35, #53)
- Batch DID→handle resolution via getProfiles for Constellation events (#58)
- Sentence embeddings for semantic topic overlap (all-MiniLM-L6-v2) (#34)
- Wire embedding-based overlap into profile scoring pipeline (#40)
- Store embedding vectors in DB and update fingerprint command (#39)
- Create SentenceEmbedder with ONNX inference + mean pooling (#38)
- Add sentence embedding model download (all-MiniLM-L6-v2) (#37)
- Reweight toxicity categories to reduce ally false positives (#31)
- Replace weighted Jaccard with cosine similarity for topic overlap (#30)
- Mode 2: Background sweep of followers-of-followers (#25)
- Surface quote text and toxicity in threat reports (#21)
- Tune threat tier thresholds for real-world score distribution (#8)

### Fixed
- post_count u32-to-i32 cast could overflow (#75)
- save_embedding silently fails if no fingerprint row exists (#71)
- Fix critical/high code review findings from PR #6 (#70)
- Recalibrate threat scoring for sentence embedding overlap scale (#44)
- Crash-resilient pipelines: incremental DB writes + panic catching (#33)
- Exclude protected user from their own threat report (#22)
- Support custom PDS endpoint for non-bsky.social accounts (#7)

### Changed
- Harden workflow: atomic commits, branch protections, issue and graph persistence (#88)
- sqlite feature flag now correctly gates sqlite-related code (#76)
- Postgres integration tests now clean up after themselves (#74)
- Document pgvector CREATE EXTENSION superuser requirement (#73)
- Add advisory lock for concurrent migration protection (#72)
- Optimize pile-on detection from O(n^2) to O(n) sliding window (#68)
- Test SQLite-to-PostgreSQL migration end-to-end (#69)
- Database migration: SQLite to PostgreSQL (#48)
- Adapt scoring formula for multi-component signals (#56)
- Organize generated files into gitignored directories (#65)
- Research AT Protocol public API authentication requirements (#61)
- Write architectural recommendations for multi-user migration (#47)
- Update docs and close session for sentence embeddings work (#42)
- Add tests for embedding DB queries, migration, and download helpers (#41)
- Update CLAUDE.md and docs to reflect contributor changes and new tests (#36)
- Increase posts per account from 20 to 50 for more stable fingerprints (#32)
- Skip follower analysis for repost events (#27)
- Stop tracking chainlink issues.db and .cache in git (#26)
- Replace dummy Perspective scorer with proper no-op (#24)
- Wire up --since flag or remove it (#23)
- Design repost scoring strategy — score all vs sample vs limit (#12)
- Cosmetic cleanup — update comments referencing Perspective as primary scorer (#14)
- Clean up CLAUDE.md and create README.md (#20)
- Add progress bar to parallel scoring (#18)
- Refactor scoring loop to use buffer_unordered (#17)
- Add --concurrency CLI flag to scan command (#16)
- Add futures dependency to Cargo.toml (#15)
- Scale scan pipeline — reduce per-account latency and support larger networks (#10)
- Close rate limiter issue as moot — ONNX scorer has no API rate limits (#9)
- Select and implement Perspective API replacement (#13)
- Research alternative toxicity scoring APIs (Perspective sunsetting Dec 2026) (#11)

## [0.1.0] - 2026-01-31

### Added
- Phase 7: Reports, markdown output, and polish (#6)
- Phase 6: Profile scoring and threat tiers
- Phase 5: Amplification detection pipeline (#5)
- Phase 4: Toxicity scoring with Perspective API (#4)
- Phase 3: Topic fingerprint with TF-IDF (#3)
- Phase 2: Bluesky auth + post fetching (#2)
- Phase 1: Project skeleton + config + database (#1)
