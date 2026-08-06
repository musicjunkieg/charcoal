// Background admitter: claims queued scans while under the concurrency cap.
//
// One of these runs per process. Admission is a database question
// (`claim_next_scan`), not a process-local bool, so the cap survives a redeploy
// and holds across replicas: anything that wants a scan to start enqueues and
// wakes the admitter.
//
// The TARGET state is that this is the only thing which turns a queued row into
// a running scan, because a second admission path bypasses the cap the claim
// enforces. It is not the state of the tree yet: `POST /api/scan`
// (`handlers/scan.rs`) and the admin trigger (`handlers/admin.rs`) still call
// `launch_scan(..., None)` directly, so their scans never touch `scan_queue`,
// are invisible to `claim_next_scan`'s running count, and can push real
// concurrency past `CHARCOAL_SCAN_CONCURRENCY`. Converting them — and deleting
// `ScanManager::try_start_scan` with them — is the next step of #257.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::db::traits::{ScanClaim, ScanQueueDepth};
use crate::db::Database;
use crate::web::AppState;

/// How long a claimed scan's lease is valid. Once it lapses any admitter may
/// reclaim the row and hand the slot to someone else, so a running scan has to
/// keep extending it.
pub const LEASE_SECS: i64 = 120;

/// How often a running scan extends its lease.
///
/// A QUARTER of `LEASE_SECS`, not a third. At a third the beats land at T+40
/// and T+80 and the next one races the T+120 expiry, so the real slack was two
/// beats even though the comment here claimed three. A quarter puts beats at
/// T+30 / T+60 / T+90: two consecutive misses (a GC pause, a DB blip) still
/// leave a full interval of margin before the lease lapses.
///
/// That margin is load-bearing now that the reclaim runs on every admitter pass
/// rather than only at boot — a lapsed lease is noticed within a tick and the
/// row really is handed to a successor, where before it was quietly ignored
/// until the next restart. One extra `UPDATE` every 30 seconds per running scan
/// is a cheap price for not abandoning a two-hour scan over two blips.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(LEASE_SECS as u64 / 4);

/// Backstop tick. Enqueue and completion both wake the admitter directly; this
/// only covers a missed wake.
const TICK: Duration = Duration::from_secs(30);

/// Concurrent scans allowed. Default 2 — conservative given the ~500MB shared
/// model floor and the Bluesky pressure #182 addresses. Raise only after #264
/// reports the fetch/inference split.
pub fn scan_concurrency() -> usize {
    std::env::var("CHARCOAL_SCAN_CONCURRENCY")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(2)
        .clamp(1, 8)
}

/// Whether a worker still owns the queue row it claimed.
///
/// `heartbeat_scan` and `finish_queued_scan` answer this as a bare `bool`, and
/// a bare bool is trivially discarded: `db.finish_queued_scan(u, &id, None)
/// .await?;` compiles clean because `?` satisfies the `Result`'s `#[must_use]`
/// while nothing marks the bool. The whole fencing property — a worker whose
/// lease lapsed must not free or extend its successor's slot — rests on that
/// value being looked at, so it is wrapped in a `#[must_use]` type here.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimStatus {
    /// This worker still owns the row.
    Held,
    /// The lease lapsed, the row was reclaimed, and someone else holds the
    /// slot. This worker must stop touching it.
    Lost,
}

impl ClaimStatus {
    fn from_db(still_owned: bool) -> Self {
        if still_owned {
            ClaimStatus::Held
        } else {
            ClaimStatus::Lost
        }
    }
}

/// What the admitter does with a claim it has won.
///
/// A seam, not an abstraction for its own sake: the production implementation
/// needs the whole `AppState` (and therefore ~500MB of ONNX models) to launch a
/// scan, which would make every test of the admission logic itself
/// model-gated. Tests substitute a recorder.
#[async_trait::async_trait]
pub trait ScanLauncher: Send + Sync {
    /// Start the scan for an already-claimed user. Returning `Err` makes the
    /// admitter release the slot immediately.
    async fn launch(&self, claim: &ScanClaim) -> anyhow::Result<()>;
}

