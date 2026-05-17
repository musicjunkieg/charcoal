//! Like detection via the public AT Protocol API.
//!
//! Fallback for when Constellation does not index likes. Uses
//! `app.bsky.feed.getLikes` to fetch accounts that liked a post.

use anyhow::Result;
use serde::Deserialize;
use tracing::debug;

use crate::bluesky::client::PublicAtpClient;

/// A single like entry from the getLikes response.
#[derive(Debug, Clone, Deserialize)]
pub struct LikeEntry {
    #[serde(rename = "indexedAt")]
    pub indexed_at: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    pub actor: LikeActor,
}

/// The actor who liked a post.
#[derive(Debug, Clone, Deserialize)]
pub struct LikeActor {
    pub did: String,
    pub handle: String,
}

/// Response from `app.bsky.feed.getLikes`.
#[derive(Debug, Clone, Deserialize)]
pub struct LikesResponse {
    pub likes: Vec<LikeEntry>,
    pub cursor: Option<String>,
}

/// Fetch DIDs of accounts that liked a post via the public API.
///
/// Paginates through all results up to `max_likers`. Returns a Vec of
/// liker DIDs.
pub async fn fetch_likers_via_api(
    client: &PublicAtpClient,
    post_uri: &str,
    max_likers: usize,
) -> Result<Vec<String>> {
    let mut likers = Vec::new();
    let mut cursor: Option<String> = None;
    let limit = 100.min(max_likers);

    loop {
        let limit_str = limit.to_string();
        let mut params: Vec<(&str, &str)> = vec![("uri", post_uri), ("limit", &limit_str)];
        if let Some(ref c) = cursor {
            params.push(("cursor", c.as_str()));
        }

        let resp: LikesResponse = client.xrpc_get("app.bsky.feed.getLikes", &params).await?;

        for entry in &resp.likes {
            if likers.len() >= max_likers {
                break;
            }
            likers.push(entry.actor.did.clone());
        }

        if likers.len() >= max_likers || resp.cursor.is_none() || resp.likes.is_empty() {
            break;
        }
        cursor = resp.cursor;
    }

    debug!(
        liker_count = likers.len(),
        post_uri = post_uri,
        "Fetched likers via API"
    );

    Ok(likers)
}
