-- #343, PR #118 review: bound the v16 cache tables.
--
-- Nothing else deletes from account_feed_snapshots / onnx_scores /
-- classifier_verdicts, so without a retention sweep every distinct DID and
-- every text hash a model or policy generation ever saw is permanent.
-- `evict_stale_cache` filters on the timestamp columns (RFC3339 TEXT computed
-- in Rust — lexicographic comparison is correct for fixed-width UTC offsets);
-- these indexes keep that sweep off a full table scan as the cache grows.

CREATE INDEX IF NOT EXISTS idx_account_feed_snapshots_fetched_at
    ON account_feed_snapshots (fetched_at);

CREATE INDEX IF NOT EXISTS idx_onnx_scores_scored_at
    ON onnx_scores (scored_at);

CREATE INDEX IF NOT EXISTS idx_classifier_verdicts_classified_at
    ON classifier_verdicts (classified_at);

-- The runner does NOT record the version for you. A migration that omits
-- this re-runs on every boot, forever.
INSERT INTO schema_version (version) VALUES (17) ON CONFLICT DO NOTHING;
