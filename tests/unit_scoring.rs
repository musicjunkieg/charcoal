// Unit tests for scoring and output functions.
//
// Tests isolated pure functions: ThreatTier::from_score boundary conditions,
// compute_threat_score edge cases (gate logic, clamping, custom weights),
// and truncate_chars UTF-8 safety.

use charcoal::db::models::ThreatTier;
use charcoal::output::truncate_chars;
use charcoal::scoring::threat::{compute_threat_score, ThreatWeights};

// ============================================================
// ThreatTier::from_score — boundary conditions
// ============================================================

#[test]
fn tier_exact_boundary_high() {
    assert_eq!(ThreatTier::from_score(35.0), ThreatTier::High);
}

#[test]
fn tier_just_below_high() {
    assert_eq!(ThreatTier::from_score(34.999), ThreatTier::Elevated);
}

#[test]
fn tier_exact_boundary_elevated() {
    assert_eq!(ThreatTier::from_score(15.0), ThreatTier::Elevated);
}

#[test]
fn tier_just_below_elevated() {
    assert_eq!(ThreatTier::from_score(14.999), ThreatTier::Watch);
}

#[test]
fn tier_exact_boundary_watch() {
    assert_eq!(ThreatTier::from_score(8.0), ThreatTier::Watch);
}

#[test]
fn tier_just_below_watch() {
    assert_eq!(ThreatTier::from_score(7.999), ThreatTier::Low);
}

#[test]
fn tier_zero() {
    assert_eq!(ThreatTier::from_score(0.0), ThreatTier::Low);
}

#[test]
fn tier_negative() {
    assert_eq!(ThreatTier::from_score(-5.0), ThreatTier::Low);
}

#[test]
fn tier_very_large() {
    assert_eq!(ThreatTier::from_score(1000.0), ThreatTier::High);
}

#[test]
fn tier_nan_falls_to_low() {
    // NaN fails all >= comparisons, so it falls through to the wildcard arm
    assert_eq!(ThreatTier::from_score(f64::NAN), ThreatTier::Low);
}

// ============================================================
// ThreatTier round-trip: from_score -> as_str -> Display
// ============================================================

#[test]
fn tier_as_str_all_variants() {
    assert_eq!(ThreatTier::Low.as_str(), "Low");
    assert_eq!(ThreatTier::Watch.as_str(), "Watch");
    assert_eq!(ThreatTier::Elevated.as_str(), "Elevated");
    assert_eq!(ThreatTier::High.as_str(), "High");
}

#[test]
fn tier_display_matches_as_str() {
    for tier in [
        ThreatTier::Low,
        ThreatTier::Watch,
        ThreatTier::Elevated,
        ThreatTier::High,
    ] {
        assert_eq!(tier.to_string(), tier.as_str());
    }
}

#[test]
fn tier_round_trip_score_to_string() {
    let cases = [
        (5.0, "Low"),
        (10.0, "Watch"),
        (20.0, "Elevated"),
        (50.0, "High"),
    ];
    for (score, expected_str) in cases {
        let tier = ThreatTier::from_score(score);
        assert_eq!(
            tier.as_str(),
            expected_str,
            "Score {score} should map to {expected_str}"
        );
    }
}

// ============================================================
// compute_threat_score — gate boundary precision
// ============================================================

#[test]
fn gate_just_below_threshold() {
    let w = ThreatWeights::default();
    let (score, _) = compute_threat_score(0.5, 0.149, &w);
    // Gated (0.149 < 0.15): 0.5 * 25 = 12.5
    assert!(
        (score - 12.5).abs() < 0.1,
        "Below gate threshold should use gated formula, got {score}"
    );
}

#[test]
fn gate_exactly_at_threshold() {
    let w = ThreatWeights::default();
    // overlap (0.15) is NOT < 0.15, so full formula applies
    let (score, _) = compute_threat_score(0.5, 0.15, &w);
    // Full: 0.5 * 70 * (1 + 0.15 * 1.5) = 35 * 1.225 = 42.875
    assert!(
        (score - 42.875).abs() < 0.1,
        "At threshold should use full formula, got {score}"
    );
}

#[test]
fn gate_just_above_threshold() {
    let w = ThreatWeights::default();
    let (score, _) = compute_threat_score(0.5, 0.151, &w);
    // Full: 0.5 * 70 * (1 + 0.151 * 1.5) = 35 * 1.2265 = 42.9275
    assert!(
        (score - 42.9275).abs() < 0.1,
        "Above threshold should use full formula, got {score}"
    );
}

// ============================================================
// compute_threat_score — clamping
// ============================================================

#[test]
fn score_clamped_to_100() {
    let w = ThreatWeights::default();
    let (score, tier) = compute_threat_score(1.5, 1.5, &w);
    // 1.5 * 70 * (1 + 1.5 * 1.5) = 105 * 3.25 = 341.25 -> clamped to 100
    assert_eq!(score, 100.0);
    assert_eq!(tier, ThreatTier::High);
}

