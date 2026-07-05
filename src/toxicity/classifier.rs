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

/// One slot of a batch classification result. The request already succeeded
/// (HTTP 200, job COMPLETED); this distinguishes a decodable verdict from a
/// single un-decodable slot. Request-level failures (transport / 5xx / 4xx /
/// cost ceiling) are the outer `Result::Err`, never this enum.
#[derive(Debug, Clone)]
pub enum ItemOutcome {
    /// The slot decoded to a verdict.
    Verdict(ClassifierVerdict),
    /// The job completed but this slot's content did not decode to "0"/"1".
    /// Carries the backend's error detail for logging.
    Error(String),
}

#[async_trait]
pub trait ToxicityClassifier: Send + Sync {
    /// Classify a single text. For replies-with-parent, callers compose the
    /// envelope via `crate::toxicity::format_parent_reply` and pass the result
    /// as `content`. There is no `classify_pair` shortcut on the trait.
    async fn classify(&self, content: &str) -> Result<ClassifierVerdict>;

    /// Classify many texts in one backend round-trip, returning one
    /// [`ItemOutcome`] per input in the SAME order. The default implementation
    /// simply loops [`classify`]; the first request-level error short-circuits
    /// to an outer `Err` (so backends without a real batch endpoint — Zentropi,
    /// the test stub — behave exactly as they do today). Backends with native
    /// batching (RunPod) override this.
    async fn classify_batch(&self, contents: &[String]) -> Result<Vec<ItemOutcome>> {
        let mut out = Vec::with_capacity(contents.len());
        for content in contents {
            out.push(ItemOutcome::Verdict(self.classify(content).await?));
        }
        Ok(out)
    }

    /// Maximum number of texts to send per [`classify_batch`] request. Default
    /// `1` (today's one-text-per-call behaviour); RunPod overrides from
    /// `CHARCOAL_RUNPOD_BATCH_SIZE`. The burst phase chunks its queue by this.
    fn max_batch_size(&self) -> usize {
        1
    }

    fn name(&self) -> &'static str;
    fn model_id(&self) -> &'static str;
    fn policy_version(&self) -> &'static str;
    /// Sole source of truth for the threshold. Each impl returns its own
    /// `const f32` calibrated for the model it wraps.
    fn threshold(&self) -> f32;
}

/// A *transient* classifier failure: the backend was briefly unreachable and the
/// client exhausted its retry budget (e.g. a RunPod serverless blip — a
/// transport/connect error or a 5xx). It is deliberately distinct from a
/// *permanent* failure (HTTP 4xx, parse error) because retrying later — on a
/// resume — is expected to succeed once the backend recovers.
///
/// The burst phase (`run_burst`) downcasts to this (just as it does for
/// `CostCeilingExceeded`) so it can stop the burst *gracefully and resumably*
/// instead of hard-aborting the whole scan. Leaving the unscored rows pending is
/// safe precisely because the failure is transient. A permanent error, by
/// contrast, must still abort — leaving its row pending would livelock every
/// resume.
#[derive(Debug, thiserror::Error)]
#[error("classifier transient failure (retries exhausted): {0}")]
pub struct ClassifierTransientError(pub String);

impl ClassifierTransientError {
    pub fn new(detail: impl Into<String>) -> Self {
        Self(detail.into())
    }
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
            let meter = std::sync::Arc::new(crate::toxicity::cost_meter::ScanCostMeter::from_env());
            let client = crate::toxicity::runpod_cope_b::RunPodCopeBClient::new(endpoint, api_key)?
                .with_meter(meter);
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

/// Build one specific backend by name, reading that backend's env vars but
/// ignoring `CHARCOAL_CLASSIFIER`. Used by the A/B compare + shadow-agreement
/// gate tooling, which compares two named backends without changing the
/// boot-time prod selection.
pub fn build_backend_named(name: &str) -> Result<Arc<dyn ToxicityClassifier>> {
    match name.trim().to_lowercase().as_str() {
        "runpod" => {
            let endpoint = std::env::var("RUNPOD_ENDPOINT_URL")
                .context("RUNPOD_ENDPOINT_URL must be set for the runpod backend")?;
            let api_key = std::env::var("RUNPOD_API_KEY")
                .context("RUNPOD_API_KEY must be set for the runpod backend")?;
            // one-off compare/gate CLI: no per-scan cost backstop (disabled meter from new()).
            let client = crate::toxicity::runpod_cope_b::RunPodCopeBClient::new(endpoint, api_key)?;
            Ok(Arc::new(client))
        }
        "zentropi" => {
            let api_key = std::env::var("ZENTROPI_API_KEY")
                .context("ZENTROPI_API_KEY must be set for the zentropi backend")?;
            let labeler_id = std::env::var("ZENTROPI_LABELER_ID")
                .context("ZENTROPI_LABELER_ID must be set for the zentropi backend")?;
            let labeler_version_id = std::env::var("ZENTROPI_LABELER_VERSION_ID").ok();
            let client = crate::toxicity::zentropi::ZentropiClient::new(
                api_key,
                labeler_id,
                labeler_version_id,
            )?;
            Ok(Arc::new(client))
        }
        other => anyhow::bail!("unknown backend: {other:?} (expected runpod | zentropi)"),
    }
}

#[cfg(test)]
mod batch_trait_tests {
    use super::*;

    fn verdict(toxic: bool) -> ClassifierVerdict {
        ClassifierVerdict {
            toxic_token: toxic,
            confidence: 0.9,
            latency_ms: 1,
            model_id: "stub".into(),
            policy_version: "stub".into(),
        }
    }

    #[tokio::test]
    async fn default_classify_batch_maps_each_content_in_order() {
        // A 2-verdict script → classify_batch over 2 inputs yields 2 Verdicts,
        // in input order.
        let c = StubClassifier::with_script(vec![verdict(true), verdict(false)]);
        let out = c
            .classify_batch(&["a".to_string(), "b".to_string()])
            .await
            .expect("batch ok");
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], ItemOutcome::Verdict(ref v) if v.toxic_token));
        assert!(matches!(out[1], ItemOutcome::Verdict(ref v) if !v.toxic_token));
    }

    #[tokio::test]
    async fn default_classify_batch_short_circuits_on_first_error() {
        // Only one scripted verdict; the second classify() bails (script
        // exhausted) → the whole batch surfaces an outer Err (today's
        // request-level semantics; the default impl never yields ItemOutcome::Error).
        let c = StubClassifier::with_script(vec![verdict(true)]);
        let res = c.classify_batch(&["a".to_string(), "b".to_string()]).await;
        assert!(res.is_err(), "second item exhausts the script → outer Err");
    }

    #[test]
    fn default_max_batch_size_is_one() {
        let c = StubClassifier::with_script(vec![]);
        assert_eq!(c.max_batch_size(), 1);
    }
}
