// Toxicity scoring — trait-based abstraction for swappable providers.
//
// The ToxicityScorer trait defines the interface. OnnxToxicityScorer is the
// default (local Detoxify model, no API key needed). PerspectiveScorer is
// available as a fallback via CHARCOAL_SCORER=perspective.

pub mod classifier;
pub mod download;
pub mod ensemble;
pub mod onnx;
pub mod perspective;
pub mod rate_limiter;
pub mod runpod_cope_b;
pub mod traits;
pub mod zentropi;

/// Build the `[Parent post] / [Reply]` envelope used to score reply pairs.
///
/// Both the ONNX clean-pass filter (`TwoStageToxicityScorer::classify_post`)
/// and the Zentropi binary classifier (`ZentropiClient::classify_pair`) use
/// this exact format so the two stages see identical text. Changing the
/// format here updates both call sites in lockstep.
pub fn format_parent_reply(parent: &str, reply: &str) -> String {
    format!("[Parent post]: {}\n\n[Reply]: {}", parent, reply)
}
