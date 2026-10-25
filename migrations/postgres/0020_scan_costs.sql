-- Migration v20 (#393): one row per full scan or refresh recording its
-- estimated GPU cost. The figure is the in-app meter's worker-seconds
-- integral, a LOWER BOUND on the RunPod bill (it cannot see the idle tail
-- or cold start each worker is billed for). Cascaded by delete_user_data.
CREATE TABLE IF NOT EXISTS scan_costs (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_did TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('full', 'refresh')),
    outcome TEXT NOT NULL CHECK (outcome IN ('completed', 'paused', 'failed')),
    started_at TEXT NOT NULL,
    finished_at TEXT NOT NULL,
    classifications BIGINT NOT NULL,
    classifier_cache_hits BIGINT NOT NULL,
    metered_worker_secs DOUBLE PRECISION NOT NULL,
    peak_in_flight BIGINT NOT NULL,
    est_cents BIGINT NOT NULL,
    rate_cents_per_hour BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS scan_costs_user_started ON scan_costs (user_did, started_at);
CREATE INDEX IF NOT EXISTS scan_costs_started ON scan_costs (started_at);

-- The runner does NOT record the version for you. A migration that omits
-- this re-runs on every boot, forever.
INSERT INTO schema_version (version) VALUES (20) ON CONFLICT DO NOTHING;
