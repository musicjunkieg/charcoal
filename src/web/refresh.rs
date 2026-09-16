//! The nightly refresh schedule (#343 §4.4, #344).
//!
//! Runs inside the admitter tick — no second loop, no second lock. Each
//! tick claims a bounded batch of due users and creates their refresh queue
//! rows in ONE transaction, so a crash between "decided" and "queued" cannot
//! happen (R04). Due-ness is `next_refresh_at <= now` OR "this user's scores
//! were last refreshed under another generation" (R07), so a generation
//! bump refreshes everyone promptly without any migration-time stamp.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tracing::{error, info, warn};

use crate::db::{Database, RefreshScheduleWrite};
use crate::scoring::generation::scoring_revision;

pub const REFRESH_INTERVAL_ENV: &str = "CHARCOAL_REFRESH_INTERVAL_HOURS";
/// The cadence spec §4.4 specifies, and the deadline a completed scan writes
/// even while the tick is switched off (see [`refresh_deadline_interval`]).
pub const DEFAULT_REFRESH_INTERVAL_HOURS: u64 = 24;
pub const MAX_REFRESH_INTERVAL_HOURS: u64 = 168;
/// After a deferred, resumable or failed refresh: try again soon, not at the
/// next nightly. Separate from the cadence so a stuck user shows up in the
/// logs hourly rather than daily.
pub const REFRESH_RETRY_HOURS: u64 = 1;
/// Users claimed per tick. Bounded so a large due population (a generation
/// bump makes everyone due at once) cannot hold the admitter pass — lease
/// reclaim and admission run on the same loop — for more than one short
/// transaction; the rest are claimed on following ticks, 30 s apart.
pub const REFRESH_BATCH_PER_TICK: usize = 25;

/// What an unset — or unparseable — knob means: the spec's nightly cadence.
///
/// The tick was deliberately disabled on this branch while no worker could
/// execute a `kind = 'refresh'` claim (deciduous 917). Task 9 landed the
/// runner, so the spec §4.4 default is restored: an unset knob means nightly,
/// and switching the job off is an explicit `0`/`off`.
fn default_refresh_interval() -> Option<Duration> {
    Some(Duration::from_secs(DEFAULT_REFRESH_INTERVAL_HOURS * 3600))
}

/// `None` disables the tick — and only `0`/`off` produce it. An unset or
/// unparseable value falls back to [`default_refresh_interval`].
pub fn parse_refresh_interval(raw: Option<&str>) -> Option<Duration> {
    parse_refresh_interval_with(raw, default_refresh_interval())
}

/// The parse with its fallback injected.
///
/// Kept separate from the caller so "`OFF` disables" and "an unparseable value
/// falls back" stay independently assertable against a fallback the test
/// chooses, rather than against whatever the process default happens to be.
fn parse_refresh_interval_with(raw: Option<&str>, default: Option<Duration>) -> Option<Duration> {
    let hours = match raw.map(str::trim) {
        None | Some("") => return default,
        // Case-insensitive: `OFF` and `Off` are spellings an operator will
        // reach for, and falling through to warn-and-default would read them
        // as "leave the tick alone" — the opposite of what was typed.
        Some(s) if s.eq_ignore_ascii_case("off") => return None,
        Some(s) => match s.parse::<u64>() {
            Ok(0) => return None,
            Ok(n) => n.min(MAX_REFRESH_INTERVAL_HOURS),
            Err(_) => {
                // Says what actually happens now that the default is the
                // nightly cadence again: a typo does NOT switch the job off.
                warn!(
                    value = s,
                    default_hours = DEFAULT_REFRESH_INTERVAL_HOURS,
                    "{REFRESH_INTERVAL_ENV} is not a number; falling back to the nightly \
                     default — use 0 or 'off' to disable the refresh job"
                );
                return default;
            }
        },
    };
    Some(Duration::from_secs(hours * 3600))
}

pub fn refresh_interval_from_env() -> Option<Duration> {
    parse_refresh_interval(std::env::var(REFRESH_INTERVAL_ENV).ok().as_deref())
}

/// The deadline a *completed* scan writes, whatever the tick knob says.
///
/// Deliberately not `Option`: a user stamped "attempted at the current
/// revision" with `next_refresh_at IS NULL` matches neither due clause, so
/// switching the tick back on would never pick them up again. Writing the
/// cadence unconditionally keeps re-enabling a one-variable change — while the
/// tick is off the deadline is simply never read.
pub fn refresh_deadline_interval() -> Duration {
    deadline_interval(refresh_interval_from_env())
}

