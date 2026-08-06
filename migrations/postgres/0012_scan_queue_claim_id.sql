-- Migration v12: backfill scan_queue.claim_id (#257).
--
-- v11 was amended in place to add the `claim_id` fencing token while #257 was
-- still on its branch. That was reasonable — v11 had never shipped — but it is
-- not sufficient: a database created from the PRE-amendment v11 has a
-- `scan_queue` table with no `claim_id` column AND version 11 already recorded
-- in schema_version, so the migration runner skips 0011 entirely and the column
-- is never added. Every claim_next_scan / heartbeat_scan / finish_queued_scan
-- then fails at runtime with a missing-column error.
--
-- Amending 0011 cannot repair that, because 0011 is exactly the migration those
-- databases no longer run. Only a NEW version does. No deployed environment is
-- affected (staging and production both stopped at v10 with no scan_queue
-- table); the exposed population is developer machines that ran this branch
-- between 7e8ec24 and 26b1830.
--
-- On a fresh database this is a clean no-op: v11 runs first and already creates
-- the column, so ADD COLUMN IF NOT EXISTS finds nothing to do.

ALTER TABLE scan_queue ADD COLUMN IF NOT EXISTS claim_id TEXT;

-- The runner does NOT record the version for you. A migration that omits this
-- re-runs on every boot, forever.
INSERT INTO schema_version (version) VALUES (12) ON CONFLICT DO NOTHING;
