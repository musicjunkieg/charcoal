//! Per-classification metrics emitted via `tracing::info!`.
//!
//! Backend identity is carried in the `backend` label so the same metric
//! names cover both runpod and zentropi backends — see spec §"Monitoring"
//! for the full list. Aggregation into per-scan totals happens in
//! `src/web/scan_job.rs` (see Chunk 7 staging metrics surfacing).

use tracing::info;

pub fn record_request(backend: &str, latency_ms: u32, toxic: bool, retries: u32) {
    info!(
        metric = "classifier_request_latency_ms",
        backend = backend,
        latency_ms = latency_ms,
        toxic = toxic,
    );
    info!(
        metric = "classifier_classification_count",
        backend = backend,
        toxic = toxic,
    );
    if retries > 0 {
        info!(
            metric = "classifier_retry_count",
            backend = backend,
            count = retries
        );
    }
}

/// RunPod's own queue/inference split from the job envelope (`delayTime` /
/// `executionTime`), alongside the client-measured wall clock. Distinguishes
/// "too few workers" (high delay) from "slow inference" (high execution).
/// Emitted exactly once per terminal completion, from `parse_job`. Fields are
/// `None` when RunPod omits them (e.g. on some non-terminal or legacy responses).
pub fn record_runpod_timing(
    delay_time_ms: Option<u32>,
    execution_time_ms: Option<u32>,
    wall_clock_ms: u32,
) {
    info!(
        metric = "classifier_runpod_timing",
        backend = "runpod-cope-b",
        delay_time_ms = ?delay_time_ms,
        execution_time_ms = ?execution_time_ms,
        wall_clock_ms = wall_clock_ms,
    );
}

pub fn record_cold_start(backend: &str, latency_ms: u32) {
    info!(
        metric = "classifier_cold_start_detected",
        backend = backend,
        latency_ms = latency_ms
    );
}

pub fn record_backend_selected(backend: &str) {
    info!(
        metric = "classifier_backend_selected_total",
        backend = backend
    );
}

pub fn estimate_cost_cents(backend: &str, elapsed_ms: u32) -> u32 {
    // RunPod: busy-time informational estimate at the backstop's default rate
    // (cost_meter::DEFAULT_RATE_CENTS_PER_HOUR). NOTE: this is busy-latency, not
    // worker-uptime — the real guard lives in ScanCostMeter. Non-runpod = 0.
    match backend {
        "runpod-cope-b" => {
            let per_ms =
                crate::toxicity::cost_meter::DEFAULT_RATE_CENTS_PER_HOUR as f64 / 3_600_000.0;
            (elapsed_ms as f64 * per_ms).round() as u32
        }
        _ => 0,
    }
}
