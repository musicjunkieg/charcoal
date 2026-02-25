// Charcoal: Predictive threat detection for Bluesky
//
// This is the library root. Each module corresponds to a major subsystem
// of the threat detection pipeline.

pub mod bluesky;
pub mod config;
pub mod constellation;
pub mod db;
pub mod output;
pub mod pipeline;
pub mod scoring;
pub mod status;
pub mod topics;
pub mod toxicity;

#[cfg(feature = "web")]
pub mod web;
