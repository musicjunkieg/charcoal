// Post fetching — paginated author feed retrieval.
//
// Fetches a user's recent posts from Bluesky. Used both for building the
// protected user's topic fingerprint (Step 0) and for analyzing target
// accounts' posting history (toxicity scoring).

use anyhow::{Context, Result};
use atrium_api::app::bsky::feed::{get_author_feed, get_posts};
use atrium_api::types::TryFromUnknown;
use bsky_sdk::BskyAgent;
use tracing::{debug, info};

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
    agent: &BskyAgent,
    handle: &str,
    max_posts: usize,
) -> Result<Vec<Post>> {
    let mut posts = Vec::new();
    let mut cursor: Option<String> = None;

    // How many to request per page (API max is 100)
    let page_size: u8 = 100.min(max_posts as u8);

    loop {
        let params = get_author_feed::ParametersData {
            actor: handle
                .parse()
                .map_err(|e: &str| anyhow::anyhow!("{}", e))
                .context("Invalid Bluesky handle")?,
            cursor: cursor.clone(),
            // "posts_no_replies" filters out reply posts — we want original posts
            // and quote posts for topic/toxicity analysis
            filter: Some("posts_no_replies".to_string()),
            include_pins: None,
            limit: Some(
                page_size
                    .try_into()
                    .map_err(|e: String| anyhow::anyhow!("{}", e))?,
            ),
        };

        let output = agent
            .api
            .app
            .bsky
            .feed
            .get_author_feed(params.into())
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

            // Skip empty posts and very short posts (likely just links/images)
            if text.len() < 15 {
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
/// Used to retrieve quote-post text for amplification events. The notification
/// gives us the URI but not the post content — this fills that gap.
pub async fn fetch_post_text(agent: &BskyAgent, uri: &str) -> Result<Option<String>> {
    let params = get_posts::ParametersData {
        uris: vec![uri.to_string()],
    };

    let output = agent
        .api
        .app
        .bsky
        .feed
        .get_posts(params.into())
        .await
        .context("Failed to fetch post by URI")?;

    let text = output
        .posts
        .first()
        .and_then(|post_view| {
            atrium_api::app::bsky::feed::post::Record::try_from_unknown(
                post_view.record.clone(),
            )
            .ok()
            .map(|record| record.data.text.clone())
        });

    Ok(text)
}
