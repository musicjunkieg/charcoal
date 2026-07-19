-- Migration v10: scan_skips — a durable record of accounts dropped from a scan.
--
-- A skipped account is a real gap in a scan's coverage. Until now the only
-- evidence was a WARN log line: #220 had to reconstruct which accounts were
-- dropped, and why, by grepping Railway logs, and #226 showed those logs are
-- not reliable — Railway drops messages by RATE, not severity, once a replica
-- exceeds 500 logs/sec, so WARN lines go over the side with the noise.
--
-- Mirrors the SQLite v10 migration in src/db/schema.rs. Uses TIMESTAMPTZ for
-- skipped_at where SQLite stores TEXT.
--
-- PK is (user_did, account_did, phase): a re-gather that fails again updates
-- rather than duplicates, so the count keeps meaning "accounts missing from
-- this scan". The same account failing at two phases is two distinct facts.

CREATE TABLE IF NOT EXISTS scan_skips (
    user_did TEXT NOT NULL,
    account_did TEXT NOT NULL,
    phase TEXT NOT NULL,
    error TEXT NOT NULL,
    skipped_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_did, account_did, phase)
);

CREATE INDEX IF NOT EXISTS idx_scan_skips_user ON scan_skips (user_did);

INSERT INTO schema_version (version) VALUES (10) ON CONFLICT DO NOTHING;