/// The env-free half of [`refresh_deadline_interval`], so the fallback can be
/// asserted without touching a process-global variable.
fn deadline_interval(tick: Option<Duration>) -> Duration {
    tick.unwrap_or_else(|| Duration::from_secs(DEFAULT_REFRESH_INTERVAL_HOURS * 3600))
}

fn plus(now: DateTime<Utc>, d: Duration) -> String {
    // `expect`: every caller passes an interval this module produced, all of
    // which are at most MAX_REFRESH_INTERVAL_HOURS — far inside chrono's range.
    (now + chrono::Duration::from_std(d).expect("bounded")).to_rfc3339()
}

/// One tick: claim up to [`REFRESH_BATCH_PER_TICK`] due users and queue them
/// in one transaction. Returns how many were claimed (schedule advanced),
/// which excludes users whose row was left alone because they already had
/// work. Errors are logged, never propagated — this runs on the admitter loop.
pub async fn enqueue_due_refreshes(
    db: &Arc<dyn Database>,
    now: DateTime<Utc>,
    interval: Duration,
) -> usize {
    match db
        .claim_and_enqueue_due_refreshes(
            &now.to_rfc3339(),
            &plus(now, interval),
            scoring_revision(),
            REFRESH_BATCH_PER_TICK,
        )
        .await
    {
        Ok(claimed) => {
            if !claimed.is_empty() {
                info!(claimed = claimed.len(), "refresh tick enqueued due users");
            }
            claimed.len()
        }
        Err(e) => {
            // Nothing advanced (single transaction); the same users are due
            // on the next tick. Loud: a persistent failure here is a wedge.
            error!(error = %format!("{e:#}"), "refresh tick failed — nothing scheduled, retrying next tick");
            crate::observability::refresh_metrics::record_tick_failure();
            0
        }
    }
}

/// After a completed refresh or a successful full scan: next nightly, and
/// this generation is proven for the user. Best-effort — the scan already
/// succeeded, so failures are logged rather than propagated.
///
/// **The deadline and the proof are one write.** The proof stamps
/// `refresh_attempted_generation` as current, which silences
/// `REFRESH_DUE_SQL`'s generation clause. For a user whose `next_refresh_at` is
/// still NULL — anyone on their first successful scan — a proof without a
/// deadline would leave neither due-ness clause able to fire, and the user
/// would never be refreshed again (#344 Codex review P2). Written together,
/// a failure leaves both stale and the generation clause catches the user on
/// a later tick.
///
/// **Fenced on `claim_id`.** The write lands only while this claim still owns
/// the user's queue row, checked in the same transaction: a worker whose
/// lease lapsed must not move a successor's schedule (CodeRabbit, PR #124).
///
/// `interval` is injected rather than read from the environment here: the
/// proof stamps `refresh_attempted_generation`, so skipping the deadline
/// would strand the user (see [`refresh_deadline_interval`]), and a function
/// that reads a process-global variable cannot be tested in parallel with
/// anything that writes one.
pub async fn schedule_after_success(
    db: &dyn Database,
    user_did: &str,
    claim_id: &str,
    now: DateTime<Utc>,
    interval: Duration,
) {
    let next_at = plus(now, interval);
    let write = RefreshScheduleWrite::Success {
        next_at_rfc3339: &next_at,
        generation: scoring_revision(),
    };
    match db.apply_refresh_schedule(user_did, claim_id, write).await {
        Ok(true) => {}
        Ok(false) => warn!(
            user_did,
            "the claim no longer owns the queue row — not scheduling the next refresh; \
             the successor owns this user's schedule"
        ),
        Err(e) => warn!(
            error = %format!("{e:#}"),
            "could not schedule the next refresh — the generation stays unproven so a later tick still finds this user"
        ),
    }
}

