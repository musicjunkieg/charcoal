//! NLI-based contextual hostility scoring.
//!
//! Uses a DeBERTa-v3-xsmall cross-encoder (quantized ONNX, ~87MB) to score
//! text pairs for hostile engagement patterns. The model takes a premise and
//! hypothesis and outputs entailment/contradiction/neutral probabilities.
//!
//! Five hypothesis templates detect different hostility signals:
//! - Attack/mockery (ad hominem, direct hostility)
//! - Contempt (dismissiveness, eye-rolling)
//! - Misrepresentation (strawmanning, goalpost moving)
//! - Good-faith disagreement (NOT a threat — reduces hostility score)
//! - Support/agreement (ally signal — reduces hostility score)
//!
//! The contextual hostility score is:
//!   hostile_signal = max(attack, contempt, misrepresent)
//!   supportive_signal = max(good_faith * 0.5, support * 0.8)
//!   hostility = clamp(hostile_signal - supportive_signal, 0.0, 1.0)

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;
use tracing::debug;

/// Raw entailment scores from running NLI hypotheses on a text pair.
#[derive(Debug, Clone)]
pub struct HypothesisScores {
    pub attack: f64,
    pub contempt: f64,
    pub misrepresent: f64,
    pub good_faith_disagree: f64,
    pub support: f64,
}

/// Compute contextual hostility score from NLI hypothesis entailment scores.
///
/// Formula:
///   hostile_signal = max(attack, contempt, misrepresent)
///   supportive_signal = max(good_faith * 0.5, support * 0.8)
///   hostility = clamp(hostile_signal - supportive_signal, 0.0, 1.0)
pub fn compute_hostility_score(scores: &HypothesisScores) -> f64 {
    let hostile_signal = scores.attack.max(scores.contempt).max(scores.misrepresent);
    let supportive_signal = (scores.good_faith_disagree * 0.5).max(scores.support * 0.8);
    (hostile_signal - supportive_signal).clamp(0.0, 1.0)
}

/// Return the maximum context score, or None if no scores provided.
/// Used when an account has multiple interactions — one hostile
/// interaction is sufficient signal.
pub fn max_context_score_opt(scores: &[f64]) -> Option<f64> {
    if scores.is_empty() {
        None
    } else {
        Some(scores.iter().copied().fold(f64::NEG_INFINITY, f64::max))
    }
}

/// Apply softmax to 3-class NLI logits and return the entailment probability.
fn softmax_entailment(logits: &[f32]) -> f64 {
    let max_logit = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exp_sum: f32 = logits.iter().map(|&x| (x - max_logit).exp()).sum();
    let entailment_prob = (logits[2] - max_logit).exp() / exp_sum;
    entailment_prob as f64
}

/// The 5 hypothesis templates used for NLI inference.
/// Each is (field_name, hypothesis_text).
const HYPOTHESES: [(&str, &str); 5] = [
    (
        "attack",
        "The second text attacks or mocks the author of the first text",
    ),
    (
        "contempt",
        "The second text dismisses the first text with contempt",
    ),
    (
        "misrepresent",
        "The second text misrepresents what the first text says",
    ),
    (
        "good_faith_disagree",
        "The second text respectfully disagrees with the first text",
    ),
    (
        "support",
        "The second text supports or agrees with the first text",
    ),
];

/// NLI cross-encoder scorer. Loads the DeBERTa-v3-xsmall ONNX model
/// and scores text pairs against hostility hypotheses.
pub struct NliScorer {
    session: Arc<Mutex<Session>>,
    tokenizer: Arc<Tokenizer>,
}

