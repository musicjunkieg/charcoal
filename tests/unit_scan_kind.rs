// #343 Phase 2 / #344: a second queue kind, `refresh`, in the one-row-per-user
// scan_queue. The rules under test keep a nightly refresh from ever costing a
// human their scan: an upgrade keeps their place, a request during a running
// refresh is honoured when it finishes, a refresh never downgrades, and the
// ETA median is sampled somewhere a refresh cannot erase it.

use std::sync::Arc;

use charcoal::db::schema::create_tables;
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::traits::LAST_FULL_SCAN_DURATION_KEY;
use charcoal::db::{Database, EnqueueOutcome, FinishCompletion, ScanKind};
use rusqlite::{params, Connection};

const USER: &str = "did:plc:kindtest0000000000000000";

fn db() -> Arc<SqliteDatabase> {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

async fn row(db: &SqliteDatabase, did: &str) -> charcoal::db::traits::ScanQueueRow {
    db.list_scan_queue()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.user_did == did)
        .expect("row exists")
}

#[tokio::test]
async fn refresh_enqueue_creates_a_refresh_row() {
    let db = db();
    db.enqueue_refresh_scan(USER).await.unwrap();
    let r = row(&db, USER).await;
    assert_eq!((r.status.as_str(), r.kind), ("queued", ScanKind::Refresh));
}

#[tokio::test]
async fn a_full_enqueue_upgrades_a_queued_refresh_in_place() {
    let db = db();
    db.enqueue_refresh_scan(USER).await.unwrap();
    let before = row(&db, USER).await.enqueued_at;
    assert_eq!(db.enqueue_scan(USER).await.unwrap(), EnqueueOutcome::Queued);
    let r = row(&db, USER).await;
    assert_eq!((r.status.as_str(), r.kind), ("queued", ScanKind::Full));
    assert_eq!(
        r.enqueued_at, before,
        "the user keeps the place the refresh held (R09)"
    );
}

#[tokio::test]
async fn a_refresh_enqueue_never_downgrades_a_queued_full() {
    let db = db();
    db.enqueue_scan(USER).await.unwrap();
    db.enqueue_refresh_scan(USER).await.unwrap();
    assert_eq!(row(&db, USER).await.kind, ScanKind::Full);
}

#[tokio::test]
async fn a_full_request_during_a_running_refresh_is_recorded_and_runs_after_it() {
    let db = db();
    db.enqueue_refresh_scan(USER).await.unwrap();
    let claim = db.claim_next_scan(1, 60).await.unwrap().expect("claimed");
    assert_eq!(claim.kind, ScanKind::Refresh);

    assert_eq!(
        db.enqueue_scan(USER).await.unwrap(),
        EnqueueOutcome::QueuedAfterRefresh
    );
    let r = row(&db, USER).await;
    assert_eq!(
        (r.status.as_str(), r.kind),
        ("running", ScanKind::Refresh),
        "the refresh keeps running"
    );
    let requested = r.full_requested_at.clone().expect("request recorded");
    // A second click does not move the request time.
    assert_eq!(
        db.enqueue_scan(USER).await.unwrap(),
        EnqueueOutcome::QueuedAfterRefresh
    );
    assert_eq!(
        row(&db, USER).await.full_requested_at.as_deref(),
        Some(requested.as_str())
    );

    // The refresh finishes: the row becomes the user's queued full scan,
    // dated from the request, not from now. The obligation stays recorded
    // until a full scan COMPLETES (V3-03).
    assert!(db
        .finish_queued_scan(USER, &claim.claim_id, FinishCompletion::Complete, None)
        .await
        .unwrap());
    let r = row(&db, USER).await;
    assert_eq!((r.status.as_str(), r.kind), ("queued", ScanKind::Full));
    assert_eq!(r.enqueued_at, requested);
    assert_eq!(
        r.full_requested_at.as_deref(),
        Some(requested.as_str()),
        "obligation kept through the handover"
    );
    let claim = db
        .claim_next_scan(1, 60)
        .await
        .unwrap()
        .expect("the full scan is admitted");
    assert_eq!(claim.kind, ScanKind::Full);
    assert!(db
        .finish_queued_scan(USER, &claim.claim_id, FinishCompletion::Complete, None)
        .await
        .unwrap());
    let r = row(&db, USER).await;
    assert!(r.full_requested_at.is_none(), "fulfilled");
    assert_eq!(r.completion, Some(FinishCompletion::Complete));
}

#[tokio::test]
async fn a_failed_refresh_still_hands_over_to_the_requested_full_scan() {
    let db = db();
    db.enqueue_refresh_scan(USER).await.unwrap();
    let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
    db.enqueue_scan(USER).await.unwrap();
    assert!(db
        .finish_queued_scan(
            USER,
            &claim.claim_id,
            FinishCompletion::Failed,
            Some("refresh blew up")
        )
        .await
        .unwrap());
    let r = row(&db, USER).await;
    assert_eq!((r.status.as_str(), r.kind), ("queued", ScanKind::Full));
    assert_eq!(
        r.last_error.as_deref(),
        Some("refresh blew up"),
        "the refresh's failure stays visible on the row"
    );
}

