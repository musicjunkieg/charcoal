// HTTP client for the Constellation XRPC backlink API.
//
// Queries `blue.microcosm.links.getBacklinks` to find all quotes and reposts
// of given post URIs. Results are converted into the same AmplificationNotification
// format used by the notification pipeline, so they can be merged seamlessly.

use anyhow::{Context, Result};
use futures::StreamExt;
use serde::Deserialize;
use tracing::{debug, warn};

use crate::bluesky::amplification::AmplificationNotification;

/// Constellation source path for like backlinks.
pub const LIKES_SOURCE: &str = "app.bsky.feed.like:subject.uri";

/// Concurrency for the discovery backlink fetches (#213). Kept low on purpose:
/// Constellation publishes no rate limit and `PublicAtpClient` has no backoff
/// (#182). Do not raise past ~8 until #182 lands.
const DISCOVERY_CONCURRENCY: usize = 8;

/// A single backlink record from the Constellation API.
#[derive(Debug, Clone, Deserialize)]
pub struct BacklinkRecord {
    pub did: String,
    pub collection: String,
    pub rkey: String,
}

/// Response from the `getBacklinks` XRPC endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct BacklinksResponse {
    pub total: Option<u64>,
    pub records: Vec<BacklinkRecord>,
    pub cursor: Option<String>,
}

/// Client for the Constellation backlink index API.
pub struct ConstellationClient {
    client: reqwest::Client,
    base_url: String,
}

impl ConstellationClient {
    /// Create a new Constellation client pointing at the given base URL.
    pub fn new(base_url: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent("charcoal/0.1 (threat-detection; @chaosgreml.in)")
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    /// Query backlinks for a single AT-URI subject.
    ///
    /// `source` is `collection:json_path` — e.g. `app.bsky.feed.post:embed.record.uri`
    /// for quote-posts, or `app.bsky.feed.repost:subject.uri` for reposts.
    pub async fn get_backlinks(
        &self,
        subject: &str,
        source: &str,
        limit: u32,
    ) -> Result<BacklinksResponse> {
        let url = format!("{}/xrpc/blue.microcosm.links.getBacklinks", self.base_url);

        let response = self
            .client
            .get(&url)
            .query(&[
                ("subject", subject),
                ("source", source),
                ("limit", &limit.to_string()),
            ])
            .send()
            .await
            .context("Constellation API request failed")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Constellation API returned {}: {}", status, body);
        }

        response
            .json::<BacklinksResponse>()
            .await
            .context("Failed to parse Constellation response")
    }

    /// Find all amplification events (quotes + reposts) for a set of post URIs.
    ///
    /// Queries Constellation for both quote-posts and reposts of each URI,
    /// deduplicates by `amplifier_post_uri`, and returns events in the same
    /// format as the notification pipeline.
    pub async fn find_amplification_events(
        &self,
        post_uris: &[String],
    ) -> Vec<AmplificationNotification> {
        // Fetch each URI's quote + repost backlinks concurrently (was one serial
        // pair of round-trips per URI — ~2N sequential calls, #213). Carry the
        // original index and sort back into `post_uris` order before the dedup
        // fold, so the single-threaded `seen` set produces byte-identical output
        // to the old serial loop. Concurrency is capped low: Constellation has
        // no documented rate limit and `PublicAtpClient` has no backoff (#182).
        let mut fetched: Vec<(
            usize,
            String,
            Result<BacklinksResponse>,
            Result<BacklinksResponse>,
        )> = futures::stream::iter(0..post_uris.len())
            .map(|i| {
                let uri = post_uris[i].clone();
                async move {
                    let quotes = self
                        .get_backlinks(&uri, "app.bsky.feed.post:embed.record.uri", 100)
                        .await;
                    let reposts = self
                        .get_backlinks(&uri, "app.bsky.feed.repost:subject.uri", 100)
                        .await;
                    (i, uri, quotes, reposts)
                }
            })
            .buffer_unordered(DISCOVERY_CONCURRENCY)
            .collect()
            .await;
        fetched.sort_by_key(|(i, _, _, _)| *i);

        let ordered = fetched
            .into_iter()
            .map(|(_, uri, quotes, reposts)| (uri, quotes, reposts))
            .collect();
        let events = dedup_amplification_events(ordered);

        debug!(
            total_events = events.len(),
            post_count = post_uris.len(),
            "Constellation backlink query complete"
        );

        events
    }

