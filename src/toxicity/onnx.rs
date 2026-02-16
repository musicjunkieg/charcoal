// Local ONNX toxicity scorer using Detoxify's unbiased-toxic-roberta model.
//
// This scorer runs entirely on the local CPU — no API calls, no rate limits,
// no network dependency. The model was specifically trained to reduce bias
// around identity mentions, which is critical for Charcoal's use case (the
// protected user posts about fat liberation, queer identity, DEI, etc.).
//
// Model: protectai/unbiased-toxic-roberta-onnx (quantized, ~126MB)
// Output: 7 toxicity categories with continuous 0-1 scores via sigmoid.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use async_trait::async_trait;
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;
use tracing::debug;

use super::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};

/// Labels output by unbiased-toxic-roberta, in the order the model returns them.
/// These map to: toxicity, severe_toxicity, obscene, identity_attack, insult, threat, sexual_explicit
const LABEL_ORDER: [&str; 7] = [
    "toxicity",
    "severe_toxicity",
    "obscene",
    "identity_attack",
    "insult",
    "threat",
    "sexual_explicit",
];

/// Local ONNX-based toxicity scorer. Holds the model session and tokenizer
/// behind Arc<Mutex> so inference can be offloaded to spawn_blocking without
/// blocking the async runtime.
pub struct OnnxToxicityScorer {
    // Arc+Mutex because:
    // 1. ort::Session::run takes &mut self, so we need interior mutability
    // 2. spawn_blocking requires 'static, so we need Arc for shared ownership
    // 3. We need Send+Sync for the ToxicityScorer trait
    // Inference is CPU-bound and serialized through spawn_blocking, so
    // contention is minimal.
    session: Arc<Mutex<Session>>,
    tokenizer: Arc<Tokenizer>,
}

impl OnnxToxicityScorer {
    /// Load the ONNX model and tokenizer from the given directory.
    ///
    /// Expects `model_quantized.onnx` and `tokenizer.json` to exist in `model_dir`.
    /// Call `download::download_model()` first if they don't.
    pub fn load(model_dir: &Path) -> Result<Self> {
        let model_path = model_dir.join("model_quantized.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");

        if !model_path.exists() {
            anyhow::bail!(
                "Model file not found: {}\nRun `charcoal download-model` to download it.",
                model_path.display()
            );
        }
        if !tokenizer_path.exists() {
            anyhow::bail!(
                "Tokenizer file not found: {}\nRun `charcoal download-model` to download it.",
                tokenizer_path.display()
            );
        }

        let session = Session::builder()
            .context("Failed to create ONNX session builder")?
            .commit_from_file(&model_path)
            .with_context(|| format!("Failed to load ONNX model from {}", model_path.display()))?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {}", e))?;

        debug!("Loaded ONNX toxicity model from {}", model_dir.display());

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer: Arc::new(tokenizer),
        })
    }
}

#[async_trait]
impl ToxicityScorer for OnnxToxicityScorer {
    async fn score_text(&self, text: &str) -> Result<ToxicityResult> {
        let mut results = self.score_batch(&[text.to_string()]).await?;
        Ok(results.remove(0))
    }

