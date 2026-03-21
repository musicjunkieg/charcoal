//! Unit tests for NLI model integration and hostility scoring.

use charcoal::db::models::ThreatTier;
use charcoal::scoring::nli::{avg_context_score, compute_hostility_score, HypothesisScores};
use charcoal::scoring::nli_audit::{should_rotate, NliAuditEntry};
use std::path::PathBuf;

// --- Tier boundary tests ---

#[test]
fn watch_threshold_is_8() {
    assert_eq!(ThreatTier::from_score(8.0), ThreatTier::Watch);
    assert_eq!(ThreatTier::from_score(7.9), ThreatTier::Low);
}

// --- NLI model file detection tests ---

#[test]
fn nli_files_present_returns_false_for_empty_dir() {
    let dir = std::env::temp_dir().join("charcoal-nli-test-nonexistent");
    assert!(!charcoal::toxicity::download::nli_files_present(&dir));
}

#[test]
fn nli_files_present_returns_true_when_both_files_exist() {
    let dir = std::env::temp_dir().join("charcoal-nli-test-present");
    let nli_dir = charcoal::toxicity::download::nli_model_dir(&dir);
    std::fs::create_dir_all(&nli_dir).unwrap();
    std::fs::write(nli_dir.join("model_quantized.onnx"), b"fake model").unwrap();
    std::fs::write(nli_dir.join("tokenizer.json"), b"fake tokenizer").unwrap();
    assert!(charcoal::toxicity::download::nli_files_present(&dir));

    // Cleanup
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn nli_files_present_returns_false_when_model_missing() {
    let dir = std::env::temp_dir().join("charcoal-nli-test-partial");
    let nli_dir = charcoal::toxicity::download::nli_model_dir(&dir);
    std::fs::create_dir_all(&nli_dir).unwrap();
    std::fs::write(nli_dir.join("tokenizer.json"), b"fake tokenizer").unwrap();
    // model_quantized.onnx missing
    assert!(!charcoal::toxicity::download::nli_files_present(&dir));

    // Cleanup
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn nli_model_dir_is_subdirectory() {
    let base = PathBuf::from("/tmp/test-models");
    let nli_dir = charcoal::toxicity::download::nli_model_dir(&base);
    assert_eq!(nli_dir, base.join("nli-deberta-v3-xsmall"));
}

// --- Hostility score computation tests ---

#[test]
fn hostile_quote_scores_high() {
    let scores = HypothesisScores {
        attack: 0.85,
        contempt: 0.60,
        misrepresent: 0.30,
        good_faith_disagree: 0.05,
        support: 0.02,
    };
    let hostility = compute_hostility_score(&scores);
    // max(0.85, 0.60, 0.30) - max(0.05*0.5, 0.02*0.8) = 0.85 - 0.025 = 0.825
    assert!(
        hostility > 0.8,
        "Hostile quote should score high, got {}",
        hostility
    );
    assert!(hostility <= 1.0);
}

#[test]
fn supportive_reply_scores_low() {
    let scores = HypothesisScores {
        attack: 0.05,
        contempt: 0.03,
        misrepresent: 0.02,
        good_faith_disagree: 0.10,
        support: 0.90,
    };
    let hostility = compute_hostility_score(&scores);
    assert!(
        hostility < 0.01,
        "Supportive reply should score near zero, got {}",
        hostility
    );
}

#[test]
fn good_faith_disagreement_scores_low() {
    let scores = HypothesisScores {
        attack: 0.20,
        contempt: 0.15,
        misrepresent: 0.10,
        good_faith_disagree: 0.70,
        support: 0.05,
    };
    let hostility = compute_hostility_score(&scores);
    assert!(
        hostility < 0.01,
        "Good-faith disagreement should score low, got {}",
        hostility
    );
}

#[test]
fn concern_trolling_with_contempt_scores_moderate() {
    let scores = HypothesisScores {
        attack: 0.30,
        contempt: 0.65,
        misrepresent: 0.40,
        good_faith_disagree: 0.20,
        support: 0.15,
    };
    let hostility = compute_hostility_score(&scores);
    assert!(
        hostility > 0.4,
        "Concern trolling should score moderate+, got {}",
        hostility
    );
    assert!(hostility < 0.7);
}

#[test]
fn neutral_response_scores_near_zero() {
    let scores = HypothesisScores {
        attack: 0.10,
        contempt: 0.08,
        misrepresent: 0.05,
        good_faith_disagree: 0.15,
        support: 0.12,
    };
    let hostility = compute_hostility_score(&scores);
    assert!(
        hostility < 0.1,
        "Neutral response should score near zero, got {}",
        hostility
    );
}

#[test]
fn hostility_score_clamped_to_zero_one() {
    // All hostile signals maxed
    let scores = HypothesisScores {
        attack: 1.0,
        contempt: 1.0,
        misrepresent: 1.0,
        good_faith_disagree: 0.0,
        support: 0.0,
    };
    let hostility = compute_hostility_score(&scores);
    assert!(hostility <= 1.0);
    assert!(hostility >= 0.0);

    // All supportive signals maxed
    let scores_support = HypothesisScores {
        attack: 0.0,
        contempt: 0.0,
        misrepresent: 0.0,
        good_faith_disagree: 1.0,
        support: 1.0,
    };
    let hostility_support = compute_hostility_score(&scores_support);
    assert!(hostility_support >= 0.0);
    assert!(hostility_support <= 1.0);
}

#[test]
fn avg_context_score_from_multiple_pairs() {
    let scores = vec![0.3, 0.7, 0.5, 0.2];
    let result = avg_context_score(&scores);
    // (0.3 + 0.7 + 0.5 + 0.2) / 4 = 0.425
    assert!((result.unwrap() - 0.425).abs() < 0.001);
}

#[test]
fn avg_context_score_empty_returns_none() {
    let scores: Vec<f64> = vec![];
    assert!(avg_context_score(&scores).is_none());
}

#[test]
fn avg_context_score_single_value() {
    let scores = vec![0.42];
    assert_eq!(avg_context_score(&scores), Some(0.42));
}

// --- NLI audit logging tests ---

#[test]
fn nli_audit_entry_serializes_to_json() {
    let entry = NliAuditEntry {
        timestamp: "2026-03-20T12:00:00Z".to_string(),
        target_did: "did:plc:abc123".to_string(),
        target_handle: "test.bsky.social".to_string(),
        pair_type: "direct".to_string(),
        original_text: "Original post".to_string(),
        response_text: "Response post".to_string(),
        hypothesis_scores: HypothesisScores {
            attack: 0.8,
            contempt: 0.3,
            misrepresent: 0.1,
            good_faith_disagree: 0.05,
            support: 0.02,
        },
        hostility_score: 0.78,
        similarity: None,
    };
    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains("\"pair_type\":\"direct\""));
    assert!(json.contains("\"hostility_score\":0.78"));
    assert!(!json.contains("similarity"));
}

#[test]
fn nli_audit_entry_with_similarity() {
    let entry = NliAuditEntry {
        timestamp: "2026-03-20T12:00:00Z".to_string(),
        target_did: "did:plc:abc123".to_string(),
        target_handle: "test.bsky.social".to_string(),
        pair_type: "inferred".to_string(),
        original_text: "Original".to_string(),
        response_text: "Response".to_string(),
        hypothesis_scores: HypothesisScores {
            attack: 0.1,
            contempt: 0.1,
            misrepresent: 0.1,
            good_faith_disagree: 0.5,
            support: 0.7,
        },
        hostility_score: 0.0,
        similarity: Some(0.85),
    };
    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains("\"similarity\":0.85"));
    assert!(json.contains("\"pair_type\":\"inferred\""));
}

#[test]
fn should_rotate_true_for_old_entry() {
    let old_ts = (chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339();
    let line = format!(r#"{{"timestamp":"{}","target_did":"x"}}"#, old_ts);
    assert!(should_rotate(&line));
}

#[test]
fn should_rotate_false_for_recent_entry() {
    let recent_ts = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
    let line = format!(r#"{{"timestamp":"{}","target_did":"x"}}"#, recent_ts);
    assert!(!should_rotate(&line));
}

#[test]
fn should_rotate_false_for_invalid_json() {
    assert!(!should_rotate("not valid json"));
}
