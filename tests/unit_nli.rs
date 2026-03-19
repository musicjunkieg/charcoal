//! Unit tests for NLI model integration and hostility scoring.

use charcoal::scoring::nli::{compute_hostility_score, max_context_score_opt, HypothesisScores};
use std::path::PathBuf;

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
fn max_context_score_from_multiple_pairs() {
    let scores = vec![0.3, 0.7, 0.5, 0.2];
    let result = max_context_score_opt(&scores);
    assert_eq!(result, Some(0.7));
}

#[test]
fn max_context_score_empty_returns_none() {
    let scores: Vec<f64> = vec![];
    assert!(max_context_score_opt(&scores).is_none());
}

#[test]
fn max_context_score_single_value() {
    let scores = vec![0.42];
    assert_eq!(max_context_score_opt(&scores), Some(0.42));
}
