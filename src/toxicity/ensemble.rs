//! Two-stage toxicity classification — ONNX clean-pass filter + Zentropi binary classifier.
//!
//! Stage 1: ONNX scores every post. Posts below `ONNX_CLEAN_THRESHOLD` (0.10) are
//! treated as genuinely safe and skip stage 2 — no Zentropi call, no token cost.
//!
//! Stage 2: Posts at or above the threshold are sent to Zentropi for a binary
//! verdict (toxic / not toxic). The Zentropi labeler holds a conversation-scoped
//! policy that correctly distinguishes ally use of identity terms ("fuck yeah, fat
//! liberation!") from hostile use ("fat people are disgusting").
//!
//! When Zentropi is not configured, stage 2 falls back to ONNX with a binary
//! threshold (0.50) — preserving safe behavior at the cost of accuracy.

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use std::sync::Arc;
use tracing::{debug, warn};

use super::traits::{BinaryVerdict, ToxicityResult, ToxicityScorer};
use super::zentropi::ZentropiClient;

/// ONNX score below this is genuinely safe — skip Zentropi entirely.
pub const ONNX_CLEAN_THRESHOLD: f64 = 0.10;

/// ONNX-only fallback threshold. Used when Zentropi is unavailable to derive
/// a binary verdict from the continuous ONNX score.
pub const ONNX_FALLBACK_BINARY_THRESHOLD: f64 = 0.50;

/// Maximum concurrent Zentropi requests in flight per batch. Conservative bound
/// to stay well under free-tier rate limits.
const ZENTROPI_CONCURRENCY: usize = 4;

/// Per-post outcome from the two-stage pipeline.
#[derive(Debug, Clone)]
pub struct TwoStageVerdict {
    /// Binary toxicity verdict — drives the threat formula's toxicity rate.
    pub is_toxic: bool,
    /// Continuous ONNX score in [0.0, 1.0]. Preserved for evidence sorting and audit.
    pub onnx_score: f64,
    /// ONNX category breakdown when the primary scorer provides it.
    pub onnx_attributes: super::traits::ToxicityAttributes,
    /// How the verdict was reached.
    pub source: VerdictSource,
    /// Zentropi confidence in [0.0, 1.0] when stage 2 ran.
    pub zentropi_confidence: Option<f64>,
}

/// Provenance of a `TwoStageVerdict`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerdictSource {
    /// ONNX cleared the post (< clean threshold). Stage 2 was skipped.
    OnnxCleared,
    /// Zentropi classified the post as toxic.
    ZentropiToxic,
    /// Zentropi classified the post as safe.
    ZentropiSafe,
    /// Zentropi unavailable; verdict came from ONNX threshold fallback.
    OnnxFallback,
}

/// Two-stage scorer: continuous primary (typically ONNX) + optional binary classifier (Zentropi).
///
/// The struct itself implements `ToxicityScorer` so legacy continuous-score callers
/// keep working: `score_text` returns the primary's continuous score. Pipelines that
/// want the binary verdict use `classify_post` / `classify_batch` directly.
pub struct TwoStageToxicityScorer {
    primary: Box<dyn ToxicityScorer>,
    zentropi: Option<Arc<ZentropiClient>>,
}

impl TwoStageToxicityScorer {
    /// Build a two-stage scorer. `zentropi = None` falls back to ONNX-only with the
    /// binary threshold — useful for local development without a Zentropi key.
    pub fn new(primary: Box<dyn ToxicityScorer>, zentropi: Option<Arc<ZentropiClient>>) -> Self {
        Self { primary, zentropi }
    }

    /// Whether this scorer has Zentropi configured. Used by callers that want to
    /// log the active classifier path at scan start.
    pub fn has_zentropi(&self) -> bool {
        self.zentropi.is_some()
    }

