use charcoal::db::models::ThreatTier;
use charcoal::scoring::behavioral::{
    apply_behavioral_modifier, compute_behavioral_boost, compute_quote_ratio, compute_reply_ratio,
    detect_pile_on_participants, is_behaviorally_benign, BehavioralSignals,
};
use charcoal::scoring::threat::{compute_threat_score, ThreatWeights};

#[test]
fn behavioral_signals_default_is_neutral() {
    let signals = BehavioralSignals::default();
    assert_eq!(signals.quote_ratio, 0.0);
    assert_eq!(signals.reply_ratio, 0.0);
    assert_eq!(signals.avg_engagement, 0.0);
    assert!(!signals.pile_on);
    assert!(!signals.benign_gate);
    assert_eq!(signals.behavioral_boost, 1.0);
}

#[test]
fn behavioral_signals_json_roundtrip() {
    let signals = BehavioralSignals {
        quote_ratio: 0.35,
        reply_ratio: 0.45,
        avg_engagement: 12.5,
        pile_on: true,
        benign_gate: false,
        behavioral_boost: 1.22,
    };
    let json = serde_json::to_string(&signals).unwrap();
    let deserialized: BehavioralSignals = serde_json::from_str(&json).unwrap();
    assert!((deserialized.quote_ratio - 0.35).abs() < f64::EPSILON);
    assert!(deserialized.pile_on);
    assert!((deserialized.behavioral_boost - 1.22).abs() < f64::EPSILON);
}

// --- Behavioral boost tests ---

#[test]
fn boost_all_zeros_is_one() {
    let boost = compute_behavioral_boost(0.0, 0.0, false);
    assert!((boost - 1.0).abs() < f64::EPSILON);
}

#[test]
fn boost_max_is_1_5() {
    let boost = compute_behavioral_boost(1.0, 1.0, true);
    assert!((boost - 1.5).abs() < 1e-10);
}

#[test]
fn boost_quote_only() {
    let boost = compute_behavioral_boost(0.5, 0.0, false);
    assert!((boost - 1.1).abs() < f64::EPSILON);
}

#[test]
fn boost_reply_only() {
    let boost = compute_behavioral_boost(0.0, 0.8, false);
    assert!((boost - 1.12).abs() < f64::EPSILON);
}

#[test]
fn boost_pile_on_only() {
    let boost = compute_behavioral_boost(0.0, 0.0, true);
    assert!((boost - 1.15).abs() < f64::EPSILON);
}

#[test]
fn boost_typical_hostile() {
    let boost = compute_behavioral_boost(0.4, 0.3, false);
    assert!((boost - 1.125).abs() < 0.001);
}

// --- Benign gate tests ---

#[test]
fn benign_gate_all_conditions_met() {
    assert!(is_behaviorally_benign(0.10, 0.20, false, 15.0, 10.0));
}

#[test]
fn benign_gate_fails_high_quote_ratio() {
    assert!(!is_behaviorally_benign(0.20, 0.20, false, 15.0, 10.0));
}

#[test]
fn benign_gate_fails_high_reply_ratio() {
    assert!(!is_behaviorally_benign(0.10, 0.35, false, 15.0, 10.0));
}

#[test]
fn benign_gate_fails_pile_on() {
    assert!(!is_behaviorally_benign(0.10, 0.20, true, 15.0, 10.0));
}

#[test]
fn benign_gate_fails_low_engagement() {
    assert!(!is_behaviorally_benign(0.10, 0.20, false, 5.0, 10.0));
}

#[test]
fn benign_gate_exact_thresholds() {
    assert!(!is_behaviorally_benign(0.15, 0.20, false, 15.0, 10.0));
    assert!(!is_behaviorally_benign(0.10, 0.30, false, 15.0, 10.0));
}

// --- Behavioral modifier tests ---

#[test]
fn modifier_benign_caps_at_12() {
    let (score, benign) = apply_behavioral_modifier(50.0, 0.05, 0.10, false, 15.0, 10.0);
    assert!(benign);
    assert!((score - 12.0).abs() < f64::EPSILON);
}

