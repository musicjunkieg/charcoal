// Post fetching — paginated author feed retrieval via public API.
//
// Fetches a user's recent posts from Bluesky. Used both for building the
// protected user's topic fingerprint (Step 0) and for analyzing target
// accounts' posting history (toxicity scoring).

use anyhow::{Context, Result};
use atrium_api::app::bsky::feed::{get_author_feed, get_posts};
use atrium_api::types::TryFromUnknown;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::client::PublicAtpClient;

/// A simplified post — just the fields Charcoal needs for analysis.
/// Strip NUL bytes from ingested post text (#224).
///
/// A post containing a NUL killed one account's gather on the 2026-07-19
/// staging scan with `unsupported Unicode escape sequence`. serde_json
/// serialises NUL as `\u0000`, which PostgreSQL rejects in JSONB — and
/// `top_toxic_posts` and `payload_json` are both JSONB. Postgres TEXT columns
/// cannot hold a NUL either, so `classification_queue.text` fails on the same
/// input.
///
/// Applied HERE, at ingestion, rather than at the database write boundary:
/// sanitising per-backend would let SQLite and Postgres hold different text for
/// the same post, so two backends would score identically-fetched content
/// differently. Strip it once, where the text enters the system.
///
/// Deliberately narrow — ONLY NUL. Other C0 controls are legal in both JSON and
/// Postgres text, and newlines in particular are load-bearing (the reply
/// envelope is built with "\n\n"), so removing more would silently change the
/// text the toxicity model scores.
pub fn sanitize_post_text(text: &str) -> String {
    if text.contains('\0') {
        text.replace('\0', "")
    } else {
        // Overwhelmingly the common case — avoid reallocating for every post.
        text.to_string()
    }
}

