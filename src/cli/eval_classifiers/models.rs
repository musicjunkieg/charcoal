//! The eval's model registry. Eval-only by design: nothing here is reachable
//! from production `build_from_env`. Part B adds jev, clef-flash, clef and
//! strands-decider here.

use anyhow::{bail, Context, Result};
use std::sync::Arc;

use crate::toxicity::classifier::ToxicityClassifier;
use crate::toxicity::cost_meter::{ScanCostMeter, DEFAULT_RATE_CENTS_PER_HOUR};
use crate::toxicity::detoxify_classifier::DetoxifyClassifier;
use crate::toxicity::onnx::OnnxToxicityScorer;
use crate::toxicity::runpod_cope_b::RunPodCopeBClient;

pub const KNOWN_MODELS: &[&str] = &["cope-b", "detoxify"];

/// The run's cap as the meter's ceiling. Never 0: a zero ceiling disables
/// the backstop entirely.
pub fn cap_cents(cap_usd: f64) -> u32 {
    ((cap_usd * 100.0).ceil() as u32).max(1)
}

pub fn build_eval_model(name: &str, cap_usd: f64) -> Result<Arc<dyn ToxicityClassifier>> {
    match name {
        // Not via `build_backend_named`, which deliberately builds the client
        // with a DISABLED meter. Here the production cost meter (real GPU
        // worker time x rate) is set to the run's cap, so CoPE-B's spend is
        // bounded by what RunPod actually ran, not by an estimate.
        "cope-b" => {
            let endpoint = std::env::var("RUNPOD_ENDPOINT_URL")
                .context("RUNPOD_ENDPOINT_URL must be set for cope-b")?;
            let api_key =
                std::env::var("RUNPOD_API_KEY").context("RUNPOD_API_KEY must be set for cope-b")?;
            let meter = Arc::new(ScanCostMeter::new(
                cap_cents(cap_usd),
                DEFAULT_RATE_CENTS_PER_HOUR,
            ));
            Ok(Arc::new(
                RunPodCopeBClient::new(endpoint, api_key)?.with_meter(meter),
            ))
        }
        "detoxify" => {
            let dir = crate::toxicity::download::resolve_model_dir();
            Ok(Arc::new(DetoxifyClassifier::new(OnnxToxicityScorer::load(
                &dir,
            )?)))
        }
        other => bail!(
            "unknown eval model {other:?} (known: {})",
            KNOWN_MODELS.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_models_are_named_in_the_error() {
        let err = match build_eval_model("nope", 1.0) {
            Ok(_) => panic!("unknown model accepted"),
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains("cope-b") && err.contains("detoxify"), "{err}");
    }

    /// A zero ceiling DISABLES the meter (cost_meter.rs), so the cap must
    /// never round down to it.
    #[test]
    fn the_cap_never_becomes_a_disabled_meter() {
        assert_eq!(cap_cents(1.0), 100);
        assert_eq!(cap_cents(0.004), 1);
        assert_eq!(cap_cents(0.0), 1);
    }
}
