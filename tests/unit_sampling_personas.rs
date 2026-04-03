// tests/unit_sampling_personas.rs
//
// Persona-based composition tests for the new sampling and scoring pipeline.
// These test the scenarios described in the design doc.

use charcoal::bluesky::posts::FingerprintQuality;
use charcoal::scoring::profile::compute_reply_weighted_toxicity;

/// The Hidden Hostile: wholesome originals, vicious replies
#[test]
fn persona_hidden_hostile() {
    // 0/20 originals toxic, 12/30 replies toxic
    let tox_rate = compute_reply_weighted_toxicity(12, 30, 0, 20);
    // reply_tox = 0.40, original_tox = 0.0
    // weighted = 0.40 * 0.7 + 0.0 * 0.3 = 0.28
    assert!(
        (tox_rate - 0.28).abs() < 0.01,
        "Hidden hostile should have weighted tox ~0.28, got {}",
        tox_rate
    );
    // Compare to flat rate: 12/50 = 0.24 — reply weighting surfaces the threat
    let flat_rate = 12.0 / 50.0;
    assert!(
        tox_rate > flat_rate,
        "Reply-weighted rate should be higher than flat rate for hidden hostiles"
    );
}

/// The Reply-Heavy Account: 4 originals about cooking, 46 hostile replies on fat lib topics
#[test]
fn persona_reply_heavy_fingerprinting() {
    let fp_quality = FingerprintQuality::from_counts(4, 46);
    assert_eq!(
        fp_quality,
        FingerprintQuality::Degraded,
        "4 originals + 46 replies should use degraded fingerprint"
    );
}

/// Account with zero originals — all replies
#[test]
fn persona_all_replies() {
    let fp_quality = FingerprintQuality::from_counts(0, 35);
    assert_eq!(
        fp_quality,
        FingerprintQuality::Unreliable,
        "0 originals should always be unreliable regardless of reply count"
    );
}

/// The Efficient Clean Account: early exit candidate
#[test]
fn persona_clean_account_flat_rate_zero() {
    // 25 posts, 0 toxic — tox rate should be 0.0
    let tox_rate = compute_reply_weighted_toxicity(0, 10, 0, 15);
    assert!((tox_rate - 0.0).abs() < 0.001);
}

/// The Borderline Concern Troll: context_score 0.8, benign behavioral signals
#[test]
fn persona_concern_troll_no_double_count() {
    use charcoal::scoring::behavioral;

    let raw_score = 20.0;
    let (score, _benign_gate, gate_bypassed) = behavioral::apply_behavioral_modifier_contextual(
        raw_score,
        0.05,  // low quote ratio (benign)
        0.10,  // low reply ratio (benign)
        false, // no pile-on
        50.0,  // above median engagement
        10.0,  // median
        Some(0.8),
    );

    assert!(gate_bypassed, "Gate should be bypassed for context >= 0.5");
    assert!(
        score < 25.0,
        "Score should not be double-amplified, got {}",
        score
    );
}

/// Normal account with sufficient originals
#[test]
fn persona_normal_account_fingerprinting() {
    let fp_quality = FingerprintQuality::from_counts(30, 20);
    assert_eq!(
        fp_quality,
        FingerprintQuality::Normal,
        "30 originals should give Normal fingerprint quality"
    );
}

/// Boundary: exactly 15 originals
#[test]
fn persona_boundary_15_originals() {
    let fp_quality = FingerprintQuality::from_counts(15, 35);
    assert_eq!(fp_quality, FingerprintQuality::Normal);
}

/// Barely enough data: 5 originals + 10 replies = 15 total
#[test]
fn persona_minimal_data() {
    let fp_quality = FingerprintQuality::from_counts(5, 10);
    assert_eq!(fp_quality, FingerprintQuality::Degraded);
}
