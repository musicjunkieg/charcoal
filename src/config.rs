use anyhow::Result;
use std::env;

/// Central configuration loaded from environment variables.
///
/// All secrets come from env vars (never hardcoded). The .env file
/// is loaded automatically at startup via dotenvy.
pub struct Config {
    pub bluesky_handle: String,
    pub bluesky_app_password: String,
    pub perspective_api_key: String,
    pub db_path: String,
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// Only db_path has a default — the API credentials are required
    /// for anything beyond `init` and `status`.
    pub fn load() -> Result<Self> {
        Ok(Self {
            bluesky_handle: env::var("BLUESKY_HANDLE")
                .unwrap_or_default(),
            bluesky_app_password: env::var("BLUESKY_APP_PASSWORD")
                .unwrap_or_default(),
            perspective_api_key: env::var("PERSPECTIVE_API_KEY")
                .unwrap_or_default(),
            db_path: env::var("CHARCOAL_DB_PATH")
                .unwrap_or_else(|_| "./charcoal.db".to_string()),
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
    /// Call this before any operation that needs toxicity scoring.
    pub fn require_perspective(&self) -> Result<()> {
        if self.perspective_api_key.is_empty() {
            anyhow::bail!(
                "PERSPECTIVE_API_KEY not set. Add it to your .env file.\n\
                 See .env.example for the required variables."
            );
        }
        Ok(())
    }
}

// Allow the status module (in the library crate) to read db_path
// without depending on this binary-only config module.
impl charcoal::status::HasDbPath for Config {
    fn db_path(&self) -> &str {
        &self.db_path
    }
}
