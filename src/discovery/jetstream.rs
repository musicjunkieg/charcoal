// Jetstream firehose sampler — collect a sample of currently-active posters.
//
// Jetstream (https://github.com/bluesky-social/jetstream) is Bluesky's
// lightweight JSON firehose: every repo commit on the network is emitted as a
// single JSON message over a WebSocket. We tap it to draw an *activity-weighted*
// sample of accounts that are posting right now — the baseline population for
// estimating how much Zentropi work a typical protected user's scan would
// generate. High-volume posters naturally appear more often in the stream, so a
// "first N unique authors in a time window" sample skews toward active accounts,
// which is exactly what we want for capacity planning.
//
// The parsing and sampling logic is split out as pure, synchronous functions
// (`extract_post_author`, `AuthorSampler`) so it can be unit-tested without a
// live network connection. `sample_active_authors` is the thin async I/O shell
// that owns the WebSocket.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::StreamExt;
use serde::Deserialize;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

/// A Jetstream event envelope. We only deserialize the fields we need; serde
/// ignores the rest. `commit` is optional because identity/account events
/// carry a `did` but no commit body.
#[derive(Deserialize)]
struct JetstreamEvent {
    did: String,
    kind: String,
    #[serde(default)]
    commit: Option<JetstreamCommit>,
}

#[derive(Deserialize)]
struct JetstreamCommit {
    operation: String,
    collection: String,
}

/// The collection we care about: top-level feed posts.
const POST_COLLECTION: &str = "app.bsky.feed.post";

/// Parse a Jetstream message and return the author DID *only* when the event is
/// the creation of a new feed post. Returns `None` for updates, deletes, other
/// collections, non-commit events, and unparseable input — so the caller can
/// blindly feed every frame through this filter.
pub fn extract_post_author(event_json: &str) -> Option<String> {
    let event: JetstreamEvent = serde_json::from_str(event_json).ok()?;
    if event.kind != "commit" {
        return None;
    }
    let commit = event.commit?;
    if commit.operation != "create" || commit.collection != POST_COLLECTION {
        return None;
    }
    Some(event.did)
}

/// Accumulates unique post-author DIDs from a stream of Jetstream messages.
///
/// Pure and network-free: feed it raw message text via [`AuthorSampler::offer`]
/// and it tracks uniqueness and ordering. The owning loop stops once
/// [`AuthorSampler::is_full`] returns true (or its own deadline fires).
pub struct AuthorSampler {
    seen: HashSet<String>,
    ordered: Vec<String>,
    target: usize,
}

impl AuthorSampler {
    /// Create a sampler that collects up to `target` unique authors.
    pub fn new(target: usize) -> Self {
        Self {
            seen: HashSet::new(),
            ordered: Vec::new(),
            target,
        }
    }

    /// Offer one raw Jetstream message. If it's a new post author, it's
    /// recorded. Returns `true` once the target count has been reached, so the
    /// caller can break out of its read loop.
    pub fn offer(&mut self, event_json: &str) -> bool {
        if let Some(did) = extract_post_author(event_json) {
            if self.seen.insert(did.clone()) {
                self.ordered.push(did);
            }
        }
        self.is_full()
    }

    /// True once `target` unique authors have been collected. A `target` of 0
    /// is considered immediately full (no work to do).
    pub fn is_full(&self) -> bool {
        self.ordered.len() >= self.target
    }

    /// Number of unique authors collected so far.
    pub fn len(&self) -> usize {
        self.ordered.len()
    }

    /// True when no authors have been collected yet.
    pub fn is_empty(&self) -> bool {
        self.ordered.is_empty()
    }

    /// Consume the sampler and return the collected authors in arrival order.
    pub fn into_authors(self) -> Vec<String> {
        self.ordered
    }
}

/// Configuration for a firehose sampling run.
#[derive(Debug, Clone)]
pub struct JetstreamConfig {
    /// Base Jetstream subscribe endpoint (e.g.
    /// `wss://jetstream2.us-east.bsky.network/subscribe`).
    pub endpoint: String,
    /// Stop after collecting this many unique post authors.
    pub target_unique: usize,
    /// Stop after this long regardless of how many authors were collected.
    pub max_duration: Duration,
}

