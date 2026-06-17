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
    // Per spec: RunPod A100 80GB = $2.72/hr ~= 0.0756 cents/sec ~= 7.56e-5 cents/ms.
    // Zentropi: hosted, billed per-call — return 0 here; per-call billing tracking
    // happens at a different layer.
    match backend {
        "runpod-cope-b" => ((elapsed_ms as f64) * 7.56e-5).round() as u32,
        _ => 0,
    }
}