/// Mark a claimed scan finished, releasing its slot.
///
/// Returns `Lost` when `claim_id` no longer owns the row: the lease lapsed,
/// the row was reclaimed, and the running scan in it belongs to someone else.
/// The caller must not treat that as success and must not retry — retrying
/// with the same dead token can only fail again, and retrying without it would
/// stomp the successor.
async fn release_slot(
    db: &Arc<dyn Database>,
    user_did: &str,
    claim_id: &str,
    error: Option<&str>,
) -> anyhow::Result<ClaimStatus> {
    let still_owned = db.finish_queued_scan(user_did, claim_id, error).await?;
    Ok(ClaimStatus::from_db(still_owned))
}

/// Release a slot and log every outcome distinctly.
///
/// The three cases are genuinely different and none of them may be silent:
/// released cleanly, superseded (a successor owns the row), or the database
/// could not be reached at all — in which case the slot stays held until
/// `reclaim_expired_scans` picks it up, which is the backstop, not a fix.
pub async fn release_and_log(
    db: &Arc<dyn Database>,
    user_did: &str,
    claim_id: &str,
    error: Option<&str>,
) {
    match release_slot(db, user_did, claim_id, error).await {
        Ok(ClaimStatus::Held) => {
            info!(user_did, failed = error.is_some(), "released scan slot");
        }
        Ok(ClaimStatus::Lost) => {
            warn!(
                user_did,
                "scan slot was already reassigned — this worker's lease had \
                 lapsed, so the row now belongs to another claim and was left \
                 alone"
            );
        }
        Err(e) => {
            error!(
                user_did,
                error = %format!("{e:#}"),
                "could not release the scan slot — it stays held until the \
                 lease expires and reclaim_expired_scans frees it"
            );
        }
    }
}

/// Extend the lease once.
async fn beat(
    db: &Arc<dyn Database>,
    user_did: &str,
    claim_id: &str,
) -> anyhow::Result<ClaimStatus> {
    let still_owned = db.heartbeat_scan(user_did, claim_id, LEASE_SECS).await?;
    Ok(ClaimStatus::from_db(still_owned))
}

/// Extend the lease on a loop until the claim is lost, then return.
///
/// Returning at all means this worker has been superseded and should stop —
/// the scan it is running is no longer the one occupying the slot, so
/// continuing would spend GPU budget on work nobody is waiting for and
/// (worse) run a second scan for the same user concurrently.
///
/// A transient database error is NOT a lost claim: `HEARTBEAT_INTERVAL` is a
/// quarter of the lease, so two more beats fit before it lapses. It logs and
/// keeps beating rather than abandoning a two-hour scan over one failed
/// round-trip.
pub async fn heartbeat_until_lost(
    db: Arc<dyn Database>,
    user_did: String,
    claim_id: String,
    interval: Duration,
) -> ClaimStatus {
    loop {
        tokio::time::sleep(interval).await;
        match beat(&db, &user_did, &claim_id).await {
            Ok(ClaimStatus::Held) => {}
            Ok(ClaimStatus::Lost) => {
                warn!(
                    user_did,
                    "scan lease lapsed and the slot was reassigned — abandoning"
                );
                return ClaimStatus::Lost;
            }
            Err(e) => {
                warn!(
                    user_did,
                    error = %format!("{e:#}"),
                    "heartbeat failed — retrying, the lease still has slack"
                );
            }
        }
    }
}

/// Why a pass stopped claiming. `claim_next_scan` answers all three with the
/// same `Ok(None)`, which is what makes a wedged queue look exactly like an
/// idle one: no errors, no warnings, scans simply never start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stall {
    /// Nothing was waiting. The overwhelmingly common case.
    Idle,
    /// Work is waiting and every slot is legitimately busy. Expected
    /// backpressure — scans run 22 minutes to 2 hours.
    AtCapacity,
    /// Work is waiting, the server is UNDER its cap, and still nothing could be
    /// claimed. An invariant violation: the reclaim runs on every pass now, so
    /// a row holding a slot under the cap should already have been re-queued.
    Wedged,
}

fn stall_kind(depth: ScanQueueDepth, cap: usize) -> Stall {
    if depth.queued == 0 {
        Stall::Idle
    } else if depth.running >= cap {
        Stall::AtCapacity
    } else {
        Stall::Wedged
    }
}

