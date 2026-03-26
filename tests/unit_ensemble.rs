// ============================================================
// Ensemble toxicity scorer tests
// ============================================================

// NOTE: These tests use mock scorers, not the real API.
// Integration tests against the live API are #[ignore].

use charcoal::toxicity::ensemble::{DisagreementStrategy, EnsembleToxicityScorer};
use charcoal::toxicity::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};

// -- Mock scorers for testing --

struct FixedScorer {
    result: ToxicityResult,
}

#[async_trait::async_trait]
impl ToxicityScorer for FixedScorer {
    async fn score_text(&self, _text: &str) -> anyhow::Result<ToxicityResult> {
        Ok(self.result.clone())
    }
}

struct FailingScorer;

#[async_trait::async_trait]
impl ToxicityScorer for FailingScorer {
    async fn score_text(&self, _text: &str) -> anyhow::Result<ToxicityResult> {
        anyhow::bail!("Scorer unavailable")
    }
}

fn make_result(toxicity: f64, identity_attack: f64, insult: f64) -> ToxicityResult {
    ToxicityResult {
        toxicity,
        attributes: ToxicityAttributes {
            severe_toxicity: Some(0.0),
            identity_attack: Some(identity_attack),
            insult: Some(insult),
            profanity: Some(0.0),
            threat: Some(0.0),
        },
    }
}

// ============================================================
// Ensemble — agreement
// ============================================================

#[tokio::test]
async fn ensemble_both_agree_low() {
    let primary = Box::new(FixedScorer {
        result: make_result(0.1, 0.05, 0.08),
    });
    let secondary = Box::new(FixedScorer {
        result: make_result(0.12, 0.06, 0.07),
    });
    let ensemble =
        EnsembleToxicityScorer::new(primary, Some(secondary), DisagreementStrategy::TakeLower);
    let result = ensemble.score_ensemble("test text").await.unwrap();
    assert!(result.classifiers_agree, "Should agree — diff is 0.02");
    assert!(result.score_difference < 0.05);
}

#[tokio::test]
async fn ensemble_both_agree_high() {
    let primary = Box::new(FixedScorer {
        result: make_result(0.85, 0.7, 0.6),
    });
    let secondary = Box::new(FixedScorer {
        result: make_result(0.88, 0.72, 0.58),
    });
    let ensemble =
        EnsembleToxicityScorer::new(primary, Some(secondary), DisagreementStrategy::TakeLower);
    let result = ensemble.score_ensemble("test text").await.unwrap();
    assert!(result.classifiers_agree);
    // Averaged: (0.85 + 0.88) / 2 = 0.865
    assert!((result.result.toxicity - 0.865).abs() < 0.01);
}

// ============================================================
// Ensemble — disagreement strategies
// ============================================================

#[tokio::test]
async fn ensemble_disagree_take_lower() {
    let primary = Box::new(FixedScorer {
        result: make_result(0.8, 0.7, 0.6),
    });
    let secondary = Box::new(FixedScorer {
        result: make_result(0.2, 0.1, 0.1),
    });
    let ensemble =
        EnsembleToxicityScorer::new(primary, Some(secondary), DisagreementStrategy::TakeLower);
    let result = ensemble
        .score_ensemble("reclaimed slur text")
        .await
        .unwrap();
    assert!(!result.classifiers_agree, "Should disagree — diff is 0.6");
    assert!(
        (result.result.toxicity - 0.2).abs() < 0.01,
        "TakeLower should use 0.2"
    );
}

#[tokio::test]
async fn ensemble_disagree_take_higher() {
    let primary = Box::new(FixedScorer {
        result: make_result(0.2, 0.1, 0.1),
    });
    let secondary = Box::new(FixedScorer {
        result: make_result(0.8, 0.7, 0.6),
    });
    let ensemble =
        EnsembleToxicityScorer::new(primary, Some(secondary), DisagreementStrategy::TakeHigher);
    let result = ensemble.score_ensemble("coded hostility").await.unwrap();
    assert!(!result.classifiers_agree);
    assert!(
        (result.result.toxicity - 0.8).abs() < 0.01,
        "TakeHigher should use 0.8"
    );
}

