//! Two-stage toxicity classification — ONNX clean-pass filter + Zentropi binary classifier.
//!
//! Stage 1: ONNX scores every post. Posts below `ONNX_CLEAN_THRESHOLD` (0.10) are
//! treated as genuinely safe and skip stage 2 — no Zentropi call, no token cost.
//!
//! Stage 2: Posts at or above the threshold are sent to the configured
//! `ToxicityClassifier` (RunPod CoPE-B or Zentropi-hosted CoPE) for a binary
//! verdict (toxic / not toxic). The classifier policy is conversation-scoped —
//! it distinguishes ally use of identity terms ("fuck yeah, fat liberation!")
//! from hostile use ("fat people are disgusting").
//!
//! There is no silent fallback: the classifier is required (constructed via
//! `classifier::build_from_env`), and a classifier error propagates rather than
//! degrading to an ONNX-threshold guess. The threshold that turns a raw
//! `ClassifierVerdict` into a binary `is_toxic` is owned by the classifier impl
//! (`classifier::is_toxic`), never by this module.

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use std::sync::Arc;
use tracing::debug;

use super::classifier::{is_toxic, ToxicityClassifier};
use super::format_parent_reply;
use super::traits::{BinaryVerdict, ToxicityResult, ToxicityScorer};

/// ONNX score below this is genuinely safe — skip the Stage-2 classifier entirely.
pub const ONNX_CLEAN_THRESHOLD: f64 = 0.10;

/// Maximum concurrent Stage-2 classifier requests in flight per batch.
/// Conservative bound to stay well under upstream rate limits.
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
    /// Stage-2 classifier confidence in [0.0, 1.0] when stage 2 ran.
    pub classifier_confidence: Option<f32>,
    /// Stage-2 model identity (e.g. "cope-b-a4b") when stage 2 ran. Carried
    /// for audit provenance.
    pub classifier_model_id: Option<String>,
    /// Stage-2 policy version when stage 2 ran. Carried for audit provenance.
    pub classifier_policy_version: Option<String>,
}

/// Provenance of a `TwoStageVerdict`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerdictSource {
    /// ONNX cleared the post (< clean threshold). Stage 2 was skipped.
    OnnxCleared,
    /// The Stage-2 classifier classified the post as toxic.
    ClassifierToxic,
    /// The Stage-2 classifier classified the post as safe.
    ClassifierSafe,
}

/// Two-stage scorer: continuous primary (typically ONNX) + optional binary classifier (Zentropi).
///
/// The struct itself implements `ToxicityScorer` so legacy continuous-score callers
/// keep working: `score_text` returns the primary's continuous score. Pipelines that
/// want the binary verdict use `classify_post` / `classify_batch` directly.
pub struct TwoStageToxicityScorer {
    primary: Box<dyn ToxicityScorer>,
    classifier: Arc<dyn ToxicityClassifier>,
}

impl TwoStageToxicityScorer {
    /// Build a two-stage scorer. The Stage-2 `classifier` is required — there is
    /// no ONNX-only fallback. Callers construct one via
    /// `classifier::build_from_env` (which refuses to boot if unconfigured).
    pub fn new(primary: Box<dyn ToxicityScorer>, classifier: Arc<dyn ToxicityClassifier>) -> Self {
        Self {
            primary,
            classifier,
        }
    }

    /// Name of the active Stage-2 classifier backend. Used by callers that want
    /// to log the active classifier path at scan start.
    pub fn classifier_name(&self) -> &'static str {
        self.classifier.name()
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
        // the < 0.10 short-circuit and never reaches Zentropi. The shared
        // `format_parent_reply` helper keeps this in lockstep with what
        // Zentropi's `classify_pair` sees — both stages get identical text.
        let envelope_owned;
        let primary_input = match context {
            Some(parent) => {
                envelope_owned = format_parent_reply(parent, text);
                envelope_owned.as_str()
            }
            None => text,
        };

        let primary = self.primary.score_text(primary_input).await?;
        let onnx_score = primary.toxicity;
        let onnx_attributes = primary.attributes;

        if onnx_score < ONNX_CLEAN_THRESHOLD {
            debug!(
                onnx_score,
                "Two-stage: ONNX cleared, skipping Stage-2 classifier"
            );
            return Ok(TwoStageVerdict {
                is_toxic: false,
                onnx_score,
                onnx_attributes,
                source: VerdictSource::OnnxCleared,
                classifier_confidence: None,
                classifier_model_id: None,
                classifier_policy_version: None,
            });
        }

        // No silent fallback: a classifier error propagates via `?`. The
        // classifier sees the same `primary_input` envelope ONNX scored, so the
        // two stages are on identical text.
        let verdict = self.classifier.classify(primary_input).await?;
        let toxic = is_toxic(self.classifier.as_ref(), &verdict);
        debug!(
            onnx_score,
            is_toxic = toxic,
            confidence = verdict.confidence,
            backend = self.classifier.name(),
            "Two-stage: classifier verdict"
        );
        Ok(TwoStageVerdict {
            is_toxic: toxic,
            onnx_score,
            onnx_attributes,
            source: if toxic {
                VerdictSource::ClassifierToxic
            } else {
                VerdictSource::ClassifierSafe
            },
            classifier_confidence: Some(verdict.confidence),
            classifier_model_id: Some(verdict.model_id),
            classifier_policy_version: Some(verdict.policy_version),
        })
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
