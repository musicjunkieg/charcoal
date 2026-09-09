//! Shared-cache DB methods (#343 §4.1) and the CachedPostFetcher decorator.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use charcoal::bluesky::posts::{FeedKind, FeedPost, Post};
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::{ClassifierVerdictRow, Database, FeedSnapshot, OnnxScoreRow};
use charcoal::observability::cache_stats::CacheStats;
use charcoal::pipeline::scan_phases::feed_cache::{
    snapshot_is_fresh, CachedPostFetcher, SNAPSHOT_FETCH_LIMIT, SNAPSHOT_SOURCE_BLUESKY,
};
use charcoal::pipeline::scan_phases::gather::{FeedSource, PostFetcher};
use chrono::{Duration as ChronoDuration, Utc};
use rusqlite::Connection;

fn setup_db() -> Arc<dyn Database> {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

#[tokio::test]
async fn feed_snapshot_round_trip_and_overwrite() {
    let db = setup_db();
    assert!(db.get_feed_snapshot("did:plc:a").await.unwrap().is_none());

    let snap = FeedSnapshot {
        did: "did:plc:a".into(),
        handle: "a.bsky.social".into(),
        posts_json: "[]".into(),
        fetched_at: "2026-09-08T00:00:00+00:00".into(),
        source: "bluesky".into(),
    };
    db.upsert_feed_snapshot(&snap).await.unwrap();
    assert_eq!(
        db.get_feed_snapshot("did:plc:a").await.unwrap(),
        Some(snap.clone())
    );

    // Same DID, new handle and time: the row is replaced, not duplicated.
    let newer = FeedSnapshot {
        handle: "renamed.bsky.social".into(),
        fetched_at: "2026-09-09T00:00:00+00:00".into(),
        posts_json: "[{}]".into(),
        ..snap
    };
    db.upsert_feed_snapshot(&newer).await.unwrap();
    assert_eq!(
        db.get_feed_snapshot("did:plc:a").await.unwrap(),
        Some(newer)
    );
}

#[tokio::test]
async fn onnx_scores_lookup_is_scoped_by_model_and_returns_only_hits() {
    let db = setup_db();
    let rows = vec![
        OnnxScoreRow {
            text_sha256: "h1".into(),
            score: 0.1,
        },
        OnnxScoreRow {
            text_sha256: "h2".into(),
            score: 0.9,
        },
    ];
    db.upsert_onnx_scores("model-a", &rows).await.unwrap();

    let got = db
        .get_onnx_scores("model-a", &["h1".into(), "h2".into(), "h3".into()])
        .await
        .unwrap();
    let want: HashMap<String, f64> = [("h1".to_string(), 0.1), ("h2".to_string(), 0.9)].into();
    assert_eq!(got, want);

    // A different model id sees nothing.
    assert!(db
        .get_onnx_scores("model-b", &["h1".into()])
        .await
        .unwrap()
        .is_empty());

    // Empty lookup is a no-op.
    assert!(db.get_onnx_scores("model-a", &[]).await.unwrap().is_empty());
    db.upsert_onnx_scores("model-a", &[]).await.unwrap();
}

#[tokio::test]
async fn onnx_scores_upsert_overwrites_same_key() {
    let db = setup_db();
    db.upsert_onnx_scores(
        "m",
        &[OnnxScoreRow {
            text_sha256: "h".into(),
            score: 0.2,
        }],
    )
    .await
    .unwrap();
    db.upsert_onnx_scores(
        "m",
        &[OnnxScoreRow {
            text_sha256: "h".into(),
            score: 0.7,
        }],
    )
    .await
    .unwrap();
    let got = db.get_onnx_scores("m", &["h".into()]).await.unwrap();
    assert_eq!(got["h"], 0.7);
}

#[tokio::test]
async fn classifier_verdicts_scoped_by_model_and_policy() {
    let db = setup_db();
    let rows = vec![
        ClassifierVerdictRow {
            text_sha256: "h1".into(),
            toxic_token: true,
            confidence: 0.95,
        },
        ClassifierVerdictRow {
            text_sha256: "h2".into(),
            toxic_token: false,
            confidence: 0.6,
        },
    ];
    db.upsert_classifier_verdicts("cope-b", "v3", &rows)
        .await
        .unwrap();

    let got = db
        .get_classifier_verdicts("cope-b", "v3", &["h1".into(), "h2".into(), "zzz".into()])
        .await
        .unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got["h1"], rows[0]);
    assert_eq!(got["h2"], rows[1]);

    // Policy bump invalidates.
    assert!(db
        .get_classifier_verdicts("cope-b", "v4", &["h1".into()])
        .await
        .unwrap()
        .is_empty());
    // Empty inputs are no-ops.
    assert!(db
        .get_classifier_verdicts("cope-b", "v3", &[])
        .await
        .unwrap()
        .is_empty());
    db.upsert_classifier_verdicts("cope-b", "v3", &[])
        .await
        .unwrap();
}

fn make_post(n: usize) -> Post {
    Post {
        uri: format!("at://did:plc:a/app.bsky.feed.post/{n}"),
        text: format!("post {n}"),
        created_at: None,
        like_count: 0,
        repost_count: 0,
        quote_count: 0,
        is_quote: false,
        langs: vec!["en".into()],
    }
}

/// 60 originals — more than SNAPSHOT_FETCH_LIMIT so the fetch limit is observable.
fn canned_feed() -> Vec<FeedPost> {
    (0..60)
        .map(|n| FeedPost {
            post: make_post(n),
            kind: FeedKind::Original,
        })
        .collect()
}

