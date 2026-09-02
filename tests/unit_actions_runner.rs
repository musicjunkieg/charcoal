//! ActionRunner against a wiremock PDS (#315, spec §4 + §6 failure table).
#![cfg(feature = "web")]

use std::sync::Arc;

use atproto_identity::key::{generate_key, KeyType};
use atproto_oauth::workflow::{OAuthClient, TokenResponse};
use charcoal::config::Config;
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::traits::NewAction;
use charcoal::db::Database;
use charcoal::web::actions::runner::{ActionRunner, RunnerConfig};
use charcoal::web::actions::session::SessionStore;
use rusqlite::Connection;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const ME: &str = "did:plc:runnertest0000000000000";

struct Harness {
    db: Arc<dyn Database>,
    mock: MockServer,
    runner: ActionRunner,
}

async fn harness() -> Harness {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    let db: Arc<dyn Database> = Arc::new(SqliteDatabase::new(conn));
    let mock = MockServer::start().await;
    let sessions = Arc::new(SessionStore::from_config(&Config::test_defaults()).unwrap());
    let key = generate_key(KeyType::P256Private).unwrap();
    sessions
        .store(
            db.as_ref(),
            ME,
            &mock.uri(),
            &key,
            &TokenResponse {
                access_token: "acc".into(),
                token_type: "DPoP".into(),
                refresh_token: Some("ref".into()),
                scope: "atproto".into(),
                expires_in: 3600,
                sub: Some(ME.into()),
                extra: Default::default(),
            },
        )
        .await
        .unwrap();
    let oauth_client = OAuthClient {
        redirect_uri: "https://charcoal.test/api/auth/callback".into(),
        client_id: "https://charcoal.test/client-metadata.json".into(),
        private_signing_key_data: generate_key(KeyType::P256Private).unwrap(),
    };
    let runner = ActionRunner::new(
        db.clone(),
        reqwest::Client::new(),
        oauth_client,
        sessions,
        RunnerConfig::fast(),
    );
    Harness { db, mock, runner }
}

fn new_action(target: &str, kind: &str, undo_of: Option<i64>) -> NewAction {
    NewAction {
        target_did: target.to_string(),
        kind: kind.to_string(),
        undo_of,
        score_at_action: Some(42.0),
        tier_at_action: Some("High".to_string()),
    }
}

