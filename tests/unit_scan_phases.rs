use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use charcoal::pipeline::scan_phases::staging::{
    AccountInput, QueueRow, ScanPhase, VerdictRow, ACCOUNT_INPUT_SCHEMA_VERSION,
};
use rusqlite::Connection;

// ── helpers ───────────────────────────────────────────────────────────────────

const TEST_USER: &str = "did:plc:testuser000000000000";

async fn setup_db() -> SqliteDatabase {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    SqliteDatabase::new(conn)
}

fn make_queue_row(account_did: &str, post_uri: &str, status: &str) -> QueueRow {
    QueueRow {
        account_did: account_did.to_string(),
        post_uri: post_uri.to_string(),
        text: "hello world".to_string(),
        context_text: None,
        post_kind: "original".to_string(),
        onnx_score: 0.05,
        status: status.to_string(),
        toxic_token: None,
        confidence: None,
        model_id: None,
        policy_version: None,
    }
}

// ── Database trait staging tests ──────────────────────────────────────────────

#[tokio::test]
async fn staging_enqueue_then_fetch_pending_honors_status() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    let pending = make_queue_row("did:plc:acct1", "at://did:plc:acct1/post/1", "pending");
    let done = make_queue_row("did:plc:acct1", "at://did:plc:acct1/post/2", "done");

    db.enqueue_classifications(TEST_USER, &[pending.clone(), done.clone()])
        .await
        .unwrap();

    let fetched = db
        .fetch_pending_classifications(TEST_USER, 100)
        .await
        .unwrap();
    assert_eq!(fetched.len(), 1, "only pending rows should be returned");
    assert_eq!(fetched[0].post_uri, "at://did:plc:acct1/post/1");
    assert_eq!(fetched[0].status, "pending");
}

#[tokio::test]
async fn staging_record_verdicts_flips_pending_to_done() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    let row = make_queue_row("did:plc:acct2", "at://did:plc:acct2/post/1", "pending");
    db.enqueue_classifications(TEST_USER, &[row]).await.unwrap();

    let verdict = VerdictRow {
        account_did: "did:plc:acct2".to_string(),
        post_uri: "at://did:plc:acct2/post/1".to_string(),
        toxic_token: true,
        confidence: 0.87,
        model_id: "cope-b-v1".to_string(),
        policy_version: "p1".to_string(),
    };
    db.record_classification_verdicts(TEST_USER, &[verdict])
        .await
        .unwrap();

    // Should no longer be pending
    let pending = db
        .fetch_pending_classifications(TEST_USER, 100)
        .await
        .unwrap();
    assert!(pending.is_empty(), "row should be flipped to done");

    // Verify via fetch_account_verdicts
    let all = db
        .fetch_account_verdicts(TEST_USER, "did:plc:acct2")
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].status, "done");
    assert_eq!(all[0].toxic_token, Some(true));
    assert!((all[0].confidence.unwrap() - 0.87).abs() < 1e-5);
    assert_eq!(all[0].model_id.as_deref(), Some("cope-b-v1"));
    assert_eq!(all[0].policy_version.as_deref(), Some("p1"));
}

#[tokio::test]
async fn staging_enqueue_upsert_same_pk_yields_one_row() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    let row = make_queue_row("did:plc:acct3", "at://did:plc:acct3/post/1", "pending");
    // Enqueue the same PK twice
    db.enqueue_classifications(TEST_USER, std::slice::from_ref(&row))
        .await
        .unwrap();
    db.enqueue_classifications(TEST_USER, &[row]).await.unwrap();

    let all = db
        .fetch_account_verdicts(TEST_USER, "did:plc:acct3")
        .await
        .unwrap();
    assert_eq!(
        all.len(),
        1,
        "UPSERT: same PK twice must yield exactly one row"
    );
}

#[tokio::test]
async fn staging_stash_and_fetch_account_input_roundtrip() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    // Nothing stashed yet
    let missing = db
        .fetch_account_input(TEST_USER, "did:plc:acct4")
        .await
        .unwrap();
    assert!(missing.is_none());

    let payload = r#"{"schema_version":1,"fingerprint_quality":"normal"}"#;
    db.stash_account_input(TEST_USER, "did:plc:acct4", payload)
        .await
        .unwrap();

    let fetched = db
        .fetch_account_input(TEST_USER, "did:plc:acct4")
        .await
        .unwrap();
    assert_eq!(fetched.as_deref(), Some(payload));

    // Re-stash replaces the blob
    let payload2 = r#"{"schema_version":1,"fingerprint_quality":"degraded"}"#;
    db.stash_account_input(TEST_USER, "did:plc:acct4", payload2)
        .await
        .unwrap();
    let fetched2 = db
        .fetch_account_input(TEST_USER, "did:plc:acct4")
        .await
        .unwrap();
    assert_eq!(fetched2.as_deref(), Some(payload2));
}

#[tokio::test]
async fn staging_count_pending_classifications() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    assert_eq!(
        db.count_pending_classifications(TEST_USER).await.unwrap(),
        0
    );

    let rows = vec![
        make_queue_row("did:plc:acct5", "at://did:plc:acct5/post/1", "pending"),
        make_queue_row("did:plc:acct5", "at://did:plc:acct5/post/2", "pending"),
        make_queue_row("did:plc:acct5", "at://did:plc:acct5/post/3", "done"),
    ];
    db.enqueue_classifications(TEST_USER, &rows).await.unwrap();

    assert_eq!(
        db.count_pending_classifications(TEST_USER).await.unwrap(),
        2
    );
}

#[tokio::test]
async fn staging_clear_scan_staging_empties_both_tables() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    let row = make_queue_row("did:plc:acct6", "at://did:plc:acct6/post/1", "pending");
    db.enqueue_classifications(TEST_USER, &[row]).await.unwrap();
    db.stash_account_input(TEST_USER, "did:plc:acct6", r#"{"schema_version":1}"#)
        .await
        .unwrap();

    // Set a scan_state marker that clear must NOT touch
    db.set_scan_state(TEST_USER, "scan_phase", "burst")
        .await
        .unwrap();

    db.clear_scan_staging(TEST_USER).await.unwrap();

    // Both tables should be empty for this user
    assert_eq!(
        db.count_pending_classifications(TEST_USER).await.unwrap(),
        0
    );
    let all = db
        .fetch_account_verdicts(TEST_USER, "did:plc:acct6")
        .await
        .unwrap();
    assert!(all.is_empty(), "classification_queue should be empty");

    let input = db
        .fetch_account_input(TEST_USER, "did:plc:acct6")
        .await
        .unwrap();
    assert!(input.is_none(), "scan_account_input should be empty");

    // scan_state must survive clear_scan_staging
    assert_eq!(
        db.get_scan_state(TEST_USER, "scan_phase").await.unwrap(),
        Some("burst".to_string()),
        "clear_scan_staging must not touch scan_state"
    );
}

#[tokio::test]
async fn staging_list_scan_accounts_returns_distinct_dids() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    let rows = vec![
        make_queue_row("did:plc:acct7", "at://did:plc:acct7/post/1", "pending"),
        make_queue_row("did:plc:acct7", "at://did:plc:acct7/post/2", "pending"),
        make_queue_row("did:plc:acct8", "at://did:plc:acct8/post/1", "pending"),
    ];
    db.enqueue_classifications(TEST_USER, &rows).await.unwrap();

    let mut accounts = db.list_scan_accounts(TEST_USER).await.unwrap();
    accounts.sort();
    assert_eq!(
        accounts,
        vec!["did:plc:acct7".to_string(), "did:plc:acct8".to_string()],
        "list_scan_accounts must return distinct account_dids"
    );
}

#[tokio::test]
async fn staging_phase_marker_via_existing_scan_state_api() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    // Initially absent
    assert!(db
        .get_scan_state(TEST_USER, "scan_phase")
        .await
        .unwrap()
        .is_none());

    // Set via existing API
    db.set_scan_state(TEST_USER, "scan_phase", "burst")
        .await
        .unwrap();
    assert_eq!(
        db.get_scan_state(TEST_USER, "scan_phase").await.unwrap(),
        Some("burst".to_string())
    );

    // Advance phase
    db.set_scan_state(TEST_USER, "scan_phase", "finalize")
        .await
        .unwrap();
    assert_eq!(
        db.get_scan_state(TEST_USER, "scan_phase").await.unwrap(),
        Some("finalize".to_string())
    );
}

#[tokio::test]
async fn staging_reenqueue_does_not_clobber_done_row() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    // Phase A: enqueue a pending row
    let pending = make_queue_row("did:plc:acctX", "at://did:plc:acctX/post/1", "pending");
    db.enqueue_classifications(TEST_USER, &[pending])
        .await
        .unwrap();

    // Phase B: record a verdict — flips the row to done with a real verdict
    let verdict = VerdictRow {
        account_did: "did:plc:acctX".to_string(),
        post_uri: "at://did:plc:acctX/post/1".to_string(),
        toxic_token: true,
        confidence: 0.9,
        model_id: "m".to_string(),
        policy_version: "p".to_string(),
    };
    db.record_classification_verdicts(TEST_USER, &[verdict])
        .await
        .unwrap();

    // Phase A re-runs (e.g. after a crash/retry): enqueue the same post_uri as fresh pending
    let re_enqueue = make_queue_row("did:plc:acctX", "at://did:plc:acctX/post/1", "pending");
    db.enqueue_classifications(TEST_USER, &[re_enqueue])
        .await
        .unwrap();

    // The row must still be done with the original verdict intact
    let all = db
        .fetch_account_verdicts(TEST_USER, "did:plc:acctX")
        .await
        .unwrap();
    assert_eq!(all.len(), 1, "re-enqueue must not create a duplicate row");
    assert_eq!(
        all[0].status, "done",
        "re-enqueue must not reset status to pending"
    );
    assert_eq!(
        all[0].toxic_token,
        Some(true),
        "re-enqueue must not clear toxic_token"
    );
    assert!(
        (all[0].confidence.unwrap() - 0.9).abs() < 1e-5,
        "re-enqueue must not clear confidence"
    );
    assert_eq!(
        all[0].model_id.as_deref(),
        Some("m"),
        "re-enqueue must not clear model_id"
    );
    assert_eq!(
        all[0].policy_version.as_deref(),
        Some("p"),
        "re-enqueue must not clear policy_version"
    );
}

// --- Schema v9 migration tests ---

fn setup_v9_db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    conn
}

#[test]
fn schema_v9_creates_classification_queue_and_scan_account_input() {
    let conn = setup_v9_db();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' \
             AND name IN ('classification_queue','scan_account_input')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 2,
        "both classification_queue and scan_account_input tables should exist after v9 migration"
    );
}

