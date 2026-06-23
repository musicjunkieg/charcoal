-- Migration v9: add classification_queue and scan_account_input tables.
--
-- classification_queue holds posts staged for remote classifier review.
-- scan_account_input holds serialised ScanAccount structs for pipeline
-- replay / envelope-aware clean-pass splitting.
--
-- Mirrors the SQLite v9 migration in src/db/schema.rs. Uses Postgres types:
--   DOUBLE PRECISION for onnx_score (f64), REAL for confidence (f32),
--   BOOLEAN for toxic_token, JSONB for payload_json.

CREATE TABLE IF NOT EXISTS classification_queue (
    user_did TEXT NOT NULL,
    account_did TEXT NOT NULL,
    post_uri TEXT NOT NULL,
    text TEXT NOT NULL,
    context_text TEXT,
    post_kind TEXT NOT NULL,
    onnx_score DOUBLE PRECISION NOT NULL,
    status TEXT NOT NULL,
    toxic_token BOOLEAN,
    confidence REAL,
    model_id TEXT,
    policy_version TEXT,
    PRIMARY KEY (user_did, account_did, post_uri)
);

CREATE INDEX IF NOT EXISTS idx_clsq_pending ON classification_queue (user_did, status);

CREATE TABLE IF NOT EXISTS scan_account_input (
    user_did TEXT NOT NULL,
    account_did TEXT NOT NULL,
    payload_json JSONB NOT NULL,
    PRIMARY KEY (user_did, account_did)
);

INSERT INTO schema_version (version) VALUES (9) ON CONFLICT DO NOTHING;
