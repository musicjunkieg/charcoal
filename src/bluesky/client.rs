// Bluesky client — session management and authentication.
//
// Wraps bsky-sdk's BskyAgent to provide a simpler interface for Charcoal.
// The agent handles session tokens, token refresh, and API endpoint routing
// automatically — we just need to log in and make calls.

use anyhow::{Context, Result};
use bsky_sdk::BskyAgent;
use tracing::info;

/// Create a new Bluesky agent and authenticate with the given credentials.
///
/// The `pds_url` parameter sets the PDS endpoint (e.g. "https://bsky.social"
/// or "https://blacksky.app" for non-default PDS instances).
pub async fn login(handle: &str, app_password: &str, pds_url: &str) -> Result<BskyAgent> {
    let config = bsky_sdk::agent::config::Config {
        endpoint: pds_url.to_string(),
        ..Default::default()
    };

    let agent = BskyAgent::builder()
        .config(config)
        .build()
        .await
        .context("Failed to initialize Bluesky agent")?;

    let session = agent
        .login(handle, app_password)
        .await
        .context("Failed to authenticate with Bluesky. Check your handle and app password.")?;

    info!(
        did = session.data.did.as_str(),
        handle = session.data.handle.as_str(),
        "Authenticated with Bluesky"
    );

    Ok(agent)
}
