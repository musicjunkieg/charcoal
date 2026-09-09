-- #343 §4.1: the shared cache. No user_did on any of these — a post's
-- toxicity is a property of the post, so one user's scan serves the next.
-- onnx_scores / classifier_verdicts are keyed by the SHA-256 (hex) of the
-- exact text the model saw: stage 1 scores raw text, the clean pass scores
-- the "[Parent post]: …\n\n[Reply]: …" envelope, so post_uri is not a valid
-- key. They store no readable text. NOT cascaded by delete_user_data.
-- Timestamps are RFC3339 TEXT computed in Rust (trait convention).

CREATE TABLE IF NOT EXISTS account_feed_snapshots (
    did TEXT PRIMARY KEY,
    handle TEXT NOT NULL,
    posts_json TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    source TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS onnx_scores (
    text_sha256 TEXT NOT NULL,
    model_id TEXT NOT NULL,
    score DOUBLE PRECISION NOT NULL,
    scored_at TEXT NOT NULL,
    PRIMARY KEY (text_sha256, model_id)
);

CREATE TABLE IF NOT EXISTS classifier_verdicts (
    text_sha256 TEXT NOT NULL,
    model_id TEXT NOT NULL,
    policy_version TEXT NOT NULL,
    toxic_token BOOLEAN NOT NULL,
    confidence DOUBLE PRECISION NOT NULL,
    classified_at TEXT NOT NULL,
    PRIMARY KEY (text_sha256, model_id, policy_version)
);

-- The runner does NOT record the version for you. A migration that omits
-- this re-runs on every boot, forever.
INSERT INTO schema_version (version) VALUES (16) ON CONFLICT DO NOTHING;