#[test]
fn schema_v9_classification_queue_has_expected_columns() {
    let conn = setup_v9_db();
    // Insert a row using all required columns — compile-time DDL check
    conn.execute(
        "INSERT INTO classification_queue \
         (user_did, account_did, post_uri, text, context_text, post_kind, onnx_score, status) \
         VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7)",
        rusqlite::params![
            "did:plc:user1",
            "did:plc:acct1",
            "at://did:plc:acct1/app.bsky.feed.post/abc",
            "hello world",
            "original",
            0.05_f64,
            "pending",
        ],
    )
    .unwrap();

    let status: String = conn
        .query_row(
            "SELECT status FROM classification_queue \
             WHERE user_did='did:plc:user1' AND account_did='did:plc:acct1' \
             AND post_uri='at://did:plc:acct1/app.bsky.feed.post/abc'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "pending");
}

#[test]
fn schema_v9_classification_queue_verdict_columns_roundtrip() {
    let conn = setup_v9_db();
    // Insert a row with nullable verdict columns left NULL
    conn.execute(
        "INSERT INTO classification_queue \
         (user_did, account_did, post_uri, text, context_text, post_kind, onnx_score, status) \
         VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7)",
        rusqlite::params![
            "did:plc:user2",
            "did:plc:acct2",
            "at://did:plc:acct2/app.bsky.feed.post/def",
            "test post",
            "original",
            0.10_f64,
            "pending",
        ],
    )
    .unwrap();

    // Update the row with verdict values
    conn.execute(
        "UPDATE classification_queue SET toxic_token=?1, confidence=?2, \
         model_id=?3, policy_version=?4 \
         WHERE user_did=?5 AND account_did=?6 AND post_uri=?7",
        rusqlite::params![
            1i64,     // toxic_token
            0.87_f64, // confidence
            "m1",     // model_id
            "p1",     // policy_version
            "did:plc:user2",
            "did:plc:acct2",
            "at://did:plc:acct2/app.bsky.feed.post/def",
        ],
    )
    .unwrap();

    // Select those four columns back and verify round-trip
    let (toxic_token, confidence, model_id, policy_version): (i64, f64, String, String) = conn
        .query_row(
            "SELECT toxic_token, confidence, model_id, policy_version FROM classification_queue \
             WHERE user_did=?1 AND account_did=?2 AND post_uri=?3",
            rusqlite::params![
                "did:plc:user2",
                "did:plc:acct2",
                "at://did:plc:acct2/app.bsky.feed.post/def",
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();

    assert_eq!(toxic_token, 1);
    assert!((confidence - 0.87_f64).abs() < 1e-6); // Float comparison with epsilon
    assert_eq!(model_id, "m1");
    assert_eq!(policy_version, "p1");
}

#[test]
fn schema_v9_scan_account_input_has_payload_json() {
    let conn = setup_v9_db();
    conn.execute(
        "INSERT INTO scan_account_input (user_did, account_did, payload_json) \
         VALUES (?1, ?2, ?3)",
        rusqlite::params!["did:plc:user1", "did:plc:acct1", r#"{"schema_version":1}"#,],
    )
    .unwrap();

    let payload: String = conn
        .query_row(
            "SELECT payload_json FROM scan_account_input \
             WHERE user_did='did:plc:user1' AND account_did='did:plc:acct1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(payload, r#"{"schema_version":1}"#);
}

#[test]
fn schema_v9_does_not_alter_scan_state() {
    let conn = setup_v9_db();
    // scan_state must still accept key/value rows — the phase marker is stored
    // as key='scan_phase', not as a column
    conn.execute(
        "INSERT INTO scan_state (user_did, key, value) VALUES (?1, ?2, ?3)",
        rusqlite::params!["did:plc:user1", "scan_phase", "gather"],
    )
    .unwrap();

    let val: String = conn
        .query_row(
            "SELECT value FROM scan_state WHERE user_did='did:plc:user1' AND key='scan_phase'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(val, "gather");
}

#[test]
fn scan_phase_roundtrips_through_str() {
    for p in [
        ScanPhase::Gather,
        ScanPhase::Burst,
        ScanPhase::Finalize,
        ScanPhase::Done,
    ] {
        assert_eq!(ScanPhase::from_value(p.as_str()), Some(p));
    }
    assert_eq!(ScanPhase::from_value("nonsense"), None);
}

#[test]
fn account_input_is_versioned_and_roundtrips() {
    let blob = AccountInput::new_for_test();
    let json = serde_json::to_string(&blob).unwrap();
    let back: AccountInput = serde_json::from_str(&json).unwrap();
    assert_eq!(back.schema_version, ACCOUNT_INPUT_SCHEMA_VERSION);
    assert_eq!(back, blob);
}

// ── Phase A: gather_account tests ───────────────────────────────────────────

mod gather_tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Arc;

    use charcoal::bluesky::posts::{Post, PostSample, ReplyPost};
    use charcoal::pipeline::scan_phases::gather::{
        gather_account, CleanPassScorer, GatherInputs, PostFetcher,
    };
    use charcoal::scoring::threat::ThreatWeights;
    use charcoal::topics::fingerprint::{TopicCluster, TopicFingerprint};
    use charcoal::toxicity::traits::{ToxicityResult, ToxicityScorer};

    const ACCT: &str = "did:plc:gatheracct00000000000";

    // Scorer returning a fixed continuous toxicity for every text. Used both as
    // the Stage-1 ONNX scorer and (via FixedCleanPass) the clean-pass.
    struct FixedScorer(f64);

    #[async_trait]
    impl ToxicityScorer for FixedScorer {
        async fn score_text(&self, _text: &str) -> Result<ToxicityResult> {
            Ok(ToxicityResult {
                toxicity: self.0,
                attributes: Default::default(),
            })
        }
    }

    // Clean-pass that returns the same fixed ONNX score for every envelope.
    struct FixedCleanPass(f64);

    #[async_trait]
    impl CleanPassScorer for FixedCleanPass {
        async fn onnx_clean_pass(&self, texts: &[String]) -> Result<Vec<f64>> {
            Ok(vec![self.0; texts.len()])
        }
    }

    // Clean-pass keyed on substring: any envelope containing `hostile_marker`
    // scores high (survivor); everything else scores clean. Proves the split
    // ran on the ENVELOPE text, not the raw reply text.
    struct MarkerCleanPass {
        hostile_marker: String,
        high: f64,
        low: f64,
    }

    #[async_trait]
    impl CleanPassScorer for MarkerCleanPass {
        async fn onnx_clean_pass(&self, texts: &[String]) -> Result<Vec<f64>> {
            Ok(texts
                .iter()
                .map(|t| {
                    if t.contains(&self.hostile_marker) {
                        self.high
                    } else {
                        self.low
                    }
                })
                .collect())
        }
    }

    // Canned fetcher: returns the same sample for the 25 and 50-post calls,
    // plus a fixed parent-text map.
    struct CannedFetcher {
        sample: PostSample,
        parents: HashMap<String, String>,
    }

    #[async_trait]
    impl PostFetcher for CannedFetcher {
        async fn fetch_sample(&self, _handle: &str, _limit: usize) -> Result<PostSample> {
            // Returns the same canned sample regardless of `limit`. The Stage-1
            // (25-post) vs Stage-2 (50-post) sample-size distinction is
            // intentionally not exercised by these unit tests; the PostFetcher
            // seam exists to inject deterministic canned data.
            Ok(self.sample.clone())
        }
        async fn fetch_parents(&self, _uris: &[String]) -> Result<HashMap<String, String>> {
            Ok(self.parents.clone())
        }
    }

    fn make_post(uri: &str, text: &str) -> Post {
        Post {
            uri: uri.to_string(),
            text: text.to_string(),
            created_at: None,
            like_count: 0,
            repost_count: 0,
            quote_count: 0,
            is_quote: false,
        }
    }

    fn make_reply(uri: &str, text: &str, parent_uri: &str) -> ReplyPost {
        ReplyPost {
            post: make_post(uri, text),
            parent_uri: parent_uri.to_string(),
        }
    }

    // Fingerprint about astrophysics — unrelated to everyday-topic posts, so
    // TF-IDF overlap stays below the 0.15 gate (drives the early-exit path).
    fn astrophysics_fingerprint() -> TopicFingerprint {
        TopicFingerprint {
            clusters: vec![TopicCluster {
                label: "astrophysics".to_string(),
                keywords: vec![
                    "quasar".to_string(),
                    "nebula".to_string(),
                    "redshift".to_string(),
                    "telescope".to_string(),
                    "pulsar".to_string(),
                    "photon".to_string(),
                    "galaxy".to_string(),
                    "cosmology".to_string(),
                ],
                weight: 1.0,
            }],
            post_count: 200,
        }
    }

    fn inputs<'a>(fp: &'a TopicFingerprint, weights: &'a ThreatWeights) -> GatherInputs<'a> {
        GatherInputs {
            account_did: ACCT,
            account_handle: "gather.bsky.social",
            protected_fingerprint: fp,
            weights,
            median_engagement: 1.0,
            is_pile_on: false,
            direct_pairs: None,
            graph_distance: None,
        }
    }

    async fn open_db() -> Arc<dyn Database> {
        let db = setup_db().await;
        db.upsert_user(TEST_USER, "testuser.bsky.social")
            .await
            .unwrap();
        Arc::new(db)
    }

    // ── < 5 posts → Insufficient Data, no enqueue/stash ──
    #[tokio::test]
    async fn gather_insufficient_data_finalizes_and_stages_nothing() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();

        let sample = PostSample {
            originals: vec![make_post("at://a/1", "hi"), make_post("at://a/2", "yo")],
            replies: vec![],
            quotes: vec![],
            reply_ratio: 0.0,
            quote_ratio: 0.0,
            total_posts: 2,
        };
        let fetcher = CannedFetcher {
            sample,
            parents: HashMap::new(),
        };
        let scorer = FixedScorer(0.0);
        let clean = FixedCleanPass(0.0);

        gather_account(
            &db,
            TEST_USER,
            &fetcher,
            &scorer,
            &clean,
            &inputs(&fp, &weights),
        )
        .await
        .unwrap();

        // The terminal "Insufficient Data" score is written. (The read path
        // recomputes threat_tier from threat_score, which is None here, so we
        // assert on the durable signals instead: the row exists, no score, and
        // the analysed-post count matches the < 5 sample.)
        let score = db
            .get_account_by_did(TEST_USER, ACCT)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            score.threat_score, None,
            "insufficient data → no threat score"
        );
        assert_eq!(score.posts_analyzed, 2);
        assert!(db
            .fetch_account_verdicts(TEST_USER, ACCT)
            .await
            .unwrap()
            .is_empty());
        assert!(db
            .fetch_account_input(TEST_USER, ACCT)
            .await
            .unwrap()
            .is_none());
    }

    // ── Early-exit (clean + topically irrelevant, >=5 first-person) → Low ──
    #[tokio::test]
    async fn gather_early_exit_finalizes_low_and_stages_nothing() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();

        let originals: Vec<Post> = (0..6)
            .map(|i| make_post(&format!("at://e/{i}"), "sandwiches and gardens and weather"))
            .collect();
        let sample = PostSample {
            originals,
            replies: vec![],
            quotes: vec![],
            reply_ratio: 0.0,
            quote_ratio: 0.0,
            total_posts: 6,
        };
        let fetcher = CannedFetcher {
            sample,
            parents: HashMap::new(),
        };
        let scorer = FixedScorer(0.0); // ONNX clean
        let clean = FixedCleanPass(0.0);

        gather_account(
            &db,
            TEST_USER,
            &fetcher,
            &scorer,
            &clean,
            &inputs(&fp, &weights),
        )
        .await
        .unwrap();

        let score = db
            .get_account_by_did(TEST_USER, ACCT)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(score.threat_tier.as_deref(), Some("Low"));
        assert_eq!(score.scoring_confidence.as_deref(), Some("low"));
        assert!(db
            .fetch_account_verdicts(TEST_USER, ACCT)
            .await
            .unwrap()
            .is_empty());
        assert!(db
            .fetch_account_input(TEST_USER, ACCT)
            .await
            .unwrap()
            .is_none());
    }

    // Build a survivor sample: enough first-person posts AND high topic overlap
    // so Stage 1 proceeds to Stage 2.
    fn survivor_sample() -> PostSample {
        // 6 originals with astrophysics keywords → high overlap, so NOT
        // early-exited even though ONNX is clean.
        let originals: Vec<Post> = (0..6)
            .map(|i| {
                make_post(
                    &format!("at://s/orig/{i}"),
                    "quasar nebula redshift telescope galaxy cosmology pulsar photon",
                )
            })
            .collect();
        PostSample {
            originals,
            replies: vec![],
            quotes: vec![],
            reply_ratio: 0.0,
            quote_ratio: 0.0,
            total_posts: 6,
        }
    }

    // ── Survivor → per-post rows + one stash, no AccountScore ──
    #[tokio::test]
    async fn gather_survivor_enqueues_rows_and_stashes_blob() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();

        let mut sample = survivor_sample();
        // Add one extra original that the clean-pass marks as a survivor.
        sample.originals.push(make_post(
            "at://s/orig/hostile",
            "quasar HOSTILE_TOKEN nebula",
        ));
        sample.total_posts = sample.originals.len();

        let fetcher = CannedFetcher {
            sample,
            parents: HashMap::new(),
        };
        let scorer = FixedScorer(0.0); // Stage-1 ONNX clean (won't early-exit: overlap high)
        let clean = MarkerCleanPass {
            hostile_marker: "HOSTILE_TOKEN".to_string(),
            high: 0.9,
            low: 0.0,
        };

        gather_account(
            &db,
            TEST_USER,
            &fetcher,
            &scorer,
            &clean,
            &inputs(&fp, &weights),
        )
        .await
        .unwrap();

        // No AccountScore — Phase C scores survivors.
        assert!(db
            .get_account_by_did(TEST_USER, ACCT)
            .await
            .unwrap()
            .is_none());

        let rows = db.fetch_account_verdicts(TEST_USER, ACCT).await.unwrap();
        assert_eq!(rows.len(), 7, "one row per post (6 clean + 1 survivor)");

        let hostile = rows
            .iter()
            .find(|r| r.post_uri == "at://s/orig/hostile")
            .unwrap();
        assert_eq!(hostile.status, "pending");
        assert_eq!(hostile.toxic_token, None);

        let clean_row = rows.iter().find(|r| r.post_uri == "at://s/orig/0").unwrap();
        assert_eq!(clean_row.status, "done");
        assert_eq!(clean_row.toxic_token, Some(false));
        assert_eq!(clean_row.confidence, None);
        assert_eq!(clean_row.model_id, None);

        // Blob stashed once and round-trips with schema_version set.
        let payload = db
            .fetch_account_input(TEST_USER, ACCT)
            .await
            .unwrap()
            .unwrap();
        let blob: AccountInput = serde_json::from_str(&payload).unwrap();
        assert_eq!(blob.schema_version, ACCOUNT_INPUT_SCHEMA_VERSION);
        assert_eq!(blob.sample.total_posts, 7);
    }

    // ── Envelope-aware split: reply clean in isolation, hostile in context ──
    #[tokio::test]
    async fn gather_envelope_aware_split_uses_parent_context() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();

        // Survivor sample (6 high-overlap originals to clear Stage 1) plus one
        // reply that is innocuous on its own ("agreed") but whose PARENT text
        // carries the hostile marker. The clean-pass keys on the marker, so it
        // only fires when scoring the [Parent]/[Reply] envelope.
        let mut sample = survivor_sample();
        sample
            .replies
            .push(make_reply("at://s/reply/1", "agreed", "at://parent/1"));
        sample.total_posts = sample.originals.len() + sample.replies.len();

        let mut parents = HashMap::new();
        parents.insert(
            "at://parent/1".to_string(),
            "this is HOSTILE_TOKEN garbage".to_string(),
        );

        let fetcher = CannedFetcher { sample, parents };
        let scorer = FixedScorer(0.0);
        let clean = MarkerCleanPass {
            hostile_marker: "HOSTILE_TOKEN".to_string(),
            high: 0.9,
            low: 0.0,
        };

        gather_account(
            &db,
            TEST_USER,
            &fetcher,
            &scorer,
            &clean,
            &inputs(&fp, &weights),
        )
        .await
        .unwrap();

        let rows = db.fetch_account_verdicts(TEST_USER, ACCT).await.unwrap();
        let reply_row = rows
            .iter()
            .find(|r| r.post_uri == "at://s/reply/1")
            .unwrap();
        // The reply text alone ("agreed") has no marker, but the envelope
        // (which includes the parent) does — so it must survive to Phase B.
        assert_eq!(
            reply_row.status, "pending",
            "reply hostile-in-context must enqueue pending, not done"
        );
        assert_eq!(reply_row.text, "agreed", "raw reply text, not the envelope");
        assert_eq!(
            reply_row.context_text.as_deref(),
            Some("this is HOSTILE_TOKEN garbage"),
            "context_text carries the parent text"
        );
        assert!(reply_row.onnx_score >= 0.9 - 1e-6);
    }

    // ── Idempotency: gather twice → one row per post ──
    #[tokio::test]
    async fn gather_twice_is_idempotent() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();

        let sample = survivor_sample();
        let fetcher = CannedFetcher {
            sample,
            parents: HashMap::new(),
        };
        let scorer = FixedScorer(0.0);
        let clean = FixedCleanPass(0.0);

        for _ in 0..2 {
            gather_account(
                &db,
                TEST_USER,
                &fetcher,
                &scorer,
                &clean,
                &inputs(&fp, &weights),
            )
            .await
            .unwrap();
        }

        let rows = db.fetch_account_verdicts(TEST_USER, ACCT).await.unwrap();
        assert_eq!(
            rows.len(),
            6,
            "UPSERT: gather twice yields one row per post"
        );
    }
}

