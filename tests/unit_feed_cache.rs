//! Shared-cache DB methods (#343 §4.1) and the CachedPostFetcher decorator.

use std::collections::HashMap;
use std::sync::Arc;

use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::{ClassifierVerdictRow, Database, FeedSnapshot, OnnxScoreRow};
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
