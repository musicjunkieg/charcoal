-- Schema v5: Contextual scoring support
--
-- Adds context_score columns, user_labels table, and inferred_pairs table.
-- Part of Phase 1.75 (contextual hostility scoring).

-- New columns on existing tables
ALTER TABLE amplification_events ADD COLUMN IF NOT EXISTS original_post_text TEXT;
ALTER TABLE amplification_events ADD COLUMN IF NOT EXISTS context_score DOUBLE PRECISION;

ALTER TABLE account_scores ADD COLUMN IF NOT EXISTS context_score DOUBLE PRECISION;

-- User-provided labels for ground-truth accuracy measurement
CREATE TABLE IF NOT EXISTS user_labels (
    user_did TEXT NOT NULL,
    target_did TEXT NOT NULL,
    label TEXT NOT NULL,
    labeled_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    notes TEXT,
    PRIMARY KEY (user_did, target_did)
);

-- Topic-matched post pairs for NLI scoring
CREATE TABLE IF NOT EXISTS inferred_pairs (
    id BIGSERIAL PRIMARY KEY,
    user_did TEXT NOT NULL,
    target_did TEXT NOT NULL,
    target_post_text TEXT NOT NULL,
    target_post_uri TEXT NOT NULL,
    user_post_text TEXT NOT NULL,
    user_post_uri TEXT NOT NULL,
    similarity DOUBLE PRECISION NOT NULL,
    context_score DOUBLE PRECISION,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_inferred_pairs_target
    ON inferred_pairs(user_did, target_did);
CREATE UNIQUE INDEX IF NOT EXISTS idx_inferred_pairs_dedup
    ON inferred_pairs(user_did, target_did, target_post_uri, user_post_uri);

-- Record migration
INSERT INTO schema_version (version) VALUES (5);
