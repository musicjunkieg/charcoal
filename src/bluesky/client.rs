// Public AT Protocol client — unauthenticated XRPC over HTTP.
//
// All AT Protocol read endpoints are public and don't require authentication.
// This client replaces the authenticated BskyAgent for the intelligence
// pipeline — auth is only needed for write operations (blocking/muting),
// which is a future feature.

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use tracing::debug;

/// Default public API endpoint for AT Protocol read operations.
pub const DEFAULT_PUBLIC_API_URL: &str = "https://public.api.bsky.app";

/// Unauthenticated HTTP client for public AT Protocol XRPC endpoints.
///
/// Modeled on the ConstellationClient pattern — a thin reqwest wrapper
/// with a generic XRPC GET helper. Replaces `bsky-sdk::BskyAgent` for
/// all read-only operations.
pub struct PublicAtpClient {
    client: reqwest::Client,
    base_url: String,
}

impl PublicAtpClient {
    /// Create a new public API client pointing at the given base URL.
    ///
    /// Defaults to `https://public.api.bsky.app` — pass a different URL
    /// for testing or alternate PDS instances.
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

    /// Make a GET request to an XRPC endpoint and deserialize the response.
    ///
    /// `nsid` is the XRPC method name (e.g. "app.bsky.feed.getAuthorFeed").
    /// `params` are query string key-value pairs. Use repeated keys for
    /// array parameters (e.g. `[("actors", "did1"), ("actors", "did2")]`).
    pub async fn xrpc_get<T: DeserializeOwned>(
        &self,
        nsid: &str,
        params: &[(&str, &str)],
    ) -> Result<T> {
        let url = format!("{}/xrpc/{}", self.base_url, nsid);

        debug!(nsid = nsid, "XRPC GET request");

        let response = self
            .client
            .get(&url)
            .query(params)
            .send()
            .await
            .with_context(|| format!("XRPC request failed: {nsid}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("XRPC {nsid} returned {status}: {body}");
        }

        response
            .json::<T>()
            .await
            .with_context(|| format!("Failed to deserialize {nsid} response"))
    }

    /// Resolve a handle to its DID via the public API.
    pub async fn resolve_handle(&self, handle: &str) -> Result<String> {
        let resp: ResolveHandleResponse = self
            .xrpc_get(
                "com.atproto.identity.resolveHandle",
                &[("handle", handle)],
            )
            .await
            .with_context(|| format!("Failed to resolve handle @{handle}"))?;
        Ok(resp.did)
    }

    /// Look up the PDS service endpoint for a DID via the PLC directory.
    ///
    /// Queries plc.directory for the DID document and extracts the
    /// `#atproto_pds` service endpoint. This tells us which server
    /// hosts the user's repo (needed for `com.atproto.repo.*` calls).
    pub async fn resolve_pds_url(&self, did: &str) -> Result<String> {
        let url = format!("https://plc.directory/{did}");

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("Failed to fetch DID document for {did}"))?;

        if !response.status().is_success() {
            let status = response.status();
            anyhow::bail!("PLC directory returned {status} for {did}");
        }

        let doc: DidDocument = response
            .json()
            .await
            .context("Failed to parse DID document")?;

        doc.service
            .iter()
            .find(|s| s.id == "#atproto_pds")
            .map(|s| s.service_endpoint.clone())
            .ok_or_else(|| anyhow::anyhow!("No PDS service found in DID document for {did}"))
    }
}

// -- Serde types for identity resolution --

#[derive(Deserialize)]
struct ResolveHandleResponse {
    did: String,
}

#[derive(Deserialize)]
struct DidDocument {
    service: Vec<DidService>,
}

#[derive(Deserialize)]
struct DidService {
    id: String,
    #[serde(rename = "serviceEndpoint")]
    service_endpoint: String,
}

// -- Serde types for com.atproto.repo.listRecords --

/// Response from `com.atproto.repo.listRecords`.
#[derive(Debug, Deserialize)]
pub struct ListRecordsResponse {
    pub records: Vec<RepoRecord>,
    pub cursor: Option<String>,
}

/// A single record from a repo listing.
#[derive(Debug, Deserialize)]
pub struct RepoRecord {
    pub uri: String,
    pub value: serde_json::Value,
}

/// A block record's value fields (from `app.bsky.graph.block`).
#[derive(Debug, Deserialize)]
pub struct BlockRecordValue {
    /// DID of the blocked account
    pub subject: String,
    /// When the block was created
    #[serde(rename = "createdAt")]
    pub created_at: String,
}
