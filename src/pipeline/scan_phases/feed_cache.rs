//! Read-through feed snapshot cache (#343 §4.1).
//!
//! Wraps a raw [`FeedSource`] and exposes the [`PostFetcher`] the gather
//! consumes. On a miss it fetches [`SNAPSHOT_FETCH_LIMIT`] posts — the
//! larger of the two sample sizes the gather asks for — and stores the whole
//! feed, so the stage-2 re-fetch (and every other scan that meets this
//! account within [`SNAPSHOT_TTL`]) is served from Postgres instead of
//! Bluesky. The cache is keyed by DID, not handle: handles change.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tracing::warn;

use crate::bluesky::posts::{sample_from_feed, FeedPost, PostSample};
use crate::db::{Database, FeedSnapshot};
use crate::observability::cache_stats::CacheStats;

use super::gather::{FeedSource, PostFetcher};

/// A snapshot older than this is refetched (spec §4.1).
pub const SNAPSHOT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Always fetch the larger sample so one miss serves both gather stages.
pub const SNAPSHOT_FETCH_LIMIT: usize = 50;

/// `account_feed_snapshots.source` for feeds read from the public AppView.
pub const SNAPSHOT_SOURCE_BLUESKY: &str = "bluesky";

/// True when `fetched_at` parses as RFC3339 and is newer than
/// `now − SNAPSHOT_TTL`. Unparseable timestamps count as stale.
pub fn snapshot_is_fresh(fetched_at: &str, now: DateTime<Utc>) -> bool {
    match DateTime::parse_from_rfc3339(fetched_at) {
        Ok(t) => {
            let ttl = chrono::Duration::from_std(SNAPSHOT_TTL).expect("24h fits chrono");
            t.with_timezone(&Utc) > now - ttl
        }
        Err(_) => false,
    }
}

pub struct CachedPostFetcher<'a> {
    source: &'a dyn FeedSource,
    db: Arc<dyn Database>,
    stats: Arc<CacheStats>,
    /// DIDs already counted toward `stats` this scan. The gather calls
    /// `fetch_sample` twice per candidate that reaches stage 2 (limit 25,
    /// then 50) — the second call is always served from the snapshot the
    /// first call just wrote, so counting both would inflate the hit rate.
    /// The Phase 1 gate (spec §4.1) is defined per candidate, not per fetch,
    /// so only the first call for a given DID updates `stats`; later calls
    /// still read/refetch normally, they just don't recount.
    seen: Mutex<HashSet<String>>,
}

impl<'a> CachedPostFetcher<'a> {
    pub fn new(source: &'a dyn FeedSource, db: Arc<dyn Database>, stats: Arc<CacheStats>) -> Self {
        Self {
            source,
            db,
            stats,
            seen: Mutex::new(HashSet::new()),
        }
    }

    /// True the first time this DID is seen this scan (and records it).
    /// Lock scope is intentionally tiny — never held across an `.await`.
    fn first_time_seeing(&self, did: &str) -> bool {
        self.seen.lock().unwrap().insert(did.to_string())
    }

    /// Fresh, decodable snapshot for `did`, or `None` (a stale or corrupt row
    /// is a miss — the refetch overwrites it).
    async fn fresh_feed(&self, did: &str) -> Result<Option<Vec<FeedPost>>> {
        let Some(snap) = self.db.get_feed_snapshot(did).await? else {
            return Ok(None);
        };
        if !snapshot_is_fresh(&snap.fetched_at, Utc::now()) {
            return Ok(None);
        }
        match serde_json::from_str::<Vec<FeedPost>>(&snap.posts_json) {
            Ok(feed) => Ok(Some(feed)),
            Err(e) => {
                // Never log the JSON itself — it contains post text.
                warn!(did, error = %e, "feed snapshot did not decode; refetching");
                Ok(None)
            }
        }
    }
}

#[async_trait]
impl PostFetcher for CachedPostFetcher<'_> {
    async fn fetch_sample(&self, did: &str, handle: &str, limit: usize) -> Result<PostSample> {
        // Count only the first call for this DID this scan — see `seen` doc comment.
        let first_time = self.first_time_seeing(did);

        if let Some(feed) = self.fresh_feed(did).await? {
            if first_time {
                self.stats.hit(1);
            }
            return Ok(sample_from_feed(&feed, limit));
        }

        if first_time {
            self.stats.miss(1);
        }
        let feed = self
            .source
            .fetch_feed(handle, limit.max(SNAPSHOT_FETCH_LIMIT))
            .await?;
        let snapshot = FeedSnapshot {
            did: did.to_string(),
            handle: handle.to_string(),
            posts_json: serde_json::to_string(&feed).context("serialising feed snapshot")?,
            fetched_at: Utc::now().to_rfc3339(),
            source: SNAPSHOT_SOURCE_BLUESKY.to_string(),
        };
        // A cache write failure is a real DB failure — surface it rather than
        // silently running uncached (the runbook's hit-rate numbers would lie).
        self.db.upsert_feed_snapshot(&snapshot).await?;
        Ok(sample_from_feed(&feed, limit))
    }

    async fn fetch_parents(&self, uris: &[String]) -> Result<HashMap<String, String>> {
        self.source.fetch_parents(uris).await
    }
}
