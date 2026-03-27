-- Migration 0007: Add last_login_at column to users table
-- Tracks when each user last authenticated via OAuth, used by admin dashboard.

ALTER TABLE users ADD COLUMN IF NOT EXISTS last_login_at TIMESTAMPTZ;

INSERT INTO schema_version (version) VALUES (7);
