//! Stage-2 toxicity classifier trait.
//!
//! Implementations live in sibling modules:
//! - `runpod_cope_b` — self-hosted CoPE-B-A4B on RunPod Serverless
//! - `zentropi` — hosted CoPE API (kept for fallback)
//!
//! The trait owns the threshold via `threshold()`. Callers never pass a
//! threshold — see spec §"Backend selection and per-backend thresholds"
//! for why threshold drift via runtime override is forbidden.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Serialize;
use std::sync::{Arc, Mutex};

/// Outcome of a single classification call. Stage-2 only — the full two-stage
/// verdict lives in `TwoStageVerdict`.
#[derive(Debug, Clone, Serialize)]
pub struct ClassifierVerdict {
    /// Did the model emit "1" (or its hosted-API equivalent)?
    pub toxic_token: bool,
    /// Normalized confidence in [0.0, 1.0].
    pub confidence: f32,
    /// Wall-clock latency for audit / metrics.
    pub latency_ms: u32,
    /// Mirrors `ToxicityClassifier::model_id()` — captured per-call so audit
    /// events carry the value without lifetime juggling.
    pub model_id: String,
    /// Mirrors `ToxicityClassifier::policy_version()`.
    pub policy_version: String,
}

#[async_trait]
pub trait ToxicityClassifier: Send + Sync {
    /// Classify a single text. For replies-with-parent, callers compose the
    /// envelope via `crate::toxicity::format_parent_reply` and pass the result
    /// as `content`. There is no `classify_pair` shortcut on the trait.
    async fn classify(&self, content: &str) -> Result<ClassifierVerdict>;
    fn name(&self) -> &'static str;
    fn model_id(&self) -> &'static str;
    fn policy_version(&self) -> &'static str;
    /// Sole source of truth for the threshold. Each impl returns its own
    /// `const f32` calibrated for the model it wraps.
    fn threshold(&self) -> f32;
}

/// Apply the implementation's threshold. Free function rather than a default
/// trait method so callers must pass a concrete `&dyn ToxicityClassifier` and
/// can't accidentally bypass the impl's threshold.
pub fn is_toxic(classifier: &dyn ToxicityClassifier, v: &ClassifierVerdict) -> bool {
    v.toxic_token && v.confidence >= classifier.threshold()
}

/// Scripted classifier for tests. Pops verdicts from the front of an internal
/// queue per `classify` call; errors when exhausted to keep tests honest.
pub struct StubClassifier {
    script: Mutex<Vec<ClassifierVerdict>>,
    threshold: f32,
}

impl StubClassifier {
    pub fn with_script(script: Vec<ClassifierVerdict>) -> Self {
        Self {
            script: Mutex::new(script),
            threshold: 0.0,
        }
    }

    pub fn with_script_and_threshold(script: Vec<ClassifierVerdict>, threshold: f32) -> Self {
        Self {
            script: Mutex::new(script),
            threshold,
        }
    }
}

#[async_trait]
impl ToxicityClassifier for StubClassifier {
    async fn classify(&self, _content: &str) -> Result<ClassifierVerdict> {
        let mut guard = self.script.lock().expect("StubClassifier script lock");
        if guard.is_empty() {
            anyhow::bail!("stub script exhausted");
        }
        Ok(guard.remove(0))
    }
    fn name(&self) -> &'static str {
        "stub"
    }
    fn model_id(&self) -> &'static str {
        "stub"
    }
    fn policy_version(&self) -> &'static str {
        "stub"
    }
    fn threshold(&self) -> f32 {
        self.threshold
    }
}

/// Read `CHARCOAL_CLASSIFIER` and build the configured backend. Returns
/// `Err` when the var is unset, empty, or holds an unrecognized value —
/// the binary refuses to boot in those cases (spec §"Backend selection").
pub fn build_from_env() -> Result<Arc<dyn ToxicityClassifier>> {
    let kind = std::env::var("CHARCOAL_CLASSIFIER")
        .ok()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("CHARCOAL_CLASSIFIER must be set (one of: runpod, zentropi)")
        })?;

    match kind.as_str() {
        "runpod" => {
            let endpoint = std::env::var("RUNPOD_ENDPOINT_URL")
                .context("RUNPOD_ENDPOINT_URL must be set for CHARCOAL_CLASSIFIER=runpod")?;
            let api_key = std::env::var("RUNPOD_API_KEY")
                .context("RUNPOD_API_KEY must be set for CHARCOAL_CLASSIFIER=runpod")?;
            let client = crate::toxicity::runpod_cope_b::RunPodCopeBClient::new(endpoint, api_key)?;
            Ok(Arc::new(client))
        }
        "zentropi" => {
            let api_key = std::env::var("ZENTROPI_API_KEY")
                .context("ZENTROPI_API_KEY must be set for CHARCOAL_CLASSIFIER=zentropi")?;
            let labeler_id = std::env::var("ZENTROPI_LABELER_ID")
                .context("ZENTROPI_LABELER_ID must be set for CHARCOAL_CLASSIFIER=zentropi")?;
            let labeler_version_id = std::env::var("ZENTROPI_LABELER_VERSION_ID").ok();
            let client = crate::toxicity::zentropi::ZentropiClient::new(
                api_key,
                labeler_id,
                labeler_version_id,
            )?;
            Ok(Arc::new(client))
        }
        other => Err(anyhow::anyhow!(
            "CHARCOAL_CLASSIFIER={other:?} is not a known backend (runpod | zentropi)"
        )),
    }
}
