-- #315: tier-based mute/block actions.
-- oauth_sessions: one write-scoped OAuth grant per user; every secret is an
--   AES-256-GCM blob (web::actions::crypto), never plaintext.
-- action_batches / actions: the user-facing log of everything Charcoal did on
--   the user's behalf, with score/tier snapshots so the log still explains
--   itself after later rescans. Timestamps are RFC3339 TEXT computed in Rust
--   (trait convention) except access_expires_at, which is unix seconds.
-- All three ARE cascaded by delete_user_data.

CREATE TABLE IF NOT EXISTS oauth_sessions (
    user_did TEXT PRIMARY KEY,
    pds_url TEXT NOT NULL,
    scope TEXT NOT NULL,
    access_token_enc BYTEA NOT NULL,
    refresh_token_enc BYTEA NOT NULL,
    dpop_key_enc BYTEA NOT NULL,
    access_expires_at BIGINT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS action_batches (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_did TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('mute','block','undo')),
    source TEXT NOT NULL,
    requested BIGINT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('queued','running','done','partial','failed')),
    error TEXT,
    created_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_action_batches_user_created
    ON action_batches (user_did, created_at);

CREATE TABLE IF NOT EXISTS actions (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    batch_id BIGINT NOT NULL REFERENCES action_batches(id),
    user_did TEXT NOT NULL,
    target_did TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('mute','block')),
    status TEXT NOT NULL CHECK (status IN ('pending','applied','skipped_already_done','failed','undone')),
    record_uri TEXT,
    undo_of BIGINT REFERENCES actions(id),
    error TEXT,
    score_at_action DOUBLE PRECISION,
    tier_at_action TEXT,
    applied_at TEXT,
    undone_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_actions_user_target_kind
    ON actions (user_did, target_did, kind);
CREATE INDEX IF NOT EXISTS idx_actions_batch ON actions (batch_id);

-- The runner does NOT record the version for you. A migration that omits
-- this re-runs on every boot, forever.
INSERT INTO schema_version (version) VALUES (15) ON CONFLICT DO NOTHING;
