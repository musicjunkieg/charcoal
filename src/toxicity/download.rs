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

/// HuggingFace repo for the NLI cross-encoder model (contextual hostility scoring).
const NLI_HF_URL: &str = "https://huggingface.co/Xenova/nli-deberta-v3-xsmall/resolve/main";

/// Files for the toxicity model.
const TOXICITY_MODEL_FILE: &str = "model_quantized.onnx";
const TOXICITY_TOKENIZER_FILE: &str = "tokenizer.json";

/// Files for the sentence embedding model (stored in a subdirectory).
const EMBEDDING_MODEL_FILE: &str = "onnx/model.onnx";
const EMBEDDING_TOKENIZER_FILE: &str = "tokenizer.json";

/// Files for the NLI cross-encoder model (stored in a subdirectory).
///
/// This is the fp32 export, NOT `model_quantized.onnx` (#231). The quantized
/// export is *dynamically* quantized: `DynamicQuantizeLinear` derives one
/// per-tensor activation scale at runtime from the min/max of the whole
/// `[batch, seq, hidden]` tensor. Batching hypotheses with different content
/// therefore changes the scale and shifts every row's result — including rows
/// that were never padded — and on x86-64 without VNNI the `u8s8 MatMulInteger`
/// path amplifies that ~20x over ARM. Measured on Linux x86_64, it drove one
/// mocking reply's contextual hostility from 0.132 to 0.000.
///
/// The fp32 export has no quantization ops, so batching is exact on both
/// platforms and the scores are finally reproducible between a dev Mac and
/// production. It costs 284MB instead of 87MB, and is still faster per pair
/// than the alternative fix of abandoning batching.
const NLI_MODEL_FILE: &str = "model.onnx";
const NLI_MODEL_REMOTE_PATH: &str = "onnx/model.onnx";
const NLI_TOKENIZER_FILE: &str = "tokenizer.json";

/// Returns the default directory for storing model files.
/// Uses the platform data directory: ~/.local/share/charcoal/models/ on Linux.
pub fn default_model_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("charcoal")
        .join("models")
}

/// Resolve the model directory from an explicit override, falling back to the
/// platform default. A blank override is treated as unset — an exported-but-
/// empty `CHARCOAL_MODEL_DIR` must not send every lookup to the filesystem root.
///
/// Takes the override as a parameter rather than reading the environment so it
/// stays pure and testable: `std::env::set_var` races Rust's parallel test
/// threads.
pub fn resolve_model_dir_from(override_dir: Option<String>) -> PathBuf {
    override_dir
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_model_dir)
}

/// [`resolve_model_dir_from`] reading `CHARCOAL_MODEL_DIR`.
pub fn resolve_model_dir() -> PathBuf {
    resolve_model_dir_from(std::env::var("CHARCOAL_MODEL_DIR").ok())
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

/// Subdirectory within model_dir for the NLI cross-encoder model.
pub fn nli_model_dir(base: &Path) -> PathBuf {
    base.join("nli-deberta-v3-xsmall")
}

/// Check whether both required NLI model files exist.
///
/// Deployments that predate #231 have the old `model_quantized.onnx` on their
/// volume; this reports absent for them, so `download_model` fetches the fp32
/// export and the switch happens without manual intervention.
pub fn nli_files_present(dir: &Path) -> bool {
    let nli_dir = nli_model_dir(dir);
    nli_dir.join(NLI_MODEL_FILE).exists() && nli_dir.join(NLI_TOKENIZER_FILE).exists()
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
    std::fs::create_dir_all(&embed_dir).with_context(|| {
        format!(
            "Failed to create embedding model directory: {}",
            embed_dir.display()
        )
    })?;

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

    // --- NLI cross-encoder model (DeBERTa-v3-xsmall for contextual scoring) ---
    println!("\nNLI cross-encoder model (nli-deberta-v3-xsmall):");

    let nli_dir = nli_model_dir(dir);
    std::fs::create_dir_all(&nli_dir).with_context(|| {
        format!(
            "Failed to create NLI model directory: {}",
            nli_dir.display()
        )
    })?;

    let nli_tokenizer_path = nli_dir.join(NLI_TOKENIZER_FILE);
    if nli_tokenizer_path.exists() {
        info!("NLI tokenizer already exists, skipping");
        println!("  {} (already exists)", NLI_TOKENIZER_FILE);
    } else {
        println!("  Downloading {}...", NLI_TOKENIZER_FILE);
        download_file(
            &format!("{}/{}", NLI_HF_URL, NLI_TOKENIZER_FILE),
            &nli_tokenizer_path,
            false,
        )
        .await?;
    }

    let nli_model_path = nli_dir.join(NLI_MODEL_FILE);
    if nli_model_path.exists() {
        info!("NLI model already exists, skipping");
        println!("  {} (already exists)", NLI_MODEL_FILE);
    } else {
        println!("  Downloading {} (~284 MB)...", NLI_MODEL_FILE);
        download_file(
            &format!("{}/{}", NLI_HF_URL, NLI_MODEL_REMOTE_PATH),
            &nli_model_path,
            true,
        )
        .await?;
    }

    // Reclaim the pre-#231 quantized export if it is still sitting on the
    // volume; nothing loads it any more and it is 87MB.
    let stale_quantized = nli_dir.join("model_quantized.onnx");
    if stale_quantized.exists() {
        match std::fs::remove_file(&stale_quantized) {
            Ok(()) => println!("  removed superseded model_quantized.onnx (#231)"),
            // Best-effort cleanup: a read-only volume must not fail the download.
            Err(e) => info!("Could not remove stale quantized NLI model: {}", e),
        }
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
