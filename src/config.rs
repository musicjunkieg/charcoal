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
    /// App password — only needed for future write operations (blocking/muting).
    /// The intelligence pipeline uses the public API and doesn't require auth.
    #[allow(dead_code)]
    pub bluesky_app_password: String,
    /// Public AT Protocol API endpoint (defaults to https://public.api.bsky.app).
    /// All read operations go through the public API — no auth needed.
    pub public_api_url: String,
    pub perspective_api_key: String,
    pub db_path: String,
    /// PostgreSQL connection URL (when set and starts with postgres://, uses Postgres backend)
    pub database_url: Option<String>,
    /// Which toxicity scorer to use (default: Onnx)
    pub scorer_backend: ScorerBackend,
    /// Directory containing the ONNX model files
    pub model_dir: PathBuf,
    /// Constellation backlink index URL (primary amplification detection)
    pub constellation_url: String,
    /// Password for the single-user web dashboard (CHARCOAL_WEB_PASSWORD env var)
    #[cfg(feature = "web")]
    pub web_password: String,
    /// Secret for HMAC session token signing (CHARCOAL_SESSION_SECRET env var)
    #[cfg(feature = "web")]
    pub session_secret: String,
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// Only db_path has a default — the Bluesky handle is required
    /// for anything beyond `init` and `status`.
    pub fn load() -> Result<Self> {
        let scorer_backend = match env::var("CHARCOAL_SCORER").as_deref() {
            Ok("perspective") => ScorerBackend::Perspective,
            // "onnx" or unset both default to ONNX
            _ => ScorerBackend::Onnx,
        };

        let model_dir = env::var("CHARCOAL_MODEL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| crate::toxicity::download::default_model_dir());

        #[cfg(feature = "web")]
        let web_password = env::var("CHARCOAL_WEB_PASSWORD").unwrap_or_default();
        #[cfg(feature = "web")]
        let session_secret = env::var("CHARCOAL_SESSION_SECRET").unwrap_or_default();

        Ok(Self {
            bluesky_handle: env::var("BLUESKY_HANDLE").unwrap_or_default(),
            bluesky_app_password: env::var("BLUESKY_APP_PASSWORD").unwrap_or_default(),
            public_api_url: env::var("PUBLIC_API_URL")
                .unwrap_or_else(|_| crate::bluesky::client::DEFAULT_PUBLIC_API_URL.to_string()),
            perspective_api_key: env::var("PERSPECTIVE_API_KEY").unwrap_or_default(),
            db_path: env::var("CHARCOAL_DB_PATH").unwrap_or_else(|_| "./charcoal.db".to_string()),
            database_url: env::var("DATABASE_URL").ok(),
            scorer_backend,
            model_dir,
            constellation_url: env::var("CONSTELLATION_URL")
                .unwrap_or_else(|_| "https://constellation.microcosm.blue".to_string()),
            #[cfg(feature = "web")]
            web_password,
            #[cfg(feature = "web")]
            session_secret,
        })
    }

    /// Check that the Bluesky handle is configured.
    /// Call this before any operation that needs to identify the protected user.
    pub fn require_bluesky(&self) -> Result<()> {
        if self.bluesky_handle.is_empty() {
            anyhow::bail!(
                "BLUESKY_HANDLE not set. Add it to your .env file.\n\
                 See .env.example for the required variables."
            );
        }
        Ok(())
    }

    /// Check that Bluesky auth credentials are configured.
    /// Call this before any future write operation (blocking/muting).
    #[allow(dead_code)]
    pub fn require_bluesky_auth(&self) -> Result<()> {
        self.require_bluesky()?;
        if self.bluesky_app_password.is_empty() {
            anyhow::bail!(
                "BLUESKY_APP_PASSWORD not set. This operation requires authentication.\n\
                 Add it to your .env file. See .env.example for details."
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
                if !crate::toxicity::download::model_files_present(&self.model_dir) {
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
