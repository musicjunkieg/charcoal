//! OAuth write sessions (#315): store on consent, load-with-refresh before
//! every PDS call, disconnect. See spec §3.4–§3.7.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::warn;

use super::crypto::TokenCrypto;
use crate::config::Config;

// Task 7 reads both fields (store/load/refresh/disconnect); this task only
// builds the shell, so `-D warnings` needs an explicit allow until then.
#[allow(dead_code)]
pub struct SessionStore {
    pub(crate) crypto: TokenCrypto,
    /// Per-DID refresh serialization. AT Protocol refresh tokens are
    /// single-use; two concurrent refreshes for one user must never both run.
    pub(crate) locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl SessionStore {
    /// `None` (feature disabled) when the key is missing or malformed. Logs a
    /// warning either way — an operator must be able to see why the buttons
    /// are gone. Never panics.
    pub fn from_config(config: &Config) -> Option<Self> {
        let Some(hex) = config.token_key.as_deref() else {
            warn!("CHARCOAL_TOKEN_KEY not set — mute/block actions are disabled");
            return None;
        };
        match TokenCrypto::from_hex(hex) {
            Ok(crypto) => Some(Self {
                crypto,
                locks: Mutex::new(HashMap::new()),
            }),
            Err(e) => {
                warn!("CHARCOAL_TOKEN_KEY rejected ({e}) — mute/block actions are disabled");
                None
            }
        }
    }
}
