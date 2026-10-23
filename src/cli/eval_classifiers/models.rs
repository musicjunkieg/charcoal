//! The eval's model registry. Eval-only by design: nothing here is reachable
//! from production `build_from_env`. Part B adds jev, clef-flash, clef and
//! strands-decider here.

use anyhow::{bail, Context, Result};
use std::sync::Arc;

use super::runner::EvalModel;
use super::s1::{S1Client, S1Mode};
use crate::toxicity::classifier::ToxicityClassifier;
use crate::toxicity::cost_meter::ScanCostMeter;
use crate::toxicity::detoxify_classifier::DetoxifyClassifier;
use crate::toxicity::onnx::OnnxToxicityScorer;
use crate::toxicity::runpod_cope_b::{RunPodCopeBClient, COPE_B_DEFAULT_POLICY, COPE_B_POLICY_ENV};

pub const KNOWN_MODELS: &[&str] = &["cope-b", "detoxify", "s1-master", "s1-bundle"];

/// The run's cap as the meter's ceiling. Never 0: a zero ceiling disables
/// the backstop entirely.
pub fn cap_cents(cap_usd: f64) -> u32 {
    ((cap_usd * 100.0).ceil() as u32).max(1)
}

/// Refuse a missing/default CoPE-B policy version. Pure (takes the raw value)
/// so it is testable without touching process-global env.
///
/// Why before the client exists: with the variable unset the client expects
/// "policy-unknown", and the mismatch used to be detected only AFTER the
/// probe's paid warm-up woke the GPU. That refuse-after-wake pattern cost
/// $193 in 16 days in production; the eval must fail before any network call.
fn require_cope_b_policy(raw: Option<&str>) -> Result<()> {
    match raw.map(str::trim) {
        None | Some("") => bail!(
            "{COPE_B_POLICY_ENV} must be set for cope-b: without it every verdict is \
             refused only after a paid GPU wake. Refusing before any network call."
        ),
        Some(v) if v == COPE_B_DEFAULT_POLICY => bail!(
            "{COPE_B_POLICY_ENV} is the placeholder {COPE_B_DEFAULT_POLICY:?}; set the real \
             policy version. Refusing before any network call."
        ),
        Some(_) => Ok(()),
    }
}

pub fn build_eval_model(name: &str, cap_usd: f64) -> Result<EvalModel> {
    match name {
        // Not via `build_backend_named`, which deliberately builds the client
        // with a DISABLED meter. Here the production cost meter (real GPU
        // worker time x rate) is set to the run's cap, so CoPE-B's spend is
        // bounded by what RunPod actually ran, not by an estimate.
        "cope-b" => {
            require_cope_b_policy(std::env::var(COPE_B_POLICY_ENV).ok().as_deref())?;
            let endpoint = std::env::var("RUNPOD_ENDPOINT_URL")
                .context("RUNPOD_ENDPOINT_URL must be set for cope-b")?;
            let api_key =
                std::env::var("RUNPOD_API_KEY").context("RUNPOD_API_KEY must be set for cope-b")?;
            // Same rate/workers resolution as production (`from_env`), with
            // the run cap as ceiling. The meter is a pre-call check: an
            // in-flight batch completes and RunPod's ~60 s idle tail per
            // worker is not counted, so real spend can overshoot the cap by
            // about one batch plus one tail.
            let meter = Arc::new(ScanCostMeter::from_env_with_ceiling(cap_cents(cap_usd)));
            Ok(EvalModel::from(Arc::new(
                RunPodCopeBClient::new(endpoint, api_key)?.with_meter(meter),
            )
                as Arc<dyn ToxicityClassifier>))
        }
        "detoxify" => {
            let dir = crate::toxicity::download::resolve_model_dir();
            Ok(EvalModel::from(
                Arc::new(DetoxifyClassifier::new(OnnxToxicityScorer::load(&dir)?))
                    as Arc<dyn ToxicityClassifier>,
            ))
        }
        // Hosted and free for development, so no cost meter: the run's $0
        // price lets it run even after a paid model used up the cap.
        "s1-master" | "s1-bundle" => {
            let key = std::env::var("ZENTROPI_API_KEY")
                .context("ZENTROPI_API_KEY must be set for s1-master / s1-bundle")?;
            let mode = if name == "s1-master" {
                S1Mode::Master
            } else {
                S1Mode::Bundle
            };
            let client = Arc::new(S1Client::new(key, mode)?);
            Ok(EvalModel {
                classifier: client.clone(),
                reasons: Some(client),
            })
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

    /// The $193 incident: an unset policy var makes the client expect
    /// "policy-unknown", and the refusal only came after a paid GPU wake.
    #[test]
    fn missing_or_default_policy_is_refused_before_any_network_call() {
        for raw in [
            None,
            Some(""),
            Some("   "),
            Some("policy-unknown"),
            Some(" policy-unknown\n"),
        ] {
            let err = require_cope_b_policy(raw).expect_err("must refuse");
            assert!(
                format!("{err:#}").contains("CHARCOAL_COPE_B_POLICY_VERSION"),
                "{raw:?}"
            );
        }
        assert!(require_cope_b_policy(Some("policy-ddac468-2026-07-03")).is_ok());
    }

    #[test]
    fn unknown_models_are_named_in_the_error() {
        let err = match build_eval_model("nope", 1.0) {
            Ok(_) => panic!("unknown model accepted"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            err.contains("cope-b")
                && err.contains("detoxify")
                && err.contains("s1-master")
                && err.contains("s1-bundle"),
            "{err}"
        );
    }

    /// A zero ceiling DISABLES the meter (cost_meter.rs), so the cap must
    /// never round down to it.
    #[test]
    fn the_cap_never_becomes_a_disabled_meter() {
        assert_eq!(cap_cents(1.0), 100);
        assert_eq!(cap_cents(0.004), 1);
        assert_eq!(cap_cents(0.0), 1);
    }

    /// Every known model must be priced, or `run` refuses it at the door.
    #[test]
    fn every_known_model_is_in_the_committed_pricing() {
        let p =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("evals/classifiers/pricing.json");
        let prices = crate::cli::eval_classifiers::cost::load_pricing(&p).unwrap();
        for m in KNOWN_MODELS {
            assert!(prices.contains_key(*m), "{m} missing from pricing.json");
        }
    }

    /// S1 is free for development: an exhausted budget must not stop it.
    #[test]
    fn s1_models_are_priced_free() {
        let p =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("evals/classifiers/pricing.json");
        let prices = crate::cli::eval_classifiers::cost::load_pricing(&p).unwrap();
        assert!(!prices["s1-master"].is_paid() && !prices["s1-bundle"].is_paid());
    }
}
