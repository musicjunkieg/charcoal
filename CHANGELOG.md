# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Added
- Tune threat tier thresholds for real-world score distribution (#8)

### Fixed
- Support custom PDS endpoint for non-bsky.social accounts (#7)

### Changed
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