/// Say out loud why a pass stopped claiming, so a stuck queue is greppable.
async fn log_admission_stall(db: &Arc<dyn Database>, cap: usize) {
    let depth = match db.scan_queue_depth().await {
        Ok(depth) => depth,
        Err(e) => {
            warn!(
                error = %format!("{e:#}"),
                "could not read the scan queue depth — cannot tell an idle queue \
                 from a wedged one"
            );
            return;
        }
    };

    match stall_kind(depth, cap) {
        // Saying anything here would bury the two below.
        Stall::Idle => {}
        Stall::AtCapacity => {
            warn!(
                queued = depth.queued,
                running = depth.running,
                cap,
                "scans are waiting because the server is at its concurrency cap — \
                 they start as slots free"
            );
        }
        Stall::Wedged => {
            error!(
                queued = depth.queued,
                running = depth.running,
                cap,
                "scan admission is WEDGED: rows are queued and the server is under \
                 its cap, yet nothing could be claimed. A running row is holding a \
                 slot without being reclaimable — check scan_queue for rows whose \
                 lease_expires is in the future but whose worker is gone"
            );
        }
    }
}

/// Claim and launch until at capacity or the queue is empty. Returns how many
/// scans actually started.
///
/// The loop matters: a single claim per pass would admit one scan and then
/// wait for the next wake, so a server that comes up with a full queue would
/// fill its slots one tick at a time.
async fn admit_ready(db: &Arc<dyn Database>, launcher: &dyn ScanLauncher, cap: usize) -> usize {
    let mut admitted = 0;
    loop {
        match db.claim_next_scan(cap, LEASE_SECS).await {
            Ok(Some(claim)) => {
                info!(user_did = %claim.user_did, cap, "admitting queued scan");
                match launcher.launch(&claim).await {
                    Ok(()) => admitted += 1,
                    Err(e) => {
                        let detail = format!("{e:#}");
                        error!(
                            user_did = %claim.user_did,
                            error = %detail,
                            "failed to start admitted scan"
                        );
                        // Release the slot rather than holding it until the
                        // lease lapses — otherwise one bad row throttles the
                        // whole server for two minutes.
                        release_and_log(db, &claim.user_did, &claim.claim_id, Some(&detail)).await;
                    }
                }
            }
            // At capacity, or nothing queued — the two are indistinguishable
            // here, which is exactly why the next line exists.
            Ok(None) => {
                log_admission_stall(db, cap).await;
                break;
            }
            Err(e) => {
                error!(error = %format!("{e:#}"), "claim failed");
                break;
            }
        }
    }
    admitted
}

/// The admitter loop. Runs until the process exits.
async fn run_admitter(
    db: Arc<dyn Database>,
    launcher: Arc<dyn ScanLauncher>,
    mut wake_rx: mpsc::Receiver<()>,
    tick: Duration,
    cap: fn() -> usize,
) {
    loop {
        // Reclaim on EVERY pass, not just at boot.
        //
        // A boot-only reclaim leaks a slot per redeploy. Railway starts the new
        // container while the old one is still serving and still heartbeating,
        // so the new admitter's boot pass sees leases with up to LEASE_SECS
        // remaining and correctly reclaims nothing. The old container is then
        // stopped; its running rows freeze with a live lease that lapses a
        // minute or two later, with no one left watching. Scans run 22 minutes
        // to 2 hours, so a redeploy landing mid-scan is ordinary — at cap 2,
        // two of them wedge admission permanently, and silently.
        //
        // Reclaiming in-process is safe precisely because the fencing token
        // exists: if this reclaims a row whose worker is alive but starved, that
        // worker's next beat returns `Lost` and it abandons rather than
        // double-running.
        match db.reclaim_expired_scans().await {
            Ok(0) => {}
            Ok(n) => info!(reclaimed = n, "re-queued scans whose lease had lapsed"),
            Err(e) => error!(error = %format!("{e:#}"), "lease reclaim failed"),
        }

        admit_ready(&db, launcher.as_ref(), cap()).await;

        tokio::select! {
            // `Some(())`, not `_`: a closed channel makes `recv()` return
            // `None` immediately and forever, so a bare `_` would complete this
            // branch instantly and spin the loop at 100% CPU. An unmatched
            // pattern disables the branch instead, leaving the tick.
            Some(()) = wake_rx.recv() => {}
            _ = tokio::time::sleep(tick) => {}
        }
    }
}