#[tokio::test]
async fn ensemble_disagree_average() {
    let primary = Box::new(FixedScorer {
        result: make_result(0.8, 0.7, 0.6),
    });
    let secondary = Box::new(FixedScorer {
        result: make_result(0.2, 0.1, 0.1),
    });
    let ensemble =
        EnsembleToxicityScorer::new(primary, Some(secondary), DisagreementStrategy::Average);
    let result = ensemble.score_ensemble("ambiguous text").await.unwrap();
    assert!(!result.classifiers_agree);
    assert!(
        (result.result.toxicity - 0.5).abs() < 0.01,
        "Average should be 0.5"
    );
}

// ============================================================
// Ensemble — fallback when secondary fails
// ============================================================

#[tokio::test]
async fn ensemble_secondary_fails_uses_primary() {
    let primary = Box::new(FixedScorer {
        result: make_result(0.4, 0.3, 0.2),
    });
    let secondary = Box::new(FailingScorer);
    let ensemble =
        EnsembleToxicityScorer::new(primary, Some(secondary), DisagreementStrategy::TakeLower);
    let result = ensemble.score_ensemble("test").await.unwrap();
    assert!(
        result.classifiers_agree,
        "Vacuously true when secondary fails"
    );
    assert!((result.result.toxicity - 0.4).abs() < 0.01);
    assert!(result.secondary_score.is_none());
}

#[tokio::test]
async fn ensemble_no_secondary_uses_primary() {
    let primary = Box::new(FixedScorer {
        result: make_result(0.4, 0.3, 0.2),
    });
    let ensemble = EnsembleToxicityScorer::new(primary, None, DisagreementStrategy::TakeLower);
    let result = ensemble.score_ensemble("test").await.unwrap();
    assert!(result.classifiers_agree);
    assert!((result.result.toxicity - 0.4).abs() < 0.01);
}

// ============================================================
// Ensemble — ToxicityScorer trait compliance
// ============================================================

#[tokio::test]
async fn ensemble_implements_toxicity_scorer_trait() {
    let primary = Box::new(FixedScorer {
        result: make_result(0.5, 0.3, 0.2),
    });
    let ensemble = EnsembleToxicityScorer::new(primary, None, DisagreementStrategy::TakeLower);
    // Use through the trait interface
    let scorer: &dyn ToxicityScorer = &ensemble;
    let result = scorer.score_text("test").await.unwrap();
    assert!((result.toxicity - 0.5).abs() < 0.01);
}

// ============================================================
// Ensemble — attribute merging
// ============================================================

#[tokio::test]
async fn ensemble_merges_attributes_on_agreement() {
    let primary = Box::new(FixedScorer {
        result: make_result(0.5, 0.4, 0.3),
    });
    let secondary = Box::new(FixedScorer {
        result: make_result(0.5, 0.6, 0.5),
    });
    let ensemble =
        EnsembleToxicityScorer::new(primary, Some(secondary), DisagreementStrategy::Average);
    let result = ensemble.score_ensemble("test").await.unwrap();
    // identity_attack: (0.4 + 0.6) / 2 = 0.5
    assert!((result.result.attributes.identity_attack.unwrap() - 0.5).abs() < 0.01);
    // insult: (0.3 + 0.5) / 2 = 0.4
    assert!((result.result.attributes.insult.unwrap() - 0.4).abs() < 0.01);
}

#[tokio::test]
async fn ensemble_handles_missing_secondary_profanity() {
    // Primary has profanity, secondary doesn't (OpenAI lacks profanity category)
    let mut primary_result = make_result(0.5, 0.3, 0.2);
    primary_result.attributes.profanity = Some(0.8);
    let mut secondary_result = make_result(0.5, 0.3, 0.2);
    secondary_result.attributes.profanity = None;

    let primary = Box::new(FixedScorer {
        result: primary_result,
    });
    let secondary = Box::new(FixedScorer {
        result: secondary_result,
    });
    let ensemble =
        EnsembleToxicityScorer::new(primary, Some(secondary), DisagreementStrategy::Average);
    let result = ensemble.score_ensemble("test").await.unwrap();
    // When one has it and other doesn't, keep the one that exists
    assert!(result.result.attributes.profanity.is_some());
    assert!((result.result.attributes.profanity.unwrap() - 0.8).abs() < 0.01);
}
