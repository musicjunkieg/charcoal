-- #309: access_requests — DB-backed allowlist for gated onboarding.
-- One row per DID, ever. 'denied' covers both "denied from waitlist" and
-- "revoked after having access". Timestamps are RFC3339 TEXT computed in
-- Rust, matching the trait convention, so both backends store identical
-- values and parity tests compare strings directly.
-- NOT cascaded by delete_user_data: admin grant/deny record, not user content.

CREATE TABLE IF NOT EXISTS access_requests (
    did TEXT PRIMARY KEY,
    handle TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending','allowed','denied')),
    requested_at TEXT NOT NULL,
    decided_at TEXT,
    decided_by TEXT
);

-- The runner does NOT record the version for you. A migration that omits
-- this re-runs on every boot, forever.
INSERT INTO schema_version (version) VALUES (14) ON CONFLICT DO NOTHING;