#[test]
fn negative_inputs_clamped_to_zero() {
    let w = ThreatWeights::default();
    let (score, tier) = compute_threat_score(-0.5, 0.2, &w);
    // -0.5 * 70 * (1 + 0.2 * 1.5) = -35 * 1.3 = -45.5 -> clamped to 0
    assert_eq!(score, 0.0);
    assert_eq!(tier, ThreatTier::Low);
}

// ============================================================
// compute_threat_score — gate cap behavior
// ============================================================

#[test]
fn gated_max_toxicity_caps_at_gate_max() {
    let w = ThreatWeights::default();
    // toxicity=1.0, overlap=0 -> gated: min(1.0*25, 25) = 25
    let (score, _) = compute_threat_score(1.0, 0.0, &w);
    assert!((score - 25.0).abs() < 0.1);
}

#[test]
fn gated_above_one_still_caps() {
    let w = ThreatWeights::default();
    // toxicity=2.0, overlap=0 -> gated: min(2.0*25, 25) = min(50,25) = 25
    let (score, _) = compute_threat_score(2.0, 0.0, &w);
    assert!((score - 25.0).abs() < 0.1);
}

// ============================================================
// compute_threat_score — custom weights
// ============================================================

#[test]
fn custom_weights_zero_produces_zero() {
    let w = ThreatWeights {
        toxicity_weight: 0.0,
        overlap_multiplier: 0.0,
        overlap_gate_threshold: 0.15,
        gate_max_score: 25.0,
    };
    let (score, tier) = compute_threat_score(0.9, 0.9, &w);
    assert_eq!(score, 0.0);
    assert_eq!(tier, ThreatTier::Low);
}

#[test]
fn custom_weights_high_multiplier() {
    let w = ThreatWeights {
        toxicity_weight: 70.0,
        overlap_multiplier: 3.0,
        overlap_gate_threshold: 0.15,
        gate_max_score: 25.0,
    };
    let (score, _) = compute_threat_score(0.5, 0.5, &w);
    // 0.5 * 70 * (1 + 0.5 * 3.0) = 35 * 2.5 = 87.5
    assert!((score - 87.5).abs() < 0.1);
}

#[test]
fn custom_gate_max_score() {
    let w = ThreatWeights {
        toxicity_weight: 70.0,
        overlap_multiplier: 1.5,
        overlap_gate_threshold: 0.15,
        gate_max_score: 10.0, // lower gate cap
    };
    let (score, _) = compute_threat_score(0.9, 0.0, &w);
    // Gated: min(0.9*10, 10) = 9.0
    assert!((score - 9.0).abs() < 0.1);
}

#[test]
fn default_weights_match_documented_values() {
    let w = ThreatWeights::default();
    assert_eq!(w.toxicity_weight, 70.0);
    assert_eq!(w.overlap_multiplier, 1.5);
    assert_eq!(w.overlap_gate_threshold, 0.15);
    assert_eq!(w.gate_max_score, 25.0);
}

// ============================================================
// truncate_chars — UTF-8 safe truncation
// ============================================================

#[test]
fn truncate_empty_string() {
    assert_eq!(truncate_chars("", 10), "");
}

#[test]
fn truncate_within_limit() {
    assert_eq!(truncate_chars("hello", 10), "hello");
}

#[test]
fn truncate_exactly_at_limit() {
    assert_eq!(truncate_chars("hello", 5), "hello");
}

#[test]
fn truncate_one_over_limit() {
    assert_eq!(truncate_chars("hello!", 5), "hello...");
}

#[test]
fn truncate_max_zero_non_empty() {
    // 0 chars taken + "..." appended
    assert_eq!(truncate_chars("hello", 0), "...");
}

#[test]
fn truncate_single_char_at_limit_one() {
    assert_eq!(truncate_chars("x", 1), "x");
}

#[test]
fn truncate_emoji_safe() {
    // "Hello 🌍!" = 8 chars (emoji is 1 char, 4 bytes)
    let text = "Hello 🌍!";
    assert_eq!(text.chars().count(), 8);
    // Truncate to 7 chars drops the "!"
    let result = truncate_chars(text, 7);
    assert_eq!(result, "Hello 🌍...");
}

#[test]
fn truncate_accented_chars() {
    // "café" = 4 chars (é is 1 char, 2 bytes)
    let text = "café résumé";
    let result = truncate_chars(text, 4);
    assert_eq!(result, "café...");
}

#[test]
fn truncate_cjk_characters() {
    let text = "日本語テスト";
    assert_eq!(text.chars().count(), 6);
    let result = truncate_chars(text, 3);
    assert_eq!(result, "日本語...");
}

#[test]
fn truncate_preserves_full_string_at_exact_length() {
    let text = "exactly ten";
    let len = text.chars().count();
    assert_eq!(truncate_chars(text, len), text);
}

#[test]
fn truncate_long_string() {
    let text = "a".repeat(200);
    let result = truncate_chars(&text, 100);
    assert_eq!(result.chars().count(), 103); // 100 + "..."
    assert!(result.ends_with("..."));
}