/// Connect to Jetstream and sample unique active post authors.
///
/// Returns as soon as either `target_unique` authors have been collected or
/// `max_duration` elapses, whichever comes first. A short or empty result is a
/// valid outcome (quiet window / early disconnect), not an error — only a
/// failure to connect is surfaced as `Err`.
pub async fn sample_active_authors(config: &JetstreamConfig) -> Result<Vec<String>> {
    // rustls 0.23 panics on the first TLS handshake if it can't determine a
    // single crypto provider from crate features (both aws-lc-rs and ring end
    // up enabled via feature unification). Install `ring` as the process
    // default. This is idempotent: a second call returns Err, which we ignore.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Ask Jetstream to send only feed-post commits — far less bandwidth than
    // the full firehose, and everything else would be filtered out anyway.
    let url = format!("{}?wantedCollections={}", config.endpoint, POST_COLLECTION);

    info!(
        endpoint = config.endpoint,
        target = config.target_unique,
        max_secs = config.max_duration.as_secs(),
        "Connecting to Jetstream firehose"
    );

    let (mut ws, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .with_context(|| format!("Failed to connect to Jetstream at {}", config.endpoint))?;

    let mut sampler = AuthorSampler::new(config.target_unique);
    if sampler.is_full() {
        return Ok(sampler.into_authors());
    }

    let deadline = tokio::time::sleep(config.max_duration);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => {
                info!(collected = sampler.len(), "Jetstream sampling window elapsed");
                break;
            }
            msg = ws.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if sampler.offer(&text) {
                            info!(collected = sampler.len(), "Reached firehose sample target");
                            break;
                        }
                    }
                    // Binary frames carry zstd-compressed events, which we don't
                    // request; ping/pong are handled by the library. Ignore both.
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        warn!(error = %e, "Jetstream read error, stopping sampler");
                        break;
                    }
                    None => {
                        debug!("Jetstream stream closed");
                        break;
                    }
                }
            }
        }
    }

    Ok(sampler.into_authors())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post_event(did: &str) -> String {
        format!(
            r#"{{"did":"{did}","time_us":1700000000000000,"kind":"commit",
                "commit":{{"rev":"abc","operation":"create",
                "collection":"app.bsky.feed.post","rkey":"xyz",
                "record":{{"$type":"app.bsky.feed.post","text":"hi"}},"cid":"c"}}}}"#
        )
    }

    #[test]
    fn extracts_author_from_post_create() {
        let did = extract_post_author(&post_event("did:plc:alice"));
        assert_eq!(did.as_deref(), Some("did:plc:alice"));
    }

    #[test]
    fn ignores_non_create_operations() {
        let json = r#"{"did":"did:plc:bob","kind":"commit",
            "commit":{"operation":"delete","collection":"app.bsky.feed.post"}}"#;
        assert_eq!(extract_post_author(json), None);
    }

    #[test]
    fn ignores_other_collections() {
        let json = r#"{"did":"did:plc:bob","kind":"commit",
            "commit":{"operation":"create","collection":"app.bsky.feed.like"}}"#;
        assert_eq!(extract_post_author(json), None);
    }

    #[test]
    fn ignores_non_commit_events() {
        // Identity events carry a did but no commit body.
        let json = r#"{"did":"did:plc:bob","kind":"identity"}"#;
        assert_eq!(extract_post_author(json), None);
    }

    #[test]
    fn ignores_unparseable_input() {
        assert_eq!(extract_post_author("not json"), None);
        assert_eq!(extract_post_author(""), None);
    }

    #[test]
    fn sampler_collects_unique_authors_in_order() {
        let mut s = AuthorSampler::new(10);
        assert!(!s.offer(&post_event("did:plc:a")));
        assert!(!s.offer(&post_event("did:plc:b")));
        // Duplicate author does not add a second entry.
        assert!(!s.offer(&post_event("did:plc:a")));
        assert_eq!(s.len(), 2);
        assert_eq!(s.into_authors(), vec!["did:plc:a", "did:plc:b"]);
    }

    #[test]
    fn sampler_reports_full_at_target() {
        let mut s = AuthorSampler::new(2);
        assert!(!s.offer(&post_event("did:plc:a")));
        // Second unique author hits the target → offer returns true.
        assert!(s.offer(&post_event("did:plc:b")));
        assert!(s.is_full());
    }

    #[test]
    fn sampler_skips_noise_frames() {
        let mut s = AuthorSampler::new(5);
        s.offer("garbage");
        s.offer(r#"{"did":"did:plc:x","kind":"identity"}"#);
        assert!(s.is_empty());
    }

    #[test]
    fn zero_target_is_immediately_full() {
        let s = AuthorSampler::new(0);
        assert!(s.is_full());
    }
}
