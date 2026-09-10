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

// --- Retention (#343 / PR #118 review) ---

/// Older than any retention window we will ever set.
const ANCIENT: &str = "2020-01-01T00:00:00+00:00";

/// A file-backed database plus a second connection to it.
///
/// File-backed, not `:memory:`, on purpose: `scored_at` / `classified_at` are
/// stamped by the backend and are deliberately not part of the row types the
/// trait accepts, so the only way to age a scoring row is raw SQL — which needs
/// a connection the `SqliteDatabase` does not own.
fn file_db() -> (tempfile::TempDir, Arc<dyn Database>, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.db");
    let conn = Connection::open(&path).unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    let side = Connection::open(&path).unwrap();
    (dir, Arc::new(SqliteDatabase::new(conn)), side)
}

/// Seed one ancient and one fresh row in each of the three cache tables.
async fn seed_old_and_fresh(db: &Arc<dyn Database>, side: &Connection) {
    let now = Utc::now().to_rfc3339();
    for (did, fetched_at) in [("did:plc:old", ANCIENT), ("did:plc:fresh", now.as_str())] {
        db.upsert_feed_snapshot(&FeedSnapshot {
            did: did.into(),
            handle: format!("{did}.bsky.social"),
            posts_json: "[]".into(),
            fetched_at: fetched_at.into(),
            source: SNAPSHOT_SOURCE_BLUESKY.into(),
        })
        .await
        .unwrap();
    }

    db.upsert_onnx_scores(
        "m",
        &[
            OnnxScoreRow {
                text_sha256: "old".into(),
                score: 0.1,
            },
            OnnxScoreRow {
                text_sha256: "fresh".into(),
                score: 0.2,
            },
        ],
    )
    .await
    .unwrap();
    db.upsert_classifier_verdicts(
        "m",
        "v1",
        &[
            ClassifierVerdictRow {
                text_sha256: "old".into(),
                toxic_token: true,
                confidence: 0.9,
            },
            ClassifierVerdictRow {
                text_sha256: "fresh".into(),
                toxic_token: false,
                confidence: 0.1,
            },
        ],
    )
    .await
    .unwrap();

    // Backdate the rows the upserts stamped with "now".
    side.execute(
        "UPDATE onnx_scores SET scored_at = ?1 WHERE text_sha256 = 'old'",
        [ANCIENT],
    )
    .unwrap();
    side.execute(
        "UPDATE classifier_verdicts SET classified_at = ?1 WHERE text_sha256 = 'old'",
        [ANCIENT],
    )
    .unwrap();
}

/// Every fresh row must still be readable through the normal accessors.
async fn assert_only_fresh_survives(db: &Arc<dyn Database>) {
    assert!(db.get_feed_snapshot("did:plc:old").await.unwrap().is_none());
    assert!(db
        .get_feed_snapshot("did:plc:fresh")
        .await
        .unwrap()
        .is_some());
    let scores = db
        .get_onnx_scores("m", &["old".into(), "fresh".into()])
        .await
        .unwrap();
    assert_eq!(scores.len(), 1);
    assert_eq!(scores["fresh"], 0.2);
    let verdicts = db
        .get_classifier_verdicts("m", "v1", &["old".into(), "fresh".into()])
        .await
        .unwrap();
    assert_eq!(verdicts.len(), 1);
    assert!(verdicts.contains_key("fresh"));
}

#[tokio::test]
async fn evict_stale_cache_removes_only_rows_past_the_cutoff() {
    let (_dir, db, side) = file_db();
    seed_old_and_fresh(&db, &side).await;

    let now = Utc::now();
    let feed_cutoff = (now - charcoal::db::cache_retention::FEED_SNAPSHOT_RETENTION).to_rfc3339();
    let score_cutoff = (now - charcoal::db::cache_retention::SCORE_RETENTION).to_rfc3339();
    let evicted = db
        .evict_stale_cache(&feed_cutoff, &score_cutoff)
        .await
        .unwrap();

    assert_eq!(
        evicted,
        charcoal::db::CacheEviction {
            feed_snapshots: 1,
            onnx_scores: 1,
            classifier_verdicts: 1,
        }
    );
    assert_only_fresh_survives(&db).await;
}