/// The production launcher: turns a claim into a running scan pipeline.
struct AppStateLauncher {
    state: AppState,
    /// Handed to the scan so it can wake the admitter the moment it finishes.
    wake: mpsc::Sender<()>,
}

#[async_trait::async_trait]
impl ScanLauncher for AppStateLauncher {
    async fn launch(&self, claim: &ScanClaim) -> anyhow::Result<()> {
        let handle = self
            .state
            .db
            .get_user_handle(&claim.user_did)
            .await?
            .ok_or_else(|| anyhow::anyhow!("user not found — they must re-authenticate"))?;

        // Register the scan as running WITHOUT the process-global gate: the
        // cap has already been enforced by claim_next_scan, and re-checking
        // `any_running` here would refuse every scan past the first that the
        // queue just legitimately admitted.
        self.state
            .scan_manager
            .write()
            .await
            .begin_admitted_scan(&claim.user_did);

        crate::web::scan_job::launch_scan(
            self.state.config.clone(),
            self.state.db.clone(),
            self.state.models.clone(),
            self.state.scan_manager.clone(),
            claim.user_did.clone(),
            handle,
            Some(crate::web::scan_job::QueueSlot {
                claim_id: claim.claim_id.clone(),
                wake: self.wake.clone(),
            }),
        );
        Ok(())
    }
}

/// Spawn the background admitter. Returns a wake channel: send `()` after an
/// enqueue or a scan completion to admit immediately instead of waiting a tick.
pub fn spawn_admitter(state: AppState) -> mpsc::Sender<()> {
    let (tx, rx) = mpsc::channel::<()>(32);
    let db = state.db.clone();
    let launcher = Arc::new(AppStateLauncher {
        state,
        wake: tx.clone(),
    });

    // Supervised only to the extent of saying something. `run_admitter` loops
    // forever, so the join resolving at all means the task panicked — and a
    // dropped JoinHandle would turn that into one default tokio stderr line
    // followed by scans silently never starting again. Restarting it is a
    // bigger question (a panicking loop that respawns can hot-loop) and is left
    // for the supervision work; being loud is the part that matters now.
    tokio::spawn(async move {
        let admitter = tokio::spawn(run_admitter(db, launcher, rx, TICK, scan_concurrency));
        match admitter.await {
            Ok(()) => error!(
                "the scan admitter loop returned, which it never should — NO queued \
                 scan will start until this process restarts"
            ),
            Err(e) => error!(
                error = %e,
                "the scan admitter task died — NO queued scan will start until this \
                 process restarts"
            ),
        }
    });

    tx
}

#[cfg(test)]
mod admitter_tests {
    use super::*;

    use std::sync::Mutex;

    use crate::db::schema::create_tables;
    use crate::db::sqlite::SqliteDatabase;

    fn test_db() -> Arc<dyn Database> {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
        create_tables(&conn).expect("schema");
        Arc::new(SqliteDatabase::new(conn))
    }

    /// Records every claim it is handed, and fails the ones whose DID is in
    /// `fail_dids` so the release-on-failure path can be exercised.
    struct RecordingLauncher {
        launched: Mutex<Vec<String>>,
        fail_dids: Vec<String>,
        notify: Option<mpsc::Sender<String>>,
    }

    impl RecordingLauncher {
        fn new() -> Self {
            Self {
                launched: Mutex::new(Vec::new()),
                fail_dids: Vec::new(),
                notify: None,
            }
        }

        fn failing(dids: &[&str]) -> Self {
            Self {
                fail_dids: dids.iter().map(|s| s.to_string()).collect(),
                ..Self::new()
            }
        }

        fn notifying(tx: mpsc::Sender<String>) -> Self {
            Self {
                notify: Some(tx),
                ..Self::new()
            }
        }

        fn launched(&self) -> Vec<String> {
            self.launched.lock().expect("launched lock").clone()
        }
    }

