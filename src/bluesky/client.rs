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
/// The agent connects to bsky.social by default. After login, it manages
/// session tokens automatically (including refreshing them when they expire).
pub async fn login(handle: &str, app_password: &str) -> Result<BskyAgent> {
    let agent = BskyAgent::builder()
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
