//! Background action runner (#315, spec §4).
//!
//! One runner per process, one batch at a time. Reconcile-first: it reads
//! the user's live mute/block lists before writing, so re-running a batch
//! (after a deploy, a 401, a retry click) never duplicates anything.

use std::sync::Arc;
use std::time::{Duration, Instant};

use atproto_oauth::workflow::OAuthClient;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use super::pds::{PdsClient, PdsError, Write, APPLY_WRITES_MAX};
use super::session::{SessionError, SessionStore};
use crate::db::traits::ActionRow;
use crate::db::Database;
use crate::web::AppState;

#[derive(Clone, Debug)]
pub struct RunnerConfig {
    /// Sleep between consecutive per-actor calls (muteActor/unmuteActor and
    /// the one-at-a-time block fallback). Keeps us well under PDS limits.
    pub pace: Duration,
    /// Base for the ×1/×2/×4 transient-error backoff.
    pub backoff: Duration,
    /// Cap on a 429 wait, whatever `ratelimit-reset` says.
    pub max_wait: Duration,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            pace: Duration::from_millis(100),
            backoff: Duration::from_secs(1),
            max_wait: Duration::from_secs(120),
        }
    }
}

impl RunnerConfig {
    /// For tests: no pacing, millisecond backoff, 50 ms rate-limit cap.
    pub fn fast() -> Self {
        Self {
            pace: Duration::ZERO,
            backoff: Duration::from_millis(1),
            max_wait: Duration::from_millis(50),
        }
    }
}

const RATE_LIMIT_MAX_WAITS: u32 = 5;
const TRANSIENT_RETRIES: u32 = 3;

pub struct ActionRunner {
    db: Arc<dyn Database>,
    http: reqwest::Client,
    oauth_client: OAuthClient,
    sessions: Arc<SessionStore>,
    cfg: RunnerConfig,
}

/// The batch cannot continue; remaining actions stay `pending`.
enum Halt {
    /// PDS answered 401: the token is dead. Session deleted, batch back to
    /// `queued` / `not_connected` so reconnect + retry resumes it.
    NotConnected,
}

/// One planned applyWrites entry: which action it belongs to and, for undo
/// rows, which original to mark `undone` on success.
struct Planned<'a> {
    action: &'a ActionRow,
    write: Write,
    undo_of: Option<i64>,
}

impl ActionRunner {
    pub fn new(
        db: Arc<dyn Database>,
        http: reqwest::Client,
        oauth_client: OAuthClient,
        sessions: Arc<SessionStore>,
        cfg: RunnerConfig,
    ) -> Self {
        Self {
            db,
            http,
            oauth_client,
            sessions,
            cfg,
        }
    }

    /// `None` when the actions feature is disabled (no token key).
    pub fn from_state(state: &AppState) -> Option<Self> {
        let sessions = state.sessions.clone()?;
        Some(Self::new(
            state.db.clone(),
            state.http.clone(),
            crate::web::handlers::oauth::oauth_client(state),
            sessions,
            RunnerConfig::default(),
        ))
    }

    /// Run every batch still `queued` or `running`, oldest first.
    pub async fn run_all_unfinished(&self) {
        match self.db.list_unfinished_batches().await {
            Ok(ids) => {
                for id in ids {
                    self.run_batch(id).await;
                }
            }
            Err(e) => error!("could not list unfinished action batches: {e:#}"),
        }
    }

    /// Run one batch to completion. Errors end up in the batch row, never
    /// in the caller.
    pub async fn run_batch(&self, batch_id: i64) {
        if let Err(e) = self.run_batch_inner(batch_id).await {
            error!(batch_id, "action batch failed: {e:#}");
            if let Err(e2) = self
                .db
                .set_action_batch_status(batch_id, "failed", Some(&format!("{e:#}")))
                .await
            {
                error!(batch_id, "could not record batch failure: {e2:#}");
            }
        }
    }

