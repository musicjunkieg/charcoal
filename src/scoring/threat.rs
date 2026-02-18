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
///
/// The formula is multiplicative: overlap amplifies toxicity rather than
/// contributing independently. This prevents high-overlap/low-toxicity
/// accounts (allies) from being flagged as threats.
///
/// `score = toxicity * toxicity_weight * (1 + overlap * overlap_multiplier)`
pub struct ThreatWeights {
    /// Base weight for toxicity (default 70.0)
    pub toxicity_weight: f64,
    /// How much overlap amplifies the toxicity signal (default 1.5).
    /// At max overlap (1.0), the toxicity score is multiplied by (1 + 1.5) = 2.5x.
    pub overlap_multiplier: f64,
    /// Topic overlap below this threshold triggers the gate (default 0.15).
    /// Adjusted for sentence embedding scale where most accounts in the
    /// social neighborhood have overlap 0.4-0.8.
    pub overlap_gate_threshold: f64,
    /// Maximum score when the gate is active (default 25.0)
    pub gate_max_score: f64,
}

impl Default for ThreatWeights {
    fn default() -> Self {
        Self {
            toxicity_weight: 70.0,
            overlap_multiplier: 1.5,
            overlap_gate_threshold: 0.15,
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
        // Multiplicative formula: overlap amplifies toxicity.
        // An ally (low tox, high overlap) stays low. A hostile account
        // in the same topic space gets amplified.
        toxicity * weights.toxicity_weight * (1.0 + topic_overlap * weights.overlap_multiplier)
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
        // 0.8 * 70 * (1 + 0.25 * 1.5) = 56 * 1.375 = 77.0
        assert!((score - 77.0).abs() < 0.1, "Expected ~77.0, got {score}");
        assert_eq!(tier, ThreatTier::High);
    }

    #[test]
    fn test_hostile_without_overlap_is_gated() {
        let weights = ThreatWeights::default();
        let (score, tier) = compute_threat_score(0.9, 0.02, &weights);
        // Gated (0.02 < 0.15): 0.9 * 25 = 22.5
        assert!((score - 22.5).abs() < 0.1, "Expected ~22.5, got {score}");
        assert_eq!(tier, ThreatTier::Elevated);
    }

    #[test]
    fn test_moderate_toxicity_high_overlap() {
        let weights = ThreatWeights::default();
        let (score, tier) = compute_threat_score(0.4, 0.5, &weights);
        // 0.4 * 70 * (1 + 0.5 * 1.5) = 28 * 1.75 = 49.0
        assert!((score - 49.0).abs() < 0.1, "Expected ~49.0, got {score}");
        assert_eq!(tier, ThreatTier::High);
    }

    #[test]
    fn test_friendly_ally() {
        // Key improvement: ally with high overlap but low toxicity now
        // scores Elevated instead of High. The multiplicative formula
        // prevents overlap from independently driving high scores.
        let weights = ThreatWeights::default();
        let (score, tier) = compute_threat_score(0.1, 0.8, &weights);
        // 0.1 * 70 * (1 + 0.8 * 1.5) = 7 * 2.2 = 15.4
        assert!((score - 15.4).abs() < 0.1, "Expected ~15.4, got {score}");
        assert_eq!(tier, ThreatTier::Elevated);
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
        // Charcoal is designed to flag. Values adjusted for embedding
        // scale where overlap 0.35 = "same general space".
        let weights = ThreatWeights::default();
        let (score, tier) = compute_threat_score(0.12, 0.35, &weights);
        // 0.12 * 70 * (1 + 0.35 * 1.5) = 8.4 * 1.525 = 12.81
        assert!((score - 12.81).abs() < 0.1, "Expected ~12.81, got {score}");
        assert_eq!(tier, ThreatTier::Watch);
    }

    #[test]
    fn test_low_toxicity_no_overlap() {
        let weights = ThreatWeights::default();
        let (score, tier) = compute_threat_score(0.08, 0.02, &weights);
        // Gated (0.02 < 0.15): 0.08 * 25 = 2.0
        assert!((score - 2.0).abs() < 0.1, "Expected ~2.0, got {score}");
        assert_eq!(tier, ThreatTier::Low);
    }
}
