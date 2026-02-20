-- Charcoal PostgreSQL schema v3: behavioral signals
-- Stores JSON object with quote_ratio, reply_ratio, avg_engagement, etc.

ALTER TABLE account_scores ADD COLUMN IF NOT EXISTS behavioral_signals JSONB;

INSERT INTO schema_version (version) VALUES (3) ON CONFLICT DO NOTHING;
