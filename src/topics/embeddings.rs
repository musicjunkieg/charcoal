// Sentence embedding-based topic overlap using all-MiniLM-L6-v2.
//
// Instead of comparing TF-IDF keyword lists (which fail when two people use
// different words for the same topic — see docs/research-overlap-diagnosis.md),
// this module embeds post text into 384-dimensional vectors using a sentence
// transformer. Cosine similarity between mean embeddings captures semantic
// proximity: "fatphobia" and "obesity" land near each other even though they
// share zero characters.
//
// The model runs locally via ONNX — no API calls, no rate limits.
// Mean pooling is applied to token embeddings (matching the model's training).

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;
use tracing::debug;

/// Embedding dimension for all-MiniLM-L6-v2.
pub const EMBEDDING_DIM: usize = 384;

/// Sentence embedder using a local ONNX model. Converts text into dense
/// 384-dimensional vectors suitable for cosine similarity comparison.
///
/// Architecture mirrors OnnxToxicityScorer: Arc<Mutex<Session>> for thread
/// safety, Arc<Tokenizer> for shared ownership across spawn_blocking.
pub struct SentenceEmbedder {
    session: Arc<Mutex<Session>>,
    tokenizer: Arc<Tokenizer>,
}

impl SentenceEmbedder {
    /// Load the sentence embedding model and tokenizer from the given directory.
    ///
    /// Expects `model.onnx` and `tokenizer.json` in the directory.
    /// Call `download_model()` first if they don't exist.
    pub fn load(model_dir: &Path) -> Result<Self> {
        let model_path = model_dir.join("model.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");

        if !model_path.exists() {
            anyhow::bail!(
                "Embedding model not found: {}\nRun `charcoal download-model` to download it.",
                model_path.display()
            );
        }
        if !tokenizer_path.exists() {
            anyhow::bail!(
                "Embedding tokenizer not found: {}\nRun `charcoal download-model` to download it.",
                tokenizer_path.display()
            );
        }

        let session = Session::builder()
            .context("Failed to create ONNX session builder")?
            .commit_from_file(&model_path)
            .with_context(|| {
                format!(
                    "Failed to load embedding model from {}",
                    model_path.display()
                )
            })?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load embedding tokenizer: {}", e))?;

        debug!(
            "Loaded sentence embedding model from {}",
            model_dir.display()
        );

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer: Arc::new(tokenizer),
        })
    }

    /// Embed a batch of texts into 384-dimensional vectors.
    ///
    /// Each text is tokenized, run through the BERT model, and mean-pooled
    /// (averaged across tokens, weighted by attention mask) to produce a
    /// single vector representing the semantic content of the text.
    ///
    /// CPU-bound work is offloaded to spawn_blocking to keep the async
    /// runtime responsive.
    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f64>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let session = Arc::clone(&self.session);
        let tokenizer = Arc::clone(&self.tokenizer);
        let texts = texts.to_vec();

        tokio::task::spawn_blocking(move || embed_sync(&session, &tokenizer, &texts))
            .await
            .context("spawn_blocking panicked")?
    }
}