// ── Phase C: finalize_account tests ─────────────────────────────────────────

mod finalize_tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    use charcoal::bluesky::posts::{Post, PostSample, ReplyPost};
    use charcoal::pipeline::scan_phases::finalize::{finalize_account, FinalizeOutcome};
    use charcoal::scoring::threat::ThreatWeights;
    use charcoal::topics::fingerprint::{TopicCluster, TopicFingerprint};

    const FIN_USER: &str = "did:plc:finuser00000000000000";
    const ACCT: &str = "did:plc:finacct0000000000000";

    async fn open_db() -> Arc<dyn Database> {
        let db = setup_db().await;
        db.upsert_user(FIN_USER, "finuser.bsky.social")
            .await
            .unwrap();
        Arc::new(db)
    }

    fn make_post(uri: &str, text: &str) -> Post {
        Post {
            uri: uri.to_string(),
            text: text.to_string(),
            created_at: None,
            like_count: 0,
            repost_count: 0,
            quote_count: 0,
            is_quote: false,
        }
    }

    fn make_reply(uri: &str, text: &str, parent_uri: &str) -> ReplyPost {
        ReplyPost {
            post: make_post(uri, text),
            parent_uri: parent_uri.to_string(),
        }
    }

    /// Astrophysics fingerprint — unrelated to the food-topic posts in the
    /// survivor sample, so TF-IDF overlap stays below the 0.15 gate. Mirrors the
    /// golden case (c) setup so we can reuse its expected scores.
    fn astrophysics_fingerprint() -> TopicFingerprint {
        TopicFingerprint {
            clusters: vec![TopicCluster {
                label: "astrophysics".to_string(),
                keywords: vec![
                    "quasar".to_string(),
                    "nebula".to_string(),
                    "redshift".to_string(),
                    "telescope".to_string(),
                    "pulsar".to_string(),
                    "photon".to_string(),
                    "galaxy".to_string(),
                    "cosmology".to_string(),
                ],
                weight: 1.0,
            }],
            post_count: 200,
        }
    }

    /// Toxicology fingerprint — shares keywords with the case (d) posts so
    /// TF-IDF overlap is >= 0.15 and the full multiplicative formula runs.
    fn toxicology_fingerprint() -> TopicFingerprint {
        TopicFingerprint {
            clusters: vec![TopicCluster {
                label: "toxicology".to_string(),
                keywords: vec![
                    "toxic".to_string(),
                    "poison".to_string(),
                    "venom".to_string(),
                    "lethal".to_string(),
                    "hazard".to_string(),
                    "dangerous".to_string(),
                    "contamination".to_string(),
                    "exposure".to_string(),
                ],
                weight: 1.0,
            }],
            post_count: 200,
        }
    }

    /// Build the golden-(c) survivor sample: 6 food-topic originals + 6 replies
    /// (first 3 hostile-worded, last 3 mild), 0 quotes. reply_ratio 0.5.
    fn survivor_sample() -> PostSample {
        let originals: Vec<Post> = (0..6)
            .map(|i| {
                make_post(
                    &format!("at://fin/c/o/{i}"),
                    "baking bread and drinking coffee",
                )
            })
            .collect();
        let replies: Vec<ReplyPost> = (0..6)
            .map(|i| {
                make_reply(
                    &format!("at://fin/c/r/{i}"),
                    "you are completely wrong about this",
                    &format!("at://parent/c/{i}"),
                )
            })
            .collect();
        PostSample {
            originals,
            replies,
            quotes: vec![],
            reply_ratio: 0.5,
            quote_ratio: 0.0,
            total_posts: 12,
        }
    }

    /// Construct an `AccountInput` blob for a sample, JSON-stash it, then enqueue
    /// one DONE QueueRow per post with the supplied (is_toxic, onnx) verdict.
    /// Verdicts are keyed positionally over originals ++ replies ++ quotes.
    async fn stage_account(
        db: &Arc<dyn Database>,
        sample: &PostSample,
        verdicts: &[(bool, f64)],
        direct_pairs: Option<Vec<(String, String)>>,
    ) {
        let blob = AccountInput {
            schema_version: ACCOUNT_INPUT_SCHEMA_VERSION,
            account_handle: "finacct.bsky.social".to_string(),
            sample: sample.clone(),
            parent_texts: HashMap::new(),
            median_engagement: 0.0,
            is_pile_on: false,
            direct_pairs,
            graph_distance: None,
            fingerprint_quality: "unreliable".to_string(),
        };
        let payload = serde_json::to_string(&blob).unwrap();
        db.stash_account_input(FIN_USER, ACCT, &payload)
            .await
            .unwrap();

        // Flatten posts in scoring order: originals ++ replies ++ quotes.
        let uris: Vec<(String, String)> = sample
            .originals
            .iter()
            .map(|p| (p.uri.clone(), "original".to_string()))
            .chain(
                sample
                    .replies
                    .iter()
                    .map(|r| (r.post.uri.clone(), "reply".to_string())),
            )
            .chain(
                sample
                    .quotes
                    .iter()
                    .map(|p| (p.uri.clone(), "quote".to_string())),
            )
            .collect();
        assert_eq!(uris.len(), verdicts.len(), "one verdict per post");

        let rows: Vec<QueueRow> = uris
            .iter()
            .zip(verdicts.iter())
            .map(|((uri, kind), (is_toxic, onnx))| QueueRow {
                account_did: ACCT.to_string(),
                post_uri: uri.clone(),
                text: "x".to_string(),
                context_text: None,
                post_kind: kind.clone(),
                onnx_score: *onnx,
                status: "done".to_string(),
                toxic_token: Some(*is_toxic),
                confidence: Some(0.5),
                model_id: Some("test".to_string()),
                policy_version: Some("p".to_string()),
            })
            .collect();
        db.enqueue_classifications(FIN_USER, &rows).await.unwrap();
    }

    // ── survivor scored: matches golden case (c) ──
    #[tokio::test]
    async fn finalize_survivor_scores_matching_golden() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();

        let sample = survivor_sample();
        // 6 originals clean; replies 0-2 toxic (onnx 0.85), replies 3-5 clean.
        let mut verdicts: Vec<(bool, f64)> = vec![(false, 0.05); 6];
        for i in 0..6 {
            verdicts.push(if i < 3 { (true, 0.85) } else { (false, 0.08) });
        }
        stage_account(&db, &sample, &verdicts, None).await;

        let outcome = finalize_account(
            &db, FIN_USER, ACCT, &fp, &weights, None, None, None, None, None,
        )
        .await
        .unwrap();
        assert_eq!(outcome, FinalizeOutcome::Scored);

        let score = db
            .get_account_by_did(FIN_USER, ACCT)
            .await
            .unwrap()
            .expect("AccountScore must be written");

        assert_eq!(score.did, ACCT);
        assert_eq!(score.handle, "finacct.bsky.social");
        assert_eq!(score.posts_analyzed, 12);

        // Reply-weighted toxicity = 0.5*0.7 + 0.0*0.3 = 0.35 (golden c).
        let tox = score.toxicity_score.expect("toxicity_score");
        assert!(
            (tox - 0.35).abs() < 1e-9,
            "expected toxicity 0.35, got {tox}"
        );
        // Threat = 9.40625 (golden c, gated formula + behavioral boost 1.075).
        let threat = score.threat_score.expect("threat_score");
        assert!(
            (threat - 9.40625).abs() < 1e-6,
            "expected threat 9.40625, got {threat}"
        );
        assert_eq!(score.threat_tier.as_deref(), Some("Watch"));
        assert!(score.context_score.is_none(), "no NLI scorer → no context");
        assert_eq!(score.top_toxic_posts.len(), 3);
    }

    // ── version mismatch → NeedsRegather + staging cleared ──
    #[tokio::test]
    async fn finalize_version_mismatch_regathers_and_clears() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();

        // Stash a blob with a bogus schema_version, plus a queue row.
        let bad_payload = r#"{"schema_version":999,"account_handle":"x","sample":{"originals":[],"replies":[],"quotes":[],"reply_ratio":0.0,"quote_ratio":0.0,"total_posts":0},"parent_texts":{},"median_engagement":0.0,"is_pile_on":false,"direct_pairs":null,"graph_distance":null,"fingerprint_quality":"normal"}"#;
        db.stash_account_input(FIN_USER, ACCT, bad_payload)
            .await
            .unwrap();
        let row = make_queue_row(ACCT, "at://fin/v/1", "pending");
        db.enqueue_classifications(FIN_USER, &[row]).await.unwrap();

        let outcome = finalize_account(
            &db, FIN_USER, ACCT, &fp, &weights, None, None, None, None, None,
        )
        .await
        .unwrap();
        assert_eq!(outcome, FinalizeOutcome::NeedsRegather);

        // Staging for this account must be gone.
        assert!(db
            .fetch_account_input(FIN_USER, ACCT)
            .await
            .unwrap()
            .is_none());
        assert!(db
            .fetch_account_verdicts(FIN_USER, ACCT)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            db.count_pending_classifications(FIN_USER).await.unwrap(),
            0,
            "version-stale account's pending rows must be cleared"
        );
    }

    // ── malformed blob → NeedsRegather + staging cleared ──
    #[tokio::test]
    async fn finalize_malformed_blob_regathers_and_clears() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();

        db.stash_account_input(FIN_USER, ACCT, "{ not valid json ]")
            .await
            .unwrap();
        let row = make_queue_row(ACCT, "at://fin/m/1", "pending");
        db.enqueue_classifications(FIN_USER, &[row]).await.unwrap();

        let outcome = finalize_account(
            &db, FIN_USER, ACCT, &fp, &weights, None, None, None, None, None,
        )
        .await
        .unwrap();
        assert_eq!(outcome, FinalizeOutcome::NeedsRegather);
        assert!(db
            .fetch_account_input(FIN_USER, ACCT)
            .await
            .unwrap()
            .is_none());
        assert!(db
            .fetch_account_verdicts(FIN_USER, ACCT)
            .await
            .unwrap()
            .is_empty());
    }

    // ── nothing staged → NeedsRegather (no clear needed) ──
    #[tokio::test]
    async fn finalize_nothing_staged_regathers() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();

        let outcome = finalize_account(
            &db, FIN_USER, ACCT, &fp, &weights, None, None, None, None, None,
        )
        .await
        .unwrap();
        assert_eq!(outcome, FinalizeOutcome::NeedsRegather);
    }

    // ── a sample post with a missing/pending verdict → NeedsRegather ──
    //
    // We stage a full set of done rows, then re-stash a blob whose sample
    // references an extra post URI that has no matching row. An incomplete
    // account must never be scored — finalize returns NeedsRegather and leaves
    // staging in place (the burst may simply be mid-flight).
    #[tokio::test]
    async fn finalize_pending_verdict_regathers() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();

        let sample = survivor_sample();
        let mut verdicts: Vec<(bool, f64)> = vec![(false, 0.05); 6];
        for _ in 0..6 {
            verdicts.push((false, 0.08));
        }
        stage_account(&db, &sample, &verdicts, None).await;

        // Re-stash a blob whose sample references a post URI that has no row.
        let mut sample2 = sample.clone();
        sample2
            .originals
            .push(make_post("at://fin/c/o/missing", "no row for this one"));
        sample2.total_posts += 1;
        let blob = AccountInput {
            schema_version: ACCOUNT_INPUT_SCHEMA_VERSION,
            account_handle: "finacct.bsky.social".to_string(),
            sample: sample2,
            parent_texts: HashMap::new(),
            median_engagement: 0.0,
            is_pile_on: false,
            direct_pairs: None,
            graph_distance: None,
            fingerprint_quality: "unreliable".to_string(),
        };
        db.stash_account_input(FIN_USER, ACCT, &serde_json::to_string(&blob).unwrap())
            .await
            .unwrap();

        let outcome = finalize_account(
            &db, FIN_USER, ACCT, &fp, &weights, None, None, None, None, None,
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            FinalizeOutcome::NeedsRegather,
            "a sample post with no matching verdict row must not be scored"
        );
        // We did NOT clear staging on the incomplete path.
        assert!(db
            .fetch_account_input(FIN_USER, ACCT)
            .await
            .unwrap()
            .is_some());
    }

    // ── status is authoritative: a `pending` row carrying a token is incomplete ──
    // A row whose status is still "pending" must be treated as incomplete even
    // if a `toxic_token` somehow leaked onto it. The queue status is the source
    // of truth → finalize must return NeedsRegather (fail closed), never score.
    #[tokio::test]
    async fn finalize_pending_status_with_token_regathers() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();

        // Stage a valid blob with a single original post.
        let mut sample = survivor_sample();
        sample.originals.truncate(1);
        sample.replies.clear();
        sample.quotes.clear();
        sample.total_posts = 1;
        let blob = AccountInput {
            schema_version: ACCOUNT_INPUT_SCHEMA_VERSION,
            account_handle: "finacct.bsky.social".to_string(),
            sample: sample.clone(),
            parent_texts: HashMap::new(),
            median_engagement: 0.0,
            is_pile_on: false,
            direct_pairs: None,
            graph_distance: None,
            fingerprint_quality: "unreliable".to_string(),
        };
        db.stash_account_input(FIN_USER, ACCT, &serde_json::to_string(&blob).unwrap())
            .await
            .unwrap();

        // Enqueue the matching row as STILL pending but with a verdict token
        // populated — the inconsistent state the status guard fails closed on.
        let row = QueueRow {
            account_did: ACCT.to_string(),
            post_uri: sample.originals[0].uri.clone(),
            text: "x".to_string(),
            context_text: None,
            post_kind: "original".to_string(),
            onnx_score: 0.5,
            status: "pending".to_string(),
            toxic_token: Some(true),
            confidence: Some(0.9),
            model_id: Some("test".to_string()),
            policy_version: Some("p".to_string()),
        };
        db.enqueue_classifications(FIN_USER, &[row]).await.unwrap();

        let outcome = finalize_account(
            &db, FIN_USER, ACCT, &fp, &weights, None, None, None, None, None,
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            FinalizeOutcome::NeedsRegather,
            "a pending-status row must be treated as incomplete despite its token"
        );
        // Incomplete path leaves staging intact (no clear).
        assert!(db
            .fetch_account_input(FIN_USER, ACCT)
            .await
            .unwrap()
            .is_some());
    }

    // ── HERMETIC GUARD: follower below the >=8.0 gate SKIPS NLI ──────────────
    //
    // This is the key new behavior-correctness guard for the decouple fix.
    // The follower path (`direct_pairs = None`, NLI scorer present, inferred
    // pairs present) is TWO-PASS: pass 1 with NO NLI, then pass 2 (with NLI)
    // ONLY when pass-1 `threat_score >= 8.0`. Here we stage a low-toxicity
    // sample so pass-1 stays below 8.0 — therefore the gate's sub-8.0 branch
    // must return pass 1 and NEVER touch the NLI scorer.
    //
    // This case is HERMETIC: because the gate short-circuits before the scorer
    // is dereferenced, no model is needed. We pass a real `NliScorer` ONLY when
    // the model happens to be present (to additionally prove a *present* scorer
    // is still skipped); otherwise we pass `None`. Either way the assertion is
    // the same — `context_score.is_none()` — and the score equals the no-NLI
    // pass. If the gate were wrong (ran NLI on ALL followers), the model-present
    // run would produce a `context_score`, failing this guard.
    #[tokio::test]
    async fn finalize_follower_below_threshold_skips_nli() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint(); // low overlap → low threat
        let weights = ThreatWeights::default();

        // Survivor sample, all posts clean → reply-weighted toxicity 0.0, so the
        // (gated) raw threat_score stays below the 8.0 Watch boundary.
        let sample = survivor_sample();
        let verdicts: Vec<(bool, f64)> = vec![(false, 0.05); 12];

        // A non-empty inferred-pairs sentinel — present, but the gate must skip
        // it because pass-1 threat < 8.0.
        let ppwe: Vec<(String, Vec<f64>)> =
            vec![("a protected post".to_string(), vec![0.1, 0.2, 0.3])];

        // Load a real scorer ONLY if the model is present; otherwise None. The
        // gate skips it either way, which is precisely what this test proves.
        let model_base = std::env::var("CHARCOAL_MODEL_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| charcoal::toxicity::download::default_model_dir());
        let maybe_nli = if charcoal::toxicity::download::nli_files_present(&model_base) {
            charcoal::scoring::nli::NliScorer::load(&model_base).ok()
        } else {
            None
        };

        stage_account(&db, &sample, &verdicts, None).await; // direct_pairs=None ⇒ follower
        let outcome = finalize_account(
            &db,
            FIN_USER,
            ACCT,
            &fp,
            &weights,
            None,               // embedder: absent → Mode B can't run anyway
            None,               // protected_embedding
            maybe_nli.as_ref(), // nli_scorer: present iff the model is on disk
            Some(&ppwe),        // protected_posts_with_embeddings (sentinel)
            None,               // data_dir
        )
        .await
        .unwrap();
        assert_eq!(outcome, FinalizeOutcome::Scored);

        let score = db
            .get_account_by_did(FIN_USER, ACCT)
            .await
            .unwrap()
            .expect("AccountScore must be written");

        // THE GUARD: a below-threshold follower must NOT have run NLI, even with
        // a scorer + inferred pairs present.
        assert!(
            score.context_score.is_none(),
            "follower with raw threat < 8.0 must SKIP NLI (context_score must be None), got {:?}",
            score.context_score
        );
        let threat = score.threat_score.expect("threat_score");
        assert!(
            threat < 8.0,
            "control sample must score below the 8.0 gate, got {threat}"
        );
    }

    // ── follower AT/ABOVE the gate runs NLI (MODEL-GATED, mirrors golden d) ──
    //
    // Inferred-pairs (Mode B) needs BOTH the NLI cross-encoder and the sentence
    // embedder, plus a protected post with its embedding. With a high-toxicity
    // sample the pass-1 raw score clears 8.0, so pass 2 (NLI) must fire and a
    // context_score must appear.
    #[tokio::test]
    async fn finalize_follower_above_threshold_runs_nli() {
        let model_base = std::env::var("CHARCOAL_MODEL_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| charcoal::toxicity::download::default_model_dir());

        if !charcoal::toxicity::download::nli_files_present(&model_base)
            || !charcoal::toxicity::download::embedding_files_present(&model_base)
        {
            eprintln!(
                "SKIP finalize follower-NLI case: NLI and/or embedding model not \
                 present at {model_base:?} — run `charcoal download-model` to enable"
            );
            return;
        }

        let nli = charcoal::scoring::nli::NliScorer::load(&model_base)
            .expect("NLI model should load when files are present");
        // SentenceEmbedder::load expects the embedding model's OWN dir (the
        // `all-MiniLM-L6-v2` subdir holding model.onnx), not the base dir.
        let embed_dir = charcoal::toxicity::download::embedding_model_dir(&model_base);
        let embedder = charcoal::topics::embeddings::SentenceEmbedder::load(&embed_dir)
            .expect("embedding model should load when files are present");

        let db = open_db().await;
        let fp = toxicology_fingerprint();
        let weights = ThreatWeights::default();

        // 20 + 5 all-toxic toxicology posts → raw >= 8.0 → pass 2 fires.
        let originals: Vec<Post> = (0..20)
            .map(|i| {
                make_post(
                    &format!("at://fin/fa/o/{i}"),
                    "toxic poison venom lethal hazard dangerous contamination exposure",
                )
            })
            .collect();
        let replies: Vec<ReplyPost> = (0..5)
            .map(|i| {
                make_reply(
                    &format!("at://fin/fa/r/{i}"),
                    "this toxic hazard is dangerous and lethal to all",
                    &format!("at://parent/fa/{i}"),
                )
            })
            .collect();
        let sample = PostSample {
            originals,
            replies,
            quotes: vec![],
            reply_ratio: 5.0 / 25.0,
            quote_ratio: 0.0,
            total_posts: 25,
        };
        let verdicts: Vec<(bool, f64)> = (0..25).map(|_| (true, 0.92)).collect();

        // Protected post + its embedding (inferred-pairs Mode B input).
        let protected_text = "this toxic hazard is dangerous and lethal to all".to_string();
        let protected_emb = embedder
            .embed_batch(std::slice::from_ref(&protected_text))
            .await
            .expect("embedding the protected post")
            .remove(0);
        let ppwe: Vec<(String, Vec<f64>)> = vec![(protected_text, protected_emb)];

        stage_account(&db, &sample, &verdicts, None).await; // direct_pairs = None ⇒ follower

        let data_dir = std::env::temp_dir().join("charcoal-finalize-follower-nli-test");
        std::fs::create_dir_all(&data_dir).ok();

        let outcome = finalize_account(
            &db,
            FIN_USER,
            ACCT,
            &fp,
            &weights,
            Some(&embedder),
            None,
            Some(&nli),
            Some(&ppwe),
            Some(&data_dir),
        )
        .await
        .unwrap();
        assert_eq!(outcome, FinalizeOutcome::Scored);

        let score = db
            .get_account_by_did(FIN_USER, ACCT)
            .await
            .unwrap()
            .expect("AccountScore must be written");
        assert!(
            score.context_score.is_some(),
            "above-threshold follower must run NLI (context_score must be Some)"
        );

        std::fs::remove_dir_all(&data_dir).ok();
    }

    // ── amplifier ALWAYS runs NLI — no >=8.0 gate (MODEL-GATED, golden d) ────
    //
    // The amplifier path (`direct_pairs = Some`) uses Mode A direct interaction
    // pairs and is NOT gated on the raw score. We deliberately stage a LOW
    // toxicity sample (raw < 8.0); NLI must STILL run and produce a context
    // score, proving amplifiers bypass the follower gate.
    #[tokio::test]
    async fn finalize_amplifier_always_runs_nli() {
        let model_base = std::env::var("CHARCOAL_MODEL_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| charcoal::toxicity::download::default_model_dir());

        if !charcoal::toxicity::download::nli_files_present(&model_base) {
            eprintln!(
                "SKIP finalize amplifier-NLI case: NLI model not present at \
                 {model_base:?} — run `charcoal download-model` to enable"
            );
            return;
        }

        let nli = charcoal::scoring::nli::NliScorer::load(&model_base)
            .expect("NLI model should load when files are present");

        let db = open_db().await;
        let fp = astrophysics_fingerprint(); // low overlap → low raw threat
        let weights = ThreatWeights::default();

        // Low-toxicity survivor sample → raw threat < 8.0 (would skip NLI for a
        // follower, but amplifiers have no gate).
        let sample = survivor_sample();
        let verdicts: Vec<(bool, f64)> = vec![(false, 0.05); 12];

        // Mode A direct interaction pair.
        let direct_pairs = vec![(
            "This community values mutual respect and kindness.".to_string(),
            "That is absolute garbage, these people are toxic poison.".to_string(),
        )];
        stage_account(&db, &sample, &verdicts, Some(direct_pairs)).await;

        let data_dir = std::env::temp_dir().join("charcoal-finalize-amp-nli-test");
        std::fs::create_dir_all(&data_dir).ok();

        let outcome = finalize_account(
            &db,
            FIN_USER,
            ACCT,
            &fp,
            &weights,
            None, // embedder (Mode A uses direct pairs, no embedder needed)
            None, // protected_embedding
            Some(&nli),
            None, // protected_posts_with_embeddings (Mode A ignores this)
            Some(&data_dir),
        )
        .await
        .unwrap();
        assert_eq!(outcome, FinalizeOutcome::Scored);

        let score = db
            .get_account_by_did(FIN_USER, ACCT)
            .await
            .unwrap()
            .expect("AccountScore must be written");

        // THE GUARD: amplifier ran NLI despite raw threat < 8.0 (no gate).
        let threat = score.threat_score.expect("threat_score");
        assert!(
            threat < 8.0,
            "amplifier sample is intentionally below the 8.0 gate, got {threat}"
        );
        assert!(
            score.context_score.is_some(),
            "amplifier (direct_pairs=Some) must ALWAYS run NLI regardless of the \
             8.0 gate (context_score must be Some)"
        );

        std::fs::remove_dir_all(&data_dir).ok();
    }
}

// ── Phase B: run_burst tests ──────────────────────────────────────────────────

mod burst_tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    use charcoal::pipeline::scan_phases::burst::{
        burst_batch, burst_concurrency, run_burst, BurstOutcome,
    };
    use charcoal::toxicity::classifier::{ClassifierVerdict, ToxicityClassifier};
    use charcoal::toxicity::cost_meter::CostCeilingExceeded;

    const BURST_USER: &str = "did:plc:burstuser0000000000000";

    async fn open_burst_db() -> Arc<dyn Database> {
        let db = setup_db().await;
        db.upsert_user(BURST_USER, "burstuser.bsky.social")
            .await
            .unwrap();
        Arc::new(db)
    }

    /// Build a pending QueueRow with the given account_did + post_uri suffix.
    fn pending_row(account_did: &str, uri_suffix: &str) -> QueueRow {
        QueueRow {
            account_did: account_did.to_string(),
            post_uri: format!("at://{account_did}/app.bsky.feed.post/{uri_suffix}"),
            text: format!("post {uri_suffix}"),
            context_text: None,
            post_kind: "original".to_string(),
            onnx_score: 0.3,
            status: "pending".to_string(),
            toxic_token: None,
            confidence: None,
            model_id: None,
            policy_version: None,
        }
    }

    /// Build a pending reply QueueRow with a parent context.
    fn pending_reply_row(account_did: &str, uri_suffix: &str, ctx: &str) -> QueueRow {
        QueueRow {
            account_did: account_did.to_string(),
            post_uri: format!("at://{account_did}/app.bsky.feed.post/{uri_suffix}"),
            text: "agreed".to_string(),
            context_text: Some(ctx.to_string()),
            post_kind: "reply".to_string(),
            onnx_score: 0.3,
            status: "pending".to_string(),
            toxic_token: None,
            confidence: None,
            model_id: None,
            policy_version: None,
        }
    }

    fn ok_verdict() -> ClassifierVerdict {
        ClassifierVerdict {
            toxic_token: false,
            confidence: 0.1,
            latency_ms: 10,
            model_id: "stub".to_string(),
            policy_version: "stub".to_string(),
        }
    }

    // ── always-ok double that accepts N calls ──────────────────────────────

    struct AlwaysOkClassifier {
        verdict: ClassifierVerdict,
        threshold: f32,
    }

    impl AlwaysOkClassifier {
        fn new(verdict: ClassifierVerdict) -> Self {
            Self {
                verdict,
                threshold: 0.0,
            }
        }
    }

    #[async_trait]
    impl ToxicityClassifier for AlwaysOkClassifier {
        async fn classify(&self, _content: &str) -> Result<ClassifierVerdict> {
            Ok(self.verdict.clone())
        }
        fn name(&self) -> &'static str {
            "always-ok"
        }
        fn model_id(&self) -> &'static str {
            "always-ok"
        }
        fn policy_version(&self) -> &'static str {
            "always-ok"
        }
        fn threshold(&self) -> f32 {
            self.threshold
        }
    }

    // ── cost-cap double: Ok for first K calls, then CostCeilingExceeded ───

    struct CostCapClassifier {
        verdict: ClassifierVerdict,
        calls_before_cap: usize,
        call_count: Mutex<usize>,
    }

    impl CostCapClassifier {
        fn new(verdict: ClassifierVerdict, calls_before_cap: usize) -> Self {
            Self {
                verdict,
                calls_before_cap,
                call_count: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl ToxicityClassifier for CostCapClassifier {
        async fn classify(&self, _content: &str) -> Result<ClassifierVerdict> {
            let mut count = self.call_count.lock().unwrap();
            *count += 1;
            if *count > self.calls_before_cap {
                Err(CostCeilingExceeded {
                    est_cents: 600,
                    ceiling_cents: 500,
                }
                .into())
            } else {
                Ok(self.verdict.clone())
            }
        }
        fn name(&self) -> &'static str {
            "cost-cap"
        }
        fn model_id(&self) -> &'static str {
            "cost-cap"
        }
        fn policy_version(&self) -> &'static str {
            "cost-cap"
        }
        fn threshold(&self) -> f32 {
            0.0
        }
    }

    // ── recording double: captures the `content` it was called with ────────

    struct RecordingClassifier {
        verdict: ClassifierVerdict,
        calls: Mutex<Vec<String>>,
    }

    impl RecordingClassifier {
        fn new(verdict: ClassifierVerdict) -> Self {
            Self {
                verdict,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn recorded_calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ToxicityClassifier for RecordingClassifier {
        async fn classify(&self, content: &str) -> Result<ClassifierVerdict> {
            self.calls.lock().unwrap().push(content.to_string());
            Ok(self.verdict.clone())
        }
        fn name(&self) -> &'static str {
            "recording"
        }
        fn model_id(&self) -> &'static str {
            "recording"
        }
        fn policy_version(&self) -> &'static str {
            "recording"
        }
        fn threshold(&self) -> f32 {
            0.0
        }
    }

    // ── Test: drain — all rows go done, returns Complete, count = 0 ────────

    #[tokio::test]
    async fn burst_drain_completes_and_flips_all_to_done() {
        let db = open_burst_db().await;
        let acct = "did:plc:burst001";

        let rows: Vec<QueueRow> = (0..5).map(|i| pending_row(acct, &i.to_string())).collect();
        db.enqueue_classifications(BURST_USER, &rows).await.unwrap();

        let classifier: Arc<dyn ToxicityClassifier> =
            Arc::new(AlwaysOkClassifier::new(ok_verdict()));

        let outcome = run_burst(&db, BURST_USER, &classifier, 4, 100)
            .await
            .unwrap();
        assert!(matches!(outcome, BurstOutcome::Complete));

        let pending = db.count_pending_classifications(BURST_USER).await.unwrap();
        assert_eq!(pending, 0, "all rows should be done after burst");

        let verdicts = db.fetch_account_verdicts(BURST_USER, acct).await.unwrap();
        assert_eq!(verdicts.len(), 5);
        for v in &verdicts {
            assert_eq!(v.status, "done");
            assert_eq!(
                v.toxic_token,
                Some(false),
                "AlwaysOkClassifier produces toxic_token: false"
            );
            assert!(v.confidence.is_some());
            assert!(v.model_id.is_some());
        }
    }

    // ── Test: batching — loop runs multiple iterations until empty ─────────

    #[tokio::test]
    async fn burst_batching_loops_until_all_done() {
        let db = open_burst_db().await;
        let acct = "did:plc:burst002";

        // 5 rows, batch size 2 → needs at least 3 iterations
        let rows: Vec<QueueRow> = (0..5).map(|i| pending_row(acct, &i.to_string())).collect();
        db.enqueue_classifications(BURST_USER, &rows).await.unwrap();

        let classifier: Arc<dyn ToxicityClassifier> =
            Arc::new(AlwaysOkClassifier::new(ok_verdict()));

        let outcome = run_burst(&db, BURST_USER, &classifier, 4, 2).await.unwrap();
        assert!(matches!(outcome, BurstOutcome::Complete));

        let pending = db.count_pending_classifications(BURST_USER).await.unwrap();
        assert_eq!(pending, 0, "all rows done after multi-iteration burst");

        let verdicts = db.fetch_account_verdicts(BURST_USER, acct).await.unwrap();
        assert_eq!(verdicts.len(), 5, "all 5 verdicts recorded");
        for v in &verdicts {
            assert_eq!(v.status, "done");
        }
    }

    // ── Test: cost-cap — classified rows are done, unclassified stay pending

    #[tokio::test]
    async fn burst_cost_cap_stops_and_partial_records_persist() {
        let db = open_burst_db().await;
        let acct = "did:plc:burst003";

        // Enqueue 6 rows total; classifier caps after 3 successes
        let rows: Vec<QueueRow> = (0..6).map(|i| pending_row(acct, &i.to_string())).collect();
        db.enqueue_classifications(BURST_USER, &rows).await.unwrap();

        // Cap fires after 3 Ok calls (call 4 returns Err)
        let classifier: Arc<dyn ToxicityClassifier> =
            Arc::new(CostCapClassifier::new(ok_verdict(), 3));

        // concurrency=1 ensures exactly 3 Ok calls land before the cap fires, making done_count/pending deterministic
        let outcome = run_burst(&db, BURST_USER, &classifier, 1, 100)
            .await
            .unwrap();
        assert!(
            matches!(outcome, BurstOutcome::CostCapped),
            "expected CostCapped but got {:?}",
            outcome
        );

        // At least the 3 classified rows must be done; the rest stay pending.
        let pending = db.count_pending_classifications(BURST_USER).await.unwrap();
        assert_eq!(
            pending, 3,
            "exactly 3 rows must remain pending after cost-cap (calls 4-6 never ran)"
        );

        let all_verdicts = db.fetch_account_verdicts(BURST_USER, acct).await.unwrap();
        let done_count = all_verdicts.iter().filter(|r| r.status == "done").count();
        assert_eq!(
            done_count, 3,
            "exactly the 3 successful rows should be done"
        );
    }

    // ── Test: envelope — reply row uses format_parent_reply envelope ───────

    #[tokio::test]
    async fn burst_envelope_reconstruction_for_reply_rows() {
        let db = open_burst_db().await;
        let acct = "did:plc:burst004";

        let ctx = "this is the parent post";
        let reply_text = "agreed";
        let row = pending_reply_row(acct, "r1", ctx);
        db.enqueue_classifications(BURST_USER, &[row])
            .await
            .unwrap();

        let classifier = Arc::new(RecordingClassifier::new(ok_verdict()));
        let classifier_trait: Arc<dyn ToxicityClassifier> = classifier.clone();

        run_burst(&db, BURST_USER, &classifier_trait, 1, 100)
            .await
            .unwrap();

        let calls = classifier.recorded_calls();
        assert_eq!(calls.len(), 1, "one classify call for one row");

        let expected = charcoal::toxicity::format_parent_reply(ctx, reply_text);
        assert_eq!(
            calls[0], expected,
            "classifier must receive the format_parent_reply envelope for reply rows"
        );
    }

    // ── Test: env helpers clamps and defaults ─────────────────────────────
    //
    // Env-var clamp tests are combined into a single test that runs all
    // assertions sequentially. Rust tests are parallel by default and env vars
    // are process-global, so splitting into separate tests causes races.
    // One test body = no inter-test interference for these vars.
    #[test]
    fn burst_env_helpers_clamps() {
        // Save the originals so this test stays hermetic — restore (or remove
        // if originally unset) after the assertions.
        let orig_concurrency = std::env::var("CHARCOAL_BURST_CONCURRENCY").ok();
        let orig_batch = std::env::var("CHARCOAL_BURST_BATCH").ok();

        // --- concurrency clamps ---
        std::env::set_var("CHARCOAL_BURST_CONCURRENCY", "0");
        assert_eq!(burst_concurrency(), 1, "0 → clamp to min=1");
        std::env::set_var("CHARCOAL_BURST_CONCURRENCY", "9999");
        assert_eq!(burst_concurrency(), 64, "9999 → clamp to max=64");
        std::env::set_var("CHARCOAL_BURST_CONCURRENCY", "8");
        assert_eq!(burst_concurrency(), 8, "8 → in-range, unchanged");
        std::env::remove_var("CHARCOAL_BURST_CONCURRENCY");
        assert_eq!(burst_concurrency(), 16, "unset → default 16");

        // --- batch clamps ---
        std::env::set_var("CHARCOAL_BURST_BATCH", "0");
        assert_eq!(burst_batch(), 1, "0 → clamp to min=1");
        std::env::set_var("CHARCOAL_BURST_BATCH", "99999");
        assert_eq!(burst_batch(), 10_000, "99999 → clamp to max=10_000");
        std::env::set_var("CHARCOAL_BURST_BATCH", "250");
        assert_eq!(burst_batch(), 250, "250 → in-range, unchanged");
        std::env::remove_var("CHARCOAL_BURST_BATCH");
        assert_eq!(burst_batch(), 500, "unset → default 500");

        // Restore the original environment.
        match orig_concurrency {
            Some(v) => std::env::set_var("CHARCOAL_BURST_CONCURRENCY", v),
            None => std::env::remove_var("CHARCOAL_BURST_CONCURRENCY"),
        }
        match orig_batch {
            Some(v) => std::env::set_var("CHARCOAL_BURST_BATCH", v),
            None => std::env::remove_var("CHARCOAL_BURST_BATCH"),
        }
    }
}