    async fn run_batch_inner(&self, batch_id: i64) -> anyhow::Result<()> {
        let Some(batch) = self.db.get_action_batch(batch_id).await? else {
            return Ok(());
        };
        if !matches!(batch.status.as_str(), "queued" | "running") {
            return Ok(());
        }
        let batch_started = Instant::now();
        self.db
            .set_action_batch_status(batch_id, "running", None)
            .await?;

        // Includes any token refresh the session store does on the way.
        let load_started = Instant::now();
        let loaded = self
            .sessions
            .load_for_write(&*self.db, &self.http, &self.oauth_client, &batch.user_did)
            .await;
        info!(
            batch_id,
            span = "load_session",
            elapsed_ms = load_started.elapsed().as_millis() as u64,
            "timing"
        );
        let session = match loaded {
            Ok(s) => s,
            Err(SessionError::NotConnected) => {
                self.db
                    .set_action_batch_status(batch_id, "queued", Some("not_connected"))
                    .await?;
                return Ok(());
            }
            Err(e) => anyhow::bail!("load write session: {e}"),
        };
        let pds = session.pds_client(self.http.clone());

        let pending: Vec<ActionRow> = self
            .db
            .list_actions_for_batch(batch_id)
            .await?
            .into_iter()
            .filter(|a| a.status == "pending")
            .collect();
        info!(batch_id, kind = %batch.kind, pending = pending.len(), "running action batch");

        let halt = match batch.kind.as_str() {
            "mute" => self.run_mutes(&pds, &pending).await?,
            "block" => self.run_blocks(&pds, &pending).await?,
            "undo" => self.run_undo(&pds, &session.did, &pending).await?,
            other => anyhow::bail!("unknown batch kind {other:?}"),
        };

        if let Some(Halt::NotConnected) = halt {
            warn!(batch_id, "PDS rejected the access token — disconnecting");
            self.db.delete_oauth_session(&batch.user_did).await?;
            self.db
                .set_action_batch_status(batch_id, "queued", Some("not_connected"))
                .await?;
            return Ok(());
        }
        self.finalize(batch_id, batch_started).await
    }

    async fn finalize(&self, batch_id: i64, started: Instant) -> anyhow::Result<()> {
        let rows = self.db.list_actions_for_batch(batch_id).await?;
        let failed = rows.iter().filter(|a| a.status == "failed").count();
        let ok = rows
            .iter()
            .filter(|a| {
                matches!(
                    a.status.as_str(),
                    "applied" | "skipped_already_done" | "undone"
                )
            })
            .count();
        let status = if failed == 0 {
            "done"
        } else if ok == 0 {
            "failed"
        } else {
            "partial"
        };
        info!(
            batch_id,
            status,
            ok,
            failed,
            span = "batch",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "action batch finished"
        );
        self.db
            .set_action_batch_status(batch_id, status, None)
            .await
    }

    /// One PDS call under the retry policy. `Err(Halt)` stops the batch;
    /// `Ok(Err(_))` fails this action only. `op` is the nsid-ish label for
    /// the timing line (`"muteActor"`, `"applyWrites"`, …) — #333 needs to
    /// see where the seconds go, per attempt.
    async fn call<T, F, Fut>(&self, op: &'static str, mut f: F) -> Result<Result<T, PdsError>, Halt>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, PdsError>>,
    {
        let mut transient = 0u32;
        let mut limited = 0u32;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let started = Instant::now();
            let result = f().await;
            info!(
                span = "pds_call",
                op,
                attempt,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "timing"
            );
            match result {
                Ok(v) => return Ok(Ok(v)),
                Err(PdsError::Auth) => return Err(Halt::NotConnected),
                Err(PdsError::RateLimited { reset_at }) if limited < RATE_LIMIT_MAX_WAITS => {
                    limited += 1;
                    let wait = reset_at
                        .map(|t| {
                            Duration::from_secs((t - chrono::Utc::now().timestamp()).max(1) as u64)
                        })
                        .unwrap_or(self.cfg.backoff * 4)
                        .min(self.cfg.max_wait);
                    info!(
                        wait_ms = wait.as_millis() as u64,
                        "rate limited by PDS — pausing"
                    );
                    tokio::time::sleep(wait).await;
                }
                Err(e) if e.is_retryable() && transient < TRANSIENT_RETRIES => {
                    tokio::time::sleep(self.cfg.backoff * 2u32.pow(transient)).await;
                    transient += 1;
                }
                Err(e) => return Ok(Err(e)),
            }
        }
    }