    /// Classify a single post. `context` is the parent post text for replies; pass
    /// `None` for originals. The pair-aware classification only runs when Zentropi
    /// is available — the labeler policy is conversation-scoped.
    pub async fn classify_post(
        &self,
        text: &str,
        context: Option<&str>,
    ) -> Result<TwoStageVerdict> {
        // For replies, score the [Parent post] / [Reply] envelope so the ONNX
        // clean-pass filter can detect context-dependent toxicity. Without this,
        // a benign-looking reply ("I agree") to a hostile parent slips through
        // the < 0.10 short-circuit and never reaches Zentropi. Same envelope
        // format as Zentropi's classify_pair, so the two stages stay aligned.
        let envelope_owned;
        let primary_input = match context {
            Some(parent) => {
                envelope_owned = format!("[Parent post]: {}\n\n[Reply]: {}", parent, text);
                envelope_owned.as_str()
            }
            None => text,
        };

        let primary = self.primary.score_text(primary_input).await?;
        let onnx_score = primary.toxicity;
        let onnx_attributes = primary.attributes;

        if onnx_score < ONNX_CLEAN_THRESHOLD {
            debug!(onnx_score, "Two-stage: ONNX cleared, skipping Zentropi");
            return Ok(TwoStageVerdict {
                is_toxic: false,
                onnx_score,
                onnx_attributes,
                source: VerdictSource::OnnxCleared,
                zentropi_confidence: None,
            });
        }

        match &self.zentropi {
            Some(client) => {
                let response = match context {
                    Some(parent) => client.classify_pair(parent, text).await,
                    None => client.classify(text).await,
                };
                match response {
                    Ok(r) => {
                        let is_toxic = r.is_toxic();
                        debug!(
                            onnx_score,
                            is_toxic,
                            confidence = r.confidence,
                            "Two-stage: Zentropi verdict"
                        );
                        Ok(TwoStageVerdict {
                            is_toxic,
                            onnx_score,
                            onnx_attributes,
                            source: if is_toxic {
                                VerdictSource::ZentropiToxic
                            } else {
                                VerdictSource::ZentropiSafe
                            },
                            zentropi_confidence: Some(r.confidence),
                        })
                    }
                    Err(e) => {
                        warn!(error = %e, "Zentropi failed, falling back to ONNX threshold");
                        Ok(TwoStageVerdict {
                            is_toxic: onnx_score >= ONNX_FALLBACK_BINARY_THRESHOLD,
                            onnx_score,
                            onnx_attributes,
                            source: VerdictSource::OnnxFallback,
                            zentropi_confidence: None,
                        })
                    }
                }
            }
            None => Ok(TwoStageVerdict {
                is_toxic: onnx_score >= ONNX_FALLBACK_BINARY_THRESHOLD,
                onnx_score,
                onnx_attributes,
                source: VerdictSource::OnnxFallback,
                zentropi_confidence: None,
            }),
        }
    }

    /// Classify a batch of posts in parallel, with per-post context. Caller supplies
    /// `contexts.len() == texts.len()`; entries are parent texts for replies, `None`
    /// for originals. Concurrency is bounded by `ZENTROPI_CONCURRENCY` to stay under
    /// free-tier rate limits.
    pub async fn classify_batch(
        &self,
        texts: &[String],
        contexts: &[Option<String>],
    ) -> Result<Vec<TwoStageVerdict>> {
        if texts.len() != contexts.len() {
            anyhow::bail!(
                "classify_batch: texts.len() ({}) != contexts.len() ({})",
                texts.len(),
                contexts.len()
            );
        }

        // Own the strings inside the stream's closures so the async-trait wrapping
        // doesn't trip over higher-rank lifetime constraints on borrowed inputs.
        let owned: Vec<(usize, String, Option<String>)> = texts
            .iter()
            .zip(contexts.iter())
            .enumerate()
            .map(|(i, (text, ctx))| (i, text.clone(), ctx.clone()))
            .collect();

        let mut indexed: Vec<(usize, Result<TwoStageVerdict>)> = stream::iter(owned)
            .map(|(i, text, ctx)| async move {
                let verdict = self.classify_post(&text, ctx.as_deref()).await;
                (i, verdict)
            })
            .buffer_unordered(ZENTROPI_CONCURRENCY)
            .collect()
            .await;

        indexed.sort_by_key(|(i, _)| *i);

        let mut out = Vec::with_capacity(texts.len());
        for (_, result) in indexed {
            out.push(result?);
        }
        Ok(out)
    }
}

#[async_trait]
impl ToxicityScorer for TwoStageToxicityScorer {
    /// Returns the primary's continuous score. Used by legacy callers that
    /// haven't migrated to `classify_post` yet (e.g. amplifier scoring paths).
    async fn score_text(&self, text: &str) -> Result<ToxicityResult> {
        self.primary.score_text(text).await
    }

    async fn score_with_context(
        &self,
        text: &str,
        context: Option<&str>,
    ) -> Result<ToxicityResult> {
        self.primary.score_with_context(text, context).await
    }

    /// Override the default trait implementation to use the two-stage pipeline:
    /// ONNX clean-pass filter, then Zentropi binary classification for the rest.
    async fn classify_batch_with_contexts(
        &self,
        texts: &[String],
        contexts: &[Option<String>],
    ) -> Result<Vec<BinaryVerdict>> {
        let verdicts = self.classify_batch(texts, contexts).await?;
        Ok(verdicts
            .into_iter()
            .map(|v| BinaryVerdict {
                is_toxic: v.is_toxic,
                onnx_score: v.onnx_score,
                onnx_attributes: v.onnx_attributes,
            })
            .collect())
    }
}
