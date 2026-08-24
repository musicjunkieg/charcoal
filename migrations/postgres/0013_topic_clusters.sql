-- #297/#302: one row per protected-user topic centroid, plus the
-- shadow-compare column for #135 recalibration data.
--
-- cluster_index is 0-based and corresponds 1:1 with clusters[i] in the
-- fingerprint JSON — save_fingerprint_bundle writes both in one transaction,
-- so they cannot diverge (#302).

CREATE TABLE IF NOT EXISTS topic_clusters (
    user_did TEXT NOT NULL REFERENCES topic_fingerprint(user_did) ON DELETE CASCADE,
    cluster_index INTEGER NOT NULL,
    centroid vector(384) NOT NULL,
    post_count INTEGER NOT NULL,
    PRIMARY KEY (user_did, cluster_index)
);

ALTER TABLE account_scores ADD COLUMN IF NOT EXISTS overlap_legacy DOUBLE PRECISION;

-- The runner does NOT record the version for you. A migration that omits
-- this re-runs on every boot, forever.
INSERT INTO schema_version (version) VALUES (13) ON CONFLICT DO NOTHING;
