# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Added
- Tune threat tier thresholds for real-world score distribution (#8)

### Fixed
- Support custom PDS endpoint for non-bsky.social accounts (#7)

### Changed
- Select and implement Perspective API replacement (#13)
- Fix Perspective API rate limiting to stay under 60 req/min quota (#9)
- Research alternative toxicity scoring APIs (Perspective sunsetting Dec 2026) (#11)
- Phase 7: Reports, markdown output, and polish (#6)
- Phase 5: Amplification detection pipeline (#5)
- Phase 4: Toxicity scoring with Perspective API (#4)
- Phase 3: Topic fingerprint with TF-IDF (#3)
- Phase 2: Bluesky auth + post fetching (#2)
- Phase 1: Project skeleton + config + database (#1)
