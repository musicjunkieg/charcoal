-- #343, PR #118 review: bound the v16 cache tables.
--
-- Nothing else deletes from account_feed_snapshots / onnx_scores /
-- classifier_verdicts, so without a retention sweep every distinct DID and
-- every text hash a model or policy generation ever saw is permanent.
-- `evict_stale_cache` filters on the timestamp columns (RFC3339 TEXT, always
-- written by `DateTime<Utc>::to_rfc3339()` so the text order matches time
-- order — see the trait doc on `Database::evict_stale_cache`); these indexes
-- keep that sweep off a full table scan as the cache grows.
--
-- Plain CREATE INDEX, not CONCURRENTLY, on purpose: v16 (the tables) and v17
-- (these indexes) ship in the same release, migrations run inside db::open()
-- before the server accepts traffic, and the runner wraps each migration in a
-- transaction (CONCURRENTLY cannot run in one). Every deployment that reaches
-- this file builds the indexes on empty tables, so there are no writers to
-- block. Revisit only if a later migration indexes a populated cache table.

CREATE INDEX IF NOT EXISTS idx_account_feed_snapshots_fetched_at
    ON account_feed_snapshots (fetched_at);

CREATE INDEX IF NOT EXISTS idx_onnx_scores_scored_at
    ON onnx_scores (scored_at);

CREATE INDEX IF NOT EXISTS idx_classifier_verdicts_classified_at
    ON classifier_verdicts (classified_at);

-- The runner does NOT record the version for you. A migration that omits
-- this re-runs on every boot, forever.
INSERT INTO schema_version (version) VALUES (17) ON CONFLICT DO NOTHING;
