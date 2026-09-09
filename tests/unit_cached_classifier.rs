//! CachedClassifier (#343 §4.1): one stage-2 verdict per (text, model, policy).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use charcoal::observability::cache_stats::CacheStats;
use charcoal::toxicity::cached::text_sha256;
use charcoal::toxicity::cached_classifier::CachedClassifier;
use charcoal::toxicity::classifier::{ClassifierVerdict, ItemOutcome, ToxicityClassifier};
use rusqlite::Connection;

fn setup_db() -> Arc<dyn Database> {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

/// Toxic iff the text contains "bad"; texts containing "undecodable" come back
/// as `ItemOutcome::Error`; texts containing "boom" fail the whole request.
struct FakeClassifier {
    policy: &'static str,
    batches: Mutex<Vec<Vec<String>>>,
    calls: AtomicUsize,
}

impl FakeClassifier {
    fn new(policy: &'static str) -> Self {
        Self {
            policy,
            batches: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        }
    }

    fn verdict(&self, toxic: bool) -> ClassifierVerdict {
        ClassifierVerdict {
            toxic_token: toxic,
            confidence: if toxic { 0.9 } else { 0.2 },
            latency_ms: 42,
            model_id: self.model_id().to_string(),
            policy_version: self.policy_version().to_string(),
        }
    }
}

#[async_trait]
impl ToxicityClassifier for FakeClassifier {
    async fn classify(&self, content: &str) -> anyhow::Result<ClassifierVerdict> {
        match self
            .classify_batch(std::slice::from_ref(&content.to_string()))
            .await?
            .remove(0)
        {
            ItemOutcome::Verdict(v) => Ok(v),
            ItemOutcome::Error(e) => anyhow::bail!("{e}"),
        }
    }

    async fn classify_batch(&self, contents: &[String]) -> anyhow::Result<Vec<ItemOutcome>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.batches.lock().unwrap().push(contents.to_vec());
        if contents.iter().any(|c| c.contains("boom")) {
            anyhow::bail!("request failed");
        }
        Ok(contents
            .iter()
            .map(|c| {
                if c.contains("undecodable") {
                    ItemOutcome::Error("slot did not decode".into())
                } else {
                    ItemOutcome::Verdict(self.verdict(c.contains("bad")))
                }
            })
            .collect())
    }

    fn max_batch_size(&self) -> usize {
        16
    }
    fn name(&self) -> &'static str {
        "fake"
    }
    fn model_id(&self) -> &'static str {
        "fake-model"
    }
    fn policy_version(&self) -> &'static str {
        self.policy
    }
    fn threshold(&self) -> f32 {
        0.5
    }
}

fn cached(
    db: &Arc<dyn Database>,
    inner: Arc<FakeClassifier>,
) -> (CachedClassifier, Arc<CacheStats>) {
    let stats = Arc::new(CacheStats::default());
    let c = CachedClassifier::new(inner, Arc::clone(db), Arc::clone(&stats));
    (c, stats)
}

fn texts(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn toxic_flags(out: &[ItemOutcome]) -> Vec<Option<bool>> {
    out.iter()
        .map(|o| match o {
            ItemOutcome::Verdict(v) => Some(v.toxic_token),
            ItemOutcome::Error(_) => None,
        })
        .collect()
}

#[tokio::test]
async fn first_batch_forwards_everything_and_persists_verdicts() {
    let db = setup_db();
    let inner = Arc::new(FakeClassifier::new("v1"));
    let (c, stats) = cached(&db, Arc::clone(&inner));

    let out = c
        .classify_batch(&texts(&["fine", "so bad", "undecodable"]))
        .await
        .unwrap();

    assert_eq!(toxic_flags(&out), vec![Some(false), Some(true), None]);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats.hits(), stats.misses()), (0, 3));
    let rows = db
        .get_classifier_verdicts(
            "fake-model",
            "v1",
            &[
                text_sha256("fine"),
                text_sha256("so bad"),
                text_sha256("undecodable"),
            ],
        )
        .await
        .unwrap();
    // Only decodable verdicts are cached.
    assert_eq!(rows.len(), 2);
    assert!(rows[&text_sha256("so bad")].toxic_token);
    assert!((rows[&text_sha256("so bad")].confidence - 0.9).abs() < 1e-6);
}