#[test]
fn modifier_benign_passes_through_low_score() {
    let (score, benign) = apply_behavioral_modifier(5.0, 0.05, 0.10, false, 15.0, 10.0);
    assert!(benign);
    assert!((score - 5.0).abs() < f64::EPSILON);
}

#[test]
fn modifier_hostile_applies_boost() {
    let (score, benign) = apply_behavioral_modifier(50.0, 0.80, 0.10, false, 15.0, 10.0);
    assert!(!benign);
    // boost = 1.0 + 0.80*0.20 + 0.10*0.15 = 1.175; 50.0 * 1.175 = 58.75
    assert!((score - 58.75).abs() < 0.1);
}

#[test]
fn modifier_no_behavioral_data_is_neutral() {
    let (score, benign) = apply_behavioral_modifier(50.0, 0.0, 0.0, false, 0.0, 10.0);
    assert!(!benign);
    assert!((score - 50.0).abs() < f64::EPSILON);
}

#[test]
fn modifier_clamped_to_100() {
    let (score, _) = apply_behavioral_modifier(90.0, 1.0, 1.0, true, 0.0, 10.0);
    assert!((score - 100.0).abs() < f64::EPSILON);
}

// --- Quote ratio tests ---

#[test]
fn quote_ratio_no_posts() {
    assert!((compute_quote_ratio(0, 0) - 0.0).abs() < f64::EPSILON);
}

#[test]
fn quote_ratio_no_quotes() {
    assert!((compute_quote_ratio(0, 10) - 0.0).abs() < f64::EPSILON);
}

#[test]
fn quote_ratio_all_quotes() {
    assert!((compute_quote_ratio(10, 10) - 1.0).abs() < f64::EPSILON);
}

#[test]
fn quote_ratio_half() {
    assert!((compute_quote_ratio(5, 10) - 0.5).abs() < f64::EPSILON);
}

// --- Reply ratio tests ---

#[test]
fn reply_ratio_no_posts() {
    assert!((compute_reply_ratio(0, 0) - 0.0).abs() < f64::EPSILON);
}

#[test]
fn reply_ratio_no_replies() {
    assert!((compute_reply_ratio(0, 20) - 0.0).abs() < f64::EPSILON);
}

#[test]
fn reply_ratio_all_replies() {
    assert!((compute_reply_ratio(20, 20) - 1.0).abs() < f64::EPSILON);
}

#[test]
fn reply_ratio_mixed() {
    assert!((compute_reply_ratio(15, 50) - 0.3).abs() < f64::EPSILON);
}

// --- Pile-on detection tests ---

#[test]
fn pile_on_below_threshold_not_detected() {
    let events = vec![
        ("did:plc:a", "at://post/1", "2026-02-19T10:00:00Z"),
        ("did:plc:b", "at://post/1", "2026-02-19T11:00:00Z"),
        ("did:plc:c", "at://post/1", "2026-02-19T12:00:00Z"),
        ("did:plc:d", "at://post/1", "2026-02-19T13:00:00Z"),
    ];
    let participants = detect_pile_on_participants(&events);
    assert!(participants.is_empty());
}

#[test]
fn pile_on_at_threshold_detected() {
    let events = vec![
        ("did:plc:a", "at://post/1", "2026-02-19T10:00:00Z"),
        ("did:plc:b", "at://post/1", "2026-02-19T11:00:00Z"),
        ("did:plc:c", "at://post/1", "2026-02-19T12:00:00Z"),
        ("did:plc:d", "at://post/1", "2026-02-19T13:00:00Z"),
        ("did:plc:e", "at://post/1", "2026-02-19T14:00:00Z"),
    ];
    let participants = detect_pile_on_participants(&events);
    assert_eq!(participants.len(), 5);
    assert!(participants.contains("did:plc:a"));
    assert!(participants.contains("did:plc:e"));
}

