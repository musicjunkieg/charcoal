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
    /// Every endpoint contact in order — `"probe"` or `"classify"` — so a test
    /// can assert not just how many round trips happened but which came first.
    contacts: Mutex<Vec<&'static str>>,
    /// When set, `probe_identity` refuses, as a mismatched policy does.
    probe_fails: bool,
}

impl FakeClassifier {
    fn new(policy: &'static str) -> Self {
        Self {
            policy,
            batches: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            contacts: Mutex::new(Vec::new()),
            probe_fails: false,
        }
    }

    fn probes(&self) -> usize {
        self.contacts
            .lock()
            .unwrap()
            .iter()
            .filter(|c| **c == "probe")
            .count()
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
        self.contacts.lock().unwrap().push("classify");
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

    async fn probe_identity(&self) -> anyhow::Result<()> {
        self.contacts.lock().unwrap().push("probe");
        if self.probe_fails {
            anyhow::bail!("endpoint serves another policy");
        }
        Ok(())
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

/// Advertises `expected` as its identity but stamps every verdict it returns
/// with `reported` — an endpoint whose policy changed after the scan-start
/// probe (or whose probe came back as an error slot and so proved nothing).
struct DriftingClassifier {
    expected: &'static str,
    reported: &'static str,
    calls: AtomicUsize,
}

#[async_trait]
impl ToxicityClassifier for DriftingClassifier {
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
        Ok(contents
            .iter()
            .map(|_| {
                ItemOutcome::Verdict(ClassifierVerdict {
                    toxic_token: true,
                    confidence: 0.9,
                    latency_ms: 42,
                    model_id: self.model_id().to_string(),
                    // The provenance the ENDPOINT reported, not the one we asked for.
                    policy_version: self.reported.to_string(),
                })
            })
            .collect())
    }

    fn max_batch_size(&self) -> usize {
        16
    }
    fn name(&self) -> &'static str {
        "drifting"
    }
    fn model_id(&self) -> &'static str {
        "fake-model"
    }
    fn policy_version(&self) -> &'static str {
        self.expected
    }
    fn threshold(&self) -> f32 {
        0.5
    }
}

/// #344 Codex review P1: a verdict whose reported identity is not the one this
/// classifier advertises must never be cached under the advertised identity.
///
/// Before the fix, the write path keyed every fresh verdict on the ADVERTISED
/// model/policy and a hit was rebuilt with the advertised identity too. So the
/// first finalize correctly rejected the foreign verdict, the bounded re-gather
/// hit the cache, and the replayed verdict now claimed to be current — a score
/// produced by the wrong policy would publish. Both halves are asserted: the
/// foreign verdict is not persisted, and a second classification of the same
/// text goes back to the endpoint and still reports the foreign policy.
#[tokio::test]
async fn a_foreign_verdict_is_never_cached_under_the_advertised_identity() {
    let db = setup_db();
    let inner = Arc::new(DriftingClassifier {
        expected: "policy-expected",
        reported: "policy-foreign",
        calls: AtomicUsize::new(0),
    });
    let stats = Arc::new(CacheStats::default());
    let c = CachedClassifier::new(
        Arc::clone(&inner) as Arc<dyn ToxicityClassifier>,
        Arc::clone(&db),
        Arc::clone(&stats),
    );
    let batch = texts(&["so bad"]);

    let first = c.classify_batch(&batch).await.unwrap();
    let ItemOutcome::Verdict(v) = &first[0] else {
        panic!("expected a verdict, got {first:?}");
    };
    assert_eq!(
        v.policy_version, "policy-foreign",
        "a fresh verdict keeps the provenance the endpoint reported"
    );

    // Nothing may be persisted under the advertised identity.
    let stored = db
        .get_classifier_verdicts("fake-model", "policy-expected", &[text_sha256("so bad")])
        .await
        .unwrap();
    assert!(
        stored.is_empty(),
        "a foreign verdict must not be cached under the advertised policy"
    );

    // The re-gather case: the same text again must NOT come from the cache.
    let second = c.classify_batch(&batch).await.unwrap();
    let ItemOutcome::Verdict(v2) = &second[0] else {
        panic!("expected a verdict, got {second:?}");
    };
    assert_eq!(
        inner.calls.load(Ordering::SeqCst),
        2,
        "a foreign verdict must be re-requested, not replayed from the cache"
    );
    assert_eq!(
        v2.policy_version, "policy-foreign",
        "a replay must never relabel a foreign verdict as the advertised policy"
    );
}