    #[async_trait::async_trait]
    impl ScanLauncher for RecordingLauncher {
        async fn launch(&self, claim: &ScanClaim) -> anyhow::Result<()> {
            self.launched
                .lock()
                .expect("launched lock")
                .push(claim.user_did.clone());
            if let Some(tx) = &self.notify {
                // Not `let _ =`: a full or closed notify channel is a broken
                // test, and swallowing it turns that into a confusing timeout
                // in whichever assertion was waiting on the message.
                tx.send(claim.user_did.clone())
                    .await
                    .expect("notify channel must accept the launch record");
            }
            if self.fail_dids.contains(&claim.user_did) {
                anyhow::bail!("launch failed for {}", claim.user_did);
            }
            Ok(())
        }
    }

    async fn status_of(db: &Arc<dyn Database>, did: &str) -> String {
        db.scan_queue_entry(did, 2)
            .await
            .expect("queue entry query")
            .expect("row exists")
            .status
    }

    /// One pass must drain the queue up to the cap, not admit a single scan and
    /// then wait 30 seconds for the next tick. With a cap of 2 and three users
    /// waiting, two start now and the third stays queued.
    #[tokio::test]
    async fn one_pass_admits_up_to_the_cap() {
        let db = test_db();
        for did in ["did:plc:a", "did:plc:b", "did:plc:c"] {
            db.enqueue_scan(did).await.expect("enqueue");
        }

        let launcher = RecordingLauncher::new();
        let admitted = admit_ready(&db, &launcher, 2).await;

        assert_eq!(admitted, 2, "a single pass must fill the cap");
        assert_eq!(launcher.launched().len(), 2);

        let mut queued = 0;
        let mut running = 0;
        for did in ["did:plc:a", "did:plc:b", "did:plc:c"] {
            match status_of(&db, did).await.as_str() {
                "queued" => queued += 1,
                "running" => running += 1,
                other => panic!("unexpected status {other}"),
            }
        }
        assert_eq!((running, queued), (2, 1));
    }

    /// A scan that cannot start must free its slot immediately. Holding it
    /// until the lease lapses would throttle the whole server for two minutes
    /// over one bad row.
    #[tokio::test]
    async fn a_failed_launch_releases_its_slot() {
        let db = test_db();
        db.enqueue_scan("did:plc:bad").await.expect("enqueue");
        db.enqueue_scan("did:plc:good").await.expect("enqueue");

        let launcher = RecordingLauncher::failing(&["did:plc:bad"]);
        // Cap of 1: the good user can only start if the bad one's slot was
        // actually released, not merely abandoned.
        admit_ready(&db, &launcher, 1).await;

        assert_eq!(
            status_of(&db, "did:plc:bad").await,
            "failed",
            "a launch error must mark the row failed, releasing the slot"
        );
        assert_eq!(
            status_of(&db, "did:plc:good").await,
            "running",
            "the next user must get the freed slot in the same pass"
        );
    }

    /// Set up a superseded worker: claim with an already-expired lease, reclaim
    /// it, then claim again. The first claim_id no longer owns the row.
    async fn superseded_claim(db: &Arc<dyn Database>) -> (ScanClaim, ScanClaim) {
        db.enqueue_scan("did:plc:zombie").await.expect("enqueue");
        // Negative lease: expired the instant it is written.
        let first = db
            .claim_next_scan(1, -1)
            .await
            .expect("claim")
            .expect("a queued row exists");
        let reclaimed = db.reclaim_expired_scans().await.expect("reclaim");
        assert_eq!(reclaimed, 1, "the expired lease must be reclaimable");
        let second = db
            .claim_next_scan(1, LEASE_SECS)
            .await
            .expect("claim")
            .expect("the reclaimed row is queued again");
        assert_ne!(first.claim_id, second.claim_id, "a new claim mints a token");
        (first, second)
    }