#[tokio::test]
async fn hits_skip_the_backend_and_keep_order() {
    let db = setup_db();
    let inner = Arc::new(FakeClassifier::new("v1"));
    let (c, stats) = cached(&db, Arc::clone(&inner));
    c.classify_batch(&texts(&["fine", "so bad"])).await.unwrap();

    let out = c
        .classify_batch(&texts(&["so bad", "new text", "fine"]))
        .await
        .unwrap();

    assert_eq!(
        toxic_flags(&out),
        vec![Some(true), Some(false), Some(false)]
    );
    assert_eq!(inner.batches.lock().unwrap()[1], texts(&["new text"]));
    assert_eq!((stats.hits(), stats.misses()), (2, 3));
    let ItemOutcome::Verdict(hit) = &out[0] else {
        panic!("expected verdict")
    };
    assert_eq!(hit.latency_ms, 0);
    assert_eq!(hit.model_id, "fake-model");
    assert_eq!(hit.policy_version, "v1");
    assert!((hit.confidence - 0.9).abs() < 1e-6);
}

#[tokio::test]
async fn all_hits_never_calls_the_backend() {
    let db = setup_db();
    let inner = Arc::new(FakeClassifier::new("v1"));
    let (c, _) = cached(&db, Arc::clone(&inner));
    c.classify_batch(&texts(&["a", "b"])).await.unwrap();
    c.classify_batch(&texts(&["b", "a"])).await.unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn policy_bump_invalidates() {
    let db = setup_db();
    let (c1, _) = cached(&db, Arc::new(FakeClassifier::new("v1")));
    c1.classify_batch(&texts(&["fine"])).await.unwrap();

    let inner2 = Arc::new(FakeClassifier::new("v2"));
    let (c2, stats2) = cached(&db, Arc::clone(&inner2));
    c2.classify_batch(&texts(&["fine"])).await.unwrap();

    assert_eq!(inner2.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats2.hits(), stats2.misses()), (0, 1));
}

#[tokio::test]
async fn request_level_error_passes_through_and_caches_nothing() {
    let db = setup_db();
    let inner = Arc::new(FakeClassifier::new("v1"));
    let (c, stats) = cached(&db, Arc::clone(&inner));

    let err = c
        .classify_batch(&texts(&["fine", "boom"]))
        .await
        .unwrap_err();

    assert!(err.to_string().contains("request failed"));
    assert_eq!((stats.hits(), stats.misses()), (0, 2));
    assert!(db
        .get_classifier_verdicts("fake-model", "v1", &[text_sha256("fine")])
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn single_classify_uses_the_cache() {
    let db = setup_db();
    let inner = Arc::new(FakeClassifier::new("v1"));
    let (c, stats) = cached(&db, Arc::clone(&inner));

    let a = c.classify("so bad").await.unwrap();
    let b = c.classify("so bad").await.unwrap();

    assert_eq!((a.toxic_token, b.toxic_token), (true, true));
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats.hits(), stats.misses()), (1, 1));
}

#[tokio::test]
async fn metadata_forwards_to_the_inner_classifier() {
    let db = setup_db();
    let (c, _) = cached(&db, Arc::new(FakeClassifier::new("v1")));
    assert_eq!(c.max_batch_size(), 16);
    assert_eq!(c.name(), "fake");
    assert_eq!(c.model_id(), "fake-model");
    assert_eq!(c.policy_version(), "v1");
    assert_eq!(c.threshold(), 0.5);
    assert!(c.classify_batch(&[]).await.unwrap().is_empty());
}