impl NliScorer {
    /// Load the NLI model and tokenizer from the nli-deberta-v3-xsmall subdirectory.
    pub fn load(model_dir: &Path) -> Result<Self> {
        let nli_dir = crate::toxicity::download::nli_model_dir(model_dir);
        let model_path = nli_dir.join("model_quantized.onnx");
        let tokenizer_path = nli_dir.join("tokenizer.json");

        if !model_path.exists() {
            anyhow::bail!(
                "NLI model not found: {}\nRun `charcoal download-model` to download it.",
                model_path.display()
            );
        }
        if !tokenizer_path.exists() {
            anyhow::bail!(
                "NLI tokenizer not found: {}\nRun `charcoal download-model` to download it.",
                tokenizer_path.display()
            );
        }

        let session = Session::builder()
            .context("Failed to create NLI ONNX session builder")?
            .commit_from_file(&model_path)
            .with_context(|| {
                format!(
                    "Failed to load NLI ONNX model from {}",
                    model_path.display()
                )
            })?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load NLI tokenizer: {}", e))?;

        debug!("Loaded NLI cross-encoder model from {}", nli_dir.display());

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer: Arc::new(tokenizer),
        })
    }

    /// Run a single NLI inference: given a premise and hypothesis, return
    /// the entailment probability (0.0-1.0).
    ///
    /// DeBERTa NLI output: 3-class logits [contradiction, neutral, entailment]
    /// We apply softmax and return the entailment score.
    async fn score_entailment(&self, premise: &str, hypothesis: &str) -> Result<f64> {
        let session = Arc::clone(&self.session);
        let tokenizer = Arc::clone(&self.tokenizer);
        let premise = premise.to_string();
        let hypothesis = hypothesis.to_string();

        tokio::task::spawn_blocking(move || {
            // DeBERTa tokenizer handles pair encoding: [CLS] premise [SEP] hypothesis [SEP]
            let encoding = tokenizer
                .encode((premise.as_str(), hypothesis.as_str()), true)
                .map_err(|e| anyhow::anyhow!("NLI tokenization failed: {}", e))?;

            let ids = encoding.get_ids();
            let mask = encoding.get_attention_mask();
            let type_ids = encoding.get_type_ids();
            let seq_len = ids.len();

            let input_ids: Vec<i64> = ids.iter().map(|&id| id as i64).collect();
            let attention_mask: Vec<i64> = mask.iter().map(|&m| m as i64).collect();
            let token_type_ids: Vec<i64> = type_ids.iter().map(|&t| t as i64).collect();

            let shape = [1i64, seq_len as i64];

            let input_ids_tensor =
                Tensor::from_array((shape, input_ids)).context("Failed to create input_ids")?;
            let attention_mask_tensor = Tensor::from_array((shape, attention_mask))
                .context("Failed to create attention_mask")?;
            let token_type_ids_tensor = Tensor::from_array((shape, token_type_ids))
                .context("Failed to create token_type_ids")?;

            let logits_data = {
                let mut session = session
                    .lock()
                    .map_err(|e| anyhow::anyhow!("NLI session lock poisoned: {}", e))?;

                // Many ONNX exports of DeBERTa (including Xenova's) omit
                // token_type_ids. Try without it first; if that fails, retry
                // with all three inputs.
                let first_err_msg = match session.run(ort::inputs! {
                    "input_ids" => input_ids_tensor.clone(),
                    "attention_mask" => attention_mask_tensor.clone()
                }) {
                    Ok(out) => {
                        let (_shape, data) = out[0]
                            .try_extract_tensor::<f32>()
                            .context("Failed to extract NLI output tensor")?;
                        return Ok(softmax_entailment(data));
                    }
                    Err(e) => e.to_string(), // Convert to String to release session borrow
                };

                debug!(error = first_err_msg, "NLI inference without token_type_ids failed, retrying with");
                let outputs = session
                    .run(ort::inputs! {
                        "input_ids" => input_ids_tensor,
                        "attention_mask" => attention_mask_tensor,
                        "token_type_ids" => token_type_ids_tensor
                    })
                    .with_context(|| format!("NLI ONNX inference failed both ways. First: {first_err_msg}"))?;

                // Output shape: [1, 3] — logits for [contradiction, neutral, entailment]
                let (_shape, data) = outputs[0]
                    .try_extract_tensor::<f32>()
                    .context("Failed to extract NLI output tensor")?;

                data.to_vec()
            };

            Ok(softmax_entailment(&logits_data))
        })
        .await
        .context("NLI spawn_blocking panicked")?
    }

    /// Score a text pair against all 5 hostility hypotheses and compute
    /// the contextual hostility score.
    ///
    /// The premise combines both texts so the model sees the interaction:
    /// "Original: {original} Response: {response}"
    /// Each hypothesis template is tested against this premise.
    pub async fn score_pair(&self, original_text: &str, response_text: &str) -> Result<f64> {
        let premise = format!("Original: {} Response: {}", original_text, response_text);

        let mut hypothesis_scores = HypothesisScores {
            attack: 0.0,
            contempt: 0.0,
            misrepresent: 0.0,
            good_faith_disagree: 0.0,
            support: 0.0,
        };

        for (name, hypothesis) in &HYPOTHESES {
            let score = self.score_entailment(&premise, hypothesis).await?;
            match *name {
                "attack" => hypothesis_scores.attack = score,
                "contempt" => hypothesis_scores.contempt = score,
                "misrepresent" => hypothesis_scores.misrepresent = score,
                "good_faith_disagree" => hypothesis_scores.good_faith_disagree = score,
                "support" => hypothesis_scores.support = score,
                _ => {}
            }
        }

        let hostility = compute_hostility_score(&hypothesis_scores);

        debug!(
            attack = format!("{:.3}", hypothesis_scores.attack),
            contempt = format!("{:.3}", hypothesis_scores.contempt),
            misrepresent = format!("{:.3}", hypothesis_scores.misrepresent),
            good_faith = format!("{:.3}", hypothesis_scores.good_faith_disagree),
            support = format!("{:.3}", hypothesis_scores.support),
            hostility = format!("{:.3}", hostility),
            "NLI scored pair"
        );

        Ok(hostility)
    }
}
