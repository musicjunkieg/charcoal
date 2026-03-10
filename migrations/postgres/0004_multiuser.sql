-- Migration v4: multi-user schema
-- Adds user_did column to all data tables, creates users table.

CREATE TABLE IF NOT EXISTS users (
    did TEXT PRIMARY KEY,
    handle TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- topic_fingerprint: replace id-based singleton with user_did primary key
ALTER TABLE topic_fingerprint ADD COLUMN IF NOT EXISTS user_did TEXT NOT NULL DEFAULT '';
-- Backfill existing rows (legacy single-user data gets empty-string tenant)
UPDATE topic_fingerprint SET user_did = '' WHERE user_did = '';
-- Drop old primary key and create new one
ALTER TABLE topic_fingerprint DROP CONSTRAINT IF EXISTS topic_fingerprint_pkey;
ALTER TABLE topic_fingerprint ADD PRIMARY KEY (user_did);
-- Drop the old id column (no longer needed)
ALTER TABLE topic_fingerprint DROP COLUMN IF EXISTS id;
-- Remove default so future inserts without user_did fail hard
ALTER TABLE topic_fingerprint ALTER COLUMN user_did DROP DEFAULT;

-- account_scores: add user_did to composite key
ALTER TABLE account_scores ADD COLUMN IF NOT EXISTS user_did TEXT NOT NULL DEFAULT '';
ALTER TABLE account_scores DROP CONSTRAINT IF EXISTS account_scores_pkey;
ALTER TABLE account_scores ADD PRIMARY KEY (user_did, did);
ALTER TABLE account_scores ALTER COLUMN user_did DROP DEFAULT;

-- amplification_events: add user_did column
ALTER TABLE amplification_events ADD COLUMN IF NOT EXISTS user_did TEXT NOT NULL DEFAULT '';
ALTER TABLE amplification_events ALTER COLUMN user_did DROP DEFAULT;

-- scan_state: add user_did to composite key
ALTER TABLE scan_state ADD COLUMN IF NOT EXISTS user_did TEXT NOT NULL DEFAULT '';
ALTER TABLE scan_state DROP CONSTRAINT IF EXISTS scan_state_pkey;
ALTER TABLE scan_state ADD PRIMARY KEY (user_did, key);
ALTER TABLE scan_state ALTER COLUMN user_did DROP DEFAULT;

-- Rebuild indices with user_did
DROP INDEX IF EXISTS idx_events_amplifier;
CREATE INDEX idx_events_amplifier ON amplification_events(user_did, amplifier_did);
DROP INDEX IF EXISTS idx_scores_tier;
CREATE INDEX idx_scores_tier ON account_scores(user_did, threat_tier);
DROP INDEX IF EXISTS idx_scores_age;
CREATE INDEX idx_scores_age ON account_scores(user_did, scored_at);

INSERT INTO schema_version (version) VALUES (4) ON CONFLICT DO NOTHING;