    async fn run_mutes(
        &self,
        pds: &PdsClient,
        pending: &[ActionRow],
    ) -> anyhow::Result<Option<Halt>> {
        // Reconcile set. PdsClient::paginate caps at MAX_LIST_PAGES pages and
        // returns Err rather than a partial list — `anyhow::bail!` below then
        // fails this batch and leaves its rows pending for Retry, instead of
        // reading a truncated set as "not muted" and re-muting someone the
        // user already muted.
        let started = Instant::now();
        let existing = self.call("getMutes", || pds.get_mutes()).await;
        info!(
            span = "reconcile",
            op = "getMutes",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "timing"
        );
        let existing = match existing {
            Ok(Ok(set)) => set,
            Ok(Err(e)) => anyhow::bail!("getMutes: {e}"),
            Err(h) => return Ok(Some(h)),
        };
        for a in pending {
            if existing.contains(&a.target_did) {
                self.db
                    .update_action(a.id, "skipped_already_done", None, None)
                    .await?;
                continue;
            }
            match self
                .call("muteActor", || pds.mute_actor(&a.target_did))
                .await
            {
                Ok(Ok(())) => self.db.update_action(a.id, "applied", None, None).await?,
                Ok(Err(e)) => {
                    self.db
                        .update_action(a.id, "failed", None, Some(&e.to_string()))
                        .await?
                }
                Err(h) => return Ok(Some(h)),
            }
            tokio::time::sleep(self.cfg.pace).await;
        }
        Ok(None)
    }

    async fn run_blocks(
        &self,
        pds: &PdsClient,
        pending: &[ActionRow],
    ) -> anyhow::Result<Option<Halt>> {
        // Reconcile set. PdsClient::paginate caps at MAX_LIST_PAGES pages and
        // returns Err rather than a partial list — `anyhow::bail!` below then
        // fails this batch and leaves its rows pending for Retry, instead of
        // reading a truncated set as "not blocked" and creating a second
        // block record for someone the user already blocked.
        let started = Instant::now();
        let existing = self.call("getBlocks", || pds.get_blocks()).await;
        info!(
            span = "reconcile",
            op = "getBlocks",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "timing"
        );
        let existing = match existing {
            Ok(Ok(map)) => map,
            Ok(Err(e)) => anyhow::bail!("getBlocks: {e}"),
            Err(h) => return Ok(Some(h)),
        };
        let mut planned = Vec::new();
        for a in pending {
            if existing.contains_key(&a.target_did) {
                // A block the user already holds. Left alone and never
                // recorded as ours, so undo can never remove it (#261).
                self.db
                    .update_action(a.id, "skipped_already_done", None, None)
                    .await?;
            } else {
                planned.push(Planned {
                    action: a,
                    write: PdsClient::block_create(&a.target_did),
                    undo_of: None,
                });
            }
        }
        self.apply_chunked(pds, &planned).await
    }

