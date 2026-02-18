# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Added
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
- Recalibrate threat scoring for sentence embedding overlap scale (#44)
- Crash-resilient pipelines: incremental DB writes + panic catching (#33)
- Exclude protected user from their own threat report (#22)
- Support custom PDS endpoint for non-bsky.social accounts (#7)

### Changed
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
- Phase 7: Reports, markdown output, and polish (#6)
- Phase 5: Amplification detection pipeline (#5)
- Phase 4: Toxicity scoring with Perspective API (#4)
- Phase 3: Topic fingerprint with TF-IDF (#3)
- Phase 2: Bluesky auth + post fetching (#2)
- Phase 1: Project skeleton + config + database (#1)