/// Synchronous embedding — runs tokenization, inference, and mean pooling.
/// Called from spawn_blocking to avoid blocking the async runtime.
fn embed_sync(
    session: &Arc<Mutex<Session>>,
    tokenizer: &Arc<Tokenizer>,
    texts: &[String],
) -> Result<Vec<Vec<f64>>> {
    // Tokenize all texts
    let encodings: Vec<_> = texts
        .iter()
        .map(|t| {
            tokenizer
                .encode(t.as_str(), true)
                .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))
        })
        .collect::<Result<Vec<_>>>()?;

    let batch_size = encodings.len();
    let max_len = encodings
        .iter()
        .map(|e| e.get_ids().len())
        .max()
        .unwrap_or(0);

    if max_len == 0 {
        return Ok(vec![vec![0.0; EMBEDDING_DIM]; batch_size]);
    }

    // Build padded input tensors. BERT uses:
    //   input_ids: token IDs (pad with 0)
    //   attention_mask: 1 for real tokens, 0 for padding
    //   token_type_ids: all zeros for single-sentence input
    let mut input_ids_flat: Vec<i64> = Vec::with_capacity(batch_size * max_len);
    let mut attention_mask_flat: Vec<i64> = Vec::with_capacity(batch_size * max_len);
    let mut token_type_ids_flat: Vec<i64> = Vec::with_capacity(batch_size * max_len);

    for enc in &encodings {
        let ids = enc.get_ids();
        let mask = enc.get_attention_mask();
        let seq_len = ids.len();

        input_ids_flat.extend(ids.iter().map(|&id| id as i64));
        attention_mask_flat.extend(mask.iter().map(|&m| m as i64));
        token_type_ids_flat.extend(std::iter::repeat_n(0i64, seq_len));

        // Pad to max_len (BERT pad token id = 0)
        let pad_len = max_len - seq_len;
        input_ids_flat.extend(std::iter::repeat_n(0i64, pad_len));
        attention_mask_flat.extend(std::iter::repeat_n(0i64, pad_len));
        token_type_ids_flat.extend(std::iter::repeat_n(0i64, pad_len));
    }

    let shape = [batch_size as i64, max_len as i64];

    let input_ids_tensor =
        Tensor::from_array((shape, input_ids_flat)).context("Failed to create input_ids tensor")?;
    let attention_mask_tensor = Tensor::from_array((shape, attention_mask_flat.clone()))
        .context("Failed to create attention_mask tensor")?;
    let token_type_ids_tensor = Tensor::from_array((shape, token_type_ids_flat))
        .context("Failed to create token_type_ids tensor")?;

    // Run inference — output is last_hidden_state: [batch, seq_len, 384]
    let hidden_states = {
        let mut session = session
            .lock()
            .map_err(|e| anyhow::anyhow!("Session lock poisoned: {}", e))?;

        let outputs = session
            .run(ort::inputs! {
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "token_type_ids" => token_type_ids_tensor
            })
            .context("Embedding ONNX inference failed")?;

        let (_shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .context("Failed to extract embedding output tensor")?;

        data.to_vec()
    };

    // Mean pooling: average token embeddings weighted by attention mask.
    // For each text in the batch, we sum (token_embedding * attention_mask)
    // across all tokens, then divide by the sum of the attention mask.
    let mut embeddings = Vec::with_capacity(batch_size);

    for i in 0..batch_size {
        let mut sum = vec![0.0_f64; EMBEDDING_DIM];
        let mut mask_sum = 0.0_f64;

        for j in 0..max_len {
            let mask_val = attention_mask_flat[i * max_len + j] as f64;
            if mask_val > 0.0 {
                mask_sum += mask_val;
                let offset = (i * max_len + j) * EMBEDDING_DIM;
                for k in 0..EMBEDDING_DIM {
                    sum[k] += hidden_states[offset + k] as f64 * mask_val;
                }
            }
        }

        // Divide by mask sum to get mean (avoid division by zero)
        if mask_sum > 0.0 {
            for val in &mut sum {
                *val /= mask_sum;
            }
        }

        embeddings.push(sum);
    }

    debug!(
        batch_size = batch_size,
        dim = EMBEDDING_DIM,
        "Computed sentence embeddings"
    );

    Ok(embeddings)
}

/// Compute the mean of multiple embedding vectors.
///
/// Used to create a single "topic vector" for an account by averaging
/// the embeddings of all their posts. This produces a stable centroid
/// that represents the overall semantic space of what someone talks about.
pub fn mean_embedding(embeddings: &[Vec<f64>]) -> Vec<f64> {
    if embeddings.is_empty() {
        return vec![0.0; EMBEDDING_DIM];
    }

    let n = embeddings.len() as f64;
    let mut mean = vec![0.0_f64; EMBEDDING_DIM];

    for emb in embeddings {
        for (i, &val) in emb.iter().enumerate() {
            if i < EMBEDDING_DIM {
                mean[i] += val;
            }
        }
    }

    for val in &mut mean {
        *val /= n;
    }

    mean
}