#[test]
fn pile_on_outside_window_not_detected() {
    let events = vec![
        ("did:plc:a", "at://post/1", "2026-02-18T10:00:00Z"),
        ("did:plc:b", "at://post/1", "2026-02-18T22:00:00Z"),
        ("did:plc:c", "at://post/1", "2026-02-19T10:00:00Z"),
        ("did:plc:d", "at://post/1", "2026-02-19T22:00:00Z"),
        ("did:plc:e", "at://post/1", "2026-02-20T10:00:00Z"),
    ];
    let participants = detect_pile_on_participants(&events);
    assert!(participants.is_empty());
}

#[test]
fn pile_on_deduplicates_same_amplifier() {
    let events = vec![
        ("did:plc:a", "at://post/1", "2026-02-19T10:00:00Z"),
        ("did:plc:a", "at://post/1", "2026-02-19T10:30:00Z"),
        ("did:plc:b", "at://post/1", "2026-02-19T11:00:00Z"),
        ("did:plc:c", "at://post/1", "2026-02-19T12:00:00Z"),
        ("did:plc:d", "at://post/1", "2026-02-19T13:00:00Z"),
    ];
    let participants = detect_pile_on_participants(&events);
    assert!(participants.is_empty());
}

#[test]
fn pile_on_multiple_posts_independent() {
    let events = vec![
        ("did:plc:a", "at://post/1", "2026-02-19T10:00:00Z"),
        ("did:plc:b", "at://post/1", "2026-02-19T11:00:00Z"),
        ("did:plc:c", "at://post/1", "2026-02-19T12:00:00Z"),
        ("did:plc:d", "at://post/2", "2026-02-19T10:00:00Z"),
        ("did:plc:e", "at://post/2", "2026-02-19T11:00:00Z"),
        ("did:plc:f", "at://post/2", "2026-02-19T12:00:00Z"),
    ];
    let participants = detect_pile_on_participants(&events);
    assert!(participants.is_empty());
}

#[test]
fn pile_on_sliding_window_catches_late_cluster() {
    let events = vec![
        ("did:plc:a", "at://post/1", "2026-02-18T08:00:00Z"),
        ("did:plc:b", "at://post/1", "2026-02-18T09:00:00Z"),
        ("did:plc:c", "at://post/1", "2026-02-19T10:00:00Z"),
        ("did:plc:d", "at://post/1", "2026-02-19T11:00:00Z"),
        ("did:plc:e", "at://post/1", "2026-02-19T12:00:00Z"),
        ("did:plc:f", "at://post/1", "2026-02-19T12:30:00Z"),
        ("did:plc:g", "at://post/1", "2026-02-19T13:00:00Z"),
    ];
    let participants = detect_pile_on_participants(&events);
    assert!(participants.len() >= 5);
    assert!(participants.contains("did:plc:c"));
    assert!(participants.contains("did:plc:g"));
}

// ============================================================
// Real-world persona scenarios
// ============================================================

/// The Quote-Dunker: 80% quotes, moderate toxicity, high overlap
/// Should get behavioral boost pushing score higher
#[test]
fn persona_the_quote_dunker() {
    let weights = ThreatWeights::default();
    let toxicity = 0.15;
    let overlap = 0.40;

    // Raw score: 0.15 * 70 * (1 + 0.40 * 1.5) = 10.5 * 1.6 = 16.8
    let (raw_score, _) = compute_threat_score(toxicity, overlap, &weights);
    assert!((raw_score - 16.8).abs() < 0.1);

    // With behavioral boost: quote_ratio=0.80, reply_ratio=0.30, no pile-on
    // boost = 1.0 + 0.80*0.20 + 0.30*0.15 = 1.0 + 0.16 + 0.045 = 1.205
    let (final_score, benign) = apply_behavioral_modifier(raw_score, 0.80, 0.30, false, 20.0, 10.0);
    assert!(!benign);
    // 16.8 * 1.205 = 20.244
    assert!(final_score > raw_score, "Boost should increase score");
    assert!((final_score - 20.244).abs() < 0.1);
}

