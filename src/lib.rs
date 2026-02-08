// Charcoal: Predictive threat detection for Bluesky
//
// This is the library root. Each module corresponds to a major subsystem
// of the threat detection pipeline.

pub mod db;
pub mod bluesky;
pub mod toxicity;
pub mod topics;
pub mod scoring;
pub mod pipeline;
pub mod output;
pub mod status;
