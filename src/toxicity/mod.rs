// Toxicity scoring — trait-based abstraction for swappable providers.
//
// The ToxicityScorer trait defines the interface. PerspectiveScorer implements
// it using Google's Perspective API. When Perspective sunsets (Dec 2026),
// we swap in a different implementation without touching the rest of the pipeline.

pub mod traits;
pub mod perspective;
pub mod rate_limiter;