/// The two cutoffs are independent: a feed sweep must not touch scores.
#[tokio::test]
async fn evict_stale_cache_applies_each_cutoff_to_its_own_tables() {
    let (_dir, db, side) = file_db();
    seed_old_and_fresh(&db, &side).await;

    // Feed cutoff evicts everything, including the row stamped `Utc::now()` by
    // the seed — one second ahead so the seed's stamp can't tie the cutoff in
    // the same instant. Score cutoff predates even the ancient rows.
    let feed_cutoff = (Utc::now() + chrono::Duration::seconds(1)).to_rfc3339();
    let evicted = db
        .evict_stale_cache(&feed_cutoff, "1999-01-01T00:00:00+00:00")
        .await
        .unwrap();
    assert_eq!(
        evicted,
        charcoal::db::CacheEviction {
            feed_snapshots: 2,
            onnx_scores: 0,
            classifier_verdicts: 0,
        }
    );
    assert_eq!(
        db.get_onnx_scores("m", &["old".into(), "fresh".into()])
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn evict_stale_cache_on_an_empty_database_is_a_no_op() {
    let db = setup_db();
    let now = Utc::now();
    let evicted = db
        .evict_stale_cache(
            &(now - ChronoDuration::days(7)).to_rfc3339(),
            &(now - ChronoDuration::days(90)).to_rfc3339(),
        )
        .await
        .unwrap();
    assert_eq!(evicted, charcoal::db::CacheEviction::default());
}

/// The best-effort wrapper sweeps with the module's own cutoffs and returns
/// unit — a caller can never be forced to handle a cache error.
#[tokio::test]
async fn best_effort_eviction_sweeps_and_is_repeatable() {
    let (_dir, db, side) = file_db();
    seed_old_and_fresh(&db, &side).await;

    charcoal::db::cache_retention::evict_stale_cache_best_effort(db.as_ref()).await;
    assert_only_fresh_survives(&db).await;

    // A second sweep against the now-clean database still succeeds.
    charcoal::db::cache_retention::evict_stale_cache_best_effort(db.as_ref()).await;
    assert_only_fresh_survives(&db).await;
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
    // Per-candidate, not per-fetch (spec §4.1): the second call for the same
    // DID is a real cache hit, but it must not be counted — it would inflate
    // the gate. Only the first call ever touches the counters.
    assert_eq!((stats.hits(), stats.misses()), (0, 1));
}

#[tokio::test]
async fn distinct_dids_each_count_once() {
    let db = setup_db();
    let source = FakeSource::new();
    let stats = Arc::new(CacheStats::default());
    let fetcher = CachedPostFetcher::new(&source, Arc::clone(&db), Arc::clone(&stats));

    fetcher
        .fetch_sample("did:plc:a", "a.bsky.social", 25)
        .await
        .unwrap();
    fetcher
        .fetch_sample("did:plc:a", "a.bsky.social", 50)
        .await
        .unwrap();
    fetcher
        .fetch_sample("did:plc:b", "b.bsky.social", 25)
        .await
        .unwrap();
    fetcher
        .fetch_sample("did:plc:b", "b.bsky.social", 50)
        .await
        .unwrap();

    assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    assert_eq!((stats.hits(), stats.misses()), (0, 2));
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

/// A `FeedSource` that always errors — for the "network failure must not
/// leave a partial/decoy snapshot" test below.
struct ErrSource;

#[async_trait]
impl FeedSource for ErrSource {
    async fn fetch_feed(&self, _handle: &str, _max_posts: usize) -> anyhow::Result<Vec<FeedPost>> {
        Err(anyhow::anyhow!("network exploded"))
    }

    async fn fetch_parents(&self, _uris: &[String]) -> anyhow::Result<HashMap<String, String>> {
        Ok(HashMap::new())
    }
}

#[tokio::test]
async fn source_error_propagates_before_any_upsert() {
    let db = setup_db();
    let source = ErrSource;
    let stats = Arc::new(CacheStats::default());
    let fetcher = CachedPostFetcher::new(&source, Arc::clone(&db), Arc::clone(&stats));

    let result = fetcher.fetch_sample("did:plc:a", "a.bsky.social", 25).await;

    assert!(result.is_err());
    assert!(db.get_feed_snapshot("did:plc:a").await.unwrap().is_none());
    // The miss is counted at the decision point (before the network call),
    // not after a successful upsert — so it's still recorded even though
    // the fetch itself failed.
    assert_eq!((stats.hits(), stats.misses()), (0, 1));
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
