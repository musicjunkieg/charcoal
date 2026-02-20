-- Charcoal PostgreSQL schema v1: initial tables
-- Equivalent to the SQLite schema created by src/db/schema.rs

CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS topic_fingerprint (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    fingerprint_json TEXT NOT NULL,
    post_count INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS account_scores (
    did TEXT PRIMARY KEY,
    handle TEXT NOT NULL,
    toxicity_score DOUBLE PRECISION,
    topic_overlap DOUBLE PRECISION,
    threat_score DOUBLE PRECISION,
    threat_tier TEXT,
    posts_analyzed INTEGER NOT NULL DEFAULT 0,
    top_toxic_posts JSONB,
    scored_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS amplification_events (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    event_type TEXT NOT NULL,
    amplifier_did TEXT NOT NULL,
    amplifier_handle TEXT NOT NULL,
    original_post_uri TEXT NOT NULL,
    amplifier_post_uri TEXT,
    amplifier_text TEXT,
    detected_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    followers_fetched BOOLEAN NOT NULL DEFAULT FALSE,
    followers_scored BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE TABLE IF NOT EXISTS scan_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_events_amplifier ON amplification_events(amplifier_did);
CREATE INDEX IF NOT EXISTS idx_scores_tier ON account_scores(threat_tier);
CREATE INDEX IF NOT EXISTS idx_scores_age ON account_scores(scored_at);

INSERT INTO schema_version (version) VALUES (1) ON CONFLICT DO NOTHING;
