// Combined threat score formula.
//
// The threat score combines toxicity and topic overlap using the formula
// from the plan: 70% toxicity, 30% topic overlap, with a gate.
//
// The gate implements a key insight: toxicity without topic overlap is
// low-priority (they're hostile but unlikely to see your content). Toxicity
// WITH topic overlap is the real danger.

use crate::db::models::ThreatTier;

/// Configurable weights for the threat score formula.
pub struct ThreatWeights {
    /// Weight for toxicity in the combined score (default 70.0)
    pub toxicity_weight: f64,
    /// Weight for topic overlap in the combined score (default 30.0)
    pub overlap_weight: f64,
    /// Topic overlap below this threshold triggers the gate (default 0.05)
    pub overlap_gate_threshold: f64,
    /// Maximum score when the gate is active (default 25.0)
    pub gate_max_score: f64,
}

impl Default for ThreatWeights {
    fn default() -> Self {
        Self {
            toxicity_weight: 70.0,
            overlap_weight: 30.0,
            overlap_gate_threshold: 0.05,
            gate_max_score: 25.0,
        }
    }
}

/// Compute the combined threat score from toxicity and topic overlap.
///
/// Returns a score from 0.0 to 100.0 and the corresponding threat tier.
pub fn compute_threat_score(
    toxicity: f64,
    topic_overlap: f64,
    weights: &ThreatWeights,
) -> (f64, ThreatTier) {
    let score = if topic_overlap < weights.overlap_gate_threshold {
        // Gate: hostile but irrelevant — cap the score
        (toxicity * weights.gate_max_score).min(weights.gate_max_score)
    } else {
        // Full formula: weighted combination
        (toxicity * weights.toxicity_weight) + (topic_overlap * weights.overlap_weight)
    };

    // Clamp to 0-100 range
    let score = score.clamp(0.0, 100.0);
    let tier = ThreatTier::from_score(score);

    (score, tier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hostile_with_overlap() {
        let weights = ThreatWeights::default();
        let (score, tier) = compute_threat_score(0.8, 0.25, &weights);
        // 0.8 * 70 + 0.25 * 30 = 56 + 7.5 = 63.5
        assert!((score - 63.5).abs() < 0.1, "Expected ~63.5, got {score}");
        assert_eq!(tier, ThreatTier::High);
    }

    #[test]
    fn test_hostile_without_overlap_is_gated() {
        let weights = ThreatWeights::default();
        let (score, tier) = compute_threat_score(0.9, 0.02, &weights);
        // Gated: 0.9 * 25 = 22.5, capped at 25
        assert!((score - 22.5).abs() < 0.1, "Expected ~22.5, got {score}");
        assert_eq!(tier, ThreatTier::Elevated);
    }

    #[test]
    fn test_moderate_toxicity_high_overlap() {
        let weights = ThreatWeights::default();
        let (score, tier) = compute_threat_score(0.4, 0.5, &weights);
        // 0.4 * 70 + 0.5 * 30 = 28 + 15 = 43
        assert!((score - 43.0).abs() < 0.1, "Expected ~43.0, got {score}");
        assert_eq!(tier, ThreatTier::High);
    }

    #[test]
    fn test_friendly_ally() {
        let weights = ThreatWeights::default();
        let (score, tier) = compute_threat_score(0.1, 0.8, &weights);
        // 0.1 * 70 + 0.8 * 30 = 7 + 24 = 31
        assert!((score - 31.0).abs() < 0.1, "Expected ~31.0, got {score}");
        assert_eq!(tier, ThreatTier::High);
    }

    #[test]
    fn test_zero_scores() {
        let weights = ThreatWeights::default();
        let (score, tier) = compute_threat_score(0.0, 0.0, &weights);
        assert!((score - 0.0).abs() < 0.1);
        assert_eq!(tier, ThreatTier::Low);
    }

    #[test]
    fn test_realistic_watch_account() {
        // Moderate toxicity with topic overlap — the kind of account
        // Charcoal is designed to flag
        let weights = ThreatWeights::default();
        let (score, tier) = compute_threat_score(0.17, 0.06, &weights);
        // 0.17 * 70 + 0.06 * 30 = 11.9 + 1.8 = 13.7
        assert!((score - 13.7).abs() < 0.1, "Expected ~13.7, got {score}");
        assert_eq!(tier, ThreatTier::Watch);
    }

    #[test]
    fn test_low_toxicity_no_overlap() {
        let weights = ThreatWeights::default();
        let (score, tier) = compute_threat_score(0.08, 0.02, &weights);
        // Gated: 0.08 * 25 = 2.0
        assert!((score - 2.0).abs() < 0.1, "Expected ~2.0, got {score}");
        assert_eq!(tier, ThreatTier::Low);
    }
}
