// Model download helper for ONNX models.
//
// Downloads two models from HuggingFace:
// 1. Detoxify unbiased-toxic-roberta — toxicity scoring (~126MB)
// 2. all-MiniLM-L6-v2 — sentence embeddings for topic overlap (~90MB)
//
// Files are stored in a platform-appropriate directory
// (~/.local/share/charcoal/models/ on Linux) so they persist across runs.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use tracing::info;

/// HuggingFace repo for the toxicity model.
const TOXICITY_HF_URL: &str =
    "https://huggingface.co/protectai/unbiased-toxic-roberta-onnx/resolve/main";

/// HuggingFace repo for the sentence embedding model.
const EMBEDDING_HF_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main";

/// Files for the toxicity model.
const TOXICITY_MODEL_FILE: &str = "model_quantized.onnx";
const TOXICITY_TOKENIZER_FILE: &str = "tokenizer.json";

/// Files for the sentence embedding model (stored in a subdirectory).
const EMBEDDING_MODEL_FILE: &str = "onnx/model.onnx";
const EMBEDDING_TOKENIZER_FILE: &str = "tokenizer.json";

/// Returns the default directory for storing model files.
/// Uses the platform data directory: ~/.local/share/charcoal/models/ on Linux.
pub fn default_model_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("charcoal")
        .join("models")
}

/// Subdirectory within model_dir for the sentence embedding model.
pub fn embedding_model_dir(base: &Path) -> PathBuf {
    base.join("all-MiniLM-L6-v2")
}

/// Check whether both required toxicity model files exist.
pub fn model_files_present(dir: &Path) -> bool {
    dir.join(TOXICITY_MODEL_FILE).exists() && dir.join(TOXICITY_TOKENIZER_FILE).exists()
}

/// Check whether both required embedding model files exist.
pub fn embedding_files_present(dir: &Path) -> bool {
    let embed_dir = embedding_model_dir(dir);
    embed_dir.join("model.onnx").exists() && embed_dir.join("tokenizer.json").exists()
}

/// Download all ONNX models (toxicity + embedding).
///
/// Shows progress bars for large files. Skips files that already exist.
/// Creates directories as needed.
pub async fn download_model(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create model directory: {}", dir.display()))?;

    // --- Toxicity model (Detoxify unbiased-toxic-roberta) ---
    println!("\nToxicity model (unbiased-toxic-roberta):");

    let tokenizer_path = dir.join(TOXICITY_TOKENIZER_FILE);
    if tokenizer_path.exists() {
        info!("Toxicity tokenizer already exists, skipping");
        println!("  {} (already exists)", TOXICITY_TOKENIZER_FILE);
    } else {
        println!("  Downloading {}...", TOXICITY_TOKENIZER_FILE);
        download_file(
            &format!("{}/{}", TOXICITY_HF_URL, TOXICITY_TOKENIZER_FILE),
            &tokenizer_path,
            false,
        )
        .await?;
    }

    let model_path = dir.join(TOXICITY_MODEL_FILE);
    if model_path.exists() {
        info!("Toxicity model already exists, skipping");
        println!("  {} (already exists)", TOXICITY_MODEL_FILE);
    } else {
        println!("  Downloading {} (~126 MB)...", TOXICITY_MODEL_FILE);
        download_file(
            &format!("{}/{}", TOXICITY_HF_URL, TOXICITY_MODEL_FILE),
            &model_path,
            true,
        )
        .await?;
    }

    // --- Sentence embedding model (all-MiniLM-L6-v2) ---
    println!("\nSentence embedding model (all-MiniLM-L6-v2):");

    let embed_dir = embedding_model_dir(dir);
    std::fs::create_dir_all(&embed_dir)
        .with_context(|| format!("Failed to create embedding model directory: {}", embed_dir.display()))?;

    let embed_tokenizer_path = embed_dir.join("tokenizer.json");
    if embed_tokenizer_path.exists() {
        info!("Embedding tokenizer already exists, skipping");
        println!("  tokenizer.json (already exists)");
    } else {
        println!("  Downloading tokenizer.json...");
        download_file(
            &format!("{}/{}", EMBEDDING_HF_URL, EMBEDDING_TOKENIZER_FILE),
            &embed_tokenizer_path,
            false,
        )
        .await?;
    }

    let embed_model_path = embed_dir.join("model.onnx");
    if embed_model_path.exists() {
        info!("Embedding model already exists, skipping");
        println!("  model.onnx (already exists)");
    } else {
        println!("  Downloading model.onnx (~90 MB)...");
        download_file(
            &format!("{}/{}", EMBEDDING_HF_URL, EMBEDDING_MODEL_FILE),
            &embed_model_path,
            true,
        )
        .await?;
    }

    Ok(())
}

/// Download a single file from a URL to a local path.
/// If `show_progress` is true, display a progress bar.
async fn download_file(url: &str, dest: &Path, show_progress: bool) -> Result<()> {
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("Failed to download {}", url))?;

    if !response.status().is_success() {
        anyhow::bail!("Download failed with status {}: {}", response.status(), url);
    }

    let total_size = response.content_length();

    // Set up progress bar if requested and we know the size
    let pb = if show_progress {
        let pb = if let Some(size) = total_size {
            let pb = ProgressBar::new(size);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("    [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                    .expect("valid template")
                    .progress_chars("=> "),
            );
            pb
        } else {
            let pb = ProgressBar::new_spinner();
            pb.set_style(
                ProgressStyle::default_spinner()
                    .template("    {spinner} {bytes}")
                    .expect("valid template"),
            );
            pb
        };
        Some(pb)
    } else {
        None
    };

    // Stream the response body to disk
    let bytes = response
        .bytes()
        .await
        .context("Failed to read response body")?;

    if let Some(ref pb) = pb {
        pb.set_position(bytes.len() as u64);
    }

    std::fs::write(dest, &bytes).with_context(|| format!("Failed to write {}", dest.display()))?;

    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    info!("Downloaded {} to {}", url, dest.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_model_dir_is_under_charcoal() {
        let dir = default_model_dir();
        let path_str = dir.to_string_lossy();
        assert!(
            path_str.contains("charcoal") && path_str.contains("models"),
            "Expected path containing charcoal/models, got: {path_str}"
        );
    }

    #[test]
    fn test_embedding_model_dir_is_subdirectory() {
        let base = PathBuf::from("/tmp/test-models");
        let embed_dir = embedding_model_dir(&base);
        assert_eq!(embed_dir, base.join("all-MiniLM-L6-v2"));
    }

    #[test]
    fn test_model_files_present_false_when_empty() {
        let dir = std::env::temp_dir().join("charcoal-test-nonexistent");
        assert!(!model_files_present(&dir));
    }

    #[test]
    fn test_embedding_files_present_false_when_empty() {
        let dir = std::env::temp_dir().join("charcoal-test-nonexistent");
        assert!(!embedding_files_present(&dir));
    }

    #[test]
    fn test_embedding_files_present_true_when_files_exist() {
        let dir = std::env::temp_dir().join("charcoal-embed-test");
        let embed_dir = embedding_model_dir(&dir);
        std::fs::create_dir_all(&embed_dir).unwrap();
        std::fs::write(embed_dir.join("model.onnx"), b"fake").unwrap();
        std::fs::write(embed_dir.join("tokenizer.json"), b"fake").unwrap();

        assert!(embedding_files_present(&dir));

        // Cleanup
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
