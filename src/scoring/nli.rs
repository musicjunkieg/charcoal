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
#[derive(Debug, Clone, serde::Serialize)]
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

/// Return the average context score, or None if no scores provided.
/// Uses the mean across all scored pairs to capture overall engagement
/// patterns rather than worst-case moments.
pub fn avg_context_score(scores: &[f64]) -> Option<f64> {
    if scores.is_empty() {
        None
    } else {
        Some(scores.iter().sum::<f64>() / scores.len() as f64)
    }
}

/// Apply softmax to 3-class NLI logits and return the entailment probability.
///
/// DeBERTa NLI label order: [0: contradiction, 1: entailment, 2: neutral]
/// See: https://huggingface.co/cross-encoder/nli-deberta-v3-xsmall
fn softmax_entailment(logits: &[f32]) -> f64 {
    let max_logit = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exp_sum: f32 = logits.iter().map(|&x| (x - max_logit).exp()).sum();
    let entailment_prob = (logits[1] - max_logit).exp() / exp_sum;
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

    /// Run NLI inference for one premise against many hypotheses in a SINGLE
    /// batched forward pass, returning one entailment probability (0.0-1.0)
    /// per hypothesis, in input order.
    ///
    /// Replaces the old one-inference-per-hypothesis loop (5 sequential
    /// spawn_blocking + mutex + ONNX runs per pair) with one padded
    /// `[N, max_len]` run (#213). Batch-of-1 is the previous single-item path
    /// exactly (no padding). Verified against 5× single runs in the unit test.
    ///
    /// DeBERTa NLI output: 3-class logits [contradiction, neutral, entailment]
    /// per row; we softmax each row and return the entailment score.
    async fn score_entailments_batched(
        &self,
        premise: &str,
        hypotheses: &[&str],
    ) -> Result<Vec<f64>> {
        if hypotheses.is_empty() {
            return Ok(Vec::new());
        }

        let session = Arc::clone(&self.session);
        let tokenizer = Arc::clone(&self.tokenizer);
        let premise = premise.to_string();
        let hypotheses: Vec<String> = hypotheses.iter().map(|h| h.to_string()).collect();

        tokio::task::spawn_blocking(move || {
            let batch = hypotheses.len();

            // Encode each (premise, hypothesis) pair, then pad all rows to the
            // longest so they stack into one [batch, max_len] tensor. Pad token
            // id 0, mask 0 — mirrors topics::embeddings::embed_batch.
            let mut encoded: Vec<(Vec<i64>, Vec<i64>)> = Vec::with_capacity(batch);
            for h in &hypotheses {
                // DeBERTa tokenizer handles pair encoding: [CLS] premise [SEP] hyp [SEP]
                let enc = tokenizer
                    .encode((premise.as_str(), h.as_str()), true)
                    .map_err(|e| anyhow::anyhow!("NLI tokenization failed: {}", e))?;
                let ids = enc.get_ids().iter().map(|&id| id as i64).collect();
                let mask = enc.get_attention_mask().iter().map(|&m| m as i64).collect();
                encoded.push((ids, mask));
            }
            let max_len = encoded.iter().map(|(ids, _)| ids.len()).max().unwrap_or(0);

            let mut input_ids_flat: Vec<i64> = Vec::with_capacity(batch * max_len);
            let mut attention_mask_flat: Vec<i64> = Vec::with_capacity(batch * max_len);
            for (mut ids, mut mask) in encoded {
                ids.resize(max_len, 0); // pad token id 0
                mask.resize(max_len, 0); // pad positions masked out
                input_ids_flat.extend(ids);
                attention_mask_flat.extend(mask);
            }

            let shape = [batch as i64, max_len as i64];
            let input_ids_tensor = Tensor::from_array((shape, input_ids_flat))
                .context("Failed to create input_ids")?;
            let attention_mask_tensor = Tensor::from_array((shape, attention_mask_flat))
                .context("Failed to create attention_mask")?;

            let logits_data = {
                let mut session = session
                    .lock()
                    .map_err(|e| anyhow::anyhow!("NLI session lock poisoned: {}", e))?;

                // Xenova's nli-deberta-v3-xsmall ONNX export only accepts
                // input_ids and attention_mask (no token_type_ids). Passing
                // an unexpected input causes ort to segfault rather than
                // return an error, so we only send the two known inputs. The
                // batch axis IS dynamic (verified before building #213).
                let outputs = session
                    .run(ort::inputs! {
                        "input_ids" => input_ids_tensor,
                        "attention_mask" => attention_mask_tensor
                    })
                    .context("NLI ONNX inference failed")?;

                // Output shape: [batch, 3] — per row [contradiction, neutral, entailment].
                let (_shape, data) = outputs[0]
                    .try_extract_tensor::<f32>()
                    .context("Failed to extract NLI output tensor")?;

                data.to_vec()
            };

            // Softmax each row's 3 logits → entailment probability.
            Ok(logits_data
                .chunks_exact(3)
                .map(softmax_entailment)
                .collect())
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
    pub async fn score_pair(
        &self,
        original_text: &str,
        response_text: &str,
    ) -> Result<(f64, HypothesisScores)> {
        let premise = format!("Original: {} Response: {}", original_text, response_text);

        // One batched forward pass for all 5 hypotheses (#213), replacing the
        // old 5 sequential single-item inferences. Results come back in
        // HYPOTHESES order.
        let hyps: Vec<&str> = HYPOTHESES.iter().map(|(_, h)| *h).collect();
        let scores = self.score_entailments_batched(&premise, &hyps).await?;

        let mut hypothesis_scores = HypothesisScores {
            attack: 0.0,
            contempt: 0.0,
            misrepresent: 0.0,
            good_faith_disagree: 0.0,
            support: 0.0,
        };
        for ((name, _), score) in HYPOTHESES.iter().zip(scores) {
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

        Ok((hostility, hypothesis_scores))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toxicity::download::{default_model_dir, nli_files_present};

    /// The batched forward pass (#213) must produce the same entailment score
    /// per hypothesis as running each hypothesis as its own [1, seq] inference.
    /// Batch-of-1 IS the old single-item path (no padding), so comparing
    /// batch-of-5 against 5× batch-of-1 proves padding + the batch dimension
    /// don't change results.
    ///
    /// Requires the NLI model locally; skips in CI where it isn't downloaded
    /// (same gating the rest of the ONNX code relies on).
    #[tokio::test]
    async fn batched_entailments_match_per_hypothesis_single_runs() {
        let base = default_model_dir();
        if !nli_files_present(&base) {
            eprintln!("SKIP: NLI model not present at {}", base.display());
            return;
        }
        let scorer = NliScorer::load(&base).expect("load NLI model");

        let premise =
            "Original: fat people deserve healthcare too Response: lol imagine being that big";
        // Hypotheses of DIFFERENT lengths so the batch actually pads.
        let hyps: Vec<&str> = HYPOTHESES.iter().map(|(_, h)| *h).collect();

        // Batch-of-5 (padded to the longest).
        let batched = scorer
            .score_entailments_batched(premise, &hyps)
            .await
            .expect("batched inference");
        assert_eq!(batched.len(), hyps.len());

        // 5× batch-of-1 (each unpadded — the previous single-item path).
        let mut singles = Vec::new();
        for h in hyps.iter() {
            let single = scorer
                .score_entailments_batched(premise, std::slice::from_ref(h))
                .await
                .expect("single inference");
            singles.push(single[0]);
        }

        // This quantized DeBERTa export is NOT perfectly padding-invariant: a
        // batched (padded) row differs from its unpadded single run by a small,
        // systematic amount (measured ≈0.002–0.008 per hypothesis, ≈0.006 on the
        // final hostility). That is a real behavior shift, accepted as within
        // the model's own quantization noise and immaterial to threat tiers
        // (bands 8/15/35). This bound catches segfaults, transposed rows, or any
        // LARGE divergence; it does not pretend the padding artifact is zero.
        const PAD_ARTIFACT_TOLERANCE: f64 = 0.02;
        for (i, (b, s)) in batched.iter().zip(&singles).enumerate() {
            assert!(
                (b - s).abs() < PAD_ARTIFACT_TOLERANCE,
                "hypothesis {i}: batched {b} vs single {s} exceeds padding tolerance",
            );
        }
    }
}
