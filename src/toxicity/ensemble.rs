// Ensemble toxicity scorer — runs primary + secondary concurrently.
//
// Detects agreement between classifiers and applies a configurable
// disagreement strategy. When both agree, averages their scores.
// When they disagree, applies the chosen strategy (TakeLower is
// the default — protects against false positives from a single model).

use anyhow::Result;
use async_trait::async_trait;
use tracing::{debug, warn};

use super::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};

/// Agreement threshold — if the absolute difference in toxicity scores
/// exceeds this value, the classifiers are considered to disagree.
const AGREEMENT_THRESHOLD: f64 = 0.25;

/// Strategy for resolving disagreement between primary and secondary scorers.
#[derive(Debug, Clone, Copy)]
pub enum DisagreementStrategy {
    /// Use the lower score (conservative — reduces false positives)
    TakeLower,
    /// Use the higher score (aggressive — reduces false negatives)
    TakeHigher,
    /// Average both scores
    Average,
}

/// Result from the ensemble scorer with metadata about agreement.
pub struct EnsembleResult {
    /// The merged toxicity result
    pub result: ToxicityResult,
    /// Whether the primary and secondary classifiers agreed
    pub classifiers_agree: bool,
    /// Absolute difference between primary and secondary toxicity scores
    pub score_difference: f64,
    /// The secondary scorer's raw result (None if secondary failed or absent)
    pub secondary_score: Option<ToxicityResult>,
}

/// Ensemble scorer that wraps a primary and optional secondary ToxicityScorer.
///
/// When both scorers are available, they run concurrently and their results
/// are merged based on agreement. When only the primary is available (or the
/// secondary fails), the primary result is used directly.
pub struct EnsembleToxicityScorer {
    primary: Box<dyn ToxicityScorer>,
    secondary: Option<Box<dyn ToxicityScorer>>,
    strategy: DisagreementStrategy,
}

impl EnsembleToxicityScorer {
    pub fn new(
        primary: Box<dyn ToxicityScorer>,
        secondary: Option<Box<dyn ToxicityScorer>>,
        strategy: DisagreementStrategy,
    ) -> Self {
        Self {
            primary,
            secondary,
            strategy,
        }
    }

    /// Score text using the ensemble, returning full metadata about agreement.
    ///
    /// When a secondary scorer is available, both run concurrently via
    /// `tokio::join!` so the wall-clock cost is max(primary, secondary)
    /// rather than primary + secondary.
    pub async fn score_ensemble(&self, text: &str) -> Result<EnsembleResult> {
        let (primary_result, secondary_result) = match &self.secondary {
            Some(scorer) => {
                let (primary, secondary) =
                    tokio::join!(self.primary.score_text(text), scorer.score_text(text),);
                let secondary_result = match secondary {
                    Ok(result) => Some(result),
                    Err(e) => {
                        warn!(error = %e, "Secondary scorer failed, using primary only");
                        None
                    }
                };
                (primary?, secondary_result)
            }
            None => (self.primary.score_text(text).await?, None),
        };

        match secondary_result {
            Some(secondary) => {
                let diff = (primary_result.toxicity - secondary.toxicity).abs();
                let agree = diff <= AGREEMENT_THRESHOLD;

                debug!(
                    primary = format!("{:.3}", primary_result.toxicity),
                    secondary = format!("{:.3}", secondary.toxicity),
                    diff = format!("{:.3}", diff),
                    agree,
                    "Ensemble comparison"
                );

                let merged = if agree {
                    // Agreement: average both scores
                    merge_results(&primary_result, &secondary)
                } else {
                    // Disagreement: apply strategy
                    match self.strategy {
                        DisagreementStrategy::TakeLower => {
                            if primary_result.toxicity <= secondary.toxicity {
                                primary_result.clone()
                            } else {
                                secondary.clone()
                            }
                        }
                        DisagreementStrategy::TakeHigher => {
                            if primary_result.toxicity >= secondary.toxicity {
                                primary_result.clone()
                            } else {
                                secondary.clone()
                            }
                        }
                        DisagreementStrategy::Average => merge_results(&primary_result, &secondary),
                    }
                };

                Ok(EnsembleResult {
                    result: merged,
                    classifiers_agree: agree,
                    score_difference: diff,
                    secondary_score: Some(secondary),
                })
            }
            None => {
                // No secondary or secondary failed — use primary directly
                Ok(EnsembleResult {
                    result: primary_result,
                    classifiers_agree: true, // Vacuously true
                    score_difference: 0.0,
                    secondary_score: None,
                })
            }
        }
    }
}

#[async_trait]
impl ToxicityScorer for EnsembleToxicityScorer {
    async fn score_text(&self, text: &str) -> Result<ToxicityResult> {
        let ensemble_result = self.score_ensemble(text).await?;
        Ok(ensemble_result.result)
    }
}

/// Average two toxicity results, merging their attributes.
fn merge_results(a: &ToxicityResult, b: &ToxicityResult) -> ToxicityResult {
    ToxicityResult {
        toxicity: (a.toxicity + b.toxicity) / 2.0,
        attributes: merge_attributes(&a.attributes, &b.attributes),
    }
}

/// Average two sets of toxicity attributes. When one side has None for a
/// field and the other has Some, keeps the Some value (e.g., OpenAI lacks
/// profanity but ONNX has it).
fn merge_attributes(a: &ToxicityAttributes, b: &ToxicityAttributes) -> ToxicityAttributes {
    ToxicityAttributes {
        severe_toxicity: merge_option(a.severe_toxicity, b.severe_toxicity),
        identity_attack: merge_option(a.identity_attack, b.identity_attack),
        insult: merge_option(a.insult, b.insult),
        profanity: merge_option(a.profanity, b.profanity),
        threat: merge_option(a.threat, b.threat),
    }
}

/// Merge two optional f64 values: average if both present, keep whichever
/// exists if only one is present, None if both are None.
fn merge_option(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some((x + y) / 2.0),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}