    /// True batch inference: tokenize all texts, run one forward pass, apply
    /// sigmoid to logits, and map outputs to ToxicityResult structs.
    ///
    /// The CPU-bound tokenization and inference are offloaded to spawn_blocking
    /// so they don't block the tokio async runtime.
    async fn score_batch(&self, texts: &[String]) -> Result<Vec<ToxicityResult>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Clone Arc handles for the spawn_blocking closure ('static requirement)
        let session = Arc::clone(&self.session);
        let tokenizer = Arc::clone(&self.tokenizer);
        let texts = texts.to_vec();

        // Offload all CPU-bound work (tokenization + inference) to a blocking
        // thread so the async runtime stays responsive for other tasks.
        tokio::task::spawn_blocking(move || {
            // Tokenize all texts, finding the max sequence length for padding
            let encodings: Vec<_> = texts
                .iter()
                .map(|t| {
                    tokenizer
                        .encode(t.as_str(), true)
                        .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))
                })
                .collect::<Result<Vec<_>>>()?;

            let batch_size = encodings.len();
            let max_len = encodings.iter().map(|e| e.get_ids().len()).max().unwrap_or(0);

            // Build flat input tensors with right-padding to max_len.
            // Shape: [batch_size, max_len]
            let mut input_ids_flat: Vec<i64> = Vec::with_capacity(batch_size * max_len);
            let mut attention_mask_flat: Vec<i64> = Vec::with_capacity(batch_size * max_len);

            for enc in &encodings {
                let ids = enc.get_ids();
                let mask = enc.get_attention_mask();
                let seq_len = ids.len();

                // Copy actual tokens
                for &id in ids {
                    input_ids_flat.push(id as i64);
                }
                for &m in mask {
                    attention_mask_flat.push(m as i64);
                }

                // Pad to max_len (pad_id = 1 for RoBERTa)
                for _ in seq_len..max_len {
                    input_ids_flat.push(1); // RoBERTa pad token id
                    attention_mask_flat.push(0);
                }
            }

            let shape = [batch_size as i64, max_len as i64];

            let input_ids_tensor = Tensor::from_array((shape, input_ids_flat))
                .context("Failed to create input_ids tensor")?;
            let attention_mask_tensor = Tensor::from_array((shape, attention_mask_flat))
                .context("Failed to create attention_mask tensor")?;

            let logits_data = {
                let mut session = session
                    .lock()
                    .map_err(|e| anyhow::anyhow!("Session lock poisoned: {}", e))?;

                let outputs = session
                    .run(ort::inputs! {
                        "input_ids" => input_ids_tensor,
                        "attention_mask" => attention_mask_tensor
                    })
                    .context("ONNX inference failed")?;

                // Output shape: [batch_size, 7] — raw logits (pre-sigmoid)
                let (_out_shape, data) = outputs[0]
                    .try_extract_tensor::<f32>()
                    .context("Failed to extract output tensor")?;

                data.to_vec()
            };

            // Convert logits to results: apply sigmoid and map to our attribute struct
            let mut results = Vec::with_capacity(batch_size);
            for (i, text) in texts.iter().enumerate() {
                let offset = i * LABEL_ORDER.len();
                let row = &logits_data[offset..offset + LABEL_ORDER.len()];

                // Apply sigmoid to each logit to get 0-1 probability
                let scores: Vec<f64> = row.iter().map(|&logit| sigmoid(logit as f64)).collect();

                let result = map_scores_to_result(&scores);

                debug!(
                    toxicity = result.toxicity,
                    severe_toxicity = ?result.attributes.severe_toxicity,
                    identity_attack = ?result.attributes.identity_attack,
                    text_preview = %crate::output::truncate_chars(text, 50),
                    "ONNX scored text"
                );

                results.push(result);
            }

            Ok(results)
        })
        .await
        .context("spawn_blocking panicked")?
    }
}

/// Sigmoid activation: maps any real number to (0, 1).
fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// Map the 7 model output scores to our ToxicityResult struct.
///
/// Model outputs (in order): toxicity, severe_toxicity, obscene, identity_attack,
/// insult, threat, sexual_explicit.
///
/// We map obscene → profanity (closest semantic match) and drop sexual_explicit
/// since we don't have a field for it.
fn map_scores_to_result(scores: &[f64]) -> ToxicityResult {
    ToxicityResult {
        toxicity: scores[0],
        attributes: ToxicityAttributes {
            severe_toxicity: Some(scores[1]),
            // "obscene" from the model maps to "profanity" in our schema
            identity_attack: Some(scores[3]),
            insult: Some(scores[4]),
            profanity: Some(scores[2]),
            threat: Some(scores[5]),
            // scores[6] = sexual_explicit — no field in ToxicityAttributes, dropped
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sigmoid_zero() {
        let result = sigmoid(0.0);
        assert!((result - 0.5).abs() < 1e-10, "sigmoid(0) should be 0.5");
    }

    #[test]
    fn test_sigmoid_large_positive() {
        let result = sigmoid(10.0);
        assert!(result > 0.999, "sigmoid(10) should be very close to 1.0");
    }

    #[test]
    fn test_sigmoid_large_negative() {
        let result = sigmoid(-10.0);
        assert!(result < 0.001, "sigmoid(-10) should be very close to 0.0");
    }

    #[test]
    fn test_sigmoid_symmetry() {
        // sigmoid(x) + sigmoid(-x) = 1.0
        for x in [0.5, 1.0, 2.0, 5.0] {
            let sum = sigmoid(x) + sigmoid(-x);
            assert!(
                (sum - 1.0).abs() < 1e-10,
                "sigmoid({x}) + sigmoid(-{x}) should equal 1.0"
            );
        }
    }

    #[test]
    fn test_map_scores_to_result() {
        // Model outputs: toxicity, severe_toxicity, obscene, identity_attack, insult, threat, sexual_explicit
        let scores = vec![0.9, 0.1, 0.8, 0.3, 0.7, 0.05, 0.4];
        let result = map_scores_to_result(&scores);

        assert!((result.toxicity - 0.9).abs() < 1e-10);
        assert!((result.attributes.severe_toxicity.unwrap() - 0.1).abs() < 1e-10);
        // obscene → profanity
        assert!((result.attributes.profanity.unwrap() - 0.8).abs() < 1e-10);
        assert!((result.attributes.identity_attack.unwrap() - 0.3).abs() < 1e-10);
        assert!((result.attributes.insult.unwrap() - 0.7).abs() < 1e-10);
        assert!((result.attributes.threat.unwrap() - 0.05).abs() < 1e-10);
    }

    #[test]
    fn test_label_order_count() {
        assert_eq!(LABEL_ORDER.len(), 7, "Model should output 7 categories");
    }
}
