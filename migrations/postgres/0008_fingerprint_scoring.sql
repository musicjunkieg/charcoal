-- Migration v8: add fingerprint_quality and scoring_confidence to account_scores.
--
-- fingerprint_quality tracks whether the topic fingerprint was built from
-- originals only (normal), mixed (degraded), or insufficient data (unreliable).
-- scoring_confidence tracks the depth of analysis (low/standard/high) so
-- borderline accounts can be re-scored sooner.
--
-- Mirrors the SQLite v8 migration in src/db/schema.rs. IF NOT EXISTS makes
-- this idempotent — Postgres errors out on duplicate column ADDs otherwise.

ALTER TABLE account_scores ADD COLUMN IF NOT EXISTS fingerprint_quality TEXT;
ALTER TABLE account_scores ADD COLUMN IF NOT EXISTS scoring_confidence TEXT;

INSERT INTO schema_version (version) VALUES (8) ON CONFLICT DO NOTHING;
