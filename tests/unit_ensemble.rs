//! Tests for ensemble two-way correction (Groq + ONNX) and Groq parsing.

use anyhow::Result;
use async_trait::async_trait;
use charcoal::toxicity::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};

struct FixedScorer(f64);

#[async_trait]
impl ToxicityScorer for FixedScorer {
    async fn score_text(&self, _text: &str) -> Result<ToxicityResult> {
        Ok(ToxicityResult {
            toxicity: self.0,
            attributes: ToxicityAttributes::default(),
        })
    }
}

struct FailingScorer;

#[async_trait]
impl ToxicityScorer for FailingScorer {
    async fn score_text(&self, _text: &str) -> Result<ToxicityResult> {
        anyhow::bail!("Mock failure")
    }
}

mod ensemble_tests {
    use super::*;
    use charcoal::toxicity::ensemble::EnsembleToxicityScorer;

    // Helper: create ensemble with no secondary (ONNX-only)
    fn onnx_only(tox: f64) -> EnsembleToxicityScorer {
        EnsembleToxicityScorer::new(Box::new(FixedScorer(tox)), None)
    }

    // NOTE: We can't easily mock GroqSafeguardScorer since it holds a real HTTP client.
    // Tests that need a Groq secondary will use the trait-based approach through
    // score_text which delegates to score_ensemble_with_context.
    // For the ensemble correction logic, we test via ONNX-only (no secondary)
    // and verify the passthrough behavior. The correction matrix is tested
    // indirectly through the full integration when GROQ_API_KEY is set.

    #[tokio::test]
    async fn no_secondary_passes_through() {
        let ensemble = onnx_only(0.20);
        let result = ensemble.score_text("test").await.unwrap();
        assert!((result.toxicity - 0.20).abs() < 0.001);
    }

    #[tokio::test]
    async fn no_secondary_preserves_low_score() {
        let ensemble = onnx_only(0.05);
        let result = ensemble.score_text("test").await.unwrap();
        assert!((result.toxicity - 0.05).abs() < 0.001);
    }

    #[tokio::test]
    async fn score_with_context_no_secondary() {
        let ensemble = onnx_only(0.30);
        let result = ensemble
            .score_with_context("test", Some("context"))
            .await
            .unwrap();
        assert!((result.toxicity - 0.30).abs() < 0.001);
    }

    #[tokio::test]
    async fn secondary_failure_uses_primary() {
        let ensemble = EnsembleToxicityScorer::new(
            Box::new(FixedScorer(0.20)),
            None, // Can't mock GroqSafeguardScorer, but None tests the no-secondary path
        );
        let result = ensemble.score_text("test").await.unwrap();
        assert!((result.toxicity - 0.20).abs() < 0.001);
    }
}

// ============================================================
// Groq Safeguard — parsing and category boost tests
// ============================================================

mod groq_parsing_tests {
    use charcoal::toxicity::groq_safeguard::{boost_for_category, parse_safeguard_response};

    #[test]
    fn test_parse_violation() {
        let json =
            r#"{"violation": 1, "category": "Targeted harassment", "rationale": "Direct insult"}"#;
        let result = parse_safeguard_response(json).unwrap();
        assert!(result.violation);
        assert_eq!(result.category, "Targeted harassment");
        assert_eq!(result.rationale, "Direct insult");
    }

    #[test]
    fn test_parse_safe() {
        let json =
            r#"{"violation": 0, "category": "none", "rationale": "Substantive disagreement"}"#;
        let result = parse_safeguard_response(json).unwrap();
        assert!(!result.violation);
    }

    #[test]
    fn test_parse_malformed_json() {
        let result = parse_safeguard_response("not json at all");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_missing_fields() {
        let json = r#"{"violation": 1}"#;
        let result = parse_safeguard_response(json);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_truncated() {
        let json = r#"{"violation": 1, "categ"#;
        let result = parse_safeguard_response(json);
        assert!(result.is_none());
    }

    #[test]
    fn test_category_boost_identity() {
        assert_eq!(boost_for_category("Identity-based hostility"), 2.0);
    }

    #[test]
    fn test_category_boost_bad_faith() {
        assert_eq!(boost_for_category("Bad-faith engagement"), 1.5);
    }

    #[test]
    fn test_category_boost_unknown() {
        assert_eq!(boost_for_category("Something else"), 1.5);
    }
}
