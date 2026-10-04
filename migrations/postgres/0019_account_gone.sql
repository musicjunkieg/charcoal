-- Migration v19 (#394): a deleted/deactivated account is retired from the
-- nightly refresh.
--
-- gone_at / gone_reason are stamped when a feed fetch answers with a
-- permanent "this account is gone" 400 (Profile not found, AccountDeactivated,
-- AccountTakedown, ...). The refresh-candidate query excludes stamped rows;
-- without that, every refresh re-fetched them, failed, rescheduled itself an
-- hour later and woke the GPU each time. NULL for every existing row: nothing
-- is known to be gone yet, and the first refresh after the deploy finds out.
-- Scoring the account again clears both, so the stamp is reversible.

ALTER TABLE account_scores ADD COLUMN IF NOT EXISTS gone_at TIMESTAMPTZ;
ALTER TABLE account_scores ADD COLUMN IF NOT EXISTS gone_reason TEXT;

-- The runner does NOT record the version for you. A migration that omits
-- this re-runs on every boot, forever.
INSERT INTO schema_version (version) VALUES (19) ON CONFLICT DO NOTHING;
