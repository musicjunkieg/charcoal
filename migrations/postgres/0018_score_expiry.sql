-- Migration v18 (#343 §4.4, #344): score expiry + refresh bookkeeping.
--
-- account_scores.scoring_generation / valid_until: fresh = generation is the
-- binary's scoring_revision() AND valid_until > NOW(). Pre-existing rows are
-- stamped 'legacy' (hidden at once) with valid_until = scored_at + 14 d so
-- the column can be NOT NULL. The DEFAULT is dropped after the backfill on
-- purpose: a writer that forgets the stamp must fail, not write 'legacy'.
--
-- users.refresh_attempted_generation / refreshed_generation stay NULL: the
-- tick treats a user with scores whose refresh_attempted_generation differs
-- from the current revision as due, so this deploy AND every later revision
-- change refresh users promptly, once (R07, V2-03). refreshed_generation is
-- the proof a completed refresh/full scan writes. No one-off time stamp.
--
-- scan_queue.kind: 'full' | 'refresh'. scan_queue.full_requested_at: a
-- full-scan request made while a refresh was running; the refresh's finish
-- turns the row into a queued full row dated by this column (R09).
--
-- scan_state.last_full_scan_finished_at is backfilled from every done queue
-- row (all pre-v18 rows are full scans) so the cooldown keeps its anchor
-- once a refresh reuses the row (R13).
--
-- topic_fingerprint.embedding_model_id: all-MiniLM-L6-v2 is the only model
-- that has ever produced a stored vector.

ALTER TABLE account_scores
    ADD COLUMN IF NOT EXISTS scoring_generation TEXT NOT NULL DEFAULT 'legacy';
ALTER TABLE account_scores ADD COLUMN IF NOT EXISTS valid_until TIMESTAMPTZ;
UPDATE account_scores SET valid_until = scored_at + INTERVAL '14 days' WHERE valid_until IS NULL;
ALTER TABLE account_scores ALTER COLUMN valid_until SET NOT NULL;
ALTER TABLE account_scores ALTER COLUMN scoring_generation DROP DEFAULT;

-- (user_did, threat_score): the refresh-candidate query is "this user, score
-- at or above the Elevated floor" with a small residual expiry/generation
-- filter, and ranked reads order by score. Plans recorded in the runbook.
CREATE INDEX IF NOT EXISTS idx_account_scores_user_score
    ON account_scores (user_did, threat_score);

ALTER TABLE scan_queue
    ADD COLUMN IF NOT EXISTS kind TEXT NOT NULL DEFAULT 'full'
    CHECK (kind IN ('full', 'refresh'));
ALTER TABLE scan_queue ADD COLUMN IF NOT EXISTS full_requested_at TIMESTAMPTZ;
ALTER TABLE scan_queue ADD COLUMN IF NOT EXISTS completion TEXT
    CHECK (completion IN ('complete', 'complete_with_skips', 'complete_unverified', 'resumable', 'failed'));

ALTER TABLE users ADD COLUMN IF NOT EXISTS next_refresh_at TIMESTAMPTZ;
ALTER TABLE users ADD COLUMN IF NOT EXISTS refreshed_generation TEXT;
ALTER TABLE users ADD COLUMN IF NOT EXISTS refresh_attempted_generation TEXT;

ALTER TABLE topic_fingerprint ADD COLUMN IF NOT EXISTS embedding_model_id TEXT;
UPDATE topic_fingerprint
    SET embedding_model_id = 'all-MiniLM-L6-v2'
    WHERE embedding_vector IS NOT NULL AND embedding_model_id IS NULL;

INSERT INTO scan_state (user_did, key, value)
    SELECT user_did, 'last_full_scan_finished_at', to_char(finished_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"+00:00"')
    FROM scan_queue
    WHERE status = 'done' AND finished_at IS NOT NULL
ON CONFLICT (user_did, key) DO NOTHING;

-- The runner does NOT record the version for you. A migration that omits
-- this re-runs on every boot, forever.
INSERT INTO schema_version (version) VALUES (18) ON CONFLICT DO NOTHING;