/// Cosine similarity between two embedding vectors.
///
/// Returns 0.0 to 1.0 — the core comparison that replaces keyword-based
/// overlap. Two accounts posting about the same topics will have high
/// cosine similarity even if they use completely different vocabulary.
pub fn cosine_similarity_embeddings(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let dot: f64 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let mag_b: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();

    let denom = mag_a * mag_b;
    if denom < f64::EPSILON {
        0.0
    } else {
        (dot / denom).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mean_embedding_single() {
        let embeddings = vec![vec![1.0, 2.0, 3.0]];
        let mean = mean_embedding(&embeddings);
        assert_eq!(mean.len(), EMBEDDING_DIM);
        assert!((mean[0] - 1.0).abs() < f64::EPSILON);
        assert!((mean[1] - 2.0).abs() < f64::EPSILON);
        assert!((mean[2] - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_mean_embedding_multiple() {
        let embeddings = vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]];
        let mean = mean_embedding(&embeddings);
        assert!((mean[0] - 0.5).abs() < f64::EPSILON);
        assert!((mean[1] - 0.5).abs() < f64::EPSILON);
        assert!((mean[2] - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_mean_embedding_empty() {
        let embeddings: Vec<Vec<f64>> = vec![];
        let mean = mean_embedding(&embeddings);
        assert_eq!(mean.len(), EMBEDDING_DIM);
        assert!(mean.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn test_cosine_identical() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity_embeddings(&a, &b);
        assert!((sim - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_cosine_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity_embeddings(&a, &b);
        assert!(sim.abs() < 1e-10);
    }

    #[test]
    fn test_cosine_proportional() {
        // Same direction, different magnitudes — should be 1.0
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![2.0, 4.0, 6.0];
        let sim = cosine_similarity_embeddings(&a, &b);
        assert!((sim - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_cosine_empty() {
        let a: Vec<f64> = vec![];
        let b: Vec<f64> = vec![];
        let sim = cosine_similarity_embeddings(&a, &b);
        assert!(sim.abs() < f64::EPSILON);
    }

    #[test]
    fn test_cosine_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity_embeddings(&a, &b);
        assert!(sim.abs() < f64::EPSILON);
    }

    #[test]
    fn test_cosine_mismatched_dimensions() {
        let a = vec![1.0, 2.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity_embeddings(&a, &b);
        assert!(sim.abs() < f64::EPSILON, "Mismatched dims should return 0.0");
    }

    #[test]
    fn test_cosine_negative_values() {
        // Opposite directions should give low similarity (clamped to 0.0)
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        let sim = cosine_similarity_embeddings(&a, &b);
        assert!(
            sim.abs() < f64::EPSILON,
            "Opposite vectors should clamp to 0.0, got {sim}"
        );
    }

    #[test]
    fn test_cosine_is_symmetric() {
        let a = vec![1.0, 3.0, -2.0, 0.5];
        let b = vec![2.0, -1.0, 4.0, 0.0];
        let sim_ab = cosine_similarity_embeddings(&a, &b);
        let sim_ba = cosine_similarity_embeddings(&b, &a);
        assert!(
            (sim_ab - sim_ba).abs() < 1e-10,
            "Cosine should be symmetric"
        );
    }

    #[test]
    fn test_mean_embedding_all_same() {
        // Averaging identical vectors should return the same vector
        let v = vec![0.5, -0.3, 0.8];
        let embeddings = vec![v.clone(), v.clone(), v.clone()];
        let mean = mean_embedding(&embeddings);
        assert!((mean[0] - 0.5).abs() < 1e-10);
        assert!((mean[1] - -0.3).abs() < 1e-10);
        assert!((mean[2] - 0.8).abs() < 1e-10);
    }

    #[test]
    fn test_mean_embedding_result_is_embedding_dim() {
        // Even short input vectors produce EMBEDDING_DIM-length output
        let embeddings = vec![vec![1.0, 2.0]];
        let mean = mean_embedding(&embeddings);
        assert_eq!(mean.len(), EMBEDDING_DIM);
        // Elements beyond input length should be 0.0
        assert!((mean[0] - 1.0).abs() < f64::EPSILON);
        assert!((mean[1] - 2.0).abs() < f64::EPSILON);
        assert!((mean[2] - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_cosine_full_dimension_vectors() {
        // Test with actual EMBEDDING_DIM-sized vectors
        let mut a = vec![0.0; EMBEDDING_DIM];
        let mut b = vec![0.0; EMBEDDING_DIM];
        a[0] = 1.0;
        a[100] = 0.5;
        b[0] = 1.0;
        b[100] = 0.5;
        let sim = cosine_similarity_embeddings(&a, &b);
        assert!(
            (sim - 1.0).abs() < 1e-10,
            "Identical sparse vectors should be 1.0"
        );
    }
}
