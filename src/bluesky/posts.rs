// Post fetching — paginated author feed retrieval via public API.
//
// Fetches a user's recent posts from Bluesky. Used both for building the
// protected user's topic fingerprint (Step 0) and for analyzing target
// accounts' posting history (toxicity scoring).

use anyhow::{Context, Result};
use atrium_api::app::bsky::feed::{get_author_feed, get_posts};
use atrium_api::types::TryFromUnknown;
use tracing::{debug, info};

use super::client::PublicAtpClient;

/// A simplified post — just the fields Charcoal needs for analysis.
#[derive(Debug, Clone)]
pub struct Post {
    pub uri: String,
    pub text: String,
    pub created_at: Option<String>,
    pub like_count: i64,
    pub repost_count: i64,
    pub quote_count: i64,
}

/// Fetch recent posts for a given account, handling pagination automatically.
///
/// `max_posts` controls how many posts to collect (the API returns up to 100 per
/// page). Posts are returned newest-first. Reposts by others that appear in the
/// feed are filtered out — we only want original posts by the account.
pub async fn fetch_recent_posts(
    client: &PublicAtpClient,
    handle: &str,
    max_posts: usize,
) -> Result<Vec<Post>> {
    let mut posts = Vec::new();
    let mut cursor: Option<String> = None;

    // How many to request per page (API max is 100).
    let page_size = max_posts.min(100).to_string();

    loop {
        let mut params: Vec<(&str, &str)> = vec![
            ("actor", handle),
            ("filter", "posts_no_replies"),
            ("limit", &page_size),
        ];
        if let Some(ref c) = cursor {
            params.push(("cursor", c));
        }

        let output: get_author_feed::Output = client
            .xrpc_get("app.bsky.feed.getAuthorFeed", &params)
            .await
            .with_context(|| format!("Failed to fetch feed for @{}", handle))?;

        for feed_item in &output.feed {
            // Skip reposts — we only want posts authored by this account.
            // Reposts show up with a `reason` of ReasonRepost.
            if feed_item.reason.is_some() {
                continue;
            }

            let post_view = &feed_item.post;

            // Decode the record to get the post text.
            // The record field is an untyped IPLD value — we deserialize it
            // into the typed post::Record to access the text.
            let text = atrium_api::app::bsky::feed::post::Record::try_from_unknown(
                post_view.record.clone(),
            )
            .map(|record| record.data.text.clone())
            .unwrap_or_default();

            // Skip empty posts and very short posts (likely just links/images).
            // Use char count, not byte length — a 5-char emoji sequence can be 20 bytes.
            if text.chars().count() < 15 {
                continue;
            }

            posts.push(Post {
                uri: post_view.uri.clone(),
                text,
                created_at: Some(post_view.indexed_at.as_ref().to_string()),
                like_count: post_view.like_count.unwrap_or(0),
                repost_count: post_view.repost_count.unwrap_or(0),
                quote_count: post_view.quote_count.unwrap_or(0),
            });

            if posts.len() >= max_posts {
                break;
            }
        }

        debug!(
            page_posts = output.feed.len(),
            total_collected = posts.len(),
            "Fetched page of posts for @{}",
            handle
        );

        // Stop if we have enough posts or there are no more pages
        if posts.len() >= max_posts {
            break;
        }

        cursor = output.data.cursor.clone();
        if cursor.is_none() || output.feed.is_empty() {
            break;
        }
    }

    info!(
        count = posts.len(),
        handle = handle,
        "Collected posts for analysis"
    );

    Ok(posts)
}

/// Fetch a single post's text by its AT URI.
///
/// Used to retrieve quote-post text for amplification events. The Constellation
/// backlink gives us the URI but not the post content — this fills that gap.
pub async fn fetch_post_text(client: &PublicAtpClient, uri: &str) -> Result<Option<String>> {
    let output: get_posts::Output = client
        .xrpc_get("app.bsky.feed.getPosts", &[("uris", uri)])
        .await
        .context("Failed to fetch post by URI")?;

    let text = output.posts.first().and_then(|post_view| {
        atrium_api::app::bsky::feed::post::Record::try_from_unknown(post_view.record.clone())
            .ok()
            .map(|record| record.data.text.clone())
    });

    Ok(text)
}