/// After a deferred, resumable, partially completed or failed attempt: retry
/// at the deadline. Sets `next_refresh_at` AND stamps
/// `refresh_attempted_generation` (V2-03, V4-01): an attempt happened —
/// whether the tick claimed it or the user clicked — so the revision clause
/// stays quiet until the deadline. `refreshed_generation` stays unproven so
/// the runbook can see the user is behind. One write, fenced on `claim_id`
/// exactly as [`schedule_after_success`] is.
pub async fn schedule_retry(db: &dyn Database, user_did: &str, claim_id: &str, now: DateTime<Utc>) {
    let at = plus(now, Duration::from_secs(REFRESH_RETRY_HOURS * 3600));
    let write = RefreshScheduleWrite::Retry {
        at_rfc3339: &at,
        attempted_generation: scoring_revision(),
    };
    match db.apply_refresh_schedule(user_did, claim_id, write).await {
        Ok(true) => {}
        Ok(false) => warn!(
            user_did,
            "the claim no longer owns the queue row — not scheduling the refresh retry; \
             the successor owns this user's schedule"
        ),
        Err(e) => warn!(error = %format!("{e:#}"), "could not schedule the refresh retry"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::create_tables;
    use crate::db::sqlite::SqliteDatabase;
    use crate::db::{Database, ScanKind};
    use crate::scoring::generation::scoring_revision;
    use chrono::{Duration as ChronoDuration, Utc};
    use rusqlite::Connection;
    use std::sync::Arc;

    fn db() -> Arc<dyn Database> {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        Arc::new(SqliteDatabase::new(conn))
    }

    /// A claim on the user's queue row. The scheduling writes are fenced on
    /// it, so a test that schedules must hold one, exactly as a worker does.
    async fn claimed(db: &Arc<dyn Database>, did: &str) -> String {
        db.enqueue_scan(did).await.unwrap();
        db.claim_next_scan(8, 60)
            .await
            .unwrap()
            .expect("the queued row is claimed")
            .claim_id
    }

    /// A user with one score row (so they are eligible), scheduled `at`,
    /// with `attempted` as the revision last attempted/proven (None = never).
    async fn user(db: &Arc<dyn Database>, did: &str, at: Option<&str>, attempted: Option<&str>) {
        db.upsert_user(did, &format!("{did}.handle")).await.unwrap();
        let mut score = crate::db::models::AccountScore::default_for_test(did);
        score.threat_score = Some(40.0);
        score.threat_tier = Some("High".into());
        db.upsert_account_score(did, &score).await.unwrap();
        if let Some(at) = at {
            db.schedule_refresh(did, at).await.unwrap();
        }
        if let Some(g) = attempted {
            // mark_refreshed_generation sets both columns — a proven revision
            // is also an attempted one.
            db.mark_refreshed_generation(did, g).await.unwrap();
        }
    }

    async fn queue_kind(db: &Arc<dyn Database>, did: &str) -> Option<(String, ScanKind)> {
        db.list_scan_queue()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.user_did == did)
            .map(|r| (r.status, r.kind))
    }

    #[test]
    fn interval_knob_defaults_disables_and_clamps() {
        let h = |n: u64| Duration::from_secs(n * 3600);
        // Unset, blank and unparseable all mean the spec's nightly cadence
        // (#344 Task 9 restored it); only an explicit 0/off disables the job.
        assert_eq!(parse_refresh_interval(None), Some(h(24)));
        assert_eq!(parse_refresh_interval(Some("")), Some(h(24)));
        assert_eq!(parse_refresh_interval(Some("abc")), Some(h(24)));
        assert_eq!(parse_refresh_interval(Some("6")), Some(h(6)));
        assert_eq!(parse_refresh_interval(Some("0")), None);
        assert_eq!(parse_refresh_interval(Some("off")), None);
        // A plausible operator spelling must not read as "leave it on". Run
        // against an explicitly ENABLED fallback so the assertion still
        // separates "off disabled it" from "the fallback happened to be None"
        // if the default ever changes again.
        for spelling in ["off", "OFF", "Off", " oFf "] {
            assert_eq!(
                parse_refresh_interval_with(Some(spelling), Some(h(24))),
                None,
                "{spelling}"
            );
        }
        assert_eq!(
            parse_refresh_interval_with(Some("offf"), Some(h(24))),
            Some(h(24)),
            "only 'off' disables — anything else is an unparseable value"
        );
        assert_eq!(parse_refresh_interval(Some("1")), Some(h(1)));
        assert_eq!(parse_refresh_interval(Some("500")), Some(h(168)));
        assert_eq!(parse_refresh_interval(Some(" 12 ")), Some(h(12)));
    }

    /// F4: a success writes its deadline even while the tick is off, so
    /// switching the knob back on picks the user up again. Without it they
    /// carry `next_refresh_at IS NULL` AND the current attempted revision,
    /// which matches neither due clause — permanently invisible.
    #[tokio::test]
    async fn a_success_while_the_tick_is_off_still_leaves_the_user_reachable() {
        let db = db();
        let now = Utc::now();
        user(&db, "did:plc:parked", None, None).await;
        // What run_scan does when the knob is off: the fallback cadence.
        assert_eq!(
            deadline_interval(None),
            Duration::from_secs(DEFAULT_REFRESH_INTERVAL_HOURS * 3600)
        );
        let claim = claimed(&db, "did:plc:parked").await;
        schedule_after_success(
            db.as_ref(),
            "did:plc:parked",
            &claim,
            now,
            deadline_interval(None),
        )
        .await;
        assert_eq!(
            db.next_refresh_at("did:plc:parked").await.unwrap(),
            Some((now + ChronoDuration::hours(DEFAULT_REFRESH_INTERVAL_HOURS as i64)).to_rfc3339()),
            "a deadline exists even with the tick disabled"
        );
        // Re-enabled a day later, the tick finds them.
        assert_eq!(
            enqueue_due_refreshes(
                &db,
                now + ChronoDuration::hours(DEFAULT_REFRESH_INTERVAL_HOURS as i64 + 1),
                Duration::from_secs(24 * 3600)
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn due_by_time_or_by_generation_and_never_without_scores() {
        let db = db();
        let now = Utc::now();
        let past = (now - ChronoDuration::hours(1)).to_rfc3339();
        let future = (now + ChronoDuration::hours(1)).to_rfc3339();
        user(
            &db,
            "did:plc:due-time",
            Some(&past),
            Some(scoring_revision()),
        )
        .await;
        user(&db, "did:plc:due-gen", Some(&future), Some("1999-01-01")).await;
        user(&db, "did:plc:due-never-refreshed", None, None).await; // v18-migrated shape
        user(
            &db,
            "did:plc:not-due",
            Some(&future),
            Some(scoring_revision()),
        )
        .await;
        db.upsert_user("did:plc:no-scores", "none.handle")
            .await
            .unwrap(); // no rows ⇒ never due

        // What each user had PROVEN before the tick ran. The tick must leave
        // this column alone for all of them — `did:plc:due-time` has already
        // proven the current revision (it is due only by time), so comparing
        // against the current revision alone could not tell "untouched" from
        // "the tick proved it".
        let mut proven_before = Vec::new();
        for did in [
            "did:plc:due-time",
            "did:plc:due-gen",
            "did:plc:due-never-refreshed",
        ] {
            proven_before.push(db.refreshed_generation(did).await.unwrap());
        }

        let n = enqueue_due_refreshes(&db, now, Duration::from_secs(24 * 3600)).await;
        assert_eq!(n, 3);
        for did in [
            "did:plc:due-time",
            "did:plc:due-gen",
            "did:plc:due-never-refreshed",
        ] {
            assert_eq!(
                queue_kind(&db, did).await,
                Some(("queued".into(), ScanKind::Refresh)),
                "{did}"
            );
        }
        assert_eq!(queue_kind(&db, "did:plc:not-due").await, None);
        assert_eq!(queue_kind(&db, "did:plc:no-scores").await, None);

        // A tick a minute later claims nobody: every claimed user was
        // rescheduled AND stamped refresh_attempted_generation = current, so
        // neither clause fires again until their deadline.
        let n = enqueue_due_refreshes(
            &db,
            now + ChronoDuration::minutes(1),
            Duration::from_secs(24 * 3600),
        )
        .await;
        assert_eq!(n, 0);
        for (did, proven) in [
            "did:plc:due-time",
            "did:plc:due-gen",
            "did:plc:due-never-refreshed",
        ]
        .into_iter()
        .zip(proven_before)
        {
            assert_eq!(
                db.refresh_attempted_generation(did)
                    .await
                    .unwrap()
                    .as_deref(),
                Some(scoring_revision()),
                "{did}"
            );
            assert_eq!(
                db.refreshed_generation(did).await.unwrap(),
                proven,
                "{did}: the tick never PROVES a revision — only a completed run does"
            );
        }
        // And the two that had nothing proven still have nothing: the tick
        // scheduled an attempt, it did not manufacture evidence.
        for did in ["did:plc:due-gen", "did:plc:due-never-refreshed"] {
            assert_ne!(
                db.refreshed_generation(did).await.unwrap().as_deref(),
                Some(scoring_revision()),
                "{did}"
            );
        }
    }

    /// V2-03: a failed attempt under a new revision waits for its retry
    /// deadline; it is not re-claimed on the next tick just because the
    /// revision is still unproven. A NEWER revision during the backoff is
    /// attempted promptly.
    #[tokio::test]
    async fn a_failed_attempt_waits_for_its_retry_deadline() {
        let db = db();
        let now = Utc::now();
        user(&db, "did:plc:retry", None, None).await; // v18-migrated shape: never attempted
        assert_eq!(
            enqueue_due_refreshes(&db, now, Duration::from_secs(24 * 3600)).await,
            1
        );
        // The refresh runs and fails: the row finishes, the retry is scheduled.
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        // Production order: schedule while the claim is held, then finish.
        schedule_retry(db.as_ref(), "did:plc:retry", &claim.claim_id, now).await;
        db.finish_queued_scan(
            "did:plc:retry",
            &claim.claim_id,
            crate::db::FinishCompletion::Failed,
            Some("boom"),
        )
        .await
        .unwrap();

        assert_eq!(
            enqueue_due_refreshes(
                &db,
                now + ChronoDuration::seconds(30),
                Duration::from_secs(24 * 3600)
            )
            .await,
            0,
            "30 s later: no new job"
        );
        assert_eq!(
            enqueue_due_refreshes(
                &db,
                now + ChronoDuration::minutes(61),
                Duration::from_secs(24 * 3600)
            )
            .await,
            1,
            "after the deadline: exactly one"
        );

        // A newer revision arrives while a fresh backoff is pending.
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        let t = now + ChronoDuration::minutes(62);
        schedule_retry(db.as_ref(), "did:plc:retry", &claim.claim_id, t).await;
        db.finish_queued_scan(
            "did:plc:retry",
            &claim.claim_id,
            crate::db::FinishCompletion::Failed,
            Some("boom again"),
        )
        .await
        .unwrap();
        let promptly = db
            .claim_and_enqueue_due_refreshes(
                &(t + ChronoDuration::seconds(30)).to_rfc3339(),
                &(t + ChronoDuration::hours(24)).to_rfc3339(),
                "rev-B",
                25,
            )
            .await
            .unwrap();
        assert_eq!(
            promptly,
            vec!["did:plc:retry".to_string()],
            "a new revision is attempted at once, once"
        );
        assert_eq!(
            db.refresh_attempted_generation("did:plc:retry")
                .await
                .unwrap()
                .as_deref(),
            Some("rev-B")
        );
    }

    /// A user with queued or running work is not delivered: their schedule
    /// and attempted revision are left alone (the completion path reschedules
    /// them) and their row is not touched — a queued full scan is never
    /// downgraded, a running scan never interrupted. On SQLite the immediate
    /// transaction makes the select and the write atomic; the write is still
    /// conditional so the two backends share one contract (V2-02).
    #[tokio::test]
    async fn a_user_with_queued_or_running_work_is_not_claimed() {
        let db = db();
        let now = Utc::now();
        let past = (now - ChronoDuration::hours(1)).to_rfc3339();
        user(&db, "did:plc:busy", Some(&past), Some(scoring_revision())).await;
        db.enqueue_scan("did:plc:busy").await.unwrap();
        user(&db, "did:plc:free", Some(&past), Some(scoring_revision())).await;

        let claimed = db
            .claim_and_enqueue_due_refreshes(
                &now.to_rfc3339(),
                &(now + ChronoDuration::hours(24)).to_rfc3339(),
                scoring_revision(),
                25,
            )
            .await
            .unwrap();
        assert_eq!(claimed, vec!["did:plc:free".to_string()]);
        assert_eq!(
            queue_kind(&db, "did:plc:busy").await,
            Some(("queued".into(), ScanKind::Full)),
            "not downgraded"
        );
        assert_eq!(
            db.next_refresh_at("did:plc:busy").await.unwrap().as_deref(),
            Some(past.as_str()),
            "schedule untouched"
        );
    }

    /// V2-02 at the SQL level: the conditional write refuses to clobber a
    /// row that changed between the select and the write. Simulated on one
    /// connection by running the select, then a manual full enqueue, then
    /// the tick's write statement.
    #[tokio::test]
    async fn the_queue_write_is_conditional_on_the_row_state_at_write_time() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        conn.execute(
            "INSERT INTO users (did, handle) VALUES ('did:plc:race', 'race.h')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO account_scores (user_did, did, handle, threat_score, scoring_generation, valid_until)
             VALUES ('did:plc:race', 'did:plc:x', 'x.h', 40.0, 'legacy', datetime('now', '-1 day'))",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_queue (user_did, status, kind, enqueued_at, finished_at)
             VALUES ('did:plc:race', 'done', 'full', '2026-09-10T00:00:00+00:00', '2026-09-10T01:00:00+00:00')",
            [],
        )
        .unwrap();
        // 1. The tick selects: the user is due (never attempted) and has a done row.
        let due: Vec<String> = conn
            .prepare(crate::db::queries::REFRESH_DUE_SQL)
            .unwrap()
            .query_map(
                rusqlite::params![Utc::now().to_rfc3339(), scoring_revision(), 25i64],
                |r| r.get(0),
            )
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(due, vec!["did:plc:race".to_string()]);
        // 2. A manual full enqueue lands in between.
        crate::db::queries::enqueue_scan(&conn, "did:plc:race").unwrap();
        // 3. The tick's conditional write affects nothing; the full request survives.
        let affected = conn
            .execute(
                crate::db::queries::REFRESH_ENQUEUE_SQL,
                rusqlite::params!["did:plc:race", Utc::now().to_rfc3339()],
            )
            .unwrap();
        assert_eq!(affected, 0);
        let (status, kind): (String, String) = conn
            .query_row(
                "SELECT status, kind FROM scan_queue WHERE user_did = 'did:plc:race'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((status.as_str(), kind.as_str()), ("queued", "full"));
    }

    /// V3-04 through the real success path: a manually queued full scan for
    /// a never-attempted user proves BOTH columns, and the next tick stays
    /// quiet until the scheduled deadline.
    #[tokio::test]
    async fn a_completed_manual_full_scan_proves_both_columns_and_the_tick_stays_quiet() {
        let db = db();
        let now = Utc::now();
        user(&db, "did:plc:manual", None, None).await; // never attempted
        db.enqueue_scan("did:plc:manual").await.unwrap();
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        // What run_scan does on Complete (Task 5 + this task):
        crate::web::scan_job::record_full_scan_completion(
            db.as_ref(),
            "did:plc:manual",
            &claim.claim_id,
            crate::web::scan_job::ScanCompletion::Complete,
        )
        .await;
        schedule_after_success(
            db.as_ref(),
            "did:plc:manual",
            &claim.claim_id,
            now,
            Duration::from_secs(24 * 3600),
        )
        .await;
        db.finish_queued_scan(
            "did:plc:manual",
            &claim.claim_id,
            crate::db::FinishCompletion::Complete,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            db.refreshed_generation("did:plc:manual")
                .await
                .unwrap()
                .as_deref(),
            Some(scoring_revision())
        );
        assert_eq!(
            db.refresh_attempted_generation("did:plc:manual")
                .await
                .unwrap()
                .as_deref(),
            Some(scoring_revision())
        );
        assert_eq!(
            enqueue_due_refreshes(
                &db,
                now + ChronoDuration::seconds(30),
                Duration::from_secs(24 * 3600)
            )
            .await,
            0,
            "no unnecessary refresh"
        );
        assert_eq!(
            enqueue_due_refreshes(
                &db,
                now + ChronoDuration::hours(25),
                Duration::from_secs(24 * 3600)
            )
            .await,
            1
        );
    }

    /// V3-03 at the tick: a finished row that owes a full scan is re-queued
    /// as FULL, not as a refresh.
    #[tokio::test]
    async fn owed_full_work_is_retried_as_a_full_scan() {
        let db = db();
        let now = Utc::now();
        user(
            &db,
            "did:plc:owed",
            Some(&(now - ChronoDuration::hours(1)).to_rfc3339()),
            Some(scoring_revision()),
        )
        .await;
        db.enqueue_scan("did:plc:owed").await.unwrap();
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        db.finish_queued_scan(
            "did:plc:owed",
            &claim.claim_id,
            crate::db::FinishCompletion::Resumable,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            enqueue_due_refreshes(&db, now, Duration::from_secs(3600)).await,
            1
        );
        assert_eq!(
            queue_kind(&db, "did:plc:owed").await,
            Some(("queued".into(), ScanKind::Full))
        );
    }

    #[tokio::test]
    async fn a_tick_is_bounded_and_the_rest_wait_for_the_next_one() {
        let db = db();
        let now = Utc::now();
        let past = (now - ChronoDuration::hours(1)).to_rfc3339();
        for i in 0..30 {
            user(
                &db,
                &format!("did:plc:bulk{i:02}"),
                Some(&past),
                Some(scoring_revision()),
            )
            .await;
        }
        let first = enqueue_due_refreshes(&db, now, Duration::from_secs(3600)).await;
        let second = enqueue_due_refreshes(&db, now, Duration::from_secs(3600)).await;
        let third = enqueue_due_refreshes(&db, now, Duration::from_secs(3600)).await;
        assert_eq!(
            (first, second, third),
            (REFRESH_BATCH_PER_TICK, 30 - REFRESH_BATCH_PER_TICK, 0)
        );
    }

    /// The claim is one transaction: when creating the queue row fails, the
    /// schedule is NOT advanced, so the user is still due next tick (R04).
    /// #344 Codex review P2: a failed deadline write must not be followed by a
    /// successful proof write.
    ///
    /// `schedule_after_success` writes the deadline, then `mark_refreshed_generation`
    /// — which stamps BOTH `refreshed_generation` and `refresh_attempted_generation`.
    /// For a user's first successful full scan `next_refresh_at` is still NULL,
    /// so if the deadline write fails and the proof succeeds, the user is left
    /// with a NULL deadline and a current attempted generation. `REFRESH_DUE_SQL`
    /// then matches neither its deadline clause (NULL) nor its generation clause
    /// (current), and the user is never refreshed again.
    ///
    /// A trigger refuses writes to `next_refresh_at` only. `mark_refreshed_generation`
    /// sets different columns, so without the fix it would succeed — which is
    /// exactly the half-write this pins.
    #[tokio::test]
    async fn a_failed_deadline_write_does_not_strand_the_user() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER refuse_deadline BEFORE UPDATE OF next_refresh_at ON users
             BEGIN SELECT RAISE(ABORT, 'disk on fire'); END;",
        )
        .unwrap();
        let db: Arc<dyn Database> = Arc::new(SqliteDatabase::new(conn));
        let now = Utc::now();
        // A first-scan user: scores, no deadline, never attempted.
        user(&db, "did:plc:first", None, None).await;
        let claim = claimed(&db, "did:plc:first").await;

        schedule_after_success(
            db.as_ref(),
            "did:plc:first",
            &claim,
            now,
            Duration::from_secs(3600),
        )
        .await;

        assert_eq!(
            db.next_refresh_at("did:plc:first").await.unwrap(),
            None,
            "the trigger refused the deadline write"
        );
        // This NULL is what keeps the user reachable: `REFRESH_DUE_SQL` selects
        // on `refresh_attempted_generation IS NULL`, so a later tick finds them.
        // (The tick cannot be exercised here — it writes `next_refresh_at` in
        // its own transaction, which the same trigger would refuse.)
        assert_eq!(
            db.refresh_attempted_generation("did:plc:first")
                .await
                .unwrap(),
            None,
            "the proof must not be stamped when the deadline write failed"
        );
        assert_eq!(
            db.refreshed_generation("did:plc:first").await.unwrap(),
            None,
            "a revision is not proven by a scan whose schedule could not be written"
        );
    }

    #[tokio::test]
    async fn a_failed_enqueue_rolls_back_the_schedule_advance() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        // A trigger that refuses refresh rows simulates the write failing
        // mid-transaction.
        conn.execute_batch(
            "CREATE TRIGGER refuse_refresh BEFORE INSERT ON scan_queue
             WHEN NEW.kind = 'refresh' BEGIN SELECT RAISE(ABORT, 'disk on fire'); END;",
        )
        .unwrap();
        let db: Arc<dyn Database> = Arc::new(SqliteDatabase::new(conn));
        let now = Utc::now();
        let past = (now - ChronoDuration::hours(1)).to_rfc3339();
        user(
            &db,
            "did:plc:unlucky",
            Some(&past),
            Some(scoring_revision()),
        )
        .await;

        let n = enqueue_due_refreshes(&db, now, Duration::from_secs(3600)).await;
        assert_eq!(n, 0, "nothing claimed when the transaction fails");
        assert_eq!(
            db.next_refresh_at("did:plc:unlucky")
                .await
                .unwrap()
                .as_deref(),
            Some(past.as_str()),
            "still due"
        );
        assert_eq!(queue_kind(&db, "did:plc:unlucky").await, None);
    }

    #[tokio::test]
    async fn success_and_retry_scheduling() {
        let db = db();
        let now = Utc::now();
        user(&db, "did:plc:u", None, None).await;
        let claim = claimed(&db, "did:plc:u").await;
        schedule_after_success(
            db.as_ref(),
            "did:plc:u",
            &claim,
            now,
            Duration::from_secs(24 * 3600),
        )
        .await;
        let next = db.next_refresh_at("did:plc:u").await.unwrap().unwrap();
        assert_eq!(next, (now + ChronoDuration::hours(24)).to_rfc3339());
        assert_eq!(
            db.refreshed_generation("did:plc:u")
                .await
                .unwrap()
                .as_deref(),
            Some(scoring_revision())
        );

        schedule_retry(db.as_ref(), "did:plc:u", &claim, now).await;
        let next = db.next_refresh_at("did:plc:u").await.unwrap().unwrap();
        assert_eq!(
            next,
            (now + ChronoDuration::hours(REFRESH_RETRY_HOURS as i64)).to_rfc3339()
        );
        assert_eq!(
            db.refreshed_generation("did:plc:u")
                .await
                .unwrap()
                .as_deref(),
            Some(scoring_revision()),
            "retry does not unset the proof"
        );
        assert_eq!(
            db.refresh_attempted_generation("did:plc:u")
                .await
                .unwrap()
                .as_deref(),
            Some(scoring_revision()),
            "retry stamps the attempt (V4-01)"
        );
    }

    /// V4-01: a first full scan that failed before writing a single score is
    /// still retried — as a full scan, after its deadline, once — across a
    /// simulated restart. A user with neither scores nor an obligation is
    /// never selected.
    #[tokio::test]
    async fn an_owed_first_scan_with_no_scores_is_retried_after_its_deadline() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let open = || -> Arc<dyn Database> {
            let conn = Connection::open(file.path()).unwrap();
            create_tables(&conn).unwrap();
            Arc::new(SqliteDatabase::new(conn))
        };
        let db = open();
        db.upsert_user("did:plc:first", "first.h").await.unwrap();
        db.upsert_user("did:plc:nobody", "nobody.h").await.unwrap(); // no scores, no request: never due
        let now = Utc::now();
        // 1–2. Their first full scan is claimed and fails before any write.
        db.enqueue_scan("did:plc:first").await.unwrap();
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        // Production order: the retry is scheduled while the claim is held,
        // then the queue row is finished.
        schedule_retry(db.as_ref(), "did:plc:first", &claim.claim_id, now).await;
        db.finish_queued_scan(
            "did:plc:first",
            &claim.claim_id,
            crate::db::FinishCompletion::Failed,
            Some("fingerprint build failed"),
        )
        .await
        .unwrap();
        let owed = db
            .list_scan_queue()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.user_did == "did:plc:first")
            .unwrap();
        let requested = owed
            .full_requested_at
            .clone()
            .expect("obligation retained on failure");
        assert_eq!(
            db.export_scores("did:plc:first").await.unwrap().len(),
            0,
            "zero scores — the case V4-01 is about"
        );
        // 3. Restart.
        drop(db);
        let db = open();
        // 4. Before the deadline: nothing.
        assert_eq!(
            enqueue_due_refreshes(
                &db,
                now + ChronoDuration::seconds(30),
                Duration::from_secs(24 * 3600)
            )
            .await,
            0
        );
        // 5. After it: exactly one FULL job, same obligation timestamp.
        assert_eq!(
            enqueue_due_refreshes(
                &db,
                now + ChronoDuration::hours(REFRESH_RETRY_HOURS as i64 + 1),
                Duration::from_secs(24 * 3600)
            )
            .await,
            1
        );
        let r = db
            .list_scan_queue()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.user_did == "did:plc:first")
            .unwrap();
        assert_eq!((r.status.as_str(), r.kind), ("queued", ScanKind::Full));
        assert_eq!(r.full_requested_at.as_deref(), Some(requested.as_str()));
        // 6. The user with nothing owed and nothing scored was not selected.
        assert!(db
            .list_scan_queue()
            .await
            .unwrap()
            .iter()
            .all(|r| r.user_did != "did:plc:nobody"));
    }

    /// A worker whose lease lapsed, and whose row a successor has reclaimed,
    /// writes no schedule and requests no full scan (CodeRabbit, PR #124).
    /// The ownership check and the writes are one atomic step, so there is no
    /// window between "still mine" and "write" for a successor to claim in.
    #[tokio::test]
    async fn a_superseded_claim_moves_no_schedule_and_requests_no_full_scan() {
        let db = db();
        let now = Utc::now();
        user(&db, "did:plc:z", None, None).await;
        db.enqueue_refresh_scan("did:plc:z").await.unwrap();
        // An already-expired lease, so the reclaim below is the real one.
        let stale = db
            .claim_next_scan(8, -1)
            .await
            .unwrap()
            .expect("the refresh is claimed")
            .claim_id;
        assert_eq!(db.reclaim_expired_scans().await.unwrap(), 1);
        let successor = db
            .claim_next_scan(8, 60)
            .await
            .unwrap()
            .expect("the successor claims the re-queued row")
            .claim_id;
        assert_ne!(stale, successor);

        schedule_after_success(
            db.as_ref(),
            "did:plc:z",
            &stale,
            now,
            Duration::from_secs(3600),
        )
        .await;
        schedule_retry(db.as_ref(), "did:plc:z", &stale, now).await;
        assert!(
            !db.request_full_after_refresh("did:plc:z", &stale)
                .await
                .unwrap(),
            "a stale claim's full-scan request reports that it wrote nothing"
        );

        assert_eq!(db.next_refresh_at("did:plc:z").await.unwrap(), None);
        assert_eq!(db.refreshed_generation("did:plc:z").await.unwrap(), None);
        assert_eq!(
            db.refresh_attempted_generation("did:plc:z").await.unwrap(),
            None
        );
        let row = db
            .list_scan_queue()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.user_did == "did:plc:z")
            .unwrap();
        assert_eq!(row.full_requested_at, None, "no full scan requested");

        // The successor's own writes still land.
        assert!(db
            .request_full_after_refresh("did:plc:z", &successor)
            .await
            .unwrap());
        schedule_retry(db.as_ref(), "did:plc:z", &successor, now).await;
        assert!(db.next_refresh_at("did:plc:z").await.unwrap().is_some());
        assert_eq!(
            db.refresh_attempted_generation("did:plc:z")
                .await
                .unwrap()
                .as_deref(),
            Some(scoring_revision())
        );
    }
}
