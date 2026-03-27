//! Ensemble toxicity scorer — ONNX primary + Groq secondary with two-way correction.
//!
//! Correction matrix:
//! - ONNX high + Groq violation = agree (keep ONNX score)
//! - ONNX high + Groq safe = dampen (0.4x — ONNX false positive)
//! - ONNX low + Groq violation = boost (category-dependent + floor)
//! - ONNX low + Groq safe = agree (keep ONNX score)

use anyhow::Result;
use async_trait::async_trait;
use tracing::{debug, warn};

use super::groq_safeguard::{boost_for_category, GroqSafeguardScorer};
use super::traits::{ToxicityResult, ToxicityScorer};

/// ONNX toxicity threshold: above this is "high", at or below is "low".
const ONNX_HIGH_THRESHOLD: f64 = 0.15;

/// Dampening factor for ONNX false positives (Groq says safe).
const DAMPEN_FACTOR: f64 = 0.4;

/// Minimum toxicity floor when Groq flags a violation.
const GROQ_FLOOR: f64 = 0.15;

/// Result from the ensemble scorer with correction metadata.
pub struct EnsembleResult {
    pub result: ToxicityResult,
    pub groq_flagged: bool,
    pub groq_category: Option<String>,
    pub groq_rationale: Option<String>,
    pub correction_applied: f64,
    pub models_agree: bool,
}

/// Ensemble scorer: ONNX primary + optional Groq secondary (concrete type).
///
/// Holds GroqSafeguardScorer directly (not Box<dyn ToxicityScorer>) so we
/// can access the full SafeguardResult with category and rationale.
pub struct EnsembleToxicityScorer {
    primary: Box<dyn ToxicityScorer>,
    secondary: Option<GroqSafeguardScorer>,
}

impl EnsembleToxicityScorer {
    pub fn new(primary: Box<dyn ToxicityScorer>, secondary: Option<GroqSafeguardScorer>) -> Self {
        Self { primary, secondary }
    }

    pub async fn score_ensemble_with_context(
        &self,
        text: &str,
        context: Option<&str>,
    ) -> Result<EnsembleResult> {
        let (primary_result, safeguard_result) = match &self.secondary {
            Some(groq) => {
                let (primary, groq_result) = tokio::join!(
                    self.primary.score_text(text),
                    groq.score_with_safeguard(text, context),
                );
                let safeguard = match groq_result {
                    Ok(result) => result,
                    Err(e) => {
                        warn!(error = %e, "Groq scorer failed, using ONNX only");
                        None
                    }
                };
                (primary?, safeguard)
            }
            None => (self.primary.score_text(text).await?, None),
        };

        let onnx_tox = primary_result.toxicity;

        match &safeguard_result {
            Some(sr) => {
                let groq_flagged = sr.violation;
                let onnx_high = onnx_tox > ONNX_HIGH_THRESHOLD;

                let (correction, models_agree) = match (onnx_high, groq_flagged) {
                    (true, true) => {
                        debug!(onnx_tox, category = %sr.category, "Ensemble: agree (both hostile)");
                        (1.0, true)
                    }
                    (true, false) => {
                        debug!(
                            onnx_tox,
                            dampened = onnx_tox * DAMPEN_FACTOR,
                            "Ensemble: dampening ONNX false positive"
                        );
                        (DAMPEN_FACTOR, false)
                    }
                    (false, true) => {
                        let boost = boost_for_category(&sr.category);
                        debug!(onnx_tox, boost, category = %sr.category, "Ensemble: boosting missed hostility");
                        (boost, false)
                    }
                    (false, false) => {
                        debug!(onnx_tox, "Ensemble: agree (both safe)");
                        (1.0, true)
                    }
                };

                let corrected_tox = if groq_flagged && !onnx_high {
                    (onnx_tox * correction).clamp(GROQ_FLOOR, 1.0)
                } else {
                    (onnx_tox * correction).min(1.0)
                };

                let mut result = primary_result;
                result.toxicity = corrected_tox;

                Ok(EnsembleResult {
                    result,
                    groq_flagged,
                    groq_category: Some(sr.category.clone()),
                    groq_rationale: Some(sr.rationale.clone()),
                    correction_applied: correction,
                    models_agree,
                })
            }
            None => Ok(EnsembleResult {
                result: primary_result,
                groq_flagged: false,
                groq_category: None,
                groq_rationale: None,
                correction_applied: 1.0,
                models_agree: true,
            }),
        }
    }
}

#[async_trait]
impl ToxicityScorer for EnsembleToxicityScorer {
    async fn score_text(&self, text: &str) -> Result<ToxicityResult> {
        let r = self.score_ensemble_with_context(text, None).await?;
        Ok(r.result)
    }

    async fn score_with_context(
        &self,
        text: &str,
        context: Option<&str>,
    ) -> Result<ToxicityResult> {
        let r = self.score_ensemble_with_context(text, context).await?;
        Ok(r.result)
    }
}
