// Public AT Protocol client — unauthenticated XRPC over HTTP.
//
// All AT Protocol read endpoints are public and don't require authentication.
// This client replaces the authenticated BskyAgent for the intelligence
// pipeline — auth is only needed for write operations (blocking/muting),
// which is a future feature.

use std::time::Duration;

use anyhow::{Context, Result};
use backon::{ExponentialBuilder, Retryable};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use tracing::debug;

/// Default public API endpoint for AT Protocol read operations.
pub const DEFAULT_PUBLIC_API_URL: &str = "https://public.api.bsky.app";

/// Retry policy for public Bluesky reads (#182).
///
/// The public API publishes no backoff contract, and `DISCOVERY_CONCURRENCY`
/// in `constellation/client.rs` is capped at ~8 explicitly "until #182 lands".
/// Scan concurrency (#257) raises in-flight requests past that ceiling, so a
/// rate-limited read has to recover rather than fail — #236 shows a gather
/// failure can cost the whole account.
const XRPC_MAX_RETRIES: usize = 4;
const XRPC_MIN_BACKOFF_MS: u64 = 250;
const XRPC_MAX_BACKOFF_MS: u64 = 8_000;

/// Per-request ceiling for a public API call.
///
/// Async reqwest has **no default timeout**, so a server that accepts the
/// connection and then never answers holds its gather task forever. The
/// retries above do not help — a request that never returns never becomes an
/// error to retry. This is the same defect fixed for Constellation in #235;
/// the same reasoning applies here, and #257's raised scan concurrency makes
/// a permanently-parked task cost a slot that never comes back.
///
/// Generous because gather is a batch path, not user-facing — the point is
/// that a hang is bounded, not that it is fast.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Ceiling for establishing the TCP/TLS connection specifically, so a
/// black-holed host fails fast instead of burning the full request budget.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Why an XRPC attempt failed, so the retry filter can be explicit rather than
/// string-matching an `anyhow` chain.
#[derive(Debug)]
enum XrpcAttemptError {
    /// Transport failure, 429, or 5xx — worth another attempt.
    Transient(anyhow::Error),
    /// 4xx other than 429, or a deserialization failure. Retrying cannot help.
    Permanent(anyhow::Error),
}

impl XrpcAttemptError {
    fn is_retryable(&self) -> bool {
        matches!(self, Self::Transient(_))
    }