/// The Supportive Ally: 5% quotes, low toxicity, high overlap
/// Should trigger benign gate, capping at 12.0
#[test]
fn persona_the_supportive_ally() {
    let weights = ThreatWeights::default();
    let toxicity = 0.10;
    let overlap = 0.70;

    // Raw score: 0.10 * 70 * (1 + 0.70 * 1.5) = 7.0 * 2.05 = 14.35
    let (raw_score, _) = compute_threat_score(toxicity, overlap, &weights);
    assert!((raw_score - 14.35).abs() < 0.1);

    // Benign: quote=0.05 (<0.15), reply=0.10 (<0.30), no pile-on, engagement 25 > median 10
    let (final_score, benign) = apply_behavioral_modifier(raw_score, 0.05, 0.10, false, 25.0, 10.0);
    assert!(benign, "Ally should trigger benign gate");
    assert!(
        (final_score - 12.0).abs() < f64::EPSILON,
        "Should be capped at 12.0"
    );
    let tier = ThreatTier::from_score(final_score);
    assert_eq!(tier, ThreatTier::Watch, "Ally should stay at Watch");
}

/// The Pile-On Participant: moderate toxicity, part of a 7-account pile-on
/// Should get pile-on boost
#[test]
fn persona_the_pile_on_participant() {
    let weights = ThreatWeights::default();
    let toxicity = 0.20;
    let overlap = 0.35;

    // Raw: 0.20 * 70 * (1 + 0.35 * 1.5) = 14.0 * 1.525 = 21.35
    let (raw_score, _) = compute_threat_score(toxicity, overlap, &weights);

    // With pile-on: quote=0.30, reply=0.20, pile_on=true
    // boost = 1.0 + 0.30*0.20 + 0.20*0.15 + 0.15 = 1.0 + 0.06 + 0.03 + 0.15 = 1.24
    let (final_score, benign) = apply_behavioral_modifier(raw_score, 0.30, 0.20, true, 8.0, 10.0);
    assert!(!benign);
    // 21.35 * 1.24 = 26.474
    assert!((final_score - 26.474).abs() < 0.1);
    let tier = ThreatTier::from_score(final_score);
    assert_eq!(tier, ThreatTier::Elevated);
}

/// The Lurker Reposter: low post count, low engagement, few quotes
/// Doesn't trigger benign gate (engagement too low), gets small boost
#[test]
fn persona_the_lurker_reposter() {
    let weights = ThreatWeights::default();
    let toxicity = 0.25;
    let overlap = 0.30;

    // Raw: 0.25 * 70 * (1 + 0.30 * 1.5) = 17.5 * 1.45 = 25.375
    let (raw_score, _) = compute_threat_score(toxicity, overlap, &weights);

    // Low engagement (2.0 < median 10.0) blocks benign gate
    // quote=0.05, reply=0.15, no pile-on
    // boost = 1.0 + 0.05*0.20 + 0.15*0.15 = 1.0 + 0.01 + 0.0225 = 1.0325
    let (final_score, benign) = apply_behavioral_modifier(raw_score, 0.05, 0.15, false, 2.0, 10.0);
    assert!(!benign, "Low engagement should block benign gate");
    // 25.375 * 1.0325 ≈ 26.2
    assert!((final_score - 26.2).abs() < 0.5);
}

/// High toxicity + benign behavior: gate should prevent High tier
#[test]
fn persona_high_tox_benign_behavior() {
    let weights = ThreatWeights::default();
    let toxicity = 0.50;
    let overlap = 0.50;

    // Raw: 0.50 * 70 * (1 + 0.50 * 1.5) = 35 * 1.75 = 61.25 (High!)
    let (raw_score, raw_tier) = compute_threat_score(toxicity, overlap, &weights);
    assert_eq!(raw_tier, ThreatTier::High);

    // But benign behavior caps at 12.0
    let (final_score, benign) = apply_behavioral_modifier(raw_score, 0.05, 0.10, false, 30.0, 10.0);
    assert!(benign);
    assert!((final_score - 12.0).abs() < f64::EPSILON);
    let tier = ThreatTier::from_score(final_score);
    assert_eq!(tier, ThreatTier::Watch, "Benign gate prevents High tier");
}