    async fn run_undo(
        &self,
        pds: &PdsClient,
        own_did: &str,
        pending: &[ActionRow],
    ) -> anyhow::Result<Option<Halt>> {
        // Reconcile sets. PdsClient::paginate caps at MAX_LIST_PAGES pages
        // and returns Err rather than a partial list, which fails this batch
        // and leaves its rows pending for Retry, instead of reading a
        // truncated set as "already gone" and settling an undo that never
        // removed anything.
        let blocks = if pending.iter().any(|a| a.kind == "block") {
            let started = Instant::now();
            let got = self.call("getBlocks", || pds.get_blocks()).await;
            info!(
                span = "reconcile",
                op = "getBlocks",
                elapsed_ms = started.elapsed().as_millis() as u64,
                "timing"
            );
            match got {
                Ok(Ok(map)) => map,
                Ok(Err(e)) => anyhow::bail!("getBlocks: {e}"),
                Err(h) => return Ok(Some(h)),
            }
        } else {
            Default::default()
        };
        let mutes = if pending.iter().any(|a| a.kind == "mute") {
            let started = Instant::now();
            let got = self.call("getMutes", || pds.get_mutes()).await;
            info!(
                span = "reconcile",
                op = "getMutes",
                elapsed_ms = started.elapsed().as_millis() as u64,
                "timing"
            );
            match got {
                Ok(Ok(set)) => set,
                Ok(Err(e)) => anyhow::bail!("getMutes: {e}"),
                Err(h) => return Ok(Some(h)),
            }
        } else {
            Default::default()
        };

        let mut planned = Vec::new();
        for a in pending {
            let orig = match a.undo_of {
                Some(id) => self.db.get_action(id).await?,
                None => None,
            };
            let Some(orig) = orig else {
                self.db
                    .update_action(a.id, "failed", None, Some("undo row has no original"))
                    .await?;
                continue;
            };
            // Belt and braces: the handlers only enqueue undos for rows
            // Charcoal itself applied. A `skipped_already_done` original is
            // the user's OWN mute or block, and mutes carry no record_uri, so
            // this is the last place the two can still be told apart (#261).
            if orig.status != "applied" {
                self.db
                    .update_action(a.id, "failed", None, Some("not created by Charcoal"))
                    .await?;
                continue;
            }
            match a.kind.as_str() {
                "mute" => {
                    if !mutes.contains(&a.target_did) {
                        self.mark_undone(a.id, "skipped_already_done", orig.id)
                            .await?;
                        continue;
                    }
                    match self
                        .call("unmuteActor", || pds.unmute_actor(&a.target_did))
                        .await
                    {
                        Ok(Ok(())) => self.mark_undone(a.id, "applied", orig.id).await?,
                        Ok(Err(e)) => {
                            self.db
                                .update_action(a.id, "failed", None, Some(&e.to_string()))
                                .await?
                        }
                        Err(h) => return Ok(Some(h)),
                    }
                    tokio::time::sleep(self.cfg.pace).await;
                }
                "block" => match blocks.get(&a.target_did) {
                    // Not blocked at all — reality already matches the undo.
                    None => {
                        self.mark_undone(a.id, "skipped_already_done", orig.id)
                            .await?
                    }
                    Some(in_force) => {
                        // Delete only a record Charcoal created (record_uri
                        // stored) that is still the block in force.
                        let ours = orig
                            .record_uri
                            .as_deref()
                            .filter(|uri| *uri == in_force.as_str())
                            .and_then(|uri| PdsClient::rkey_from_uri(own_did, uri));
                        match ours {
                            Some(rkey) => planned.push(Planned {
                                action: a,
                                write: PdsClient::block_delete(&rkey),
                                undo_of: Some(orig.id),
                            }),
                            // Blocked, but by a record Charcoal did not
                            // create (the user deleted ours and made their
                            // own). Saying so beats stamping the original
                            // `undone` over a block that is still live.
                            None => {
                                self.db
                                    .update_action(
                                        a.id,
                                        "failed",
                                        None,
                                        Some("block was not created by Charcoal"),
                                    )
                                    .await?
                            }
                        }
                    }
                },
                other => {
                    self.db
                        .update_action(
                            a.id,
                            "failed",
                            None,
                            Some(&format!("unknown kind {other:?}")),
                        )
                        .await?
                }
            }
        }
        self.apply_chunked(pds, &planned).await
    }