    /// Find accounts that liked the given post URIs via Constellation backlinks.
    ///
    /// Queries `app.bsky.feed.like:subject.uri` for each URI. Likes don't create
    /// a new post, so `amplifier_post_uri` is empty. Deduplicates by
    /// (amplifier_did, original_post_uri).
    pub async fn find_likers(&self, post_uris: &[String]) -> Vec<AmplificationNotification> {
        // Fetch each URI's like backlinks concurrently, sort back to URI order,
        // then run the single-threaded dedup fold — byte-identical to the old
        // serial loop. Same low concurrency cap as `find_amplification_events`.
        let mut fetched: Vec<(usize, String, Result<BacklinksResponse>)> =
            futures::stream::iter(0..post_uris.len())
                .map(|i| {
                    let uri = post_uris[i].clone();
                    async move {
                        let result = self.get_backlinks(&uri, LIKES_SOURCE, 100).await;
                        (i, uri, result)
                    }
                })
                .buffer_unordered(DISCOVERY_CONCURRENCY)
                .collect()
                .await;
        fetched.sort_by_key(|(i, _, _)| *i);

        let ordered = fetched
            .into_iter()
            .map(|(_, uri, result)| (uri, result))
            .collect();
        let results = dedup_liker_events(ordered);

        debug!(
            like_count = results.len(),
            post_count = post_uris.len(),
            "Constellation likes query complete"
        );

        results
    }
}

/// Pure dedup fold for amplification events (#213). Consumes per-URI
/// `(uri, quote_result, repost_result)` in `post_uris` order and produces the
/// deduped event list — quotes then reposts per URI, deduped by
/// `amplifier_post_uri` across the whole set. Extracted from the (now parallel)
/// network loop so the ordering/dedup logic is unit-testable without network,
/// mirroring `sweep::dedup_second_degree`.
pub fn dedup_amplification_events(
    fetch_results: Vec<(String, Result<BacklinksResponse>, Result<BacklinksResponse>)>,
) -> Vec<AmplificationNotification> {
    let mut events = Vec::new();
    let mut seen_uris = std::collections::HashSet::new();

    for (uri, quotes, reposts) in fetch_results {
        push_backlink_events(&mut events, &mut seen_uris, &uri, "quote", quotes, "quotes");
        push_backlink_events(
            &mut events,
            &mut seen_uris,
            &uri,
            "repost",
            reposts,
            "reposts",
        );
    }

    events
}

/// Push deduped events for one backlink response into `events`. Shared by the
/// quote and repost passes so both dedup against the same `seen_uris` set in
/// exactly the order the serial loop used.
fn push_backlink_events(
    events: &mut Vec<AmplificationNotification>,
    seen_uris: &mut std::collections::HashSet<String>,
    uri: &str,
    event_type: &str,
    result: Result<BacklinksResponse>,
    what: &str,
) {
    match result {
        Ok(resp) => {
            for record in &resp.records {
                let amp_uri = format!("at://{}/{}/{}", record.did, record.collection, record.rkey);
                if seen_uris.insert(amp_uri.clone()) {
                    events.push(AmplificationNotification {
                        event_type: event_type.to_string(),
                        amplifier_did: record.did.clone(),
                        amplifier_handle: record.did.clone(),
                        original_post_uri: Some(uri.to_string()),
                        amplifier_post_uri: amp_uri,
                        indexed_at: String::new(),
                    });
                }
            }
        }
        Err(e) => {
            warn!(uri = uri, error = %e, "Failed to query Constellation for {what}");
        }
    }
}

/// Pure dedup fold for like events (#213). Consumes per-URI `(uri, result)` in
/// order and dedups by `(amplifier_did, original_post_uri)`, mirroring the old
/// serial `find_likers` loop.
pub fn dedup_liker_events(
    fetch_results: Vec<(String, Result<BacklinksResponse>)>,
) -> Vec<AmplificationNotification> {
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for (uri, result) in fetch_results {
        match result {
            Ok(resp) => {
                for record in &resp.records {
                    let key = (record.did.clone(), uri.clone());
                    if seen.insert(key) {
                        results.push(AmplificationNotification {
                            event_type: "like".to_string(),
                            amplifier_did: record.did.clone(),
                            amplifier_handle: record.did.clone(),
                            original_post_uri: Some(uri.clone()),
                            amplifier_post_uri: String::new(),
                            indexed_at: String::new(),
                        });
                    }
                }
            }
            Err(e) => {
                warn!(uri = uri, error = %e, "Failed to query Constellation for likes");
            }
        }
    }

    results
}