async fn mount_list(mock: &MockServer, nsid: &str, key: &str, items: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(format!("/xrpc/{nsid}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ key: items })))
        .mount(mock)
        .await;
}

/// Matches applyWrites bodies by entry count and (optionally) one subject
/// or rkey present among the entries. `body_partial_json` cannot express
/// "exactly N entries", and chunk-vs-single is the whole point here.
struct Writes {
    len: usize,
    containing: Option<&'static str>,
}

impl wiremock::Match for Writes {
    fn matches(&self, request: &Request) -> bool {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
            return false;
        };
        let Some(w) = v["writes"].as_array() else {
            return false;
        };
        w.len() == self.len
            && self.containing.is_none_or(|s| {
                w.iter()
                    .any(|e| e["value"]["subject"] == s || e["rkey"] == s)
            })
    }
}

fn statuses(rows: &[charcoal::db::traits::ActionRow]) -> Vec<(String, String)> {
    rows.iter()
        .map(|r| (r.target_did.clone(), r.status.clone()))
        .collect()
}

#[tokio::test]
async fn mute_batch_skips_existing_and_mutes_the_rest() {
    let h = harness().await;
    mount_list(
        &h.mock,
        "app.bsky.graph.getMutes",
        "mutes",
        serde_json::json!([{ "did": "did:plc:m1", "handle": "m1.test" }]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .and(body_partial_json(
            serde_json::json!({ "actor": "did:plc:m2" }),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&h.mock)
        .await;

    let id =
        h.db.create_action_batch(
            ME,
            "mute",
            "tier:High",
            &[
                new_action("did:plc:m1", "mute", None),
                new_action("did:plc:m2", "mute", None),
            ],
        )
        .await
        .unwrap();
    h.runner.run_batch(id).await;

    let rows = h.db.list_actions_for_batch(id).await.unwrap();
    assert_eq!(
        statuses(&rows),
        vec![
            ("did:plc:m1".into(), "skipped_already_done".into()),
            ("did:plc:m2".into(), "applied".into()),
        ]
    );
    let b = h.db.get_action_batch(id).await.unwrap().unwrap();
    assert_eq!(b.status, "done");
    assert!(b.started_at.is_some() && b.finished_at.is_some());
}

#[tokio::test]
async fn block_batch_uses_one_chunk_and_stores_record_uris() {
    let h = harness().await;
    mount_list(
        &h.mock,
        "app.bsky.graph.getBlocks",
        "blocks",
        serde_json::json!([]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.repo.applyWrites"))
        .and(Writes { len: 3, containing: None })
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "results": [
                { "$type": "com.atproto.repo.applyWrites#createResult", "uri": format!("at://{ME}/app.bsky.graph.block/r1"), "cid": "c" },
                { "$type": "com.atproto.repo.applyWrites#createResult", "uri": format!("at://{ME}/app.bsky.graph.block/r2"), "cid": "c" },
                { "$type": "com.atproto.repo.applyWrites#createResult", "uri": format!("at://{ME}/app.bsky.graph.block/r3"), "cid": "c" }
            ]
        })))
        .expect(1)
        .mount(&h.mock)
        .await;

    let id =
        h.db.create_action_batch(
            ME,
            "block",
            "tier:High",
            &[
                new_action("did:plc:b1", "block", None),
                new_action("did:plc:b2", "block", None),
                new_action("did:plc:b3", "block", None),
            ],
        )
        .await
        .unwrap();
    h.runner.run_batch(id).await;

    let rows = h.db.list_actions_for_batch(id).await.unwrap();
    assert!(rows.iter().all(|r| r.status == "applied"));
    assert_eq!(
        rows[1].record_uri.as_deref(),
        Some(format!("at://{ME}/app.bsky.graph.block/r2").as_str())
    );
    assert_eq!(
        h.db.get_action_batch(id).await.unwrap().unwrap().status,
        "done"
    );
}

/// `results` entries for chunk positions `start..start + len`, so a test can
/// assert each row got the URI its own index produced.
fn create_results(start: usize, len: usize) -> serde_json::Value {
    let items: Vec<serde_json::Value> = (start..start + len)
        .map(|i| {
            serde_json::json!({
                "$type": "com.atproto.repo.applyWrites#createResult",
                "uri": format!("at://{ME}/app.bsky.graph.block/r{i}"),
                "cid": "c"
            })
        })
        .collect();
    serde_json::json!({ "results": items })
}

/// Spec §10: a batch larger than `APPLY_WRITES_MAX` must split, and the URIs
/// must zip back to the right rows ACROSS the chunk boundary — the one place
/// an off-by-one would silently hand row 200 row 0's record.
#[tokio::test]
async fn block_batch_over_two_hundred_splits_and_zips_uris_across_the_boundary() {
    let h = harness().await;
    mount_list(
        &h.mock,
        "app.bsky.graph.getBlocks",
        "blocks",
        serde_json::json!([]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.repo.applyWrites"))
        .and(Writes {
            len: 200,
            containing: None,
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(create_results(0, 200)))
        .expect(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.repo.applyWrites"))
        .and(Writes {
            len: 50,
            containing: None,
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(create_results(200, 50)))
        .expect(1)
        .mount(&h.mock)
        .await;

    let targets: Vec<NewAction> = (0..250)
        .map(|i| new_action(&format!("did:plc:chunk{i:04}"), "block", None))
        .collect();
    let id =
        h.db.create_action_batch(ME, "block", "tier:High", &targets)
            .await
            .unwrap();
    h.runner.run_batch(id).await;

    let rows = h.db.list_actions_for_batch(id).await.unwrap();
    assert_eq!(rows.len(), 250);
    assert!(
        rows.iter().all(|r| r.status == "applied"),
        "every row should be applied"
    );
    for (i, r) in rows.iter().enumerate() {
        assert_eq!(
            r.record_uri.as_deref(),
            Some(format!("at://{ME}/app.bsky.graph.block/r{i}").as_str()),
            "row {i} got the wrong record URI"
        );
    }
    assert_eq!(
        h.db.get_action_batch(id).await.unwrap().unwrap().status,
        "done"
    );
}

/// The highest-consequence idempotency claim in §4.5: resuming a half-applied
/// BLOCK batch must not create a second record in the user's repo.
#[tokio::test]
async fn block_batch_resume_never_recreates_the_row_already_applied() {
    let h = harness().await;
    let ours = format!("at://{ME}/app.bsky.graph.block/r0");
    let id =
        h.db.create_action_batch(
            ME,
            "block",
            "tier:High",
            &[
                new_action("did:plc:b1", "block", None),
                new_action("did:plc:b2", "block", None),
                new_action("did:plc:b3", "block", None),
            ],
        )
        .await
        .unwrap();
    let rows = h.db.list_actions_for_batch(id).await.unwrap();
    // Simulate a deploy that died after b1's create landed.
    h.db.update_action(rows[0].id, "applied", Some(&ours), None)
        .await
        .unwrap();
    h.db.set_action_batch_status(id, "running", None)
        .await
        .unwrap();

    mount_list(
        &h.mock,
        "app.bsky.graph.getBlocks",
        "blocks",
        serde_json::json!([
            { "did": "did:plc:b1", "handle": "b1", "viewer": { "blocking": ours.clone() } }
        ]),
    )
    .await;
    // Exactly two entries: a third would be the duplicate block record.
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.repo.applyWrites"))
        .and(Writes {
            len: 2,
            containing: Some("did:plc:b2"),
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(create_results(2, 2)))
        .expect(1)
        .mount(&h.mock)
        .await;

    h.runner.run_all_unfinished().await;

    let rows = h.db.list_actions_for_batch(id).await.unwrap();
    assert_eq!(rows[0].status, "applied");
    assert_eq!(
        rows[0].record_uri.as_deref(),
        Some(ours.as_str()),
        "the resumed row's stored URI must not be rewritten"
    );
    assert_eq!(
        rows[1].record_uri.as_deref(),
        Some(format!("at://{ME}/app.bsky.graph.block/r2").as_str())
    );
    assert_eq!(
        h.db.get_action_batch(id).await.unwrap().unwrap().status,
        "done"
    );
}

/// A create the PDS accepted but answered without a URI is un-undoable. It is
/// recorded `failed` so the batch reads `partial` and Retry can re-attempt it
/// — never `applied` with a NULL `record_uri`.
#[tokio::test]
async fn create_with_no_returned_uri_is_recorded_failed() {
    let h = harness().await;
    mount_list(
        &h.mock,
        "app.bsky.graph.getBlocks",
        "blocks",
        serde_json::json!([]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.repo.applyWrites"))
        .and(Writes {
            len: 2,
            containing: None,
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "results": [
                { "$type": "com.atproto.repo.applyWrites#createResult", "uri": format!("at://{ME}/app.bsky.graph.block/r0"), "cid": "c" },
                { "$type": "com.atproto.repo.applyWrites#createResult", "cid": "c" }
            ]
        })))
        .expect(1)
        .mount(&h.mock)
        .await;

    let id =
        h.db.create_action_batch(
            ME,
            "block",
            "tier:High",
            &[
                new_action("did:plc:b1", "block", None),
                new_action("did:plc:b2", "block", None),
            ],
        )
        .await
        .unwrap();
    h.runner.run_batch(id).await;

    let rows = h.db.list_actions_for_batch(id).await.unwrap();
    assert_eq!(rows[0].status, "applied");
    assert_eq!(rows[1].status, "failed");
    assert_eq!(rows[1].error.as_deref(), Some("PDS returned no record URI"));
    assert!(rows[1].record_uri.is_none());
    assert_eq!(
        h.db.get_action_batch(id).await.unwrap().unwrap().status,
        "partial"
    );
}

/// A reconcile read that never succeeds fails the BATCH and leaves every row
/// `pending` — which is what Retry re-queues (I2).
#[tokio::test]
async fn reconcile_failure_fails_the_batch_and_leaves_rows_pending() {
    let h = harness().await;
    Mock::given(method("GET"))
        .and(path("/xrpc/app.bsky.graph.getBlocks"))
        .respond_with(ResponseTemplate::new(500))
        .expect(4) // 1 + 3 transient retries
        .mount(&h.mock)
        .await;

    let id =
        h.db.create_action_batch(
            ME,
            "block",
            "tier:High",
            &[
                new_action("did:plc:b1", "block", None),
                new_action("did:plc:b2", "block", None),
            ],
        )
        .await
        .unwrap();
    h.runner.run_batch(id).await;

    let b = h.db.get_action_batch(id).await.unwrap().unwrap();
    assert_eq!(b.status, "failed");
    assert!(
        b.error.as_deref().is_some_and(|e| e.contains("getBlocks")),
        "the stored error should name the failed call: {:?}",
        b.error
    );
    assert!(h
        .db
        .list_actions_for_batch(id)
        .await
        .unwrap()
        .iter()
        .all(|r| r.status == "pending"));
}

#[tokio::test]
async fn block_chunk_4xx_falls_back_one_at_a_time_and_batch_is_partial() {
    let h = harness().await;
    mount_list(
        &h.mock,
        "app.bsky.graph.getBlocks",
        "blocks",
        serde_json::json!([]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.repo.applyWrites"))
        .and(Writes {
            len: 2,
            containing: None,
        })
        .respond_with(ResponseTemplate::new(400).set_body_json(
            serde_json::json!({ "error": "InvalidRequest", "message": "bad subject" }),
        ))
        .expect(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST")).and(path("/xrpc/com.atproto.repo.applyWrites"))
        .and(Writes { len: 1, containing: Some("did:plc:good") })
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "results": [{ "$type": "com.atproto.repo.applyWrites#createResult", "uri": format!("at://{ME}/app.bsky.graph.block/ok"), "cid": "c" }]
        })))
        .expect(1).mount(&h.mock).await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.repo.applyWrites"))
        .and(Writes {
            len: 1,
            containing: Some("did:plc:bad"),
        })
        .respond_with(ResponseTemplate::new(400).set_body_json(
            serde_json::json!({ "error": "InvalidRequest", "message": "bad subject" }),
        ))
        .expect(1)
        .mount(&h.mock)
        .await;

    let id =
        h.db.create_action_batch(
            ME,
            "block",
            "tier:High",
            &[
                new_action("did:plc:good", "block", None),
                new_action("did:plc:bad", "block", None),
            ],
        )
        .await
        .unwrap();
    h.runner.run_batch(id).await;

    let rows = h.db.list_actions_for_batch(id).await.unwrap();
    assert_eq!(rows[0].status, "applied");
    assert_eq!(
        rows[0].record_uri.as_deref(),
        Some(format!("at://{ME}/app.bsky.graph.block/ok").as_str())
    );
    assert_eq!(rows[1].status, "failed");
    assert_eq!(
        rows[1].error.as_deref(),
        Some("400: InvalidRequest: bad subject")
    );
    assert_eq!(
        h.db.get_action_batch(id).await.unwrap().unwrap().status,
        "partial"
    );
}

#[tokio::test]
async fn rate_limit_pauses_then_retries() {
    let h = harness().await;
    mount_list(
        &h.mock,
        "app.bsky.graph.getMutes",
        "mutes",
        serde_json::json!([]),
    )
    .await;
    let reset = chrono::Utc::now().timestamp() + 30; // capped to max_wait (50 ms) by RunnerConfig::fast
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .respond_with(
            ResponseTemplate::new(429).insert_header("ratelimit-reset", reset.to_string().as_str()),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&h.mock)
        .await;

    let id =
        h.db.create_action_batch(
            ME,
            "mute",
            "tier:High",
            &[new_action("did:plc:m1", "mute", None)],
        )
        .await
        .unwrap();
    let started = std::time::Instant::now();
    h.runner.run_batch(id).await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "max_wait cap not applied"
    );
    assert_eq!(
        h.db.list_actions_for_batch(id).await.unwrap()[0].status,
        "applied"
    );
}

#[tokio::test]
async fn server_error_retries_three_times_then_fails_action() {
    let h = harness().await;
    mount_list(
        &h.mock,
        "app.bsky.graph.getMutes",
        "mutes",
        serde_json::json!([]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .respond_with(ResponseTemplate::new(503))
        .expect(4) // 1 + 3 retries
        .mount(&h.mock)
        .await;

    let id =
        h.db.create_action_batch(
            ME,
            "mute",
            "tier:High",
            &[new_action("did:plc:m1", "mute", None)],
        )
        .await
        .unwrap();
    h.runner.run_batch(id).await;
    let row = &h.db.list_actions_for_batch(id).await.unwrap()[0];
    assert_eq!(row.status, "failed");
    assert_eq!(row.error.as_deref(), Some("server error 503"));
    assert_eq!(
        h.db.get_action_batch(id).await.unwrap().unwrap().status,
        "failed"
    );
}

#[tokio::test]
async fn pds_401_disconnects_and_requeues_with_not_connected() {
    let h = harness().await;
    mount_list(
        &h.mock,
        "app.bsky.graph.getMutes",
        "mutes",
        serde_json::json!([]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(serde_json::json!({ "error": "ExpiredToken" })),
        )
        .mount(&h.mock)
        .await;

    let id =
        h.db.create_action_batch(
            ME,
            "mute",
            "tier:High",
            &[
                new_action("did:plc:m1", "mute", None),
                new_action("did:plc:m2", "mute", None),
            ],
        )
        .await
        .unwrap();
    h.runner.run_batch(id).await;

    let b = h.db.get_action_batch(id).await.unwrap().unwrap();
    assert_eq!(
        (b.status.as_str(), b.error.as_deref()),
        ("queued", Some("not_connected"))
    );
    assert!(h.db.get_oauth_session(ME).await.unwrap().is_none());
    assert!(h
        .db
        .list_actions_for_batch(id)
        .await
        .unwrap()
        .iter()
        .all(|r| r.status == "pending"));
}

#[tokio::test]
async fn no_session_leaves_batch_queued_not_connected() {
    let h = harness().await;
    h.db.delete_oauth_session(ME).await.unwrap();
    let id =
        h.db.create_action_batch(
            ME,
            "mute",
            "tier:High",
            &[new_action("did:plc:m1", "mute", None)],
        )
        .await
        .unwrap();
    h.runner.run_batch(id).await;
    let b = h.db.get_action_batch(id).await.unwrap().unwrap();
    assert_eq!(
        (b.status.as_str(), b.error.as_deref()),
        ("queued", Some("not_connected"))
    );
}

#[tokio::test]
async fn resume_only_sends_pending_actions() {
    let h = harness().await;
    mount_list(
        &h.mock,
        "app.bsky.graph.getMutes",
        "mutes",
        serde_json::json!([]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .and(body_partial_json(
            serde_json::json!({ "actor": "did:plc:m2" }),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&h.mock)
        .await;

    let id =
        h.db.create_action_batch(
            ME,
            "mute",
            "tier:High",
            &[
                new_action("did:plc:m1", "mute", None),
                new_action("did:plc:m2", "mute", None),
            ],
        )
        .await
        .unwrap();
    let rows = h.db.list_actions_for_batch(id).await.unwrap();
    // Simulate a deploy that died after m1 was applied.
    h.db.update_action(rows[0].id, "applied", None, None)
        .await
        .unwrap();
    h.db.set_action_batch_status(id, "running", None)
        .await
        .unwrap();

    h.runner.run_all_unfinished().await;
    assert_eq!(
        h.db.get_action_batch(id).await.unwrap().unwrap().status,
        "done"
    );
}

#[tokio::test]
async fn finished_batch_is_a_noop() {
    let h = harness().await;
    let id =
        h.db.create_action_batch(
            ME,
            "mute",
            "tier:High",
            &[new_action("did:plc:m1", "mute", None)],
        )
        .await
        .unwrap();
    h.db.set_action_batch_status(id, "done", None)
        .await
        .unwrap();
    h.runner.run_batch(id).await; // no mocks mounted: any HTTP call would 404 → fail
    assert_eq!(
        h.db.get_action_batch(id).await.unwrap().unwrap().status,
        "done"
    );
}

#[tokio::test]
async fn undo_block_deletes_only_charcoals_record() {
    let h = harness().await;
    let our_uri = format!("at://{ME}/app.bsky.graph.block/ours");
    // Original batch: t1 blocked by us and still blocked, t2 blocked by us but
    // since unblocked by hand.
    let orig =
        h.db.create_action_batch(
            ME,
            "block",
            "tier:High",
            &[
                new_action("did:plc:t1", "block", None),
                new_action("did:plc:t2", "block", None),
            ],
        )
        .await
        .unwrap();
    let orig_rows = h.db.list_actions_for_batch(orig).await.unwrap();
    h.db.update_action(orig_rows[0].id, "applied", Some(&our_uri), None)
        .await
        .unwrap();
    h.db.update_action(
        orig_rows[1].id,
        "applied",
        Some(&format!("at://{ME}/app.bsky.graph.block/gone")),
        None,
    )
    .await
    .unwrap();
    h.db.set_action_batch_status(orig, "done", None)
        .await
        .unwrap();

    // t2 is absent from getBlocks entirely: reality already matches the undo.
    mount_list(
        &h.mock,
        "app.bsky.graph.getBlocks",
        "blocks",
        serde_json::json!([
            { "did": "did:plc:t1", "handle": "t1", "viewer": { "blocking": our_uri } }
        ]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.repo.applyWrites"))
        .and(Writes {
            len: 1,
            containing: Some("ours"),
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "results": [{ "$type": "com.atproto.repo.applyWrites#deleteResult" }]
        })))
        .expect(1)
        .mount(&h.mock)
        .await;

    let undo =
        h.db.create_action_batch(
            ME,
            "undo",
            &format!("undo:{orig}"),
            &[
                new_action("did:plc:t1", "block", Some(orig_rows[0].id)),
                new_action("did:plc:t2", "block", Some(orig_rows[1].id)),
            ],
        )
        .await
        .unwrap();
    h.runner.run_batch(undo).await;

    let undo_rows = h.db.list_actions_for_batch(undo).await.unwrap();
    assert_eq!(undo_rows[0].status, "applied");
    assert_eq!(undo_rows[1].status, "skipped_already_done");
    let orig_rows = h.db.list_actions_for_batch(orig).await.unwrap();
    assert!(orig_rows
        .iter()
        .all(|r| r.status == "undone" && r.undone_at.is_some()));
    assert_eq!(
        h.db.get_action_batch(undo).await.unwrap().unwrap().status,
        "done"
    );
}

/// The handlers no longer enqueue undos for rows Charcoal did not apply, but
/// the runner refuses them anyway — mutes carry no `record_uri`, so this is
/// the last guard between "Undo all" and the user's own mute list (#261).
#[tokio::test]
async fn undo_refuses_an_original_charcoal_did_not_apply() {
    let h = harness().await;
    let orig =
        h.db.create_action_batch(
            ME,
            "mute",
            "tier:High",
            &[new_action("did:plc:m1", "mute", None)],
        )
        .await
        .unwrap();
    let a1 = h.db.list_actions_for_batch(orig).await.unwrap()[0].id;
    h.db.update_action(a1, "skipped_already_done", None, None)
        .await
        .unwrap();
    // Still muted — but the user muted them, not Charcoal.
    mount_list(
        &h.mock,
        "app.bsky.graph.getMutes",
        "mutes",
        serde_json::json!([{ "did": "did:plc:m1", "handle": "m1" }]),
    )
    .await;
    // No unmuteActor mock: a call would 404 and change the error text below.

    let undo =
        h.db.create_action_batch(
            ME,
            "undo",
            &format!("undo:{orig}"),
            &[new_action("did:plc:m1", "mute", Some(a1))],
        )
        .await
        .unwrap();
    h.runner.run_batch(undo).await;

    let u = &h.db.list_actions_for_batch(undo).await.unwrap()[0];
    assert_eq!(u.status, "failed");
    assert_eq!(u.error.as_deref(), Some("not created by Charcoal"));
    let o = h.db.get_action(a1).await.unwrap().unwrap();
    assert_eq!(
        o.status, "skipped_already_done",
        "the original is untouched"
    );
    assert!(o.undone_at.is_none());
}

#[tokio::test]
async fn undo_block_fails_honestly_when_the_block_in_force_is_not_ours() {
    let h = harness().await;
    let orig =
        h.db.create_action_batch(
            ME,
            "block",
            "tier:High",
            &[new_action("did:plc:t1", "block", None)],
        )
        .await
        .unwrap();
    let a1 = h.db.list_actions_for_batch(orig).await.unwrap()[0].id;
    h.db.update_action(
        a1,
        "applied",
        Some(&format!("at://{ME}/app.bsky.graph.block/ours")),
        None,
    )
    .await
    .unwrap();
    // The user deleted our block and made their own: a different rkey.
    mount_list(&h.mock, "app.bsky.graph.getBlocks", "blocks", serde_json::json!([
        { "did": "did:plc:t1", "handle": "t1", "viewer": { "blocking": format!("at://{ME}/app.bsky.graph.block/theirs") } }
    ])).await;
    // No applyWrites mock: a call would 404 and fail the action.

    let undo =
        h.db.create_action_batch(
            ME,
            "undo",
            &format!("undo:{orig}"),
            &[new_action("did:plc:t1", "block", Some(a1))],
        )
        .await
        .unwrap();
    h.runner.run_batch(undo).await;
    // The block is still live, so "undone" would be a lie. The undo row
    // carries the reason and the original keeps its status.
    let u = &h.db.list_actions_for_batch(undo).await.unwrap()[0];
    assert_eq!(u.status, "failed");
    assert_eq!(
        u.error.as_deref(),
        Some("block was not created by Charcoal")
    );
    let o = h.db.get_action(a1).await.unwrap().unwrap();
    assert_eq!(o.status, "applied");
    assert!(o.undone_at.is_none());
}

#[tokio::test]
async fn undo_mute_unmutes_when_still_muted() {
    let h = harness().await;
    let orig =
        h.db.create_action_batch(
            ME,
            "mute",
            "tier:High",
            &[
                new_action("did:plc:m1", "mute", None),
                new_action("did:plc:m2", "mute", None),
            ],
        )
        .await
        .unwrap();
    let rows = h.db.list_actions_for_batch(orig).await.unwrap();
    for r in &rows {
        h.db.update_action(r.id, "applied", None, None)
            .await
            .unwrap();
    }
    // m2 is no longer muted (user unmuted by hand) — skip it.
    mount_list(
        &h.mock,
        "app.bsky.graph.getMutes",
        "mutes",
        serde_json::json!([{ "did": "did:plc:m1", "handle": "m1" }]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.unmuteActor"))
        .and(body_partial_json(
            serde_json::json!({ "actor": "did:plc:m1" }),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&h.mock)
        .await;

    let undo =
        h.db.create_action_batch(
            ME,
            "undo",
            &format!("undo:{orig}"),
            &[
                new_action("did:plc:m1", "mute", Some(rows[0].id)),
                new_action("did:plc:m2", "mute", Some(rows[1].id)),
            ],
        )
        .await
        .unwrap();
    h.runner.run_batch(undo).await;
    let u = h.db.list_actions_for_batch(undo).await.unwrap();
    assert_eq!(
        (u[0].status.as_str(), u[1].status.as_str()),
        ("applied", "skipped_already_done")
    );
    assert!(h
        .db
        .list_actions_for_batch(orig)
        .await
        .unwrap()
        .iter()
        .all(|r| r.status == "undone"));
}
