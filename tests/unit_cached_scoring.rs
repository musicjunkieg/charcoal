//! CachedToxicityScorer (#343 §4.1): score each distinct text once per model.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use charcoal::observability::cache_stats::CacheStats;
use charcoal::toxicity::cached::{text_sha256, CachedToxicityScorer};
use charcoal::toxicity::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};
use rusqlite::Connection;

fn setup_db() -> Arc<dyn Database> {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

/// Scores `text.len() / 100` so results are distinguishable, records every
/// batch it is asked to score, and sets an attribute so cached-vs-fresh
/// results are distinguishable too.
#[derive(Default)]
struct CountingScorer {
    batches: Mutex<Vec<Vec<String>>>,
    calls: AtomicUsize,
}

#[async_trait]
impl ToxicityScorer for CountingScorer {
    async fn score_text(&self, text: &str) -> anyhow::Result<ToxicityResult> {
        Ok(self
            .score_batch(std::slice::from_ref(&text.to_string()))
            .await?
            .remove(0))
    }

    async fn score_batch(&self, texts: &[String]) -> anyhow::Result<Vec<ToxicityResult>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.batches.lock().unwrap().push(texts.to_vec());
        Ok(texts
            .iter()
            .map(|t| ToxicityResult {
                toxicity: t.len() as f64 / 100.0,
                attributes: ToxicityAttributes {
                    insult: Some(0.5),
                    ..Default::default()
                },
            })
            .collect())
    }
}

fn cached(
    db: &Arc<dyn Database>,
    inner: Arc<CountingScorer>,
    model: &'static str,
) -> (CachedToxicityScorer, Arc<CacheStats>) {
    let stats = Arc::new(CacheStats::default());
    let scorer =
        CachedToxicityScorer::new(Box::new(inner), Arc::clone(db), model, Arc::clone(&stats));
    (scorer, stats)
}

fn texts(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn text_sha256_is_lowercase_hex_of_the_utf8_bytes() {
    assert_eq!(
        text_sha256("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_ne!(text_sha256("abc"), text_sha256("abc "));
}

#[tokio::test]
async fn first_batch_scores_everything_and_persists() {
    let db = setup_db();
    let inner = Arc::new(CountingScorer::default());
    let (scorer, stats) = cached(&db, Arc::clone(&inner), "m");

    let out = scorer
        .score_batch(&texts(&["a", "bb", "ccc"]))
        .await
        .unwrap();

    assert_eq!(
        out.iter().map(|r| r.toxicity).collect::<Vec<_>>(),
        vec![0.01, 0.02, 0.03]
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats.hits(), stats.misses()), (0, 3));
    let rows = db
        .get_onnx_scores(
            "m",
            &[text_sha256("a"), text_sha256("bb"), text_sha256("ccc")],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[&text_sha256("bb")], 0.02);
}

#[tokio::test]
async fn second_batch_scores_only_misses_and_keeps_order() {
    let db = setup_db();
    let inner = Arc::new(CountingScorer::default());
    let (scorer, stats) = cached(&db, Arc::clone(&inner), "m");
    scorer
        .score_batch(&texts(&["a", "bb", "ccc"]))
        .await
        .unwrap();

    let out = scorer
        .score_batch(&texts(&["ccc", "dddd", "a"]))
        .await
        .unwrap();

    assert_eq!(
        out.iter().map(|r| r.toxicity).collect::<Vec<_>>(),
        vec![0.03, 0.04, 0.01]
    );
    // Only "dddd" went to the model.
    assert_eq!(inner.batches.lock().unwrap()[1], texts(&["dddd"]));
    assert_eq!((stats.hits(), stats.misses()), (2, 4));
    // Hits carry no attribute breakdown (only the score is cached); the
    // fresh one keeps the model's attributes.
    assert_eq!(out[0].attributes.insult, None);
    assert_eq!(out[1].attributes.insult, Some(0.5));
}

#[tokio::test]
async fn all_hits_never_calls_the_model() {
    let db = setup_db();
    let inner = Arc::new(CountingScorer::default());
    let (scorer, stats) = cached(&db, Arc::clone(&inner), "m");
    scorer.score_batch(&texts(&["a", "bb"])).await.unwrap();

    let out = scorer.score_batch(&texts(&["bb", "a"])).await.unwrap();

    assert_eq!(
        out.iter().map(|r| r.toxicity).collect::<Vec<_>>(),
        vec![0.02, 0.01]
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats.hits(), stats.misses()), (2, 2));
}

#[tokio::test]
async fn duplicate_texts_in_one_batch_are_scored_once() {
    let db = setup_db();
    let inner = Arc::new(CountingScorer::default());
    let (scorer, stats) = cached(&db, Arc::clone(&inner), "m");

    let out = scorer
        .score_batch(&texts(&["a", "a", "bb", "a"]))
        .await
        .unwrap();

    assert_eq!(
        out.iter().map(|r| r.toxicity).collect::<Vec<_>>(),
        vec![0.01, 0.01, 0.02, 0.01]
    );
    assert_eq!(inner.batches.lock().unwrap()[0], texts(&["a", "bb"]));
    assert_eq!((stats.hits(), stats.misses()), (0, 4));
}

#[tokio::test]
async fn score_text_uses_the_cache() {
    let db = setup_db();
    let inner = Arc::new(CountingScorer::default());
    let (scorer, stats) = cached(&db, Arc::clone(&inner), "m");

    let first = scorer.score_text("hello").await.unwrap();
    let second = scorer.score_text("hello").await.unwrap();

    assert_eq!(first.toxicity, second.toxicity);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats.hits(), stats.misses()), (1, 1));
}

#[tokio::test]
async fn empty_batch_is_a_no_op() {
    let db = setup_db();
    let inner = Arc::new(CountingScorer::default());
    let (scorer, stats) = cached(&db, Arc::clone(&inner), "m");
    assert!(scorer.score_batch(&[]).await.unwrap().is_empty());
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    assert_eq!((stats.hits(), stats.misses()), (0, 0));
}

#[tokio::test]
async fn cache_is_scoped_by_model_id() {
    let db = setup_db();
    let inner = Arc::new(CountingScorer::default());
    let (scorer_a, _) = cached(&db, Arc::clone(&inner), "model-a");
    let (scorer_b, stats_b) = cached(&db, Arc::clone(&inner), "model-b");

    scorer_a.score_batch(&texts(&["a"])).await.unwrap();
    scorer_b.score_batch(&texts(&["a"])).await.unwrap();

    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    assert_eq!((stats_b.hits(), stats_b.misses()), (0, 1));
}