/// V3-03's other half, at the queue layer: a full scan that only got as far as
/// `Resumable` (or failed outright) keeps the obligation, so the tick can
/// retry it as full work; only a fulfilled completion clears it.
#[tokio::test]
async fn an_interrupted_full_scan_keeps_the_obligation_and_a_fulfilled_one_clears_it() {
    for interrupted in [FinishCompletion::Resumable, FinishCompletion::Failed] {
        let db = db();
        db.enqueue_scan(USER).await.unwrap();
        let requested = row(&db, USER).await.full_requested_at.expect("recorded");
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        let error = (interrupted == FinishCompletion::Failed).then_some("boom");
        db.finish_queued_scan(USER, &claim.claim_id, interrupted, error)
            .await
            .unwrap();
        let r = row(&db, USER).await;
        assert_eq!(r.completion, Some(interrupted));
        assert_eq!(
            r.full_requested_at.as_deref(),
            Some(requested.as_str()),
            "{interrupted:?} is not fulfilment — the full scan is still owed"
        );

        // The user clicks again: the obligation coalesces onto the first ask.
        assert_eq!(db.enqueue_scan(USER).await.unwrap(), EnqueueOutcome::Queued);
        assert_eq!(
            row(&db, USER).await.full_requested_at.as_deref(),
            Some(requested.as_str()),
            "a re-queue does not move the request time"
        );
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        db.finish_queued_scan(
            USER,
            &claim.claim_id,
            FinishCompletion::CompleteUnverified,
            None,
        )
        .await
        .unwrap();
        let r = row(&db, USER).await;
        assert!(
            r.full_requested_at.is_none(),
            "an unverified completion is still fulfilment (V6-01)"
        );
        assert_eq!(r.completion, Some(FinishCompletion::CompleteUnverified));
    }
}

/// A refresh that finishes with no full scan owed stays a refresh — the
/// handover arm must not fire on every refresh row.
#[tokio::test]
async fn an_unrequested_refresh_just_finishes() {
    let db = db();
    db.enqueue_refresh_scan(USER).await.unwrap();
    let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
    assert!(db
        .finish_queued_scan(USER, &claim.claim_id, FinishCompletion::Complete, None)
        .await
        .unwrap());
    let r = row(&db, USER).await;
    assert_eq!((r.status.as_str(), r.kind), ("done", ScanKind::Refresh));
    assert!(r.full_requested_at.is_none());
    assert!(r.finished_at.is_some());
}

/// `enqueue_refresh_scan` re-queues owed full work AS FULL — the same
/// conditional statement the scheduling tick uses (Task 6).
#[tokio::test]
async fn a_refresh_enqueue_over_an_owed_finished_row_re_queues_full_work() {
    let db = db();
    db.enqueue_scan(USER).await.unwrap();
    let requested = row(&db, USER).await.full_requested_at.expect("recorded");
    let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
    db.finish_queued_scan(USER, &claim.claim_id, FinishCompletion::Resumable, None)
        .await
        .unwrap();

    db.enqueue_refresh_scan(USER).await.unwrap();
    let r = row(&db, USER).await;
    assert_eq!(
        (r.status.as_str(), r.kind),
        ("queued", ScanKind::Full),
        "owed work is retried as a FULL scan, never a refresh"
    );
    assert_eq!(r.full_requested_at.as_deref(), Some(requested.as_str()));
    assert_eq!(r.completion, None, "a re-queue clears the stale completion");
}

/// A refresh enqueue must not touch a RUNNING row of either kind — the tick
/// would otherwise reset a job a worker is holding.
#[tokio::test]
async fn a_refresh_enqueue_never_touches_a_running_row() {
    let db = db();
    db.enqueue_scan(USER).await.unwrap();
    let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
    db.enqueue_refresh_scan(USER).await.unwrap();
    let r = row(&db, USER).await;
    assert_eq!((r.status.as_str(), r.kind), ("running", ScanKind::Full));
    // The claim still owns the row.
    assert!(db
        .finish_queued_scan(USER, &claim.claim_id, FinishCompletion::Complete, None)
        .await
        .unwrap());
}