/// Extract primary language tags from a decoded post record's `langs`.
/// Region/script subtags are stripped and tags lowercased ("en-US" → "en").
fn extract_langs(record: &atrium_api::app::bsky::feed::post::Record) -> Vec<String> {
    record
        .data
        .langs
        .as_ref()
        .map(|langs| {
            langs
                .iter()
                .filter_map(|l| {
                    l.as_ref()
                        .language()
                        .map(|p| p.as_str().to_ascii_lowercase())
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Post {
    pub uri: String,
    pub text: String,
    pub created_at: Option<String>,
    pub like_count: i64,
    pub repost_count: i64,
    pub quote_count: i64,
    /// Whether this post is a quote-post (embeds another post).
    pub is_quote: bool,
    /// Declared post languages (`app.bsky.feed.post.langs`), primary tags only,
    /// region/script subtags stripped and lowercased ("en-US" → "en"). Empty
    /// when the client omitted the field (~6% of posts). Used by the #222
    /// language-assessability gate.
    pub langs: Vec<String>,
}

/// A reply post with its parent URI for context pair formation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplyPost {
    pub post: Post,
    /// AT URI of the post being replied to (for fetching parent text)
    pub parent_uri: String,
}

/// Partitioned post sample from an account's feed.
///
/// Separates posts into originals, replies, and quotes so different
/// consumers can use the appropriate subset:
/// - Topic fingerprinting: originals (chosen topics, not inherited from arguments)
/// - Toxicity scoring: all posts, with replies weighted 70%
/// - Context pairs: replies with parent URIs for NLI/Zentropi pair scoring
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostSample {
    /// Original posts (not replies, not quotes)
    pub originals: Vec<Post>,
    /// Reply posts with parent URI for context pair fetching
    pub replies: Vec<ReplyPost>,
    /// Quote posts (embed another post)
    pub quotes: Vec<Post>,
    /// Computed reply ratio (replies / total non-repost posts)
    pub reply_ratio: f64,
    /// Computed quote ratio (quotes / total non-repost posts)
    pub quote_ratio: f64,
    /// Total non-repost posts seen (denominator for ratios)
    pub total_posts: usize,
}

/// Quality of a topic fingerprint based on data availability.
///
/// When an account is reply-heavy, fingerprinting from originals alone
/// may produce unreliable results. This flag lets downstream scoring
/// account for fingerprint confidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FingerprintQuality {
    /// >= 15 originals — fingerprint from originals only
    Normal,
    /// < 15 originals but >= 15 total — fingerprint from all posts
    Degraded,
    /// < 15 total posts OR 0 originals — fingerprint is unreliable
    Unreliable,
}

impl FingerprintQuality {
    /// Determine fingerprint quality from post counts.
    ///
    /// Special case: 0 originals is always Unreliable even if total >= 15,
    /// because fingerprinting entirely from replies captures the topics of
    /// people they're arguing with, not their own interests.
    pub fn from_counts(originals: usize, replies_and_quotes: usize) -> Self {
        if originals >= 15 {
            FingerprintQuality::Normal
        } else if originals == 0 {
            FingerprintQuality::Unreliable
        } else if originals + replies_and_quotes >= 15 {
            FingerprintQuality::Degraded
        } else {
            FingerprintQuality::Unreliable
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            FingerprintQuality::Normal => "normal",
            FingerprintQuality::Degraded => "degraded",
            FingerprintQuality::Unreliable => "unreliable",
        }
    }
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

            // Decode the record once to get both text and declared languages.
            let record = atrium_api::app::bsky::feed::post::Record::try_from_unknown(
                post_view.record.clone(),
            )
            .ok();
            let text = record
                .as_ref()
                .map(|r| sanitize_post_text(&r.data.text))
                .unwrap_or_default();
            let langs = record.as_ref().map(extract_langs).unwrap_or_default();

            // Skip empty posts and very short posts (likely just links/images).
            // Use char count, not byte length — a 5-char emoji sequence can be 20 bytes.
            if text.chars().count() < 15 {
                continue;
            }

            // Detect quote-posts by checking the embed type.
            // Quote-posts embed another post via AppBskyEmbedRecordView or
            // AppBskyEmbedRecordWithMediaView (quote + image/video).
            let is_quote = post_view.embed.as_ref().is_some_and(|embed| {
                use atrium_api::types::Union;
                matches!(
                    embed,
                    Union::Refs(
                        atrium_api::app::bsky::feed::defs::PostViewEmbedRefs::AppBskyEmbedRecordView(_)
                            | atrium_api::app::bsky::feed::defs::PostViewEmbedRefs::AppBskyEmbedRecordWithMediaView(_)
                    )
                )
            });

            posts.push(Post {
                uri: post_view.uri.clone(),
                text,
                created_at: Some(post_view.indexed_at.as_ref().to_string()),
                like_count: post_view.like_count.unwrap_or(0),
                repost_count: post_view.repost_count.unwrap_or(0),
                quote_count: post_view.quote_count.unwrap_or(0),
                is_quote,
                langs,
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

/// Fetch recent posts with replies included, partitioned into a PostSample.
///
/// Uses the `posts_with_replies` filter to get both original posts and replies
/// from the same API call. Partitions into originals, replies (with parent URIs),
/// and quotes. Computes reply and quote ratios from the same data.
///
/// This replaces the pattern of calling `fetch_recent_posts` + `fetch_reply_ratio`
/// separately — one API call yields both toxicity-scoreable text AND behavioral ratios.
pub async fn fetch_posts_with_replies(
    client: &PublicAtpClient,
    handle: &str,
    max_posts: usize,
) -> Result<PostSample> {
    let mut originals = Vec::new();
    let mut replies = Vec::new();
    let mut quotes = Vec::new();
    let mut cursor: Option<String> = None;
    let mut total_collected: usize = 0;

    // How many to request per page (API max is 100).
    let page_size = max_posts.min(100).to_string();

    loop {
        let mut params: Vec<(&str, &str)> = vec![
            ("actor", handle),
            ("filter", "posts_with_replies"),
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
            if feed_item.reason.is_some() {
                continue;
            }

            let post_view = &feed_item.post;

            // Decode the record to get the post text and reply reference.
            let record = match atrium_api::app::bsky::feed::post::Record::try_from_unknown(
                post_view.record.clone(),
            ) {
                Ok(r) => r,
                Err(_) => continue,
            };

            let text = sanitize_post_text(&record.data.text);
            let langs = extract_langs(&record);

            // Skip empty posts and very short posts (likely just links/images).
            if text.chars().count() < 15 {
                continue;
            }

            // Detect quote-posts by checking the embed type.
            let is_quote = post_view.embed.as_ref().is_some_and(|embed| {
                use atrium_api::types::Union;
                matches!(
                    embed,
                    Union::Refs(
                        atrium_api::app::bsky::feed::defs::PostViewEmbedRefs::AppBskyEmbedRecordView(_)
                            | atrium_api::app::bsky::feed::defs::PostViewEmbedRefs::AppBskyEmbedRecordWithMediaView(_)
                    )
                )
            });

            let post = Post {
                uri: post_view.uri.clone(),
                text,
                created_at: Some(post_view.indexed_at.as_ref().to_string()),
                like_count: post_view.like_count.unwrap_or(0),
                repost_count: post_view.repost_count.unwrap_or(0),
                quote_count: post_view.quote_count.unwrap_or(0),
                is_quote,
                langs,
            };

            total_collected += 1;

            // Classify: reply takes priority over quote (reply context is more
            // important for NLI pair scoring than the quote relationship).
            if feed_item.reply.is_some() {
                let parent_uri = record
                    .data
                    .reply
                    .as_ref()
                    .map(|r| r.parent.uri.clone())
                    .unwrap_or_default();

                if parent_uri.is_empty() {
                    // Edge case: feed says it's a reply but no parent URI in record.
                    // Treat as original.
                    originals.push(post);
                } else {
                    replies.push(ReplyPost { post, parent_uri });
                }
            } else if is_quote {
                quotes.push(post);
            } else {
                originals.push(post);
            }

            if total_collected >= max_posts {
                break;
            }
        }

        debug!(
            page_posts = output.feed.len(),
            total_collected = total_collected,
            "Fetched page of posts (with replies) for @{}",
            handle
        );

        if total_collected >= max_posts {
            break;
        }

        cursor = output.data.cursor.clone();
        if cursor.is_none() || output.feed.is_empty() {
            break;
        }
    }

    let reply_ratio = if total_collected > 0 {
        replies.len() as f64 / total_collected as f64
    } else {
        0.0
    };
    let quote_ratio = if total_collected > 0 {
        quotes.len() as f64 / total_collected as f64
    } else {
        0.0
    };

    info!(
        originals = originals.len(),
        replies = replies.len(),
        quotes = quotes.len(),
        reply_ratio = format!("{:.2}", reply_ratio),
        quote_ratio = format!("{:.2}", quote_ratio),
        handle = handle,
        "Partitioned post sample"
    );

    Ok(PostSample {
        originals,
        replies,
        quotes,
        reply_ratio,
        quote_ratio,
        total_posts: total_collected,
    })
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
            .map(|record| sanitize_post_text(&record.data.text))
    });

    Ok(text)
}

/// Batch-fetch parent post texts for reply context pair formation.
///
/// Given a set of AT URIs, fetches the post texts via `getPosts` (up to 25
/// per call, as per API limit). Returns a map from URI to text.
///
/// Used to form (parent_text, reply_text) pairs for Zentropi or NLI scoring.
pub async fn fetch_parent_posts(
    client: &PublicAtpClient,
    parent_uris: &[String],
) -> Result<std::collections::HashMap<String, String>> {
    use std::collections::HashMap;

    let mut result = HashMap::new();
    if parent_uris.is_empty() {
        return Ok(result);
    }

    // Deduplicate URIs — multiple replies may share the same parent thread.
    let unique_uris: Vec<&str> = {
        let mut seen = std::collections::HashSet::new();
        parent_uris
            .iter()
            .filter(|u| seen.insert(u.as_str()))
            .map(|u| u.as_str())
            .collect()
    };

    // getPosts accepts up to 25 URIs per call.
    for chunk in unique_uris.chunks(25) {
        let params: Vec<(&str, &str)> = chunk.iter().map(|uri| ("uris", *uri)).collect();

        match client
            .xrpc_get::<get_posts::Output>("app.bsky.feed.getPosts", &params)
            .await
        {
            Ok(output) => {
                for post_view in &output.posts {
                    if let Ok(record) = atrium_api::app::bsky::feed::post::Record::try_from_unknown(
                        post_view.record.clone(),
                    ) {
                        result.insert(post_view.uri.clone(), sanitize_post_text(&record.data.text));
                    }
                }
            }
            Err(e) => {
                warn!(
                    chunk_size = chunk.len(),
                    error = %e,
                    "Failed to fetch parent posts batch, skipping"
                );
            }
        }
    }

    debug!(
        requested = parent_uris.len(),
        fetched = result.len(),
        "Fetched parent posts for context pairs"
    );

    Ok(result)
}

/// Fetch the reply ratio for an account by sampling one page of posts.
///
/// Makes a single API call with `posts_and_author_threads` filter (which
/// includes replies), then counts how many have a `reply` field set.
/// Returns (reply_count, total_count) so the caller can compute the ratio.
pub async fn fetch_reply_ratio(client: &PublicAtpClient, handle: &str) -> Result<(usize, usize)> {
    let params: Vec<(&str, &str)> = vec![
        ("actor", handle),
        ("filter", "posts_and_author_threads"),
        ("limit", "50"),
    ];

    let output: get_author_feed::Output = client
        .xrpc_get("app.bsky.feed.getAuthorFeed", &params)
        .await
        .with_context(|| format!("Failed to fetch reply ratio for @{}", handle))?;

    let mut reply_count = 0;
    let mut total = 0;

    for feed_item in &output.feed {
        // Skip reposts — they aren't posts authored by this account.
        if feed_item.reason.is_some() {
            continue;
        }
        total += 1;
        if feed_item.reply.is_some() {
            reply_count += 1;
        }
    }

    debug!(
        handle = handle,
        replies = reply_count,
        total = total,
        "Reply ratio sample"
    );

    Ok((reply_count, total))
}
