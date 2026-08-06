-- Migration v11: scan_queue — durable scan admission (#257).
--
-- Replaces ScanManager's process-local `any_running` bool. Admission becomes
-- "how many rows are running?", which survives a redeploy, is correct with more
-- than one replica, and makes queue position and ETA real rather than guessed.
--
-- user_did is the PK: one queued-or-running scan per user, so a double-click
-- cannot double-book. lease_expires is what makes a Railway redeploy safe — a
-- killed scan's lease lapses and the next boot re-queues it, and #208's
-- scan_phase means it resumes rather than restarts.

-- claim_id is a fencing token: each successful claim mints a fresh one and the
-- worker must present it to heartbeat or finish. Without it a worker whose
-- lease lapsed (so its row was reclaimed and handed to someone else) can still
-- mark the row done, freeing a slot the new worker is still occupying, or
-- extend the new worker's lease on its behalf. TEXT rather than UUID so both
-- backends carry the same Rust type; the value is generated database-side
-- (gen_random_uuid here, randomblob in SQLite).

CREATE TABLE IF NOT EXISTS scan_queue (
    user_did TEXT PRIMARY KEY,
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'done', 'failed')),
    enqueued_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    lease_expires TIMESTAMPTZ,
    last_error TEXT,
    claim_id TEXT
);

-- Admission scans for the oldest queued row and counts running rows; both are
-- hot on every admitter tick.
CREATE INDEX IF NOT EXISTS idx_scan_queue_status_enqueued
    ON scan_queue (status, enqueued_at);

INSERT INTO schema_version (version) VALUES (11) ON CONFLICT DO NOTHING;