    /// applyWrites in ≤200-entry chunks; a non-429 4xx on a multi-entry
    /// chunk is redone one entry at a time so only the bad entry fails.
    async fn apply_chunked(
        &self,
        pds: &PdsClient,
        planned: &[Planned<'_>],
    ) -> anyhow::Result<Option<Halt>> {
        for chunk in planned.chunks(APPLY_WRITES_MAX) {
            let writes: Vec<Write> = chunk.iter().map(|p| p.write.clone()).collect();
            match self.call("applyWrites", || pds.apply_writes(&writes)).await {
                Ok(Ok(uris)) => {
                    for (p, uri) in chunk.iter().zip(uris) {
                        self.record_success(p, uri.as_deref()).await?;
                    }
                }
                Ok(Err(PdsError::Client { .. })) if chunk.len() > 1 => {
                    for p in chunk {
                        let one = [p.write.clone()];
                        match self.call("applyWrites", || pds.apply_writes(&one)).await {
                            Ok(Ok(uris)) => {
                                self.record_success(p, uris.first().cloned().flatten().as_deref())
                                    .await?
                            }
                            Ok(Err(e)) => {
                                self.db
                                    .update_action(
                                        p.action.id,
                                        "failed",
                                        None,
                                        Some(&e.to_string()),
                                    )
                                    .await?
                            }
                            Err(h) => return Ok(Some(h)),
                        }
                        tokio::time::sleep(self.cfg.pace).await;
                    }
                }
                Ok(Err(e)) => {
                    for p in chunk {
                        self.db
                            .update_action(p.action.id, "failed", None, Some(&e.to_string()))
                            .await?;
                    }
                }
                Err(h) => return Ok(Some(h)),
            }
        }
        Ok(None)
    }

    async fn record_success(&self, p: &Planned<'_>, uri: Option<&str>) -> anyhow::Result<()> {
        match p.undo_of {
            Some(orig) => self.mark_undone(p.action.id, "applied", orig).await,
            None if matches!(p.write, Write::Create { .. }) && uri.is_none() => {
                // The record landed on the PDS but came back without a URI,
                // so undo could never remove it. Recording `applied` here
                // would silently cost the user the reversibility this whole
                // feature promises; `failed` makes the batch `partial` and
                // lets Retry re-attempt it (the reconcile step then finds the
                // block and settles the row honestly).
                self.db
                    .update_action(
                        p.action.id,
                        "failed",
                        None,
                        Some("PDS returned no record URI"),
                    )
                    .await
            }
            None => {
                self.db
                    .update_action(p.action.id, "applied", uri, None)
                    .await
            }
        }
    }

    async fn mark_undone(
        &self,
        undo_id: i64,
        undo_status: &str,
        orig_id: i64,
    ) -> anyhow::Result<()> {
        self.db
            .update_action(undo_id, undo_status, None, None)
            .await?;
        self.db.update_action(orig_id, "undone", None, None).await
    }
}

/// Spawn the process-wide runner. Resumes anything left `queued`/`running`
/// by the previous deploy, then services wakes. Each wake runs every
/// unfinished batch (the id is informational), so a full channel or a
/// dropped send never strands work — the next wake or boot picks it up.
pub fn spawn_runner(state: AppState) -> mpsc::Sender<i64> {
    let (tx, mut rx) = mpsc::channel::<i64>(256);
    let Some(runner) = ActionRunner::from_state(&state) else {
        return tx;
    };
    tokio::spawn(async move {
        runner.run_all_unfinished().await;
        while let Some(id) = rx.recv().await {
            info!(batch_id = id, "action runner woken");
            runner.run_all_unfinished().await;
        }
    });
    tx
}
