//! Drive-by reply detection.
//!
//! Fetches reply threads on the protected user's posts and filters
//! out followed accounts and self-replies. Remaining repliers are
//! "drive-by" candidates for scoring.

use std::collections::HashSet;

use anyhow::Result;
use tracing::debug;

use crate::bluesky::client::PublicAtpClient;

/// Filter reply DIDs to only non-followed accounts.
pub fn filter_drive_by_replies(reply_dids: &[String], follows: &HashSet<String>) -> Vec<String> {
    reply_dids
        .iter()
        .filter(|did| !follows.contains(did.as_str()))
        .cloned()
        .collect()
}

/// Filter reply DIDs, also excluding the protected user's own DID.
pub fn filter_drive_by_replies_excluding_self(
    reply_dids: &[String],
    follows: &HashSet<String>,
    protected_did: &str,
) -> Vec<String> {
    reply_dids
        .iter()
        .filter(|did| did.as_str() != protected_did && !follows.contains(did.as_str()))
        .cloned()
        .collect()
}

/// Extract (did, text, uri) tuples from a getPostThread JSON response.
///
/// Parses the `thread.replies[]` array. Skips entries missing a DID or text.
pub fn extract_reply_dids_from_thread(
    thread_json: &serde_json::Value,
) -> Vec<(String, String, String)> {
    let mut replies = Vec::new();

    if let Some(thread_replies) = thread_json["thread"]["replies"].as_array() {
        for reply in thread_replies {
            let did = reply["post"]["author"]["did"].as_str().unwrap_or_default();
            let text = reply["post"]["record"]["text"].as_str().unwrap_or_default();
            let uri = reply["post"]["uri"].as_str().unwrap_or_default();
            if !did.is_empty() && !text.is_empty() {
                replies.push((did.to_string(), text.to_string(), uri.to_string()));
            }
        }
    }

    replies
}

/// Fetch the protected user's follows list (paginated).
/// Returns a HashSet of followed DIDs for fast lookup.
pub async fn fetch_follows_set(
    client: &PublicAtpClient,
    protected_did: &str,
) -> Result<HashSet<String>> {
    let mut follows = HashSet::new();
    let mut cursor: Option<String> = None;

    loop {
        let mut params: Vec<(&str, &str)> = vec![("actor", protected_did), ("limit", "100")];
        if let Some(ref c) = cursor {
            params.push(("cursor", c.as_str()));
        }

        let resp: serde_json::Value = client
            .xrpc_get("app.bsky.graph.getFollows", &params)
            .await?;

        if let Some(follows_arr) = resp["follows"].as_array() {
            for follow in follows_arr {
                if let Some(did) = follow["did"].as_str() {
                    follows.insert(did.to_string());
                }
            }
        }

        cursor = resp["cursor"].as_str().map(String::from);
        if cursor.is_none() {
            break;
        }
    }

    debug!(follows_count = follows.len(), "Fetched follows set");

    Ok(follows)
}

/// Fetch direct replies to a post via getPostThread.
/// Returns Vec of (replier_did, reply_text, reply_uri).
pub async fn fetch_replies_to_post(
    client: &PublicAtpClient,
    post_uri: &str,
) -> Result<Vec<(String, String, String)>> {
    let resp: serde_json::Value = client
        .xrpc_get(
            "app.bsky.feed.getPostThread",
            &[("uri", post_uri), ("depth", "1")],
        )
        .await?;

    Ok(extract_reply_dids_from_thread(&resp))
}