    fn into_inner(self) -> anyhow::Error {
        match self {
            Self::Transient(e) | Self::Permanent(e) => e,
        }
    }
}

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
        Self::with_timeout(base_url, REQUEST_TIMEOUT)
    }

    /// As [`new`](Self::new), with an explicit request timeout. Exists so the
    /// timeout behaviour is testable without a 30-second test.
    pub fn with_timeout(base_url: &str, request_timeout: Duration) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent("charcoal/0.1 (threat-detection; @chaosgreml.in)")
            .timeout(request_timeout)
            .connect_timeout(CONNECT_TIMEOUT)
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

        let attempt = || async {
            let response = self
                .client
                .get(&url)
                .query(params)
                .send()
                .await
                .map_err(|e| {
                    // Transport-level failure: DNS, connect, timeout. Transient.
                    XrpcAttemptError::Transient(
                        anyhow::Error::new(e).context(format!("XRPC request failed: {nsid}")),
                    )
                })?;

            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                let err = anyhow::anyhow!("XRPC {nsid} returned {status}: {body}");
                // 429 and 5xx recover on their own; every other 4xx is our bug.
                return Err(if status.as_u16() == 429 || status.is_server_error() {
                    XrpcAttemptError::Transient(err)
                } else {
                    XrpcAttemptError::Permanent(err)
                });
            }

            // Split the read from the parse: `Response::json` does both under
            // one error type, which misclassified a body read interrupted
            // mid-transfer (genuinely transient, like #183's RunPod transport
            // blip) as a permanent, non-retryable deserialization failure.
            let bytes = response.bytes().await.map_err(|e| {
                XrpcAttemptError::Transient(
                    anyhow::Error::new(e).context(format!("Failed to read {nsid} response body")),
                )
            })?;

            serde_json::from_slice(&bytes).map_err(|e| {
                XrpcAttemptError::Permanent(
                    anyhow::Error::new(e).context(format!("Failed to deserialize {nsid} response")),
                )
            })
        };

        attempt
            .retry(
                ExponentialBuilder::default()
                    .with_min_delay(Duration::from_millis(XRPC_MIN_BACKOFF_MS))
                    .with_max_delay(Duration::from_millis(XRPC_MAX_BACKOFF_MS))
                    .with_max_times(XRPC_MAX_RETRIES)
                    .with_jitter(),
            )
            .when(|e: &XrpcAttemptError| e.is_retryable())
            .await
            .map_err(XrpcAttemptError::into_inner)
    }

    /// Resolve a handle to its DID via the public API.
    pub async fn resolve_handle(&self, handle: &str) -> Result<String> {
        let resp: ResolveHandleResponse = self
            .xrpc_get("com.atproto.identity.resolveHandle", &[("handle", handle)])
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

#[cfg(test)]
mod retry_tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(serde::Deserialize, Debug)]
    struct Probe {
        ok: bool,
    }

    /// A 429 must be retried, not surfaced as a failure. Bluesky publishes no
    /// backoff contract for the public read API, so a rate-limited gather
    /// currently loses the account outright (#236 shows the cost).
    #[tokio::test]
    async fn xrpc_get_retries_429_then_succeeds() {
        let server = MockServer::start().await;
        // First call 429, subsequent calls 200. `up_to_n_times(1)` makes the
        // 429 mock fire once; the 200 mock then serves the retry.
        Mock::given(method("GET"))
            .and(path("/xrpc/probe.test"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xrpc/probe.test"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .mount(&server)
            .await;

        let client = PublicAtpClient::new(&server.uri()).unwrap();
        let got: Probe = client.xrpc_get("probe.test", &[]).await.unwrap();
        assert!(got.ok, "the retry must return the successful body");
    }

    /// A 400 is the caller's fault and will never succeed. Retrying it wastes
    /// the budget and delays the real error.
    #[tokio::test]
    async fn xrpc_get_does_not_retry_4xx_other_than_429() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/probe.test"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .expect(1) // exactly one attempt — a retry would make this 2+
            .mount(&server)
            .await;

        let client = PublicAtpClient::new(&server.uri()).unwrap();
        let err = client
            .xrpc_get::<Probe>("probe.test", &[])
            .await
            .expect_err("400 must surface");
        assert!(
            format!("{err:#}").contains("400"),
            "error must name the status"
        );
        // MockServer asserts `.expect(1)` on drop.
    }

    /// 5xx is the server failing transiently — retry it.
    #[tokio::test]
    async fn xrpc_get_retries_5xx_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/probe.test"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xrpc/probe.test"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .mount(&server)
            .await;

        let client = PublicAtpClient::new(&server.uri()).unwrap();
        let got: Probe = client.xrpc_get("probe.test", &[]).await.unwrap();
        assert!(got.ok);
    }

    /// A stalled public-API response must terminate, not hang.
    ///
    /// Async reqwest has no default timeout, so before this the client would
    /// wait forever on a server that accepts the connection and then never
    /// answers — and the #182 retries above cannot help, because a request
    /// that never returns never produces an error to retry. Under #257 that
    /// parks a scan's gather task, and its queue slot, permanently.
    ///
    /// The elapsed bound is what discriminates: without a timeout the single
    /// stalled attempt below alone takes the mock's full 30s. With one, the
    /// wall clock is the retry budget (5 attempts plus jittered backoff,
    /// ~4s), not the stall.
    #[tokio::test]
    async fn xrpc_get_times_out_instead_of_hanging() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/probe.test"))
            // Answer far later than the timeout below — stands in for a stall.
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;

        let client =
            PublicAtpClient::with_timeout(&server.uri(), Duration::from_millis(100)).unwrap();

        let started = std::time::Instant::now();
        let result = client.xrpc_get::<Probe>("probe.test", &[]).await;
        let elapsed = started.elapsed();

        assert!(
            result.is_err(),
            "a stalled request must surface as an error"
        );
        assert!(
            elapsed < Duration::from_secs(15),
            "must give up on its own timeout, took {elapsed:?}"
        );
    }

    /// The timeout must not fire on responses that arrive normally.
    #[tokio::test]
    async fn xrpc_get_succeeds_well_within_the_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/probe.test"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .mount(&server)
            .await;

        let client = PublicAtpClient::new(&server.uri()).unwrap();
        let got: Probe = client
            .xrpc_get("probe.test", &[])
            .await
            .expect("a prompt response must not be cut short by the timeout");
        assert!(got.ok);
    }
}
