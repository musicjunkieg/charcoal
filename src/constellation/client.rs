// HTTP client for the Constellation XRPC backlink API.
//
// Queries `blue.microcosm.links.getBacklinks` to find all quotes and reposts
// of given post URIs. Results are converted into the same AmplificationNotification
// format used by the notification pipeline, so they can be merged seamlessly.

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::{debug, warn};

use crate::bluesky::notifications::AmplificationNotification;

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
        let mut events = Vec::new();
        let mut seen_uris = std::collections::HashSet::new();

        for uri in post_uris {
            // Query for quote-posts referencing this URI
            // Source format: collection:json_path — quotes embed the original via embed.record.uri
            match self
                .get_backlinks(uri, "app.bsky.feed.post:embed.record.uri", 100)
                .await
            {
                Ok(resp) => {
                    for record in &resp.records {
                        let amp_uri =
                            format!("at://{}/{}/{}", record.did, record.collection, record.rkey);
                        if seen_uris.insert(amp_uri.clone()) {
                            events.push(AmplificationNotification {
                                event_type: "quote".to_string(),
                                amplifier_did: record.did.clone(),
                                amplifier_handle: record.did.clone(),
                                original_post_uri: Some(uri.clone()),
                                amplifier_post_uri: amp_uri,
                                indexed_at: String::new(),
                            });
                        }
                    }
                }
                Err(e) => {
                    warn!(uri = uri, error = %e, "Failed to query Constellation for quotes");
                }
            }

            // Query for reposts referencing this URI
            // Source format: collection:json_path — reposts reference the original via subject.uri
            match self
                .get_backlinks(uri, "app.bsky.feed.repost:subject.uri", 100)
                .await
            {
                Ok(resp) => {
                    for record in &resp.records {
                        let amp_uri =
                            format!("at://{}/{}/{}", record.did, record.collection, record.rkey);
                        if seen_uris.insert(amp_uri.clone()) {
                            events.push(AmplificationNotification {
                                event_type: "repost".to_string(),
                                amplifier_did: record.did.clone(),
                                amplifier_handle: record.did.clone(),
                                original_post_uri: Some(uri.clone()),
                                amplifier_post_uri: amp_uri,
                                indexed_at: String::new(),
                            });
                        }
                    }
                }
                Err(e) => {
                    warn!(uri = uri, error = %e, "Failed to query Constellation for reposts");
                }
            }
        }

        debug!(
            total_events = events.len(),
            post_count = post_uris.len(),
            "Constellation backlink query complete"
        );

        events
    }
}