// ── run_phased_scan: orchestration state machine tests ──────────────────────────

mod orchestration_tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use charcoal::bluesky::posts::{Post, PostSample};
    use charcoal::pipeline::scan_phases::gather::{CleanPassScorer, PostFetcher};
    use charcoal::pipeline::scan_phases::{run_phased_scan, CandidateInput, PhasedScanDeps};
    use charcoal::scoring::threat::ThreatWeights;
    use charcoal::topics::fingerprint::{TopicCluster, TopicFingerprint};
    use charcoal::toxicity::classifier::{ClassifierVerdict, ToxicityClassifier};
    use charcoal::toxicity::cost_meter::CostCeilingExceeded;
    use charcoal::toxicity::traits::{ToxicityResult, ToxicityScorer};

    const ORCH_USER: &str = "did:plc:orchuser0000000000000";

    async fn open_db() -> Arc<dyn Database> {
        let db = setup_db().await;
        db.upsert_user(ORCH_USER, "orchuser.bsky.social")
            .await
            .unwrap();
        Arc::new(db)
    }

    // Fingerprint about astrophysics — the survivor sample's posts share these
    // keywords so Stage-1 overlap clears the gate and gather proceeds to Stage 2.
    fn astrophysics_fingerprint() -> TopicFingerprint {
        TopicFingerprint {
            clusters: vec![TopicCluster {
                label: "astrophysics".to_string(),
                keywords: vec![
                    "quasar".to_string(),
                    "nebula".to_string(),
                    "redshift".to_string(),
                    "telescope".to_string(),
                    "pulsar".to_string(),
                    "photon".to_string(),
                    "galaxy".to_string(),
                    "cosmology".to_string(),
                ],
                weight: 1.0,
            }],
            post_count: 200,
        }
    }

    fn make_post(uri: &str, text: &str) -> Post {
        Post {
            uri: uri.to_string(),
            text: text.to_string(),
            created_at: None,
            like_count: 0,
            repost_count: 0,
            quote_count: 0,
            is_quote: false,
        }
    }

    // A survivor sample for one account: 6 high-overlap originals (clears Stage 1)
    // plus one "survivor" original the clean-pass keeps pending for the burst.
    fn survivor_sample(prefix: &str) -> PostSample {
        let mut originals: Vec<Post> = (0..6)
            .map(|i| {
                make_post(
                    &format!("at://{prefix}/o/{i}"),
                    "quasar nebula redshift telescope galaxy cosmology pulsar photon",
                )
            })
            .collect();
        // One survivor post the MarkerCleanPass keeps pending.
        originals.push(make_post(
            &format!("at://{prefix}/o/survivor"),
            "quasar SURVIVOR nebula",
        ));
        PostSample {
            originals,
            replies: vec![],
            quotes: vec![],
            reply_ratio: 0.0,
            quote_ratio: 0.0,
            total_posts: 7,
        }
    }

    // ── doubles ──────────────────────────────────────────────────────────────

    // Fetcher returning a per-handle canned sample.
    struct MapFetcher {
        by_handle: HashMap<String, PostSample>,
    }

    #[async_trait]
    impl PostFetcher for MapFetcher {
        async fn fetch_sample(&self, handle: &str, _limit: usize) -> Result<PostSample> {
            Ok(self
                .by_handle
                .get(handle)
                .cloned()
                .unwrap_or_else(|| PostSample {
                    originals: vec![],
                    replies: vec![],
                    quotes: vec![],
                    reply_ratio: 0.0,
                    quote_ratio: 0.0,
                    total_posts: 0,
                }))
        }
        async fn fetch_parents(&self, _uris: &[String]) -> Result<HashMap<String, String>> {
            Ok(HashMap::new())
        }
    }

    // Fetcher whose fetch_sample always errors — drives the resilient-gather
    // skip path (a per-account gather failure must mark the scan degraded).
    struct FailingFetcher;

    #[async_trait]
    impl PostFetcher for FailingFetcher {
        async fn fetch_sample(&self, _handle: &str, _limit: usize) -> Result<PostSample> {
            anyhow::bail!("simulated fetch failure")
        }
        async fn fetch_parents(&self, _uris: &[String]) -> Result<HashMap<String, String>> {
            Ok(HashMap::new())
        }
    }

    // Fetcher that PANICS if fetch_sample is called — proves gather was skipped.
    struct PanicFetcher;

    #[async_trait]
    impl PostFetcher for PanicFetcher {
        async fn fetch_sample(&self, _handle: &str, _limit: usize) -> Result<PostSample> {
            panic!("fetch_sample called — gather must be skipped on resume");
        }
        async fn fetch_parents(&self, _uris: &[String]) -> Result<HashMap<String, String>> {
            Ok(HashMap::new())
        }
    }

    struct FixedScorer(f64);

    #[async_trait]
    impl ToxicityScorer for FixedScorer {
        async fn score_text(&self, _text: &str) -> Result<ToxicityResult> {
            Ok(ToxicityResult {
                toxicity: self.0,
                attributes: Default::default(),
            })
        }
    }

    // Clean-pass: any envelope containing "SURVIVOR" stays pending (survivor);
    // everything else is clean.
    struct MarkerCleanPass;

    #[async_trait]
    impl CleanPassScorer for MarkerCleanPass {
        async fn onnx_clean_pass(&self, texts: &[String]) -> Result<Vec<f64>> {
            Ok(texts
                .iter()
                .map(|t| if t.contains("SURVIVOR") { 0.9 } else { 0.0 })
                .collect())
        }
    }

    fn ok_verdict() -> ClassifierVerdict {
        ClassifierVerdict {
            toxic_token: false,
            confidence: 0.1,
            latency_ms: 10,
            model_id: "stub".to_string(),
            policy_version: "stub".to_string(),
        }
    }

    struct AlwaysOkClassifier;

    #[async_trait]
    impl ToxicityClassifier for AlwaysOkClassifier {
        async fn classify(&self, _content: &str) -> Result<ClassifierVerdict> {
            Ok(ok_verdict())
        }
        fn name(&self) -> &'static str {
            "always-ok"
        }
        fn model_id(&self) -> &'static str {
            "always-ok"
        }
        fn policy_version(&self) -> &'static str {
            "always-ok"
        }
        fn threshold(&self) -> f32 {
            0.0
        }
    }

    // Cost-cap classifier: Ok for the first K calls, then CostCeilingExceeded.
    struct CostCapClassifier {
        calls_before_cap: usize,
        call_count: Mutex<usize>,
    }

    #[async_trait]
    impl ToxicityClassifier for CostCapClassifier {
        async fn classify(&self, _content: &str) -> Result<ClassifierVerdict> {
            let mut count = self.call_count.lock().unwrap();
            *count += 1;
            if *count > self.calls_before_cap {
                Err(CostCeilingExceeded {
                    est_cents: 600,
                    ceiling_cents: 500,
                }
                .into())
            } else {
                Ok(ok_verdict())
            }
        }
        fn name(&self) -> &'static str {
            "cost-cap"
        }
        fn model_id(&self) -> &'static str {
            "cost-cap"
        }
        fn policy_version(&self) -> &'static str {
            "cost-cap"
        }
        fn threshold(&self) -> f32 {
            0.0
        }
    }

    // Build deps from borrowed parts. Concurrency 1 keeps cost-cap deterministic.
    #[allow(clippy::too_many_arguments)]
    fn deps<'a>(
        fetcher: &'a dyn PostFetcher,
        scorer: &'a dyn ToxicityScorer,
        clean_pass: &'a dyn CleanPassScorer,
        classifier: &'a Arc<dyn ToxicityClassifier>,
        fp: &'a TopicFingerprint,
        weights: &'a ThreatWeights,
    ) -> PhasedScanDeps<'a> {
        PhasedScanDeps {
            fetcher,
            scorer,
            clean_pass,
            classifier,
            protected_fingerprint: fp,
            weights,
            embedder: None,
            protected_embedding: None,
            nli_scorer: None,
            protected_posts_with_embeddings: None,
            data_dir: None,
            median_engagement: 1.0,
            gather_concurrency: 1,
            burst_concurrency: 1,
            burst_batch: 100,
        }
    }

    fn candidate(did: &str, handle: &str) -> CandidateInput {
        CandidateInput {
            account_did: did.to_string(),
            account_handle: handle.to_string(),
            is_pile_on: false,
            direct_pairs: None,
            graph_distance: None,
        }
    }

    // ── Test 1: fresh scan Gather → Burst → Finalize → Done ──
    #[tokio::test]
    async fn fresh_scan_walks_all_phases_to_done() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();

        let acct_a = "did:plc:orcha0000000000000000";
        let acct_b = "did:plc:orchb0000000000000000";

        let mut by_handle = HashMap::new();
        by_handle.insert("a.bsky.social".to_string(), survivor_sample("a"));
        by_handle.insert("b.bsky.social".to_string(), survivor_sample("b"));
        let fetcher = MapFetcher { by_handle };
        let scorer = FixedScorer(0.0);
        let clean = MarkerCleanPass;
        let classifier: Arc<dyn ToxicityClassifier> = Arc::new(AlwaysOkClassifier);

        let candidates = vec![
            candidate(acct_a, "a.bsky.social"),
            candidate(acct_b, "b.bsky.social"),
        ];

        let summary = run_phased_scan(
            &db,
            ORCH_USER,
            &candidates,
            &deps(&fetcher, &scorer, &clean, &classifier, &fp, &weights),
        )
        .await
        .unwrap();

        assert_eq!(summary.accounts_scored, 2, "both accounts scored");
        assert!(!summary.degraded);

        // Final phase marker is "done".
        assert_eq!(
            db.get_scan_state(ORCH_USER, "scan_phase").await.unwrap(),
            Some("done".to_string())
        );
        // Staging is cleared (Done step).
        assert_eq!(
            db.count_pending_classifications(ORCH_USER).await.unwrap(),
            0
        );
        assert!(db.list_scan_accounts(ORCH_USER).await.unwrap().is_empty());

        // Both AccountScores were upserted.
        assert!(db
            .get_account_by_did(ORCH_USER, acct_a)
            .await
            .unwrap()
            .is_some());
        assert!(db
            .get_account_by_did(ORCH_USER, acct_b)
            .await
            .unwrap()
            .is_some());
    }

    // ── Test 2: resume at burst skips gather ──
    #[tokio::test]
    async fn resume_at_burst_skips_gather() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();
        let acct = "did:plc:orchres000000000000000";

        // Pre-seed staging: a multi-post sample (rich enough for TF-IDF) +
        // matching pending rows, so finalize can score the account WITHOUT a
        // gather. The burst drains the pending rows; finalize then scores.
        let sample = survivor_sample("res");
        let blob = AccountInput {
            schema_version: ACCOUNT_INPUT_SCHEMA_VERSION,
            account_handle: "res.bsky.social".to_string(),
            sample: sample.clone(),
            parent_texts: HashMap::new(),
            median_engagement: 0.0,
            is_pile_on: false,
            direct_pairs: None,
            graph_distance: None,
            fingerprint_quality: "unreliable".to_string(),
        };
        db.stash_account_input(ORCH_USER, acct, &serde_json::to_string(&blob).unwrap())
            .await
            .unwrap();
        // One pending QueueRow per sampled original — the burst must classify
        // them all before finalize can score the account.
        let rows: Vec<QueueRow> = sample
            .originals
            .iter()
            .map(|p| QueueRow {
                account_did: acct.to_string(),
                post_uri: p.uri.clone(),
                text: p.text.clone(),
                context_text: None,
                post_kind: "original".to_string(),
                onnx_score: 0.3,
                status: "pending".to_string(),
                toxic_token: None,
                confidence: None,
                model_id: None,
                policy_version: None,
            })
            .collect();
        db.enqueue_classifications(ORCH_USER, &rows).await.unwrap();
        db.set_scan_state(ORCH_USER, "scan_phase", "burst")
            .await
            .unwrap();

        // PanicFetcher proves gather is never called on resume.
        let fetcher = PanicFetcher;
        let scorer = FixedScorer(0.0);
        let clean = MarkerCleanPass;
        let classifier: Arc<dyn ToxicityClassifier> = Arc::new(AlwaysOkClassifier);

        let candidates = vec![candidate(acct, "res.bsky.social")];

        let summary = run_phased_scan(
            &db,
            ORCH_USER,
            &candidates,
            &deps(&fetcher, &scorer, &clean, &classifier, &fp, &weights),
        )
        .await
        .unwrap();

        assert_eq!(summary.accounts_scored, 1, "resumed account scored");
        assert!(!summary.degraded);
        assert_eq!(
            db.get_scan_state(ORCH_USER, "scan_phase").await.unwrap(),
            Some("done".to_string())
        );
        assert!(db
            .get_account_by_did(ORCH_USER, acct)
            .await
            .unwrap()
            .is_some());
    }

    // ── Test 3: CostCapped → degraded, phase stays "burst", nothing finalized ──
    #[tokio::test]
    async fn cost_capped_returns_degraded_and_stays_in_burst() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();
        let acct = "did:plc:orchcap000000000000000";

        let mut by_handle = HashMap::new();
        // Sample with multiple survivors so the burst has >0 pending and the cap
        // can fire mid-drain.
        let mut sample = survivor_sample("cap");
        sample
            .originals
            .push(make_post("at://cap/o/survivor2", "quasar SURVIVOR two"));
        sample
            .originals
            .push(make_post("at://cap/o/survivor3", "quasar SURVIVOR three"));
        sample.total_posts = sample.originals.len();
        by_handle.insert("cap.bsky.social".to_string(), sample);
        let fetcher = MapFetcher { by_handle };
        let scorer = FixedScorer(0.0);
        let clean = MarkerCleanPass;
        // Cap after 1 successful call: 3 survivors enqueued, call 2 caps.
        let classifier: Arc<dyn ToxicityClassifier> = Arc::new(CostCapClassifier {
            calls_before_cap: 1,
            call_count: Mutex::new(0),
        });

        let candidates = vec![candidate(acct, "cap.bsky.social")];

        let summary = run_phased_scan(
            &db,
            ORCH_USER,
            &candidates,
            &deps(&fetcher, &scorer, &clean, &classifier, &fp, &weights),
        )
        .await
        .unwrap();

        assert!(summary.degraded, "cost cap must mark the scan degraded");
        assert_eq!(
            summary.accounts_scored, 0,
            "no account finalized in a cost-capped call"
        );
        // Phase stays at "burst" so a later call can resume the burst.
        assert_eq!(
            db.get_scan_state(ORCH_USER, "scan_phase").await.unwrap(),
            Some("burst".to_string())
        );
        // Some rows remain pending (the cap stopped the drain).
        assert!(
            db.count_pending_classifications(ORCH_USER).await.unwrap() > 0,
            "cost cap leaves pending rows for resume"
        );
        // No AccountScore written (finalize never ran).
        assert!(db
            .get_account_by_did(ORCH_USER, acct)
            .await
            .unwrap()
            .is_none());
    }

    // ── Test 4: NeedsRegather → re-gather + re-burst + re-finalize → Scored ──
    #[tokio::test]
    async fn needs_regather_re_gathers_then_scores() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();
        let acct = "did:plc:orchrg0000000000000000";

        // Pre-seed a STALE blob (wrong schema_version) so the first finalize
        // returns NeedsRegather + clears the account's staging. Also enqueue a
        // row so list_scan_accounts surfaces the account.
        let bad_payload = r#"{"schema_version":999,"account_handle":"rg.bsky.social","sample":{"originals":[],"replies":[],"quotes":[],"reply_ratio":0.0,"quote_ratio":0.0,"total_posts":0},"parent_texts":{},"median_engagement":0.0,"is_pile_on":false,"direct_pairs":null,"graph_distance":null,"fingerprint_quality":"normal"}"#;
        db.stash_account_input(ORCH_USER, acct, bad_payload)
            .await
            .unwrap();
        let stale_row = QueueRow {
            account_did: acct.to_string(),
            post_uri: format!("at://{acct}/o/stale"),
            text: "stale".to_string(),
            context_text: None,
            post_kind: "original".to_string(),
            onnx_score: 0.3,
            status: "pending".to_string(),
            toxic_token: None,
            confidence: None,
            model_id: None,
            policy_version: None,
        };
        db.enqueue_classifications(ORCH_USER, &[stale_row])
            .await
            .unwrap();
        // Enter the state machine directly at finalize — gather + burst already
        // "happened" (we hand-staged stale data).
        db.set_scan_state(ORCH_USER, "scan_phase", "finalize")
            .await
            .unwrap();

        // The fetcher returns a FRESH good sample so the re-gather produces a
        // scorable account.
        let mut by_handle = HashMap::new();
        by_handle.insert("rg.bsky.social".to_string(), survivor_sample("rg"));
        let fetcher = MapFetcher { by_handle };
        let scorer = FixedScorer(0.0);
        let clean = MarkerCleanPass;
        let classifier: Arc<dyn ToxicityClassifier> = Arc::new(AlwaysOkClassifier);

        let candidates = vec![candidate(acct, "rg.bsky.social")];

        let summary = run_phased_scan(
            &db,
            ORCH_USER,
            &candidates,
            &deps(&fetcher, &scorer, &clean, &classifier, &fp, &weights),
        )
        .await
        .unwrap();

        assert_eq!(summary.accounts_scored, 1, "account scored after re-gather");
        assert_eq!(summary.regathered, 1, "one account was re-gathered");
        assert!(!summary.degraded);
        assert_eq!(
            db.get_scan_state(ORCH_USER, "scan_phase").await.unwrap(),
            Some("done".to_string())
        );
        assert!(
            db.get_account_by_did(ORCH_USER, acct)
                .await
                .unwrap()
                .is_some(),
            "re-gathered account must end up Scored"
        );
    }

    // ── Test 5: clean Done clears staging ──
    #[tokio::test]
    async fn clean_done_clears_staging() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();
        let acct = "did:plc:orchdone00000000000000";

        let mut by_handle = HashMap::new();
        by_handle.insert("done.bsky.social".to_string(), survivor_sample("done"));
        let fetcher = MapFetcher { by_handle };
        let scorer = FixedScorer(0.0);
        let clean = MarkerCleanPass;
        let classifier: Arc<dyn ToxicityClassifier> = Arc::new(AlwaysOkClassifier);

        let candidates = vec![candidate(acct, "done.bsky.social")];

        run_phased_scan(
            &db,
            ORCH_USER,
            &candidates,
            &deps(&fetcher, &scorer, &clean, &classifier, &fp, &weights),
        )
        .await
        .unwrap();

        assert_eq!(
            db.count_pending_classifications(ORCH_USER).await.unwrap(),
            0,
            "clear_scan_staging empties the queue"
        );
        assert!(
            db.list_scan_accounts(ORCH_USER).await.unwrap().is_empty(),
            "clear_scan_staging empties scan_account_input"
        );
    }

    // ── Test: fresh-start wipe runs when phase is None or Done ──
    #[tokio::test]
    async fn fresh_start_wipes_stale_staging() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();
        let acct = "did:plc:orchstale0000000000000";

        // Leftover stale staging from a "prior run" with NO phase marker set.
        let stale = QueueRow {
            account_did: acct.to_string(),
            post_uri: format!("at://{acct}/stale"),
            text: "stale".to_string(),
            context_text: None,
            post_kind: "original".to_string(),
            onnx_score: 0.3,
            status: "pending".to_string(),
            toxic_token: None,
            confidence: None,
            model_id: None,
            policy_version: None,
        };
        db.enqueue_classifications(ORCH_USER, &[stale])
            .await
            .unwrap();
        // No scan_phase set → fresh start path must clear that stale row.

        // Empty candidate list: gather does nothing, burst drains nothing,
        // finalize finds no accounts (because the stale row was wiped).
        let fetcher = MapFetcher {
            by_handle: HashMap::new(),
        };
        let scorer = FixedScorer(0.0);
        let clean = MarkerCleanPass;
        let classifier: Arc<dyn ToxicityClassifier> = Arc::new(AlwaysOkClassifier);

        let summary = run_phased_scan(
            &db,
            ORCH_USER,
            &[],
            &deps(&fetcher, &scorer, &clean, &classifier, &fp, &weights),
        )
        .await
        .unwrap();

        assert_eq!(summary.accounts_scored, 0);
        assert_eq!(
            db.count_pending_classifications(ORCH_USER).await.unwrap(),
            0,
            "fresh start must wipe the stale leftover row"
        );
    }

    // ── Test: unknown scan_phase marker fails closed (does NOT wipe staging) ──
    #[tokio::test]
    async fn unknown_scan_phase_fails_closed() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();
        let acct = "did:plc:orchunkn0000000000000";

        // Stage a resumable pending row, then write a corrupt/unknown phase
        // marker. A fresh-start path would wipe this row — fail-closed must not.
        let row = QueueRow {
            account_did: acct.to_string(),
            post_uri: format!("at://{acct}/keep"),
            text: "keep me".to_string(),
            context_text: None,
            post_kind: "original".to_string(),
            onnx_score: 0.3,
            status: "pending".to_string(),
            toxic_token: None,
            confidence: None,
            model_id: None,
            policy_version: None,
        };
        db.enqueue_classifications(ORCH_USER, &[row]).await.unwrap();
        db.set_scan_state(ORCH_USER, "scan_phase", "frobnicate")
            .await
            .unwrap();

        let fetcher = MapFetcher {
            by_handle: HashMap::new(),
        };
        let scorer = FixedScorer(0.0);
        let clean = MarkerCleanPass;
        let classifier: Arc<dyn ToxicityClassifier> = Arc::new(AlwaysOkClassifier);

        let result = run_phased_scan(
            &db,
            ORCH_USER,
            &[],
            &deps(&fetcher, &scorer, &clean, &classifier, &fp, &weights),
        )
        .await;

        assert!(
            result.is_err(),
            "an unknown scan_phase marker must error, not fresh-start"
        );
        assert_eq!(
            db.count_pending_classifications(ORCH_USER).await.unwrap(),
            1,
            "fail-closed must NOT wipe the resumable staging row"
        );
    }

    // ── Test: a gather panic is caught, panicking account is skipped, scan
    //         completes for the other account and is marked degraded ──
    //
    // This is the regression guard for the `catch_unwind` fix (chainlink #177).
    // Before the fix, a panic inside `gather_one` (e.g. atrium-api `unwrap()`
    // on a truncated API response) would unwind `buffer_unordered` and kill the
    // entire scan. After the fix, the panic is caught, turned into a warn+skip,
    // the healthy account still gets gathered → burst → finalized, and the
    // summary reports `degraded = true` to signal the scan is incomplete.
    //
    // Without the fix this test itself would propagate the panic and fail.
    struct PartialPanicFetcher {
        /// Handle for the account that should panic.
        panicking_handle: String,
    }

    #[async_trait]
    impl PostFetcher for PartialPanicFetcher {
        async fn fetch_sample(&self, handle: &str, _limit: usize) -> Result<PostSample> {
            if handle == self.panicking_handle {
                panic!("simulated atrium-api unwrap panic: premature end of input");
            }
            // Return a normal survivor sample for every other handle.
            Ok(PostSample {
                originals: (0..6)
                    .map(|i| {
                        make_post(
                            &format!("at://{handle}/o/{i}"),
                            "quasar nebula redshift telescope galaxy cosmology pulsar photon",
                        )
                    })
                    .chain(std::iter::once(make_post(
                        &format!("at://{handle}/o/survivor"),
                        "quasar SURVIVOR nebula",
                    )))
                    .collect(),
                replies: vec![],
                quotes: vec![],
                reply_ratio: 0.0,
                quote_ratio: 0.0,
                total_posts: 7,
            })
        }
        async fn fetch_parents(&self, _uris: &[String]) -> Result<HashMap<String, String>> {
            Ok(HashMap::new())
        }
    }

    #[tokio::test]
    async fn gather_panic_is_isolated_and_healthy_account_still_scores() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();

        let panicking_acct = "did:plc:orchpanic0000000000000";
        let healthy_acct = "did:plc:orchhealthy000000000";

        let fetcher = PartialPanicFetcher {
            panicking_handle: "panicking.bsky.social".to_string(),
        };
        let scorer = FixedScorer(0.0);
        let clean = MarkerCleanPass;
        let classifier: Arc<dyn ToxicityClassifier> = Arc::new(AlwaysOkClassifier);

        let candidates = vec![
            candidate(panicking_acct, "panicking.bsky.social"),
            candidate(healthy_acct, "healthy.bsky.social"),
        ];

        // This call must RETURN Ok(…) — the panic must not propagate.
        let summary = run_phased_scan(
            &db,
            ORCH_USER,
            &candidates,
            &deps(&fetcher, &scorer, &clean, &classifier, &fp, &weights),
        )
        .await
        .unwrap();

        // The panicking account is skipped → scan is degraded.
        assert!(
            summary.degraded,
            "a gather panic must mark the scan degraded"
        );
        // The healthy account was still gathered, burst, and finalized.
        assert!(
            summary.accounts_scored >= 1,
            "the healthy account must still be scored despite the panic on its sibling"
        );
        // Panicking account has no score (it was skipped).
        assert!(
            db.get_account_by_did(ORCH_USER, panicking_acct)
                .await
                .unwrap()
                .is_none(),
            "panicking account must have no score (it was skipped)"
        );
        // Healthy account was scored.
        assert!(
            db.get_account_by_did(ORCH_USER, healthy_acct)
                .await
                .unwrap()
                .is_some(),
            "healthy account must have been scored"
        );
        // Scan still reaches Done (the skip is tolerated, not fatal).
        assert_eq!(
            db.get_scan_state(ORCH_USER, "scan_phase").await.unwrap(),
            Some("done".to_string()),
            "scan must reach Done despite the gather panic"
        );
    }

    // ── Test: a per-account gather failure marks the scan degraded ──
    // The resilient-gather path skips a failing account and continues; the
    // skipped account was never enqueued, so the scan is incomplete and the
    // summary must report `degraded = true` (the scan reaches Done either way).
    #[tokio::test]
    async fn gather_failure_marks_scan_degraded() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();
        let acct = "did:plc:orchgfail0000000000000";

        // FailingFetcher → gather_account errors → account skipped.
        let fetcher = FailingFetcher;
        let scorer = FixedScorer(0.0);
        let clean = MarkerCleanPass;
        let classifier: Arc<dyn ToxicityClassifier> = Arc::new(AlwaysOkClassifier);

        let candidates = vec![candidate(acct, "gfail.bsky.social")];

        let summary = run_phased_scan(
            &db,
            ORCH_USER,
            &candidates,
            &deps(&fetcher, &scorer, &clean, &classifier, &fp, &weights),
        )
        .await
        .unwrap();

        assert!(
            summary.degraded,
            "a skipped (failed) gather must mark the scan degraded"
        );
        assert_eq!(
            summary.accounts_scored, 0,
            "the failing account was never scored"
        );
        // The scan still completes to Done (the skip is tolerated, not fatal).
        assert_eq!(
            db.get_scan_state(ORCH_USER, "scan_phase").await.unwrap(),
            Some("done".to_string())
        );
        // No score was written for the skipped account.
        assert!(db
            .get_account_by_did(ORCH_USER, acct)
            .await
            .unwrap()
            .is_none());
    }
}