struct FakeSource {
    feed: Vec<FeedPost>,
    calls: AtomicUsize,
    last_max_posts: Mutex<Option<usize>>,
}

impl FakeSource {
    fn new() -> Self {
        Self {
            feed: canned_feed(),
            calls: AtomicUsize::new(0),
            last_max_posts: Mutex::new(None),
        }
    }
}

#[async_trait]
impl FeedSource for FakeSource {
    async fn fetch_feed(&self, _handle: &str, max_posts: usize) -> anyhow::Result<Vec<FeedPost>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_max_posts.lock().unwrap() = Some(max_posts);
        Ok(self.feed.iter().take(max_posts).cloned().collect())
    }

    async fn fetch_parents(&self, _uris: &[String]) -> anyhow::Result<HashMap<String, String>> {
        Ok(HashMap::new())
    }
}

#[tokio::test]
async fn miss_fetches_snapshot_limit_and_stores_a_snapshot() {
    let db = setup_db();
    let source = FakeSource::new();
    let stats = Arc::new(CacheStats::default());
    let fetcher = CachedPostFetcher::new(&source, Arc::clone(&db), Arc::clone(&stats));

    let sample = fetcher
        .fetch_sample("did:plc:a", "a.bsky.social", 25)
        .await
        .unwrap();

    // The caller asked for 25 but the source was asked for the snapshot size.
    assert_eq!(sample.total_posts, 25);
    assert_eq!(
        *source.last_max_posts.lock().unwrap(),
        Some(SNAPSHOT_FETCH_LIMIT)
    );
    assert_eq!((stats.hits(), stats.misses()), (0, 1));

    let snap = db
        .get_feed_snapshot("did:plc:a")
        .await
        .unwrap()
        .expect("snapshot stored");
    assert_eq!(snap.handle, "a.bsky.social");
    assert_eq!(snap.source, SNAPSHOT_SOURCE_BLUESKY);
    let stored: Vec<FeedPost> = serde_json::from_str(&snap.posts_json).unwrap();
    assert_eq!(stored.len(), SNAPSHOT_FETCH_LIMIT);
    assert!(snapshot_is_fresh(&snap.fetched_at, Utc::now()));
}

#[tokio::test]
async fn second_call_is_served_from_the_snapshot() {
    let db = setup_db();
    let source = FakeSource::new();
    let stats = Arc::new(CacheStats::default());
    let fetcher = CachedPostFetcher::new(&source, Arc::clone(&db), Arc::clone(&stats));

    let first = fetcher
        .fetch_sample("did:plc:a", "a.bsky.social", 25)
        .await
        .unwrap();
    // The stage-2 re-fetch at 50 is the whole point: it must not hit the network.
    let second = fetcher
        .fetch_sample("did:plc:a", "a.bsky.social", 50)
        .await
        .unwrap();

    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    assert_eq!(first.total_posts, 25);
    assert_eq!(second.total_posts, 50);
    assert_eq!((stats.hits(), stats.misses()), (1, 1));
}

#[tokio::test]
async fn expired_snapshot_is_refetched() {
    let db = setup_db();
    let stale = FeedSnapshot {
        did: "did:plc:a".into(),
        handle: "old.bsky.social".into(),
        posts_json: serde_json::to_string(&canned_feed()).unwrap(),
        fetched_at: (Utc::now() - ChronoDuration::hours(25)).to_rfc3339(),
        source: SNAPSHOT_SOURCE_BLUESKY.into(),
    };
    db.upsert_feed_snapshot(&stale).await.unwrap();

    let source = FakeSource::new();
    let stats = Arc::new(CacheStats::default());
    let fetcher = CachedPostFetcher::new(&source, Arc::clone(&db), Arc::clone(&stats));
    fetcher
        .fetch_sample("did:plc:a", "new.bsky.social", 25)
        .await
        .unwrap();

    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats.hits(), stats.misses()), (0, 1));
    let snap = db.get_feed_snapshot("did:plc:a").await.unwrap().unwrap();
    assert_eq!(snap.handle, "new.bsky.social");
    assert!(snapshot_is_fresh(&snap.fetched_at, Utc::now()));
}

#[tokio::test]
async fn undecodable_snapshot_is_treated_as_a_miss() {
    let db = setup_db();
    db.upsert_feed_snapshot(&FeedSnapshot {
        did: "did:plc:a".into(),
        handle: "a.bsky.social".into(),
        posts_json: "this is not json".into(),
        fetched_at: Utc::now().to_rfc3339(),
        source: SNAPSHOT_SOURCE_BLUESKY.into(),
    })
    .await
    .unwrap();

    let source = FakeSource::new();
    let stats = Arc::new(CacheStats::default());
    let fetcher = CachedPostFetcher::new(&source, Arc::clone(&db), Arc::clone(&stats));
    let sample = fetcher
        .fetch_sample("did:plc:a", "a.bsky.social", 25)
        .await
        .unwrap();

    assert_eq!(sample.total_posts, 25);
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats.hits(), stats.misses()), (0, 1));
    // And the bad row was repaired.
    let snap = db.get_feed_snapshot("did:plc:a").await.unwrap().unwrap();
    assert!(serde_json::from_str::<Vec<FeedPost>>(&snap.posts_json).is_ok());
}

#[test]
fn snapshot_freshness_is_a_24h_window() {
    let now = Utc::now();
    assert!(snapshot_is_fresh(
        &(now - ChronoDuration::hours(23)).to_rfc3339(),
        now
    ));
    assert!(!snapshot_is_fresh(
        &(now - ChronoDuration::hours(25)).to_rfc3339(),
        now
    ));
    assert!(!snapshot_is_fresh("garbage", now));
}