// ── #394 C: the identity probe is paid lazily, once per run ─────────────────
//
// The probe is a real round trip to the GPU endpoint, and on RunPod serverless
// every round trip bills a worker's idle tail. #394: a refresh whose candidates
// were all deleted accounts paid that probe hourly while classifying nothing.
// So the probe now happens only when a verdict must actually be computed — the
// first cache MISS — and at most once per classifier instance (one per run).

/// A run whose every verdict is already cached never contacts the endpoint —
/// not to classify, and not to probe either.
#[tokio::test]
async fn an_all_hit_run_never_probes_the_endpoint() {
    let db = setup_db();
    // Prime the cache with an earlier run.
    let (earlier, _) = cached(&db, Arc::new(FakeClassifier::new("v1")));
    earlier.classify_batch(&texts(&["a", "b"])).await.unwrap();

    let inner = Arc::new(FakeClassifier::new("v1"));
    let (c, _) = cached(&db, Arc::clone(&inner));
    c.classify_batch(&texts(&["b", "a"])).await.unwrap();

    assert_eq!(
        *inner.contacts.lock().unwrap(),
        Vec::<&str>::new(),
        "an all-hit run must not wake the endpoint at all"
    );
}

/// The first miss probes, BEFORE it classifies; later misses do not re-probe.
#[tokio::test]
async fn the_first_miss_probes_once_before_classifying() {
    let db = setup_db();
    let inner = Arc::new(FakeClassifier::new("v1"));
    let (c, _) = cached(&db, Arc::clone(&inner));

    c.classify_batch(&texts(&["one"])).await.unwrap();
    c.classify_batch(&texts(&["two"])).await.unwrap();

    assert_eq!(
        *inner.contacts.lock().unwrap(),
        vec!["probe", "classify", "classify"]
    );
}

/// An explicit probe (the full scan's fail-fast check at start) counts as the
/// run's probe: the first miss afterwards does not pay a second one.
#[tokio::test]
async fn an_explicit_probe_is_not_repeated_by_the_first_miss() {
    let db = setup_db();
    let inner = Arc::new(FakeClassifier::new("v1"));
    let (c, _) = cached(&db, Arc::clone(&inner));

    c.probe_identity().await.unwrap();
    c.classify_batch(&texts(&["one"])).await.unwrap();

    assert_eq!(*inner.contacts.lock().unwrap(), vec!["probe", "classify"]);
}

/// A refused probe fails the batch without classifying anything, and is NOT
/// remembered: the next attempt asks the endpoint again rather than replaying
/// a stale refusal (or, worse, a stale success).
#[tokio::test]
async fn a_refused_probe_fails_the_batch_and_is_not_memoised() {
    let db = setup_db();
    let mut fake = FakeClassifier::new("v1");
    fake.probe_fails = true;
    let inner = Arc::new(fake);
    let (c, _) = cached(&db, Arc::clone(&inner));

    let err = c.classify_batch(&texts(&["one"])).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("another policy"),
        "the refusal must surface as the batch's error, got {err:#}"
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0, "never classified");
    assert!(db
        .get_classifier_verdicts("fake-model", "v1", &[text_sha256("one")])
        .await
        .unwrap()
        .is_empty());

    c.classify_batch(&texts(&["one"])).await.unwrap_err();
    assert_eq!(inner.probes(), 2, "a failed probe is retried, not cached");
}