    /// The fencing property. A worker whose lease lapsed must not be able to
    /// finish its successor's row — and must be TOLD it could not, so it stops
    /// rather than reporting success.
    #[tokio::test]
    async fn releasing_a_superseded_claim_reports_it_lost() {
        let db = test_db();
        let (zombie, successor) = superseded_claim(&db).await;

        let status = release_slot(&db, &zombie.user_did, &zombie.claim_id, None)
            .await
            .expect("the release query itself must succeed");

        assert_eq!(
            status,
            ClaimStatus::Lost,
            "the zombie must be told its claim is gone, not that it released a slot"
        );
        assert_eq!(
            status_of(&db, &zombie.user_did).await,
            "running",
            "the successor's row must be untouched"
        );
        // And the successor can still release its own slot.
        assert_eq!(
            release_slot(&db, &successor.user_did, &successor.claim_id, None)
                .await
                .expect("release query"),
            ClaimStatus::Held
        );
    }

    /// A superseded worker must stop heartbeating rather than keep extending a
    /// lease it no longer holds.
    #[tokio::test]
    async fn heartbeat_stops_when_the_claim_is_lost() {
        let db = test_db();
        let (zombie, _successor) = superseded_claim(&db).await;

        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            heartbeat_until_lost(
                db.clone(),
                zombie.user_did.clone(),
                zombie.claim_id.clone(),
                Duration::from_millis(10),
            ),
        )
        .await
        .expect("the heartbeat must stop on its own when the claim is lost");

