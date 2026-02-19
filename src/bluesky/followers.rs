// Follower list fetching with pagination via public API.
//
// Used to get the follower list of accounts that amplify the protected user's
// content. Those followers are the exposure surface — people who can now see
// the protected user's content, framed by whatever the amplifier said.

use anyhow::{Context, Result};
use atrium_api::app::bsky::graph::get_followers;
use tracing::{debug, info};

use super::client::PublicAtpClient;

/// A simplified follower profile — just the fields Charcoal needs.
#[derive(Debug, Clone)]
pub struct Follower {
    pub did: String,
    pub handle: String,
    pub display_name: Option<String>,
}

/// Fetch all followers for a given account, handling pagination automatically.
///
/// Warning: accounts with large follower counts (10k+) will require many API
/// calls. The `max_followers` parameter caps how many we collect to stay within
/// reasonable rate limits.
pub async fn fetch_followers(
    client: &PublicAtpClient,
    handle: &str,
    max_followers: usize,
) -> Result<Vec<Follower>> {
    let mut followers = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
        let mut params: Vec<(&str, &str)> = vec![("actor", handle), ("limit", "100")];
        if let Some(ref c) = cursor {
            params.push(("cursor", c));
        }

        let output: get_followers::Output = client
            .xrpc_get("app.bsky.graph.getFollowers", &params)
            .await
            .with_context(|| format!("Failed to fetch followers for @{}", handle))?;

        for profile in &output.followers {
            followers.push(Follower {
                did: profile.did.as_str().to_string(),
                handle: profile.handle.as_str().to_string(),
                display_name: profile.display_name.clone(),
            });

            if followers.len() >= max_followers {
                break;
            }
        }

        debug!(
            page_size = output.followers.len(),
            total = followers.len(),
            "Fetched page of followers for @{}",
            handle
        );

        if followers.len() >= max_followers {
            break;
        }

        cursor = output.data.cursor.clone();
        if cursor.is_none() || output.followers.is_empty() {
            break;
        }
    }

    info!(
        count = followers.len(),
        handle = handle,
        "Collected followers"
    );

    Ok(followers)
}
