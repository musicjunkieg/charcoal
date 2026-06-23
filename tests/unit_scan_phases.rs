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
            assert!(v.toxic_token.is_some());
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
        assert!(
            pending > 0,
            "some rows must still be pending after cost-cap"
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
    }
}