        assert_eq!(outcome, ClaimStatus::Lost);
    }

    /// A row left 'running' by a process that is no longer alive must be
    /// re-queued BEFORE the first claim, or it occupies a slot forever. With a
    /// cap of 1 and one stale running row, nothing can be admitted unless the
    /// reclaim ran first.
    #[tokio::test]
    async fn boot_reclaims_before_the_first_claim() {
        let db = test_db();
        db.enqueue_scan("did:plc:orphan").await.expect("enqueue");
        db.claim_next_scan(1, -1)
            .await
            .expect("claim")
            .expect("row");

        let (notify_tx, mut notify_rx) = mpsc::channel(4);
        let launcher = Arc::new(RecordingLauncher::notifying(notify_tx));
        let (_wake_tx, wake_rx) = mpsc::channel(4);

        // A tick far longer than the test: anything that happens must be the
        // boot pass, not a retry.
        let handle = tokio::spawn(run_admitter(
            db.clone(),
            launcher.clone(),
            wake_rx,
            Duration::from_secs(600),
            || 1,
        ));

        let launched = tokio::time::timeout(Duration::from_secs(2), notify_rx.recv())
            .await
            .expect("the orphaned row must be reclaimed and admitted at boot")
            .expect("launcher notified");
        assert_eq!(launched, "did:plc:orphan");
        handle.abort();
    }

    /// A wedged queue must be distinguishable from an idle one. Before this,
    /// both produced silence and "scans just stopped" was the only symptom.
    #[test]
    fn a_stall_under_the_cap_is_a_wedge_not_backpressure() {
        let d = |queued, running| ScanQueueDepth { queued, running };

        assert_eq!(stall_kind(d(0, 0), 2), Stall::Idle);
        assert_eq!(
            stall_kind(d(0, 2), 2),
            Stall::Idle,
            "a full server with nothing waiting is not a problem"
        );
        assert_eq!(
            stall_kind(d(3, 2), 2),
            Stall::AtCapacity,
            "waiting while every slot is busy is expected backpressure"
        );
        assert_eq!(
            stall_kind(d(3, 1), 2),
            Stall::Wedged,
            "queued work plus a free slot plus nothing claimed is the wedge"
        );
        assert_eq!(
            stall_kind(d(1, 0), 1),
            Stall::Wedged,
            "the cap-1 case: one waiting, nothing running, nothing claimed"
        );
    }

    /// The redeploy case, and the one a boot-only reclaim cannot handle.
    ///
    /// Railway starts the new container while the old one is still serving and
    /// still heartbeating, so the new admitter's BOOT reclaim correctly finds
    /// nothing — the lease is live. The old container is then stopped, its
    /// running row freezes with a lease that lapses seconds later, and nobody
    /// is left to reclaim it. Unless the reclaim runs on every pass, that slot
    /// is occupied for the rest of the process's life and the wedge is silent
    /// (`claim_next_scan` returns `Ok(None)`, indistinguishable from an empty
    /// queue).
    ///
    /// The lease here is one second: valid at boot, lapsed by the second pass.
    #[tokio::test]
    async fn a_lease_that_lapses_after_boot_is_still_reclaimed() {
        let db = test_db();
        db.enqueue_scan("did:plc:redeploy").await.expect("enqueue");
        // Positive lease — NOT the already-expired `-1` the boot test uses.
        // This row is legitimately held when the admitter starts.
        db.claim_next_scan(1, 1).await.expect("claim").expect("row");

        let (notify_tx, mut notify_rx) = mpsc::channel(4);
        let launcher = Arc::new(RecordingLauncher::notifying(notify_tx));
        // Held for the whole test so the wake channel never closes.
        let (_wake_tx, wake_rx) = mpsc::channel(4);

        let handle = tokio::spawn(run_admitter(
            db.clone(),
            launcher.clone(),
            wake_rx,
            Duration::from_millis(50),
            || 1,
        ));

        let launched = tokio::time::timeout(Duration::from_secs(10), notify_rx.recv())
            .await
            .expect(
                "a lease that lapses AFTER boot must still be reclaimed — the reclaim \
                 has to run on every pass, not only before the first claim",
            )
            .expect("launcher notified");
        assert_eq!(launched, "did:plc:redeploy");
        handle.abort();
    }

    /// The wake channel must admit immediately. Otherwise a user who enqueues
    /// into an idle server waits up to a full tick before anything happens.
    #[tokio::test]
    async fn a_wake_admits_without_waiting_for_the_tick() {
        let db = test_db();

        let (notify_tx, mut notify_rx) = mpsc::channel(4);
        let launcher = Arc::new(RecordingLauncher::notifying(notify_tx));
        let (wake_tx, wake_rx) = mpsc::channel(4);

        let handle = tokio::spawn(run_admitter(
            db.clone(),
            launcher.clone(),
            wake_rx,
            Duration::from_secs(600),
            || 1,
        ));

        // Let the boot pass find an empty queue first, so the admission below
        // can only come from the wake.
        tokio::time::sleep(Duration::from_millis(50)).await;
        db.enqueue_scan("did:plc:late").await.expect("enqueue");
        wake_tx.send(()).await.expect("wake");

        let launched = tokio::time::timeout(Duration::from_secs(2), notify_rx.recv())
            .await
            .expect("a wake must admit without waiting for the 600s tick")
            .expect("launcher notified");
        assert_eq!(launched, "did:plc:late");
        handle.abort();
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    // All four mutate the same process-global env var, so they cannot run
    // concurrently with each other (or with anything else reading it).
    use serial_test::serial;

    #[test]
    #[serial(scan_concurrency_env)]
    fn concurrency_defaults_to_two_when_unset() {
        std::env::remove_var("CHARCOAL_SCAN_CONCURRENCY");
        assert_eq!(scan_concurrency(), 2);
    }

    #[test]
    #[serial(scan_concurrency_env)]
    fn concurrency_honours_the_env_var() {
        std::env::set_var("CHARCOAL_SCAN_CONCURRENCY", "3");
        assert_eq!(scan_concurrency(), 3);
        std::env::remove_var("CHARCOAL_SCAN_CONCURRENCY");
    }

    /// A malformed or zero value must not disable admission entirely, which
    /// would silently stop every scan on the server.
    #[test]
    #[serial(scan_concurrency_env)]
    fn concurrency_rejects_zero_and_garbage() {
        std::env::set_var("CHARCOAL_SCAN_CONCURRENCY", "0");
        assert_eq!(
            scan_concurrency(),
            2,
            "zero must fall back, not block all scans"
        );
        std::env::set_var("CHARCOAL_SCAN_CONCURRENCY", "banana");
        assert_eq!(scan_concurrency(), 2);
        std::env::remove_var("CHARCOAL_SCAN_CONCURRENCY");
    }

    /// Clamped so a fat-fingered value cannot exhaust memory or breach the
    /// Bluesky ceiling by an order of magnitude.
    #[test]
    #[serial(scan_concurrency_env)]
    fn concurrency_is_clamped() {
        std::env::set_var("CHARCOAL_SCAN_CONCURRENCY", "500");
        assert_eq!(scan_concurrency(), 8);
        std::env::remove_var("CHARCOAL_SCAN_CONCURRENCY");
    }
}
