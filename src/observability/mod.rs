//! Observability helpers — structured metric emission via `tracing`.
//!
//! Metrics are emitted as `tracing::info!` events whose first field is
//! `metric = "<name>"`, so a log scraper can aggregate without parsing
//! human-readable message strings. See `classifier_metrics` for the
//! Stage-2 toxicity classifier metric set (spec §"Monitoring").

pub mod classifier_metrics;