#[tokio::test]
async fn enqueue_outcomes_name_the_state() {
    let db = db();
    assert_eq!(db.enqueue_scan(USER).await.unwrap(), EnqueueOutcome::Queued);
    assert_eq!(
        db.enqueue_scan(USER).await.unwrap(),
        EnqueueOutcome::AlreadyQueued
    );
    let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
    assert_eq!(
        db.enqueue_scan(USER).await.unwrap(),
        EnqueueOutcome::AlreadyRunning
    );
    db.finish_queued_scan(USER, &claim.claim_id, FinishCompletion::Complete, None)
        .await
        .unwrap();
    assert_eq!(db.enqueue_scan(USER).await.unwrap(), EnqueueOutcome::Queued);
}

/// Admission order is FIFO across kinds (#271): a refresh queued first is
/// admitted first, and the upgrade in place does not jump the queue.
#[tokio::test]
async fn admission_is_fifo_across_kinds() {
    const EARLY: &str = "did:plc:kindfifo_early";
    const LATE: &str = "did:plc:kindfifo_late";
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    conn.execute(
        "INSERT INTO scan_queue (user_did, status, kind, enqueued_at)
         VALUES (?1, 'queued', 'refresh', '2026-09-10T00:00:00+00:00'),
                (?2, 'queued', 'full',    '2026-09-11T00:00:00+00:00')",
        params![EARLY, LATE],
    )
    .unwrap();
    let db = SqliteDatabase::new(conn);
    let claim = db.claim_next_scan(2, 60).await.unwrap().unwrap();
    assert_eq!(
        (claim.user_did.as_str(), claim.kind),
        (EARLY, ScanKind::Refresh),
        "the older refresh is admitted before the newer full scan"
    );
}

/// #344 Minor 3: the `COALESCE` on the already-queued and already-running
/// arms is what records the obligation for a user whose own full scan is
/// already in flight. A row written by a pre-v18 binary carries
/// `full_requested_at IS NULL`, and the enqueue has to repair it — otherwise
/// an interrupted attempt on that row is never retried, because nothing says
/// a full scan is owed.
#[tokio::test]
async fn a_full_enqueue_records_the_obligation_on_a_queued_full_row_that_lacks_one() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    conn.execute(
        "INSERT INTO scan_queue (user_did, status, kind, enqueued_at, full_requested_at)
         VALUES (?1, 'queued', 'full', ?2, NULL)",
        params![USER, "2026-09-10T00:00:00+00:00"],
    )
    .unwrap();
    let db = SqliteDatabase::new(conn);

    assert_eq!(
        db.enqueue_scan(USER).await.unwrap(),
        EnqueueOutcome::AlreadyQueued
    );
    let r = row(&db, USER).await;
    assert_eq!((r.status.as_str(), r.kind), ("queued", ScanKind::Full));
    assert!(
        r.full_requested_at.is_some(),
        "the obligation is recorded even though nothing else changed"
    );
    assert_eq!(
        r.enqueued_at, "2026-09-10T00:00:00+00:00",
        "and the user keeps the place they already held"
    );
}

/// The running twin of the test above (#344 Minor 3).
#[tokio::test]
async fn a_full_enqueue_records_the_obligation_on_a_running_full_row_that_lacks_one() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    conn.execute(
        "INSERT INTO scan_queue (user_did, status, kind, claim_id, enqueued_at, started_at, full_requested_at)
         VALUES (?1, 'running', 'full', 'claim-1', ?2, ?2, NULL)",
        params![USER, "2026-09-10T00:00:00+00:00"],
    )
    .unwrap();
    let db = SqliteDatabase::new(conn);

    assert_eq!(
        db.enqueue_scan(USER).await.unwrap(),
        EnqueueOutcome::AlreadyRunning
    );
    let r = row(&db, USER).await;
    assert_eq!((r.status.as_str(), r.kind), ("running", ScanKind::Full));
    assert!(
        r.full_requested_at.is_some(),
        "the obligation is recorded; the running scan is left alone"
    );
    assert_eq!(r.started_at.as_deref(), Some("2026-09-10T00:00:00+00:00"));
}

