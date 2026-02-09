use std::env;
use std::path::PathBuf;

use anyhow::Result;

/// Which toxicity scoring backend to use.
#[derive(Debug, Clone, PartialEq)]
pub enum ScorerBackend {
    /// Local ONNX model (default) — no API key needed, no rate limits
    Onnx,
    /// Google Perspective API — requires PERSPECTIVE_API_KEY, 1 QPS limit
    Perspective,
}

/// Central configuration loaded from environment variables.
///
/// All secrets come from env vars (never hardcoded). The .env file
/// is loaded automatically at startup via dotenvy.
pub struct Config {
    pub bluesky_handle: String,
    pub bluesky_app_password: String,
    /// PDS endpoint URL (defaults to https://bsky.social).
    /// Set BLUESKY_PDS_URL for non-default PDS like blackskyapp.com.
    pub bluesky_pds_url: String,
    pub perspective_api_key: String,
    pub db_path: String,
    /// Which toxicity scorer to use (default: Onnx)
    pub scorer_backend: ScorerBackend,
    /// Directory containing the ONNX model files
    pub model_dir: PathBuf,
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// Only db_path has a default — the API credentials are required
    /// for anything beyond `init` and `status`.
    pub fn load() -> Result<Self> {
        let scorer_backend = match env::var("CHARCOAL_SCORER").as_deref() {
            Ok("perspective") => ScorerBackend::Perspective,
            // "onnx" or unset both default to ONNX
            _ => ScorerBackend::Onnx,
        };

        let model_dir = env::var("CHARCOAL_MODEL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| charcoal::toxicity::download::default_model_dir());

        Ok(Self {
            bluesky_handle: env::var("BLUESKY_HANDLE")
                .unwrap_or_default(),
            bluesky_app_password: env::var("BLUESKY_APP_PASSWORD")
                .unwrap_or_default(),
            bluesky_pds_url: env::var("BLUESKY_PDS_URL")
                .unwrap_or_else(|_| "https://bsky.social".to_string()),
            perspective_api_key: env::var("PERSPECTIVE_API_KEY")
                .unwrap_or_default(),
            db_path: env::var("CHARCOAL_DB_PATH")
                .unwrap_or_else(|_| "./charcoal.db".to_string()),
            scorer_backend,
            model_dir,
        })
    }

    /// Check that Bluesky credentials are configured.
    /// Call this before any operation that needs the Bluesky API.
    pub fn require_bluesky(&self) -> Result<()> {
        if self.bluesky_handle.is_empty() {
            anyhow::bail!(
                "BLUESKY_HANDLE not set. Add it to your .env file.\n\
                 See .env.example for the required variables."
            );
        }
        if self.bluesky_app_password.is_empty() {
            anyhow::bail!(
                "BLUESKY_APP_PASSWORD not set. Add it to your .env file.\n\
                 See .env.example for the required variables."
            );
        }
        Ok(())
    }

    /// Check that the Perspective API key is configured.
    /// Call this before any operation that needs toxicity scoring via Perspective.
    pub fn require_perspective(&self) -> Result<()> {
        if self.perspective_api_key.is_empty() {
            anyhow::bail!(
                "PERSPECTIVE_API_KEY not set. Add it to your .env file.\n\
                 See .env.example for the required variables."
            );
        }
        Ok(())
    }

    /// Validate that the chosen scorer backend has what it needs.
    /// For ONNX: model files must exist (or user should run download-model).
    /// For Perspective: API key must be set.
    pub fn require_scorer(&self) -> Result<()> {
        match self.scorer_backend {
            ScorerBackend::Onnx => {
                if !charcoal::toxicity::download::model_files_present(&self.model_dir) {
                    anyhow::bail!(
                        "ONNX model files not found in {}\n\
                         Run `charcoal download-model` to download them.\n\
                         Or set CHARCOAL_SCORER=perspective to use the Perspective API instead.",
                        self.model_dir.display()
                    );
                }
                Ok(())
            }
            ScorerBackend::Perspective => self.require_perspective(),
        }
    }
}

// Allow the status module (in the library crate) to read db_path
// without depending on this binary-only config module.
impl charcoal::status::HasDbPath for Config {
    fn db_path(&self) -> &str {
        &self.db_path
    }
}
