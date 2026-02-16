// Toxicity scoring — trait-based abstraction for swappable providers.
//
// The ToxicityScorer trait defines the interface. OnnxToxicityScorer is the
// default (local Detoxify model, no API key needed). PerspectiveScorer is
// available as a fallback via CHARCOAL_SCORER=perspective.

pub mod traits;
pub mod perspective;
pub mod rate_limiter;
pub mod onnx;
pub mod download;