/// #344 F1, the whole point of moving the sample out of `scan_queue`: a
/// fulfilled full scan records its duration in `scan_state`, and the nightly
/// refresh that reuses the very same queue row — resetting `started_at` and
/// `finished_at`, flipping `kind` to `refresh` — cannot erase it.
///
/// Sourced from the queue row, this test's second half went to `None` and
/// every queued user's ETA disappeared for good the first night Task 6's tick
/// ran.
#[tokio::test]
async fn the_eta_median_survives_the_refresh_that_rewrites_the_queue_row() {
    const SCANNER: &str = "did:plc:kindtest_scanner0000000";
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    // A running full scan started an hour before it fulfils, so the derived
    // duration is a number this test can name.
    conn.execute(
        "INSERT INTO scan_queue (user_did, status, kind, claim_id, enqueued_at, started_at)
         VALUES (?1, 'running', 'full', 'claim-1', ?2, ?2)",
        params![SCANNER, "2026-09-10T00:00:00+00:00"],
    )
    .unwrap();
    let db = SqliteDatabase::new(conn);

    // Production order: the marker while the row is still running, then the
    // row is finished. The carried key is just an argument here; its own
    // contract is covered in `scan_job.rs`.
    db.finish_full_scan_state(
        SCANNER,
        "2026-09-10T01:00:00+00:00",
        "full_carried_completion",
    )
    .await
    .unwrap();
    assert_eq!(
        db.get_scan_state(SCANNER, LAST_FULL_SCAN_DURATION_KEY)
            .await
            .unwrap()
            .as_deref(),
        Some("3600"),
        "one hour of full scan, in whole seconds"
    );
    assert!(db
        .finish_queued_scan(SCANNER, "claim-1", FinishCompletion::Complete, None)
        .await
        .unwrap());

    db.enqueue_scan(USER).await.unwrap();
    assert_eq!(
        db.scan_queue_entry(USER, 1)
            .await
            .unwrap()
            .unwrap()
            .eta_seconds,
        Some(3600),
        "the median is the recorded sample"
    );

    // The nightly refresh now takes over that user's one queue row.
    db.enqueue_refresh_scan(SCANNER).await.unwrap();
    let r = row(&db, SCANNER).await;
    assert_eq!((r.status.as_str(), r.kind), ("queued", ScanKind::Refresh));
    assert_eq!(
        r.started_at, None,
        "the refresh really did wipe the timing the old median read"
    );
    assert_eq!(
        db.scan_queue_entry(USER, 1)
            .await
            .unwrap()
            .unwrap()
            .eta_seconds,
        Some(3600),
        "the sample lives in scan_state, so the refresh cannot take it away"
    );
}

/// The negative control for the move (#344 F1): the queue row is not a median
/// source at all any more. Clean, done, `kind = 'full'` rows with real
/// durations and no `scan_state` sample must quote no ETA — if they do, the
/// old query is still in there somewhere.
#[tokio::test]
async fn the_eta_median_is_not_read_from_the_queue_row() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    conn.execute(
        "INSERT INTO scan_queue (user_did, status, kind, completion, enqueued_at, started_at, finished_at)
         VALUES ('did:plc:clean', 'done', 'full', 'complete',
                 '2026-09-10T00:00:00+00:00', '2026-09-10T00:00:00+00:00', '2026-09-10T01:00:00+00:00'),
                ('did:plc:clean2', 'done', 'full', 'complete',
                 '2026-09-11T00:00:00+00:00', '2026-09-11T00:00:00+00:00', '2026-09-11T02:00:00+00:00')",
        [],
    )
    .unwrap();
    let db = SqliteDatabase::new(conn);
    db.enqueue_scan(USER).await.unwrap();
    let entry = db.scan_queue_entry(USER, 1).await.unwrap().unwrap();
    assert_eq!(
        entry.eta_seconds, None,
        "no recorded duration sample means no ETA, however many done rows there are"
    );
}

#[test]
fn scan_kind_round_trips() {
    for k in [ScanKind::Full, ScanKind::Refresh] {
        assert_eq!(ScanKind::from_str(k.as_str()), Some(k));
    }
    assert_eq!(ScanKind::from_str("nightly"), None);
}

#[test]
fn finish_completion_round_trips() {
    for c in [
        FinishCompletion::Complete,
        FinishCompletion::CompleteWithSkips,
        FinishCompletion::CompleteUnverified,
        FinishCompletion::Resumable,
        FinishCompletion::Failed,
    ] {
        assert_eq!(FinishCompletion::from_str(c.as_str()), Some(c));
    }
    assert_eq!(FinishCompletion::from_str("done"), None);
}

#[test]
fn legacy_rows_default_to_full() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    conn.execute(
        "INSERT INTO scan_queue (user_did, status, enqueued_at) VALUES (?1, 'done', '2026-09-10T00:00:00+00:00')",
        params![USER],
    )
    .unwrap();
    let kind: String = conn
        .query_row(
            "SELECT kind FROM scan_queue WHERE user_did = ?1",
            params![USER],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kind, "full");
}

/// An unrecognised `kind` or `completion` is an error, not a silent default:
/// a row this binary cannot interpret must be visible to an operator rather
/// than rendered as an ordinary full scan.
#[tokio::test]
async fn an_unknown_kind_is_an_error_not_a_default() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    // Written past the trait, because the trait has no way to say "nightly".
    conn.execute(
        "INSERT INTO scan_queue (user_did, status, kind, enqueued_at)
         VALUES (?1, 'queued', 'nightly', '2026-09-10T00:00:00+00:00')",
        params![USER],
    )
    .unwrap();
    let db = SqliteDatabase::new(conn);
    let err = db.list_scan_queue().await.unwrap_err();
    assert!(
        format!("{err:#}").contains("nightly"),
        "the offending value must be in the message: {err:#}"
    );
}
