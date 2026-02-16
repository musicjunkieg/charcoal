// Model download helper for the ONNX toxicity scorer.
//
// Downloads the Detoxify unbiased-toxic-roberta model files from HuggingFace.
// The quantized ONNX model is ~126MB and the tokenizer is ~1MB. Files are
// stored in a platform-appropriate directory (~/.local/share/charcoal/models/
// on Linux) so they persist across runs.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use tracing::info;

/// HuggingFace repo for the pre-exported ONNX model.
const HF_BASE_URL: &str =
    "https://huggingface.co/protectai/unbiased-toxic-roberta-onnx/resolve/main";

/// Files we need from the repo.
const MODEL_FILE: &str = "model_quantized.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";

/// Returns the default directory for storing model files.
/// Uses the platform data directory: ~/.local/share/charcoal/models/ on Linux.
pub fn default_model_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("charcoal")
        .join("models")
}

/// Check whether both required model files exist in the given directory.
pub fn model_files_present(dir: &Path) -> bool {
    dir.join(MODEL_FILE).exists() && dir.join(TOKENIZER_FILE).exists()
}

/// Download the ONNX model and tokenizer files to the given directory.
///
/// Shows a progress bar for the large model file. Skips files that already
/// exist. Creates the directory if it doesn't exist.
pub async fn download_model(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create model directory: {}", dir.display()))?;

    // Download tokenizer first (small, fast)
    let tokenizer_path = dir.join(TOKENIZER_FILE);
    if tokenizer_path.exists() {
        info!("Tokenizer already exists, skipping");
        println!("  {} (already exists)", TOKENIZER_FILE);
    } else {
        println!("  Downloading {}...", TOKENIZER_FILE);
        download_file(
            &format!("{}/{}", HF_BASE_URL, TOKENIZER_FILE),
            &tokenizer_path,
            false,
        )
        .await?;
    }

    // Download quantized model (large, ~126MB — show progress bar)
    let model_path = dir.join(MODEL_FILE);
    if model_path.exists() {
        info!("Model already exists, skipping");
        println!("  {} (already exists)", MODEL_FILE);
    } else {
        println!("  Downloading {} (~126 MB)...", MODEL_FILE);
        download_file(
            &format!("{}/{}", HF_BASE_URL, MODEL_FILE),
            &model_path,
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
