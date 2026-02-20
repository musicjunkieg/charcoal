-- Charcoal PostgreSQL schema v2: pgvector embedding column
-- Stores the protected user's mean sentence embedding (384-dim)

ALTER TABLE topic_fingerprint ADD COLUMN IF NOT EXISTS embedding_vector vector(384);

INSERT INTO schema_version (version) VALUES (2) ON CONFLICT DO NOTHING;
