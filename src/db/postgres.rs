// PgDatabase — PostgreSQL backend implementing the Database trait.
//
// Uses sqlx PgPool for native async queries. All queries use runtime
// parameter binding (not compile-time macros) to avoid requiring
// DATABASE_URL at compile time.
//
// Key differences from SQLite:
// - TIMESTAMPTZ instead of TEXT for timestamps
// - JSONB instead of TEXT for structured data
// - pgvector for embedding storage
// - $1/$2 parameter syntax (handled by sqlx)
// - GENERATED ALWAYS AS IDENTITY for auto-increment

use anyhow::{Context, Result};
use async_trait::async_trait;
use sqlx_core::pool::Pool;
use sqlx_core::row::Row;
use sqlx_postgres::Postgres;

use super::models::{
    AccountScore, AccuracyMetrics, AmplificationEvent, ClusterCentroid, ExportedExpiry,
    InferredPair, NewAmplificationEvent, StoredScore, ThreatTier, ToxicPost, UserLabel, UserRow,
};
use super::traits::{
    eta_seconds, AccessRequestRow, ActionBatchRow, ActionRow, ClassifierVerdictRow, Database,
    EnqueueOutcome, FeedSnapshot, FinishCompletion, NewAction, OauthSessionRow, OnnxScoreRow,
    RefreshCandidate, ScanClaim, ScanKind, ScanQueueDepth, ScanQueueEntry, ScanQueueRow, ScanSkip,
    ScoreSnapshot,
};
use crate::db::models::ScoringConfidence;
use crate::pipeline::scan_phases::staging::{QueueRow, VerdictRow};
use crate::scoring::generation::scoring_revision;

/// Type alias for the PostgreSQL connection pool.
pub type PgPool = Pool<Postgres>;

/// Advisory-lock key that serializes scan admission (#257).
///
/// The pool runs at READ COMMITTED, so `SELECT COUNT(*) WHERE status='running'`
/// takes no lock and sees only rows committed before the statement began. Two
/// admitters therefore both read the same pre-claim count, both pass the cap
/// guard, and `FOR UPDATE SKIP LOCKED` hands them *different* rows — so it
/// cannot enforce the cap; it only stops double-claiming one row. Taking a
/// transaction-scoped advisory lock before the count makes admission
/// single-file, which is what the cap actually requires.
///
/// The value is arbitrary but must be identical in every admitter, so it lives
/// here as a constant rather than inline. Nothing else in this codebase takes a
/// Postgres advisory lock, so there is no collision to avoid; the digits are a
/// mnemonic for "charcoal #257 scan queue".
const SCAN_ADMISSION_ADVISORY_LOCK_KEY: i64 = 0x0000_0257_5CA4_0001;

/// The refresh tick's SELECT (#343 §4.4, #344). Params: `$1` now (RFC3339
/// text, cast), `$2` current scoring revision, `$3` limit.
///
/// `pub` so the V2-02 interleaving tests can run the real statement on their
/// own connections around a concurrent manual enqueue — a test against a
/// paraphrase of it would prove nothing about production.
///
/// Differences from the SQLite twin, all deliberate: `IS DISTINCT FROM`
/// instead of `IS NULL OR != `; `NULLS FIRST` because Postgres sorts NULLs
/// last by default and SQLite sorts them first; and `FOR UPDATE OF u SKIP
/// LOCKED`, which is what lets two replicas tick at the same time and
/// partition the due set rather than block on each other.
pub const REFRESH_DUE_SQL: &str = "SELECT u.did FROM users u
     WHERE (EXISTS (SELECT 1 FROM account_scores s WHERE s.user_did = u.did)
            OR EXISTS (SELECT 1 FROM scan_queue o
                       WHERE o.user_did = u.did AND o.full_requested_at IS NOT NULL))
       AND NOT EXISTS (SELECT 1 FROM scan_queue q
                       WHERE q.user_did = u.did AND q.status IN ('queued', 'running'))
       AND (u.next_refresh_at <= $1::timestamptz
            OR u.refresh_attempted_generation IS DISTINCT FROM $2)
     ORDER BY u.next_refresh_at NULLS FIRST, u.did
     LIMIT $3
     FOR UPDATE OF u SKIP LOCKED";

/// The conditional queue write shared by `enqueue_refresh_scan` and the tick.
/// Params: `$1` user_did, `$2` now (RFC3339 text, cast).
///
/// The `WHERE` is evaluated against the row as it is AT THE WRITE, so a full
/// row queued or admitted after the tick's select is never clobbered (V2-02);
/// it affects 0 rows in that case. A finished row that still owes a full scan
/// is re-queued as FULL (V3-03) — `full_requested_at` is deliberately absent
/// from the SET list, because owed work stays owed until a full scan
/// completes.
pub const REFRESH_ENQUEUE_SQL: &str = "INSERT INTO scan_queue (user_did, status, kind, enqueued_at)
     VALUES ($1, 'queued', 'refresh', $2::timestamptz)
     ON CONFLICT (user_did) DO UPDATE
       SET status = 'queued',
           kind = CASE WHEN scan_queue.full_requested_at IS NOT NULL
                       THEN 'full' ELSE 'refresh' END,
           enqueued_at = $2::timestamptz,
           started_at = NULL, finished_at = NULL,
           lease_expires = NULL, last_error = NULL,
           claim_id = NULL, completion = NULL
     WHERE scan_queue.status IN ('done', 'failed')";

pub struct PgDatabase {
    pool: PgPool,
}

impl PgDatabase {
    /// Test seam for the atomicity of `finish_full_scan_state` (#344 F2).
    ///
    /// Runs the identical statements inside the identical transaction and then
    /// fails, so a test can assert that NOTHING the transaction wrote
    /// survives. `#[doc(hidden)]`, never called in production: the real method
    /// has no statement a caller can make fail from outside, and "the source
    /// says BEGIN" is not evidence that the rollback works. `pub` rather than
    /// `#[cfg(test)]` because the Postgres tests live in an integration test
    /// binary, which compiles against the library.
    #[doc(hidden)]
    pub async fn finish_full_scan_state_failing_for_test(
        &self,
        user_did: &str,
        finished_at_rfc3339: &str,
        carried_key: &str,
        claim_id: &str,
    ) -> Result<()> {
        self.finish_full_scan_state_inner(
            user_did,
            finished_at_rfc3339,
            carried_key,
            claim_id,
            true,
        )
        .await
    }

    /// One transaction: the cooldown anchor, the ETA duration sample and the
    /// retirement of the carried drain outcome are a single fact (#344 V7-02,
    /// F1). Split, a crash between them either starts a cooldown for a run
    /// still owed or leaves a drain outcome behind to taint an unrelated later
    /// scan.
    async fn finish_full_scan_state_inner(
        &self,
        user_did: &str,
        finished_at_rfc3339: &str,
        carried_key: &str,
        claim_id: &str,
        fail_before_commit: bool,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        // Fenced by the claim (#344 N2) — see the SQLite twin for why all
        // three writes belong to the worker that still owns the row.
        let owner = sqlx_core::query::query(
            "SELECT started_at, claim_id FROM scan_queue WHERE user_did = $1 FOR UPDATE",
        )
        .bind(user_did)
        .fetch_optional(&mut *tx)
        .await?
        .map(|r| {
            (
                r.get::<Option<chrono::DateTime<chrono::Utc>>, _>(0),
                r.get::<Option<String>, _>(1),
            )
        });
        let started_at = match owner {
            Some((started_at, Some(owner_claim))) if owner_claim == claim_id => started_at,
            _ => {
                tracing::warn!(
                    user_did,
                    "full scan finished but its claim no longer owns the queue row — no \
                     cooldown anchor, no ETA sample, no carried-outcome retirement"
                );
                return Ok(());
            }
        };
        sqlx_core::query::query(
            "INSERT INTO scan_state (user_did, key, value, updated_at)
             VALUES ($1, 'last_full_scan_finished_at', $2, NOW())
             ON CONFLICT(user_did, key) DO UPDATE SET value = $2, updated_at = NOW()",
        )
        .bind(user_did)
        .bind(finished_at_rfc3339)
        .execute(&mut *tx)
        .await?;
        // The ETA sample (F1). Read from the row's own `started_at` inside
        // this transaction: right now the queue row still describes the
        // attempt being fulfilled, and by the time a refresh reuses the row it
        // will not. Parsed through the shared helper rather than computed in
        // SQL so both backends agree on what a duration is.
        let started_at = started_at.map(|s| s.to_rfc3339());
        if let Some(secs) =
            super::traits::full_scan_duration_secs(started_at.as_deref(), finished_at_rfc3339)
        {
            sqlx_core::query::query(
                "INSERT INTO scan_state (user_did, key, value, updated_at)
                 VALUES ($1, $2, $3, NOW())
                 ON CONFLICT(user_did, key) DO UPDATE SET value = $3, updated_at = NOW()",
            )
            .bind(user_did)
            .bind(super::traits::LAST_FULL_SCAN_DURATION_KEY)
            .bind(secs.to_string())
            .execute(&mut *tx)
            .await?;
        }
        sqlx_core::query::query("DELETE FROM scan_state WHERE user_did = $1 AND key = $2")
            .bind(user_did)
            .bind(carried_key)
            .execute(&mut *tx)
            .await?;
        if fail_before_commit {
            // Valid SQL that cannot succeed — `scan_state.value` is NOT NULL —
            // so the failure happens INSIDE the transaction exactly as a real
            // error would, and `?` returns without ever reaching the commit.
            sqlx_core::query::query(
                "INSERT INTO scan_state (user_did, key, value, updated_at)
                 VALUES ($1, 'injected_failure_for_test', NULL, NOW())",
            )
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Connect to PostgreSQL and run migrations.
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = PgPool::connect(database_url)
            .await
            .with_context(|| format!("Failed to connect to PostgreSQL at {database_url}"))?;

        let db = Self { pool };
        db.run_migrations().await?;
        Ok(db)
    }

    /// Run all pending migrations.
    ///
    /// Acquires a Postgres session-level advisory lock (key 0x_CHAR_COAL) so
    /// that concurrent processes (e.g. two app instances starting together)
    /// don't race to apply the same migration.
    ///
    /// Session-level advisory locks are bound to the backend session that
    /// acquired them, so the lock and unlock MUST run on the same physical
    /// connection. We acquire a dedicated connection (`lock_conn`) for this
    /// purpose and keep it alive for the duration of the migration loop.
    /// Migrations themselves can use the pool normally. The unlock always runs
    /// even if a migration fails — we capture the migration result first, then
    /// unlock, then surface any error.
    ///
    /// Migration 1 contains `CREATE EXTENSION` which cannot run inside a
    /// transaction. All of its DDL uses `IF NOT EXISTS` so it is safe to
    /// retry if partially applied. Migrations 2+ are wrapped in a transaction
    /// so the schema change and the schema_version insert are atomic.
    async fn run_migrations(&self) -> Result<()> {
        // 0x43484152434F414C = ASCII "CHARCOAL" as a big-endian i64.
        // Used as the advisory lock key to namespace this lock to Charcoal.
        const MIGRATION_LOCK_KEY: i64 = 0x43484152434F414C_u64 as i64;

        // Acquire a dedicated connection to hold the advisory lock for the
        // entire migration sequence. Dropping this connection returns it to
        // the pool AND releases the session-level advisory lock automatically.
        let mut lock_conn = self
            .pool
            .acquire()
            .await
            .context("Failed to acquire connection for migration advisory lock")?;

        // Block until no other Charcoal process is running migrations.
        sqlx_core::query::query("SELECT pg_advisory_lock($1)")
            .bind(MIGRATION_LOCK_KEY)
            .execute(&mut *lock_conn)
            .await
            .context("Failed to acquire migration advisory lock")?;

        // Run all migrations using the shared pool. The advisory lock is held
        // on lock_conn independently, so pool connections can be used freely.
        // `apply_migrations_up_to` is shared with `migrate_postgres_through`
        // (test support), so the boot path and test fixtures replay exactly
        // the same migration sequence.
        let migration_result: Result<()> = apply_migrations_up_to(&self.pool, i64::MAX).await;

        // Release the advisory lock on the same connection that acquired it.
        // This always runs even if migrations failed — we surface the migration
        // error below, but we never skip the unlock.
        let unlock_result = sqlx_core::query::query("SELECT pg_advisory_unlock($1)")
            .bind(MIGRATION_LOCK_KEY)
            .execute(&mut *lock_conn)
            .await
            .context("Failed to release migration advisory lock");

        // Migration error takes priority over unlock error.
        migration_result?;
        unlock_result?;

        Ok(())
    }
}

/// The full, ordered list of Postgres migrations. Shared between the
/// boot-time `run_migrations` (always plays through the latest version) and
/// `migrate_postgres_through` (test support for an authentic older-schema
/// fixture — V2-07) so the two paths can never drift apart.
fn all_migrations() -> [(i64, &'static str); 18] {
    [
        (
            1,
            include_str!("../../migrations/postgres/0001_initial.sql"),
        ),
        (
            2,
            include_str!("../../migrations/postgres/0002_pgvector.sql"),
        ),
        (
            3,
            include_str!("../../migrations/postgres/0003_behavioral_signals.sql"),
        ),
        (
            4,
            include_str!("../../migrations/postgres/0004_multiuser.sql"),
        ),
        (
            5,
            include_str!("../../migrations/postgres/0005_contextual_scoring.sql"),
        ),
        (
            6,
            include_str!("../../migrations/postgres/0006_graph_distance.sql"),
        ),
        (
            7,
            include_str!("../../migrations/postgres/0007_last_login_at.sql"),
        ),
        (
            8,
            include_str!("../../migrations/postgres/0008_fingerprint_scoring.sql"),
        ),
        (
            9,
            include_str!("../../migrations/postgres/0009_classification_staging.sql"),
        ),
        (
            10,
            include_str!("../../migrations/postgres/0010_scan_skips.sql"),
        ),
        (
            11,
            include_str!("../../migrations/postgres/0011_scan_queue.sql"),
        ),
        (
            12,
            include_str!("../../migrations/postgres/0012_scan_queue_claim_id.sql"),
        ),
        (
            13,
            include_str!("../../migrations/postgres/0013_topic_clusters.sql"),
        ),
        (
            14,
            include_str!("../../migrations/postgres/0014_access_requests.sql"),
        ),
        (
            15,
            include_str!("../../migrations/postgres/0015_actions.sql"),
        ),
        (
            16,
            include_str!("../../migrations/postgres/0016_shared_cache.sql"),
        ),
        (
            17,
            include_str!("../../migrations/postgres/0017_cache_indexes.sql"),
        ),
        (
            18,
            include_str!("../../migrations/postgres/0018_score_expiry.sql"),
        ),
    ]
}

/// Apply every migration up to and including `max_version` against `pool`.
/// Migration 1 contains `CREATE EXTENSION`, which cannot run inside a
/// transaction, so it runs bare (its DDL is all `IF NOT EXISTS`, safe to
/// retry); migrations 2+ run inside a transaction so the schema change and
/// the `schema_version` insert commit or roll back together — though every
/// migration from v4 onward self-records its own version row, per the
/// project rule that the runner does not do it for them.
async fn apply_migrations_up_to(pool: &PgPool, max_version: i64) -> Result<()> {
    // Ensure schema_version table exists (idempotent DDL, no transaction needed)
    sqlx_core::query::query(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER PRIMARY KEY,
            applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )",
    )
    .execute(pool)
    .await?;

    for (version, sql) in all_migrations() {
        if version > max_version {
            continue;
        }

        let applied: bool =
            sqlx_core::query::query("SELECT COUNT(*) > 0 FROM schema_version WHERE version = $1")
                .bind(version)
                .fetch_one(pool)
                .await
                .map(|row| row.get::<bool, _>(0))
                .unwrap_or(false);

        if !applied {
            if version == 1 {
                // Migration 1 contains CREATE EXTENSION which cannot run inside a
                // transaction. All statements use IF NOT EXISTS so they are safe
                // to retry if the process is interrupted partway through.
                sqlx_core::raw_sql::raw_sql(sql).execute(pool).await?;
            } else {
                // Migrations 2+ are wrapped in a transaction so the schema change
                // and schema_version insert are committed or rolled back together.
                let mut tx = pool.begin().await?;
                sqlx_core::raw_sql::raw_sql(sql).execute(&mut *tx).await?;
                tx.commit().await?;
            }
        }
    }

    Ok(())
}

/// Test support (V2-07): reset a dedicated Postgres database and replay
/// migrations up to `max_version`, producing an AUTHENTIC older-schema
/// fixture rather than one built by dropping columns off a current database
/// — that leaves every OTHER new column in place, so the migration that
/// re-adds the dropped ones fails on duplicates instead of exercising the
/// real upgrade path.
///
/// Refuses (`bail!`) to run against anything but a database whose name ends
/// in `_migrations` (V3-06): this drops every table it finds, and the
/// ordinary test suite's fixtures live in a database this must never touch.
pub async fn migrate_postgres_through(url: &str, max_version: i64) -> Result<()> {
    let db_name = url
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split(['?', '#'])
        .next()
        .unwrap_or("");
    if !db_name.ends_with("_migrations") {
        anyhow::bail!(
            "migrate_postgres_through refuses to run against database {db_name:?} — \
             it drops every table in the database, and only a database whose \
             name ends in `_migrations` may be reset this way"
        );
    }

    let pool = PgPool::connect(url)
        .await
        .context("Failed to connect to the Postgres migrations database")?;

    // Drop every table in the public schema so the migration replay starts
    // from a truly blank database, not whatever a previous test left behind.
    let tables: Vec<String> =
        sqlx_core::query::query("SELECT tablename FROM pg_tables WHERE schemaname = 'public'")
            .fetch_all(&pool)
            .await?
            .into_iter()
            .map(|row| row.get::<String, _>(0))
            .collect();

    if !tables.is_empty() {
        let quoted = tables
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        sqlx_core::raw_sql::raw_sql(&format!("DROP TABLE IF EXISTS {quoted} CASCADE;"))
            .execute(&pool)
            .await?;
    }

    apply_migrations_up_to(&pool, max_version).await
}

#[async_trait]
impl Database for PgDatabase {
    async fn table_count(&self) -> Result<i64> {
        let row = sqlx_core::query::query(
            "SELECT COUNT(*)::bigint FROM information_schema.tables
             WHERE table_schema = 'public' AND table_type = 'BASE TABLE'",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>(0))
    }

    async fn upsert_user(&self, did: &str, handle: &str) -> Result<()> {
        sqlx_core::query::query(
            "INSERT INTO users (did, handle) VALUES ($1, $2)
             ON CONFLICT(did) DO UPDATE SET handle = $2",
        )
        .bind(did)
        .bind(handle)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_user_handle(&self, did: &str) -> Result<Option<String>> {
        let row = sqlx_core::query::query("SELECT handle FROM users WHERE did = $1")
            .bind(did)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<String, _>(0)))
    }

    async fn get_scan_state(&self, user_did: &str, key: &str) -> Result<Option<String>> {
        let row = sqlx_core::query::query(
            "SELECT value FROM scan_state WHERE user_did = $1 AND key = $2",
        )
        .bind(user_did)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.get::<String, _>(0)))
    }

    async fn set_scan_state(&self, user_did: &str, key: &str, value: &str) -> Result<()> {
        sqlx_core::query::query(
            "INSERT INTO scan_state (user_did, key, value, updated_at)
             VALUES ($1, $2, $3, NOW())
             ON CONFLICT(user_did, key) DO UPDATE SET value = $3, updated_at = NOW()",
        )
        .bind(user_did)
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete_scan_state(&self, user_did: &str, key: &str) -> Result<()> {
        sqlx_core::query::query("DELETE FROM scan_state WHERE user_did = $1 AND key = $2")
            .bind(user_did)
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn finish_full_scan_state(
        &self,
        user_did: &str,
        finished_at_rfc3339: &str,
        carried_key: &str,
        claim_id: &str,
    ) -> Result<()> {
        self.finish_full_scan_state_inner(
            user_did,
            finished_at_rfc3339,
            carried_key,
            claim_id,
            false,
        )
        .await
    }

    async fn save_fingerprint(
        &self,
        user_did: &str,
        fingerprint_json: &str,
        post_count: u32,
    ) -> Result<()> {
        sqlx_core::query::query(
            "INSERT INTO topic_fingerprint (user_did, fingerprint_json, post_count, updated_at)
             VALUES ($1, $2, $3, NOW())
             ON CONFLICT(user_did) DO UPDATE SET
                fingerprint_json = $2,
                post_count = $3,
                updated_at = NOW()",
        )
        .bind(user_did)
        .bind(fingerprint_json)
        .bind(i32::try_from(post_count).context("post_count exceeds i32 range")?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn save_embedding(&self, user_did: &str, embedding: &[f64]) -> Result<()> {
        // Convert f64 to f32 for pgvector (which uses 32-bit floats)
        let floats: Vec<f32> = embedding.iter().map(|&v| v as f32).collect();
        let vector = pgvector::Vector::from(floats);
        let result = sqlx_core::query::query(
            "UPDATE topic_fingerprint SET embedding_vector = $1, updated_at = NOW()
             WHERE user_did = $2",
        )
        .bind(vector)
        .bind(user_did)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            anyhow::bail!(
                "save_embedding: no fingerprint row found — run `charcoal fingerprint` first"
            );
        }
        Ok(())
    }

    async fn save_fingerprint_bundle(
        &self,
        user_did: &str,
        fingerprint_json: &str,
        post_count: u32,
        embedding: Option<&[f64]>,
        embedding_model_id: Option<&str>,
        clusters: &[ClusterCentroid],
    ) -> Result<()> {
        crate::db::traits::validate_bundle(embedding, clusters)?;
        // One transaction for the whole generation (#302): fingerprint row
        // (JSON + embedding + updated_at in one upsert), then cluster rows.
        let mut tx = self.pool.begin().await?;
        let vector = embedding.map(|e| {
            let floats: Vec<f32> = e.iter().map(|&v| v as f32).collect();
            pgvector::Vector::from(floats)
        });
        sqlx_core::query::query(
            "INSERT INTO topic_fingerprint (user_did, fingerprint_json, post_count, embedding_vector, embedding_model_id, updated_at)
             VALUES ($1, $2, $3, $4, $5, NOW())
             ON CONFLICT(user_did) DO UPDATE SET
                fingerprint_json = $2,
                post_count = $3,
                embedding_vector = $4,
                embedding_model_id = $5,
                updated_at = NOW()",
        )
        .bind(user_did)
        .bind(fingerprint_json)
        .bind(i32::try_from(post_count).context("post_count exceeds i32 range")?)
        .bind(vector)
        .bind(embedding_model_id)
        .execute(&mut *tx)
        .await?;
        sqlx_core::query::query("DELETE FROM topic_clusters WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        for (i, cluster) in clusters.iter().enumerate() {
            let floats: Vec<f32> = cluster.centroid.iter().map(|&v| v as f32).collect();
            sqlx_core::query::query(
                "INSERT INTO topic_clusters (user_did, cluster_index, centroid, post_count)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(user_did)
            .bind(i as i32)
            .bind(pgvector::Vector::from(floats))
            .bind(
                i32::try_from(cluster.post_count)
                    .context("cluster post_count exceeds i32 range")?,
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn get_topic_centroids(&self, user_did: &str) -> Result<Vec<ClusterCentroid>> {
        let rows = sqlx_core::query::query(
            "SELECT centroid, post_count FROM topic_clusters
             WHERE user_did = $1 ORDER BY cluster_index",
        )
        .bind(user_did)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                let vector: pgvector::Vector = r.get(0);
                Ok(ClusterCentroid {
                    centroid: vector.to_vec().iter().map(|&v| v as f64).collect(),
                    post_count: r.get::<i32, _>(1) as u32,
                })
            })
            .collect()
    }

    async fn fingerprint_embedding_model(&self, user_did: &str) -> Result<Option<String>> {
        let row = sqlx_core::query::query(
            "SELECT embedding_model_id FROM topic_fingerprint WHERE user_did = $1",
        )
        .bind(user_did)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| r.get::<Option<String>, _>(0)))
    }

    async fn get_fingerprint(&self, user_did: &str) -> Result<Option<(String, u32, String)>> {
        let row = sqlx_core::query::query(
            "SELECT fingerprint_json, post_count,
                    to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') as updated_at
             FROM topic_fingerprint WHERE user_did = $1",
        )
        .bind(user_did)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| {
            (
                r.get::<String, _>(0),
                r.get::<i32, _>(1) as u32,
                r.get::<String, _>(2),
            )
        }))
    }

    async fn get_embedding(&self, user_did: &str) -> Result<Option<Vec<f64>>> {
        let row = sqlx_core::query::query(
            "SELECT embedding_vector FROM topic_fingerprint WHERE user_did = $1",
        )
        .bind(user_did)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(r) => {
                let vec: Option<pgvector::Vector> = r.get(0);
                Ok(vec.map(|v| v.to_vec().into_iter().map(|f| f as f64).collect()))
            }
            None => Ok(None),
        }
    }

    async fn upsert_account_score(&self, user_did: &str, score: &AccountScore) -> Result<()> {
        let top_posts_json = serde_json::to_value(&score.top_toxic_posts)?;
        let behavioral_json: Option<serde_json::Value> = score
            .behavioral_signals
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok());
        // Stamp both freshness columns from the clock (#344) — this IS the
        // scoring write path. `import_score` is the only other writer and
        // never stamps from now; it carries a source's own values verbatim.
        let staleness_days = i32::try_from(ScoringConfidence::staleness_days_for_label(
            score.scoring_confidence.as_deref(),
        ))
        .context("staleness_days exceeds i32 range")?;

        sqlx_core::query::query(
            "INSERT INTO account_scores
                (user_did, did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
                 posts_analyzed, top_toxic_posts, scored_at, behavioral_signals, context_score, graph_distance,
                 fingerprint_quality, scoring_confidence, overlap_legacy, scoring_generation, valid_until)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW(), $10, $11, $12, $13, $14, $15, $16, NOW() + make_interval(days => $17))
             ON CONFLICT(user_did, did) DO UPDATE SET
                handle = $3,
                toxicity_score = $4,
                topic_overlap = $5,
                threat_score = $6,
                threat_tier = $7,
                posts_analyzed = $8,
                top_toxic_posts = $9,
                scored_at = NOW(),
                behavioral_signals = $10,
                context_score = $11,
                graph_distance = $12,
                fingerprint_quality = $13,
                scoring_confidence = $14,
                overlap_legacy = $15,
                scoring_generation = $16,
                valid_until = NOW() + make_interval(days => $17)",
        )
        .bind(user_did)
        .bind(&score.did)
        .bind(&score.handle)
        .bind(score.toxicity_score)
        .bind(score.topic_overlap)
        .bind(score.threat_score)
        .bind(&score.threat_tier)
        .bind(score.posts_analyzed as i32)
        .bind(&top_posts_json)
        .bind(&behavioral_json)
        .bind(score.context_score)
        .bind(&score.graph_distance)
        .bind(&score.fingerprint_quality)
        .bind(&score.scoring_confidence)
        .bind(score.overlap_legacy)
        .bind(scoring_revision())
        .bind(staleness_days)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_ranked_threats(
        &self,
        user_did: &str,
        min_score: f64,
    ) -> Result<Vec<AccountScore>> {
        let rows = sqlx_core::query::query(
            "SELECT did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
                    posts_analyzed, top_toxic_posts,
                    to_char(scored_at, 'YYYY-MM-DD HH24:MI:SS') as scored_at,
                    behavioral_signals, context_score,
                    fingerprint_quality, scoring_confidence, graph_distance,
                    overlap_legacy
             FROM account_scores
             WHERE user_did = $1 AND threat_score >= $2 AND scoring_generation = $3 AND valid_until > NOW()
             ORDER BY threat_score DESC, did",
        )
        .bind(user_did)
        .bind(min_score)
        .bind(scoring_revision())
        .fetch_all(&self.pool)
        .await?;

        let mut accounts = Vec::new();
        for row in rows {
            let top_posts_json: serde_json::Value = row.get(7);
            let top_toxic_posts: Vec<ToxicPost> =
                serde_json::from_value(top_posts_json).unwrap_or_default();

            // Recalculate tier from stored score so threshold changes
            // take effect without rescanning — unless the stored tier is
            // NotAssessed (#222), which must survive unchanged since its
            // NULL score would otherwise resolve to no tier at all.
            let threat_score: Option<f64> = row.get(4);
            let stored_tier: Option<String> = row.get(5);
            let threat_tier = match stored_tier.as_deref() {
                Some(s) if s == ThreatTier::NotAssessed.as_str() => Some(s.to_string()),
                _ => threat_score.map(|s| ThreatTier::from_score(s).to_string()),
            };

            let behavioral_signals: Option<serde_json::Value> = row.get(9);

            accounts.push(AccountScore {
                did: row.get(0),
                handle: row.get(1),
                toxicity_score: row.get(2),
                topic_overlap: row.get(3),
                threat_score,
                threat_tier,
                posts_analyzed: row.get::<i32, _>(6) as u32,
                top_toxic_posts,
                scored_at: row.get(8),
                behavioral_signals: behavioral_signals.map(|v| v.to_string()),
                context_score: row.get(10),
                graph_distance: row.get(13),
                fingerprint_quality: row.get(11),
                scoring_confidence: row.get(12),
                overlap_legacy: row.get(14),
            });
        }
        Ok(accounts)
    }

    async fn is_score_stale(&self, user_did: &str, did: &str) -> Result<bool> {
        // Same predicate as SQLite (#344 R11), natively boolean here: no
        // COALESCE needed because valid_until is NOT NULL on Postgres.
        let row = sqlx_core::query::query(
            "SELECT scoring_generation = $3 AND valid_until > NOW()
             FROM account_scores WHERE user_did = $1 AND did = $2",
        )
        .bind(user_did)
        .bind(did)
        .bind(scoring_revision())
        .fetch_optional(&self.pool)
        .await?;

        match row {
            None => Ok(true), // No score exists — treat as stale
            Some(r) => Ok(!r.get::<bool, _>(0)),
        }
    }

    async fn get_fresh_scored_dids(&self, user_did: &str) -> Result<Vec<String>> {
        let rows = sqlx_core::query::query(
            "SELECT did FROM account_scores
             WHERE user_did = $1 AND scoring_generation = $2 AND valid_until > NOW()",
        )
        .bind(user_did)
        .bind(scoring_revision())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get::<String, _>(0)).collect())
    }

    async fn count_expired(&self, user_did: &str) -> Result<i64> {
        // valid_until is NOT NULL on Postgres so, unlike SQLite, no COALESCE
        // is needed — the comparison is never SQL-unknown here.
        let row = sqlx_core::query::query(
            "SELECT COUNT(*) FROM account_scores
             WHERE user_did = $1 AND NOT (scoring_generation = $2 AND valid_until > NOW())",
        )
        .bind(user_did)
        .bind(scoring_revision())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>(0))
    }

    async fn export_scores(&self, user_did: &str) -> Result<Vec<StoredScore>> {
        let rows = sqlx_core::query::query(
            "SELECT did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
                    posts_analyzed, top_toxic_posts, behavioral_signals, graph_distance,
                    fingerprint_quality, scoring_confidence, context_score, overlap_legacy,
                    to_char(scored_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"+00:00\"'),
                    scoring_generation,
                    to_char(valid_until AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"+00:00\"')
             FROM account_scores WHERE user_did = $1 ORDER BY did",
        )
        .bind(user_did)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            // NULL (never written by the app's own upsert, but reachable
            // from a raw INSERT — test fixtures, a hand-patched row) and
            // corrupted JSON both present as empty (#364), matching the
            // SQLite side of this same function.
            let top_posts_json: Option<serde_json::Value> = row.get(7);
            let top_toxic_posts: Vec<ToxicPost> = top_posts_json
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            let behavioral_signals: Option<serde_json::Value> = row.get(8);
            // valid_until is NOT NULL on Postgres — every row is `At` (V2-06).
            let valid_until: String = row.get(16);
            out.push(StoredScore {
                score: AccountScore {
                    did: row.get(0),
                    handle: row.get(1),
                    toxicity_score: row.get(2),
                    topic_overlap: row.get(3),
                    threat_score: row.get(4),
                    threat_tier: row.get(5),
                    posts_analyzed: row.get::<i32, _>(6) as u32,
                    top_toxic_posts,
                    scored_at: String::new(),
                    behavioral_signals: behavioral_signals.map(|v| v.to_string()),
                    graph_distance: row.get(9),
                    fingerprint_quality: row.get(10),
                    scoring_confidence: row.get(11),
                    context_score: row.get(12),
                    overlap_legacy: row.get(13),
                },
                scored_at: row.get(14),
                scoring_generation: row.get(15),
                valid_until: ExportedExpiry::At(valid_until),
            });
        }
        Ok(out)
    }

    async fn import_score(&self, user_did: &str, row: &StoredScore) -> Result<()> {
        let s = &row.score;
        let top_posts_json = serde_json::to_value(&s.top_toxic_posts)?;
        let behavioral_json: Option<serde_json::Value> = s
            .behavioral_signals
            .as_ref()
            .and_then(|v| serde_json::from_str(v).ok());
        // Same Missing/Invalid -> scored_at mapping as SQLite (V2-06): a
        // SQLite source can hand this backend either, since export_scores on
        // SQLite is not itself NOT-NULL-constrained.
        let valid_until = match &row.valid_until {
            ExportedExpiry::At(t) => t.clone(),
            ExportedExpiry::Missing => row.scored_at.clone(),
            ExportedExpiry::Invalid(raw) => {
                tracing::warn!(did = %s.did, raw, "invalid expiry on export — importing as expired-when-scored");
                row.scored_at.clone()
            }
        };
        sqlx_core::query::query(
            "INSERT INTO account_scores (user_did, did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
                 posts_analyzed, top_toxic_posts, scored_at, behavioral_signals, context_score, graph_distance,
                 fingerprint_quality, scoring_confidence, overlap_legacy, scoring_generation, valid_until)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::timestamptz, $11, $12, $13, $14, $15, $16, $17, $18::timestamptz)
             ON CONFLICT(user_did, did) DO UPDATE SET
                 handle = $3, toxicity_score = $4, topic_overlap = $5, threat_score = $6, threat_tier = $7,
                 posts_analyzed = $8, top_toxic_posts = $9, scored_at = $10::timestamptz, behavioral_signals = $11,
                 context_score = $12, graph_distance = $13, fingerprint_quality = $14, scoring_confidence = $15,
                 overlap_legacy = $16, scoring_generation = $17, valid_until = $18::timestamptz",
        )
        .bind(user_did)
        .bind(&s.did)
        .bind(&s.handle)
        .bind(s.toxicity_score)
        .bind(s.topic_overlap)
        .bind(s.threat_score)
        .bind(&s.threat_tier)
        .bind(s.posts_analyzed as i32)
        .bind(&top_posts_json)
        .bind(&row.scored_at)
        .bind(&behavioral_json)
        .bind(s.context_score)
        .bind(&s.graph_distance)
        .bind(&s.fingerprint_quality)
        .bind(&s.scoring_confidence)
        .bind(s.overlap_legacy)
        .bind(&row.scoring_generation)
        .bind(&valid_until)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_refresh_candidates(
        &self,
        user_did: &str,
        horizon_days: i64,
    ) -> Result<Vec<RefreshCandidate>> {
        // No COALESCE here (unlike SQLite's REFRESH_CANDIDATES_SQL): Postgres
        // valid_until is NOT NULL after the v18 backfill, so the comparison
        // is never SQL-unknown — the same reason count_expired/is_score_stale
        // skip it on this backend.
        let horizon_days = i32::try_from(horizon_days).context("horizon_days exceeds i32 range")?;
        let rows = sqlx_core::query::query(
            "SELECT did, handle, graph_distance FROM account_scores
             WHERE user_did = $1
               AND threat_score >= $2
               AND (scoring_generation <> $4
                    OR valid_until <= NOW() + make_interval(days => $3))
             ORDER BY threat_score DESC, did",
        )
        .bind(user_did)
        .bind(ThreatTier::ELEVATED_MIN)
        .bind(horizon_days)
        .bind(scoring_revision())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| RefreshCandidate {
                did: r.get(0),
                handle: r.get(1),
                graph_distance: r.get(2),
            })
            .collect())
    }

    async fn insert_amplification_event(
        &self,
        user_did: &str,
        event_type: &str,
        amplifier_did: &str,
        amplifier_handle: &str,
        original_post_uri: &str,
        amplifier_post_uri: Option<&str>,
        amplifier_text: Option<&str>,
        original_post_text: Option<&str>,
        context_score: Option<f64>,
    ) -> Result<i64> {
        let row = sqlx_core::query::query(
            "INSERT INTO amplification_events
                (user_did, event_type, amplifier_did, amplifier_handle, original_post_uri,
                 amplifier_post_uri, amplifier_text, original_post_text, context_score)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             RETURNING id",
        )
        .bind(user_did)
        .bind(event_type)
        .bind(amplifier_did)
        .bind(amplifier_handle)
        .bind(original_post_uri)
        .bind(amplifier_post_uri)
        .bind(amplifier_text)
        .bind(original_post_text)
        .bind(context_score)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>(0))
    }

    async fn insert_amplification_events_batch(
        &self,
        user_did: &str,
        events: &[NewAmplificationEvent],
    ) -> Result<usize> {
        if events.is_empty() {
            return Ok(0);
        }

        // UNNEST binds 8 arrays plus $1 (user_did, a single scalar broadcast
        // by the SELECT to every row) regardless of row count, so this is one
        // round-trip for any batch size and never approaches Postgres's
        // 65535-parameter statement cap.
        //
        // `WITH ORDINALITY` + `ORDER BY ord` is load-bearing, not decorative.
        // Postgres does NOT guarantee row order for INSERT ... SELECT without
        // an explicit ORDER BY — the planner is free to reorder. Our contract
        // requires serial ids to ascend in slice order, so we cannot rely on
        // the observed behavior that UNNEST happens to emit in array order.
        // WITH ORDINALITY numbers the elements at their source, and ordering
        // by that number makes the guarantee explicit instead of incidental.
        let event_types: Vec<String> = events.iter().map(|e| e.event_type.clone()).collect();
        let amplifier_dids: Vec<String> = events.iter().map(|e| e.amplifier_did.clone()).collect();
        let amplifier_handles: Vec<String> =
            events.iter().map(|e| e.amplifier_handle.clone()).collect();
        let original_post_uris: Vec<String> =
            events.iter().map(|e| e.original_post_uri.clone()).collect();
        let amplifier_post_uris: Vec<Option<String>> = events
            .iter()
            .map(|e| e.amplifier_post_uri.clone())
            .collect();
        let amplifier_texts: Vec<Option<String>> =
            events.iter().map(|e| e.amplifier_text.clone()).collect();
        let original_post_texts: Vec<Option<String>> = events
            .iter()
            .map(|e| e.original_post_text.clone())
            .collect();
        let context_scores: Vec<Option<f64>> = events.iter().map(|e| e.context_score).collect();

        // All eight arrays carry explicit ::text[]/::float8[] casts so
        // Postgres can type UNNEST's output columns without inspecting
        // values. That's required for context_score in particular — an
        // all-NULL array has no inferable element type, and Postgres
        // rejects it without the explicit ::float8[] cast.
        let result = sqlx_core::query::query(
            "INSERT INTO amplification_events
                (user_did, event_type, amplifier_did, amplifier_handle, original_post_uri,
                 amplifier_post_uri, amplifier_text, original_post_text, context_score)
             SELECT $1, t.event_type, t.amplifier_did, t.amplifier_handle, t.original_post_uri,
                    t.amplifier_post_uri, t.amplifier_text, t.original_post_text, t.context_score
             FROM UNNEST($2::text[], $3::text[], $4::text[], $5::text[],
                         $6::text[], $7::text[], $8::text[], $9::float8[])
                  WITH ORDINALITY
                  AS t(event_type, amplifier_did, amplifier_handle, original_post_uri,
                       amplifier_post_uri, amplifier_text, original_post_text, context_score,
                       ord)
             ORDER BY t.ord",
        )
        .bind(user_did)
        .bind(&event_types)
        .bind(&amplifier_dids)
        .bind(&amplifier_handles)
        .bind(&original_post_uris)
        .bind(&amplifier_post_uris)
        .bind(&amplifier_texts)
        .bind(&original_post_texts)
        .bind(&context_scores)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() as usize)
    }

    async fn get_recent_events(
        &self,
        user_did: &str,
        limit: u32,
    ) -> Result<Vec<AmplificationEvent>> {
        // Cap at i32::MAX before casting to avoid overflow — PostgreSQL LIMIT
        // accepts i64 but sqlx binds integers as i32 here. Values above i32::MAX
        // are effectively unlimited for any realistic dataset.
        let rows = sqlx_core::query::query(
            "SELECT id, event_type, amplifier_did, amplifier_handle, original_post_uri,
                    amplifier_post_uri, amplifier_text,
                    to_char(detected_at, 'YYYY-MM-DD HH24:MI:SS') as detected_at,
                    followers_fetched, followers_scored,
                    original_post_text, context_score
             FROM amplification_events
             WHERE user_did = $1
             ORDER BY detected_at DESC, id DESC
             LIMIT $2",
        )
        .bind(user_did)
        .bind(limit.min(i32::MAX as u32) as i32)
        .fetch_all(&self.pool)
        .await?;

        let mut events = Vec::new();
        for row in rows {
            events.push(AmplificationEvent {
                id: row.get(0),
                event_type: row.get(1),
                amplifier_did: row.get(2),
                amplifier_handle: row.get(3),
                original_post_uri: row.get(4),
                amplifier_post_uri: row.get(5),
                amplifier_text: row.get(6),
                detected_at: row.get(7),
                followers_fetched: row.get(8),
                followers_scored: row.get(9),
                original_post_text: row.get(10),
                context_score: row.get(11),
            });
        }
        Ok(events)
    }

    async fn get_events_for_pile_on(
        &self,
        user_did: &str,
    ) -> Result<Vec<(String, String, String)>> {
        let rows = sqlx_core::query::query(
            "SELECT amplifier_did, original_post_uri,
                    to_char(detected_at, 'YYYY-MM-DD HH24:MI:SS') as detected_at
             FROM amplification_events
             WHERE user_did = $1
             ORDER BY original_post_uri, detected_at",
        )
        .bind(user_did)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<String, _>(0),
                    r.get::<String, _>(1),
                    r.get::<String, _>(2),
                )
            })
            .collect())
    }

    async fn get_events_by_amplifier(
        &self,
        user_did: &str,
        amplifier_did: &str,
    ) -> Result<Vec<AmplificationEvent>> {
        let rows = sqlx_core::query::query(
            "SELECT id, event_type, amplifier_did, amplifier_handle, original_post_uri,
                    amplifier_post_uri, amplifier_text,
                    to_char(detected_at, 'YYYY-MM-DD HH24:MI:SS') as detected_at,
                    followers_fetched, followers_scored, original_post_text, context_score
             FROM amplification_events
             WHERE user_did = $1 AND amplifier_did = $2
             ORDER BY detected_at DESC, id DESC",
        )
        .bind(user_did)
        .bind(amplifier_did)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| AmplificationEvent {
                id: r.get::<i64, _>(0),
                event_type: r.get::<String, _>(1),
                amplifier_did: r.get::<String, _>(2),
                amplifier_handle: r.get::<String, _>(3),
                original_post_uri: r.get::<String, _>(4),
                amplifier_post_uri: r.get::<Option<String>, _>(5),
                amplifier_text: r.get::<Option<String>, _>(6),
                detected_at: r.get::<String, _>(7),
                followers_fetched: r.get::<bool, _>(8),
                followers_scored: r.get::<bool, _>(9),
                original_post_text: r.get::<Option<String>, _>(10),
                context_score: r.get::<Option<f64>, _>(11),
            })
            .collect())
    }

    async fn get_median_engagement(&self, user_did: &str) -> Result<f64> {
        // Use percentile_cont for a true median calculation
        let row = sqlx_core::query::query(
            "SELECT COALESCE(
                percentile_cont(0.5) WITHIN GROUP (
                    ORDER BY (behavioral_signals->>'avg_engagement')::double precision
                ),
                0.0
             )
             FROM account_scores
             WHERE user_did = $1
               AND behavioral_signals IS NOT NULL
               AND behavioral_signals->>'avg_engagement' IS NOT NULL",
        )
        .bind(user_did)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<f64, _>(0))
    }

    async fn get_all_scan_state(&self, user_did: &str) -> Result<Vec<(String, String)>> {
        let rows = sqlx_core::query::query("SELECT key, value FROM scan_state WHERE user_did = $1")
            .bind(user_did)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<String, _>(0), r.get::<String, _>(1)))
            .collect())
    }

    async fn insert_amplification_event_raw(
        &self,
        user_did: &str,
        event: &AmplificationEvent,
    ) -> Result<i64> {
        // Insert with the original detected_at so migrated events keep their
        // real timestamps. Pile-on detection depends on accurate timestamps.
        let row = sqlx_core::query::query(
            "INSERT INTO amplification_events
                (user_did, event_type, amplifier_did, amplifier_handle, original_post_uri,
                 amplifier_post_uri, amplifier_text, detected_at, original_post_text, context_score)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8::timestamptz, $9, $10)
             RETURNING id",
        )
        .bind(user_did)
        .bind(&event.event_type)
        .bind(&event.amplifier_did)
        .bind(&event.amplifier_handle)
        .bind(&event.original_post_uri)
        .bind(&event.amplifier_post_uri)
        .bind(&event.amplifier_text)
        .bind({
            // Check if timestamp already has an explicit timezone offset (Z, +HH, or -HH
            // after the time portion). The '-' check only looks after 'T' to avoid matching
            // date separators like 2026-03-10.
            let has_tz = event.detected_at.ends_with('Z')
                || event.detected_at.contains('+')
                || event
                    .detected_at
                    .find('T')
                    .is_some_and(|t| event.detected_at[t..].contains('-'));
            if has_tz {
                event.detected_at.clone()
            } else {
                // Append UTC offset so PostgreSQL doesn't interpret via session TimeZone
                format!("{}+00", event.detected_at)
            }
        })
        .bind(&event.original_post_text)
        .bind(event.context_score)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>(0))
    }

    async fn get_account_by_handle(
        &self,
        user_did: &str,
        handle: &str,
    ) -> Result<Option<AccountScore>> {
        let row = sqlx_core::query::query(
            "SELECT did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
                    posts_analyzed, top_toxic_posts,
                    to_char(scored_at, 'YYYY-MM-DD HH24:MI:SS') as scored_at,
                    behavioral_signals, context_score,
                    fingerprint_quality, scoring_confidence, graph_distance,
                    overlap_legacy
             FROM account_scores
             WHERE user_did = $1 AND lower(handle) = lower($2)
             LIMIT 1",
        )
        .bind(user_did)
        .bind(handle)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| {
            let top_posts_json: serde_json::Value = r.get(7);
            let top_toxic_posts: Vec<ToxicPost> =
                serde_json::from_value(top_posts_json).unwrap_or_default();
            let threat_score: Option<f64> = r.get(4);
            // Preserve a stored NotAssessed tier (#222) instead of
            // recomputing from the (NULL) score.
            let stored_tier: Option<String> = r.get(5);
            let threat_tier = match stored_tier.as_deref() {
                Some(s) if s == ThreatTier::NotAssessed.as_str() => Some(s.to_string()),
                _ => threat_score.map(|s| ThreatTier::from_score(s).to_string()),
            };
            let behavioral_signals: Option<serde_json::Value> = r.get(9);
            AccountScore {
                did: r.get(0),
                handle: r.get(1),
                toxicity_score: r.get(2),
                topic_overlap: r.get(3),
                threat_score,
                threat_tier,
                posts_analyzed: r.get::<i32, _>(6) as u32,
                top_toxic_posts,
                scored_at: r.get(8),
                behavioral_signals: behavioral_signals.map(|v| v.to_string()),
                context_score: r.get(10),
                graph_distance: r.get(13),
                fingerprint_quality: r.get(11),
                scoring_confidence: r.get(12),
                overlap_legacy: r.get(14),
            }
        }))
    }

    async fn get_account_by_did(&self, user_did: &str, did: &str) -> Result<Option<AccountScore>> {
        let row = sqlx_core::query::query(
            "SELECT did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
                    posts_analyzed, top_toxic_posts,
                    to_char(scored_at, 'YYYY-MM-DD HH24:MI:SS') as scored_at,
                    behavioral_signals, context_score,
                    fingerprint_quality, scoring_confidence, graph_distance,
                    overlap_legacy
             FROM account_scores
             WHERE user_did = $1 AND did = $2
             LIMIT 1",
        )
        .bind(user_did)
        .bind(did)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| {
            let top_posts_json: serde_json::Value = r.get(7);
            let top_toxic_posts: Vec<ToxicPost> =
                serde_json::from_value(top_posts_json).unwrap_or_default();
            let threat_score: Option<f64> = r.get(4);
            // Preserve a stored NotAssessed tier (#222) instead of
            // recomputing from the (NULL) score.
            let stored_tier: Option<String> = r.get(5);
            let threat_tier = match stored_tier.as_deref() {
                Some(s) if s == ThreatTier::NotAssessed.as_str() => Some(s.to_string()),
                _ => threat_score.map(|s| ThreatTier::from_score(s).to_string()),
            };
            let behavioral_signals: Option<serde_json::Value> = r.get(9);
            AccountScore {
                did: r.get(0),
                handle: r.get(1),
                toxicity_score: r.get(2),
                topic_overlap: r.get(3),
                threat_score,
                threat_tier,
                posts_analyzed: r.get::<i32, _>(6) as u32,
                top_toxic_posts,
                scored_at: r.get(8),
                behavioral_signals: behavioral_signals.map(|v| v.to_string()),
                context_score: r.get(10),
                graph_distance: r.get(13),
                fingerprint_quality: r.get(11),
                scoring_confidence: r.get(12),
                overlap_legacy: r.get(14),
            }
        }))
    }

    async fn upsert_user_label(
        &self,
        user_did: &str,
        target_did: &str,
        label: &str,
        notes: Option<&str>,
    ) -> Result<()> {
        sqlx_core::query::query(
            "INSERT INTO user_labels (user_did, target_did, label, labeled_at, notes)
             VALUES ($1, $2, $3, NOW(), $4)
             ON CONFLICT(user_did, target_did) DO UPDATE SET
                label = $3, labeled_at = NOW(), notes = $4",
        )
        .bind(user_did)
        .bind(target_did)
        .bind(label)
        .bind(notes)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_user_label(&self, user_did: &str, target_did: &str) -> Result<Option<UserLabel>> {
        let row = sqlx_core::query::query(
            "SELECT user_did, target_did, label,
                    to_char(labeled_at, 'YYYY-MM-DD HH24:MI:SS') as labeled_at, notes
             FROM user_labels
             WHERE user_did = $1 AND target_did = $2",
        )
        .bind(user_did)
        .bind(target_did)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| UserLabel {
            user_did: r.get(0),
            target_did: r.get(1),
            label: r.get(2),
            labeled_at: r.get(3),
            notes: r.get(4),
        }))
    }

    async fn get_unlabeled_accounts(
        &self,
        user_did: &str,
        limit: i64,
    ) -> Result<Vec<AccountScore>> {
        let rows = sqlx_core::query::query(
            "SELECT a.did, a.handle, a.toxicity_score, a.topic_overlap, a.threat_score, a.threat_tier,
                    a.posts_analyzed, a.top_toxic_posts,
                    to_char(a.scored_at, 'YYYY-MM-DD HH24:MI:SS') as scored_at,
                    a.behavioral_signals, a.context_score,
                    a.fingerprint_quality, a.scoring_confidence, a.graph_distance,
                    a.overlap_legacy
             FROM account_scores a
             LEFT JOIN user_labels ul ON a.user_did = ul.user_did AND a.did = ul.target_did
             WHERE a.user_did = $1 AND ul.target_did IS NULL AND a.threat_score IS NOT NULL
             ORDER BY a.threat_score DESC
             LIMIT $2",
        )
        .bind(user_did)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let mut accounts = Vec::new();
        for row in rows {
            let top_posts_json: serde_json::Value = row.get(7);
            let top_toxic_posts: Vec<ToxicPost> =
                serde_json::from_value(top_posts_json).unwrap_or_default();
            let threat_score: Option<f64> = row.get(4);
            // Preserve a stored NotAssessed tier (#222) instead of
            // recomputing from the (NULL) score.
            let stored_tier: Option<String> = row.get(5);
            let threat_tier = match stored_tier.as_deref() {
                Some(s) if s == ThreatTier::NotAssessed.as_str() => Some(s.to_string()),
                _ => threat_score.map(|s| ThreatTier::from_score(s).to_string()),
            };
            let behavioral_signals: Option<serde_json::Value> = row.get(9);

            accounts.push(AccountScore {
                did: row.get(0),
                handle: row.get(1),
                toxicity_score: row.get(2),
                topic_overlap: row.get(3),
                threat_score,
                threat_tier,
                posts_analyzed: row.get::<i32, _>(6) as u32,
                top_toxic_posts,
                scored_at: row.get(8),
                behavioral_signals: behavioral_signals.map(|v| v.to_string()),
                context_score: row.get(10),
                graph_distance: row.get(13),
                fingerprint_quality: row.get(11),
                scoring_confidence: row.get(12),
                overlap_legacy: row.get(14),
            });
        }
        Ok(accounts)
    }

    async fn get_accuracy_metrics(&self, user_did: &str) -> Result<AccuracyMetrics> {
        // Compute tier rank in SQL using CASE expressions, then compare
        let rows = sqlx_core::query::query(
            "SELECT
                CASE lower(a.threat_tier)
                    WHEN 'high' THEN 3
                    WHEN 'elevated' THEN 2
                    WHEN 'watch' THEN 1
                    ELSE 0
                END as predicted,
                CASE lower(ul.label)
                    WHEN 'high' THEN 3
                    WHEN 'elevated' THEN 2
                    WHEN 'watch' THEN 1
                    ELSE 0
                END as actual
             FROM user_labels ul
             INNER JOIN account_scores a ON a.user_did = ul.user_did AND a.did = ul.target_did
             WHERE ul.user_did = $1",
        )
        .bind(user_did)
        .fetch_all(&self.pool)
        .await?;

        let total_labeled = rows.len() as i64;
        let mut exact_matches: i64 = 0;
        let mut overscored: i64 = 0;
        let mut underscored: i64 = 0;

        for row in &rows {
            let predicted: i32 = row.get(0);
            let actual: i32 = row.get(1);
            if predicted == actual {
                exact_matches += 1;
            } else if predicted > actual {
                overscored += 1;
            } else {
                underscored += 1;
            }
        }

        let accuracy = if total_labeled > 0 {
            exact_matches as f64 / total_labeled as f64
        } else {
            0.0
        };

        Ok(AccuracyMetrics {
            total_labeled,
            exact_matches,
            overscored,
            underscored,
            accuracy,
        })
    }

    async fn delete_inferred_pairs(&self, user_did: &str, target_did: &str) -> Result<()> {
        sqlx_core::query::query(
            "DELETE FROM inferred_pairs WHERE user_did = $1 AND target_did = $2",
        )
        .bind(user_did)
        .bind(target_did)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn insert_inferred_pair(
        &self,
        user_did: &str,
        target_did: &str,
        target_post_text: &str,
        target_post_uri: &str,
        user_post_text: &str,
        user_post_uri: &str,
        similarity: f64,
        context_score: Option<f64>,
    ) -> Result<i64> {
        let row = sqlx_core::query::query(
            "INSERT INTO inferred_pairs
                (user_did, target_did, target_post_text, target_post_uri,
                 user_post_text, user_post_uri, similarity, context_score)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT(user_did, target_did, target_post_uri, user_post_uri)
             DO UPDATE SET similarity = $7, context_score = $8
             RETURNING id",
        )
        .bind(user_did)
        .bind(target_did)
        .bind(target_post_text)
        .bind(target_post_uri)
        .bind(user_post_text)
        .bind(user_post_uri)
        .bind(similarity)
        .bind(context_score)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>(0))
    }

    async fn get_inferred_pairs(
        &self,
        user_did: &str,
        target_did: &str,
    ) -> Result<Vec<InferredPair>> {
        let rows = sqlx_core::query::query(
            "SELECT id, user_did, target_did, target_post_text, target_post_uri,
                    user_post_text, user_post_uri, similarity, context_score,
                    to_char(created_at, 'YYYY-MM-DD HH24:MI:SS') as created_at
             FROM inferred_pairs
             WHERE user_did = $1 AND target_did = $2
             ORDER BY similarity DESC",
        )
        .bind(user_did)
        .bind(target_did)
        .fetch_all(&self.pool)
        .await?;

        let mut pairs = Vec::new();
        for row in rows {
            pairs.push(InferredPair {
                id: row.get(0),
                user_did: row.get(1),
                target_did: row.get(2),
                target_post_text: row.get(3),
                target_post_uri: row.get(4),
                user_post_text: row.get(5),
                user_post_uri: row.get(6),
                similarity: row.get(7),
                context_score: row.get(8),
                created_at: row.get(9),
            });
        }
        Ok(pairs)
    }

    async fn list_users(&self) -> Result<Vec<UserRow>> {
        let rows = sqlx_core::query::query(
            "SELECT did, handle,
                    to_char(created_at, 'YYYY-MM-DD HH24:MI:SS') as created_at,
                    to_char(last_login_at, 'YYYY-MM-DD HH24:MI:SS') as last_login_at
             FROM users ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| UserRow {
                did: r.get(0),
                handle: r.get(1),
                created_at: r.get(2),
                last_login_at: r.get(3),
            })
            .collect())
    }

    async fn get_scored_account_count(&self, user_did: &str) -> Result<i64> {
        let row = sqlx_core::query::query(
            "SELECT COUNT(*)::bigint FROM account_scores
             WHERE user_did = $1 AND threat_score IS NOT NULL",
        )
        .bind(user_did)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>(0))
    }

    async fn has_fingerprint(&self, user_did: &str) -> Result<bool> {
        let row = sqlx_core::query::query(
            "SELECT COUNT(*) > 0 FROM topic_fingerprint WHERE user_did = $1",
        )
        .bind(user_did)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<bool, _>(0))
    }

    async fn delete_user_data(&self, user_did: &str) -> Result<()> {
        // Run all deletes in a single transaction so a mid-flight failure
        // can't leave the user's data half-deleted. Delete in dependency
        // order to avoid FK issues if constraints are added later.
        let mut tx = self.pool.begin().await?;
        // Staging tables first (#208) — a user's queued classification work
        // must not outlive the account itself.
        sqlx_core::query::query("DELETE FROM classification_queue WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        sqlx_core::query::query("DELETE FROM scan_account_input WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        // #234: scan_skips holds the user's DID, the DIDs of accounts scanned
        // on their behalf, and raw error text. It is user-scoped like
        // everything else here and must not outlive the account.
        sqlx_core::query::query("DELETE FROM scan_skips WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        // #257: scan_queue holds the user's admission state; a queued or
        // running row must not outlive the account.
        sqlx_core::query::query("DELETE FROM scan_queue WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        sqlx_core::query::query("DELETE FROM inferred_pairs WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        sqlx_core::query::query("DELETE FROM user_labels WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        sqlx_core::query::query("DELETE FROM amplification_events WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        sqlx_core::query::query("DELETE FROM account_scores WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        sqlx_core::query::query("DELETE FROM scan_state WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        sqlx_core::query::query("DELETE FROM topic_fingerprint WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        // #315: the user's write grant and everything Charcoal did with it.
        // actions references action_batches, so it goes first.
        sqlx_core::query::query("DELETE FROM actions WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        sqlx_core::query::query("DELETE FROM action_batches WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        sqlx_core::query::query("DELETE FROM oauth_sessions WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        sqlx_core::query::query("DELETE FROM users WHERE did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn update_last_login(&self, did: &str) -> Result<()> {
        sqlx_core::query::query("UPDATE users SET last_login_at = NOW() WHERE did = $1")
            .bind(did)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn get_all_scored_dids(&self, user_did: &str) -> Result<Vec<String>> {
        let rows = sqlx_core::query::query("SELECT did FROM account_scores WHERE user_did = $1")
            .bind(user_did)
            .fetch_all(&self.pool)
            .await?;
        let dids = rows.iter().map(|row| row.get::<String, _>("did")).collect();
        Ok(dids)
    }

    // --- Classification staging (#208) ---

    async fn enqueue_classifications(&self, user_did: &str, rows: &[QueueRow]) -> Result<()> {
        // Batch all inserts inside a single transaction to avoid per-row
        // connection churn and ensure atomicity across the whole batch.
        let mut tx = self.pool.begin().await?;
        for row in rows {
            sqlx_core::query::query(
                "INSERT INTO classification_queue
                     (user_did, account_did, post_uri, text, context_text,
                      post_kind, onnx_score, status,
                      toxic_token, confidence, model_id, policy_version)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                 ON CONFLICT (user_did, account_did, post_uri) DO UPDATE SET
                     text           = EXCLUDED.text,
                     context_text   = EXCLUDED.context_text,
                     post_kind      = EXCLUDED.post_kind,
                     onnx_score     = EXCLUDED.onnx_score,
                     status         = CASE WHEN classification_queue.status = 'done'
                                           THEN classification_queue.status
                                           ELSE EXCLUDED.status END,
                     toxic_token    = CASE WHEN classification_queue.status = 'done'
                                           THEN classification_queue.toxic_token
                                           ELSE EXCLUDED.toxic_token END,
                     confidence     = CASE WHEN classification_queue.status = 'done'
                                           THEN classification_queue.confidence
                                           ELSE EXCLUDED.confidence END,
                     model_id       = CASE WHEN classification_queue.status = 'done'
                                           THEN classification_queue.model_id
                                           ELSE EXCLUDED.model_id END,
                     policy_version = CASE WHEN classification_queue.status = 'done'
                                           THEN classification_queue.policy_version
                                           ELSE EXCLUDED.policy_version END",
            )
            .bind(user_did)
            .bind(&row.account_did)
            .bind(&row.post_uri)
            .bind(&row.text)
            .bind(&row.context_text)
            .bind(&row.post_kind)
            .bind(row.onnx_score)
            .bind(&row.status)
            .bind(row.toxic_token)
            .bind(row.confidence)
            .bind(&row.model_id)
            .bind(&row.policy_version)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn stash_account_input(
        &self,
        user_did: &str,
        account_did: &str,
        payload_json: &str,
    ) -> Result<()> {
        sqlx_core::query::query(
            "INSERT INTO scan_account_input (user_did, account_did, payload_json)
             VALUES ($1, $2, $3::jsonb)
             ON CONFLICT (user_did, account_did) DO UPDATE SET payload_json = EXCLUDED.payload_json",
        )
        .bind(user_did)
        .bind(account_did)
        .bind(payload_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn fetch_pending_classifications(
        &self,
        user_did: &str,
        limit: i64,
    ) -> Result<Vec<QueueRow>> {
        let rows = sqlx_core::query::query(
            "SELECT account_did, post_uri, text, context_text, post_kind,
                    onnx_score, status, toxic_token, confidence, model_id, policy_version
             FROM classification_queue
             WHERE user_did = $1 AND status = 'pending'
             LIMIT $2",
        )
        .bind(user_did)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(pg_map_queue_row).collect())
    }

    async fn record_classification_verdicts(
        &self,
        user_did: &str,
        verdicts: &[VerdictRow],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for v in verdicts {
            sqlx_core::query::query(
                "UPDATE classification_queue
                 SET status = 'done',
                     toxic_token    = $1,
                     confidence     = $2,
                     model_id       = $3,
                     policy_version = $4
                 WHERE user_did = $5 AND account_did = $6 AND post_uri = $7",
            )
            .bind(v.toxic_token)
            .bind(v.confidence)
            .bind(&v.model_id)
            .bind(&v.policy_version)
            .bind(user_did)
            .bind(&v.account_did)
            .bind(&v.post_uri)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn list_scan_accounts(&self, user_did: &str) -> Result<Vec<String>> {
        let rows = sqlx_core::query::query(
            "SELECT DISTINCT account_did FROM classification_queue WHERE user_did = $1",
        )
        .bind(user_did)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get::<String, _>(0)).collect())
    }

    async fn fetch_account_verdicts(
        &self,
        user_did: &str,
        account_did: &str,
    ) -> Result<Vec<QueueRow>> {
        let rows = sqlx_core::query::query(
            "SELECT account_did, post_uri, text, context_text, post_kind,
                    onnx_score, status, toxic_token, confidence, model_id, policy_version
             FROM classification_queue
             WHERE user_did = $1 AND account_did = $2",
        )
        .bind(user_did)
        .bind(account_did)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(pg_map_queue_row).collect())
    }

    async fn fetch_account_input(
        &self,
        user_did: &str,
        account_did: &str,
    ) -> Result<Option<String>> {
        let row = sqlx_core::query::query(
            "SELECT payload_json::text FROM scan_account_input
             WHERE user_did = $1 AND account_did = $2",
        )
        .bind(user_did)
        .bind(account_did)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.get::<String, _>(0)))
    }

    async fn count_pending_classifications(&self, user_did: &str) -> Result<i64> {
        let row = sqlx_core::query::query(
            "SELECT COUNT(*)::bigint FROM classification_queue
             WHERE user_did = $1 AND status = 'pending'",
        )
        .bind(user_did)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>(0))
    }

    async fn clear_scan_staging(&self, user_did: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx_core::query::query("DELETE FROM classification_queue WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        sqlx_core::query::query("DELETE FROM scan_account_input WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn clear_account_staging(&self, user_did: &str, account_did: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx_core::query::query(
            "DELETE FROM classification_queue WHERE user_did = $1 AND account_did = $2",
        )
        .bind(user_did)
        .bind(account_did)
        .execute(&mut *tx)
        .await?;
        sqlx_core::query::query(
            "DELETE FROM scan_account_input WHERE user_did = $1 AND account_did = $2",
        )
        .bind(user_did)
        .bind(account_did)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn record_scan_skip(
        &self,
        user_did: &str,
        account_did: &str,
        phase: &str,
        error: &str,
    ) -> Result<()> {
        sqlx_core::query::query(
            "INSERT INTO scan_skips (user_did, account_did, phase, error, skipped_at)
             VALUES ($1, $2, $3, $4, NOW())
             ON CONFLICT (user_did, account_did, phase) DO UPDATE SET
                 error = EXCLUDED.error,
                 skipped_at = EXCLUDED.skipped_at",
        )
        .bind(user_did)
        .bind(account_did)
        .bind(phase)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn count_scan_skips(&self, user_did: &str) -> Result<i64> {
        let row = sqlx_core::query::query("SELECT COUNT(*) FROM scan_skips WHERE user_did = $1")
            .bind(user_did)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>(0))
    }

    async fn list_scan_skips(&self, user_did: &str) -> Result<Vec<ScanSkip>> {
        let rows = sqlx_core::query::query(
            "SELECT account_did, phase, error, skipped_at
             FROM scan_skips WHERE user_did = $1
             ORDER BY skipped_at DESC, account_did ASC",
        )
        .bind(user_did)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| ScanSkip {
                account_did: row.get::<String, _>(0),
                phase: row.get::<String, _>(1),
                error: row.get::<String, _>(2),
                // TIMESTAMPTZ here vs TEXT in SQLite — normalise to a string so
                // the trait surface is identical across backends.
                skipped_at: row.get::<chrono::DateTime<chrono::Utc>, _>(3).to_rfc3339(),
            })
            .collect())
    }

    async fn clear_scan_skips(&self, user_did: &str) -> Result<()> {
        sqlx_core::query::query("DELETE FROM scan_skips WHERE user_did = $1")
            .bind(user_did)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn count_not_assessed(&self, user_did: &str) -> Result<i64> {
        let row = sqlx_core::query::query(
            "SELECT COUNT(*) FROM account_scores
             WHERE user_did = $1 AND threat_tier = 'NotAssessed'
               AND scoring_generation = $2 AND valid_until > NOW()",
        )
        .bind(user_did)
        .bind(scoring_revision())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>(0))
    }

    // --- Scan admission queue (#257) ---

    async fn enqueue_scan(&self, user_did: &str) -> Result<EnqueueOutcome> {
        let mut tx = self.pool.begin().await?;

        // Per-user advisory lock BEFORE the state read (V4-03). `SELECT … FOR
        // UPDATE` on an ABSENT row locks nothing, so without this two
        // first-time enqueues can both read "no row" and the loser's
        // `ON CONFLICT DO UPDATE` would reset a job the winner's worker has
        // already claimed. SQLite's BEGIN IMMEDIATE hides this; Postgres does
        // not. Transaction-scoped, so it releases on commit/rollback.
        //
        // Lock order is acyclic: advisory(user) → queue row here; the
        // scheduling tick takes `users` rows then queue rows and never this
        // advisory lock; `finish_queued_scan` takes only the queue row.
        sqlx_core::query::query("SELECT pg_advisory_xact_lock(hashtext('scan_queue:' || $1))")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;

        let current = sqlx_core::query::query(
            "SELECT status, kind FROM scan_queue WHERE user_did = $1 FOR UPDATE",
        )
        .bind(user_did)
        .fetch_optional(&mut *tx)
        .await?
        .map(|r| (r.get::<String, _>(0), r.get::<String, _>(1)));

        // Every branch records the obligation (V3-03): the user asked for a
        // full scan, and only a full scan that COMPLETES may clear it.
        const RECORD_OBLIGATION: &str =
            "UPDATE scan_queue SET full_requested_at = COALESCE(full_requested_at, NOW())
             WHERE user_did = $1";

        let outcome = match current.as_ref().map(|(s, k)| (s.as_str(), k.as_str())) {
            None | Some(("done", _)) | Some(("failed", _)) => {
                // Conditional as defense in depth even under the advisory
                // lock: if it somehow affects no row, the state is re-read and
                // the truthful outcome returned rather than a fabricated one.
                let affected = sqlx_core::query::query(
                    "INSERT INTO scan_queue (user_did, status, kind, enqueued_at, full_requested_at)
                     VALUES ($1, 'queued', 'full', NOW(), NOW())
                     ON CONFLICT (user_did) DO UPDATE
                       SET status = 'queued', kind = 'full', enqueued_at = NOW(),
                           started_at = NULL, finished_at = NULL,
                           lease_expires = NULL, last_error = NULL,
                           claim_id = NULL, completion = NULL,
                           full_requested_at = COALESCE(scan_queue.full_requested_at, NOW())
                     WHERE scan_queue.status IN ('done', 'failed')",
                )
                .bind(user_did)
                .execute(&mut *tx)
                .await?
                .rows_affected();

                if affected > 0 {
                    EnqueueOutcome::Queued
                } else {
                    // The row was absent (or finished) at the read but is
                    // queued/running now. `SELECT … FOR UPDATE` locks nothing
                    // on an ABSENT row, and the scheduling tick writes queue
                    // rows without taking this advisory lock — so a refresh
                    // the tick created in between lands here. Re-read and
                    // answer for the state actually found, applying the same
                    // in-place upgrade the main arm would.
                    let row = sqlx_core::query::query(
                        "SELECT status, kind FROM scan_queue WHERE user_did = $1 FOR UPDATE",
                    )
                    .bind(user_did)
                    .fetch_optional(&mut *tx)
                    .await?;
                    let found = row
                        .as_ref()
                        .map(|r| (r.get::<String, _>(0), r.get::<String, _>(1)));
                    let upgrade = matches!(
                        found.as_ref().map(|(s, k)| (s.as_str(), k.as_str())),
                        Some(("queued", "refresh"))
                    );
                    sqlx_core::query::query(if upgrade {
                        "UPDATE scan_queue
                         SET kind = 'full', full_requested_at = COALESCE(full_requested_at, NOW())
                         WHERE user_did = $1"
                    } else {
                        RECORD_OBLIGATION
                    })
                    .bind(user_did)
                    .execute(&mut *tx)
                    .await?;
                    match found.as_ref().map(|(s, k)| (s.as_str(), k.as_str())) {
                        Some(("queued", "refresh")) => EnqueueOutcome::Queued,
                        Some(("running", "refresh")) => EnqueueOutcome::QueuedAfterRefresh,
                        Some(("running", _)) => EnqueueOutcome::AlreadyRunning,
                        Some(_) => EnqueueOutcome::AlreadyQueued,
                        // A 0-row `INSERT … ON CONFLICT` means a conflicting
                        // row exists, so finding none here is an invariant
                        // violation, not a state to report. Folding it into
                        // `AlreadyQueued` would tell the user their scan is
                        // waiting in a queue it is not in — a plausible lie is
                        // worse than an error the handler can surface (#344
                        // F5).
                        None => anyhow::bail!(
                            "enqueue_scan: the conditional insert for {user_did} affected no row, \
                             yet no scan_queue row exists to explain it"
                        ),
                    }
                }
            }
            Some(("queued", "refresh")) => {
                // In place: the user keeps the position the refresh held.
                sqlx_core::query::query(
                    "UPDATE scan_queue
                     SET kind = 'full', full_requested_at = COALESCE(full_requested_at, NOW())
                     WHERE user_did = $1",
                )
                .bind(user_did)
                .execute(&mut *tx)
                .await?;
                EnqueueOutcome::Queued
            }
            Some(("queued", _)) => {
                sqlx_core::query::query(RECORD_OBLIGATION)
                    .bind(user_did)
                    .execute(&mut *tx)
                    .await?;
                EnqueueOutcome::AlreadyQueued
            }
            Some(("running", "refresh")) => {
                // Honoured when the refresh finishes (finish_queued_scan). The
                // first request's time wins so repeated clicks do not move it.
                sqlx_core::query::query(RECORD_OBLIGATION)
                    .bind(user_did)
                    .execute(&mut *tx)
                    .await?;
                EnqueueOutcome::QueuedAfterRefresh
            }
            Some(("running", _)) => {
                sqlx_core::query::query(RECORD_OBLIGATION)
                    .bind(user_did)
                    .execute(&mut *tx)
                    .await?;
                EnqueueOutcome::AlreadyRunning
            }
            Some((other, _)) => {
                anyhow::bail!("scan_queue.status holds an unknown value {other:?}")
            }
        };

        tx.commit().await?;
        Ok(outcome)
    }

    async fn enqueue_refresh_scan(&self, user_did: &str) -> Result<()> {
        // Literally the statement the scheduling tick binds
        // ([`REFRESH_ENQUEUE_SQL`]): owed full work is re-queued as FULL,
        // never downgraded to a refresh (V3-03), and queued/running rows are
        // excluded at the write itself so a manual enqueue landing in between
        // survives untouched (V2-02).
        sqlx_core::query::query(REFRESH_ENQUEUE_SQL)
            .bind(user_did)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn request_full_after_refresh(&self, user_did: &str) -> Result<()> {
        sqlx_core::query::query(
            "UPDATE scan_queue SET full_requested_at = COALESCE(full_requested_at, $2)
             WHERE user_did = $1 AND status = 'running' AND kind = 'refresh'",
        )
        .bind(user_did)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn claim_next_scan(&self, limit: usize, lease_secs: i64) -> Result<Option<ScanClaim>> {
        let mut tx = self.pool.begin().await?;

        // Serialize admitters before the count — see
        // SCAN_ADMISSION_ADVISORY_LOCK_KEY for why the count alone races.
        // Transaction-scoped, so it releases on commit/rollback automatically
        // and blocks nothing except another admitter.
        sqlx_core::query::query("SELECT pg_advisory_xact_lock($1)")
            .bind(SCAN_ADMISSION_ADVISORY_LOCK_KEY)
            .execute(&mut *tx)
            .await?;

        let running: i64 =
            sqlx_core::query::query("SELECT COUNT(*) FROM scan_queue WHERE status = 'running'")
                .fetch_one(&mut *tx)
                .await?
                .get(0);
        if running >= limit as i64 {
            tx.commit().await?;
            return Ok(None);
        }

        // SKIP LOCKED so two admitters (or two replicas) never claim the same
        // row, and neither blocks waiting for the other.
        //
        // `(enqueued_at, user_did)`, not `enqueued_at` alone: `enqueue_scan`
        // stamps NOW(), so two requests inside the same microsecond tie, and
        // among tied rows a bare ORDER BY admits whichever row the plan
        // reaches first — not the one `list_scan_queue` displays as next. One
        // total order for display, position, and admission (#271).
        //
        // No `kind` in the ORDER BY: the queue is FIFO ACROSS kinds (#271), so
        // an older refresh is admitted before a newer full scan.
        let row = sqlx_core::query::query(
            "SELECT user_did, kind FROM scan_queue
             WHERE status = 'queued'
             ORDER BY enqueued_at, user_did
             LIMIT 1
             FOR UPDATE SKIP LOCKED",
        )
        .fetch_optional(&mut *tx)
        .await?;

        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let did: String = row.get(0);
        let raw_kind: String = row.get(1);
        let kind = ScanKind::from_str(&raw_kind)
            .with_context(|| format!("scan_queue.kind holds an unknown value {raw_kind:?}"))?;

        // gen_random_uuid() is core Postgres from 13 on, so the fencing token
        // costs no extension and no round-trip.
        let claim_row = sqlx_core::query::query(
            "UPDATE scan_queue
             SET status = 'running', started_at = NOW(),
                 lease_expires = NOW() + make_interval(secs => $2),
                 claim_id = gen_random_uuid()::TEXT
             WHERE user_did = $1
             RETURNING claim_id",
        )
        .bind(&did)
        .bind(lease_secs as f64)
        .fetch_one(&mut *tx)
        .await?;
        let claim_id: String = claim_row.get(0);

        tx.commit().await?;
        Ok(Some(ScanClaim {
            user_did: did,
            claim_id,
            kind,
        }))
    }

    async fn heartbeat_scan(
        &self,
        user_did: &str,
        claim_id: &str,
        lease_secs: i64,
    ) -> Result<bool> {
        let result = sqlx_core::query::query(
            "UPDATE scan_queue
             SET lease_expires = NOW() + make_interval(secs => $3)
             WHERE user_did = $1 AND status = 'running' AND claim_id = $2",
        )
        .bind(user_did)
        .bind(claim_id)
        .bind(lease_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn scan_claim_is_current(&self, user_did: &str, claim_id: &str) -> Result<bool> {
        // Read-only and single-statement, so no transaction and no `FOR
        // UPDATE`: the caller is about to skip its writes on a false, and a
        // row that changes owner a microsecond later would have raced this
        // worker anyway. Mirrors queries::scan_claim_is_current.
        let row = sqlx_core::query::query("SELECT claim_id FROM scan_queue WHERE user_did = $1")
            .bind(user_did)
            .fetch_optional(&self.pool)
            .await?;
        let owner: Option<String> = match row {
            Some(row) => row.try_get("claim_id")?,
            None => return Ok(false),
        };
        Ok(owner.is_some_and(|owner| owner == claim_id))
    }

    async fn finish_queued_scan(
        &self,
        user_did: &str,
        claim_id: &str,
        completion: FinishCompletion,
        error: Option<&str>,
    ) -> Result<bool> {
        // status = 'running' AND claim_id together are what make this safe: a
        // worker whose lease lapsed has had its row reclaimed and re-claimed
        // under a new claim_id, so its late finish matches nothing instead of
        // stomping the new owner's running row to 'done' and freeing a slot
        // that is still occupied.
        //
        // Three shapes, all decided from the row's PRE-update values (an
        // UPDATE's expressions see the old row):
        //   refresh + owed full  → hand over: queued full, obligation kept (R09)
        //   full + fulfilled     → done, obligation cleared (V3-03)
        //   anything else        → done/failed, obligation kept
        let result = sqlx_core::query::query(
            "UPDATE scan_queue
             SET status = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN 'queued'
                               WHEN $3::TEXT IS NULL THEN 'done' ELSE 'failed' END,
                 kind = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN 'full' ELSE kind END,
                 enqueued_at = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN full_requested_at ELSE enqueued_at END,
                 started_at = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN NULL ELSE started_at END,
                 finished_at = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN NULL ELSE NOW() END,
                 claim_id = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN NULL ELSE claim_id END,
                 completion = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN NULL ELSE $4 END,
                 full_requested_at = CASE WHEN kind = 'full' AND $5::boolean THEN NULL ELSE full_requested_at END,
                 lease_expires = NULL,
                 last_error = $3
             WHERE user_did = $1 AND status = 'running' AND claim_id = $2",
        )
        .bind(user_did)
        .bind(claim_id)
        .bind(error)
        .bind(completion.as_str())
        .bind(completion.fulfils_full_request())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn reclaim_expired_scans(&self) -> Result<usize> {
        // A NULL lease on a running row is unrecoverable otherwise — nothing
        // would ever reclaim it and the slot would stay occupied forever.
        let result = sqlx_core::query::query(
            "UPDATE scan_queue
             SET status = 'queued', started_at = NULL, lease_expires = NULL,
                 claim_id = NULL
             WHERE status = 'running'
               AND (lease_expires IS NULL OR lease_expires < NOW())",
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() as usize)
    }

    async fn scan_queue_depth(&self) -> Result<ScanQueueDepth> {
        let row = sqlx_core::query::query(
            "SELECT COUNT(*) FILTER (WHERE status = 'queued'),
                    COUNT(*) FILTER (WHERE status = 'running')
             FROM scan_queue",
        )
        .fetch_one(&self.pool)
        .await?;
        let queued: i64 = row.get(0);
        let running: i64 = row.get(1);
        Ok(ScanQueueDepth {
            queued: queued as usize,
            running: running as usize,
        })
    }

    async fn scan_queue_entry(
        &self,
        user_did: &str,
        concurrency_limit: usize,
    ) -> Result<Option<ScanQueueEntry>> {
        // The `(enqueued_at, user_did)` row-value predicate is the same total
        // order `list_scan_queue` displays by and `claim_next_scan` admits by
        // — `enqueued_at` alone ties and hands every tied row one number
        // (#271).
        let row = sqlx_core::query::query(
            "SELECT status, enqueued_at,
                    (SELECT COUNT(*) FROM scan_queue q2
                      WHERE q2.status = 'queued'
                        AND (q2.enqueued_at, q2.user_did)
                            <= (q.enqueued_at, q.user_did)) AS position
             FROM scan_queue q WHERE user_did = $1",
        )
        .bind(user_did)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else { return Ok(None) };
        let status: String = row.get(0);
        // TIMESTAMPTZ here vs TEXT in SQLite. `::TEXT` would render
        // "2026-08-06 00:36:25.231997-07" — a different separator and offset
        // format from SQLite's RFC3339, varying with the connection's TimeZone
        // and rejected by DateTime::parse_from_rfc3339. Normalise the way
        // list_scan_skips above does.
        let enqueued_at: String = row.get::<chrono::DateTime<chrono::Utc>, _>(1).to_rfc3339();
        let position: i64 = if status == "queued" { row.get(2) } else { 0 };

        // Rolling median over the 20 most recently recorded full-scan
        // durations. None until any full scan is fulfilled, so ETA is absent
        // rather than fabricated on a fresh install.
        //
        // The sample comes from `scan_state`, NOT from `scan_queue` (#344 F1).
        // There is one queue row per user and a refresh enqueue resets its
        // `started_at`/`finished_at` and sets `kind = 'refresh'`, so a median
        // read from the queue would go permanently empty the first night the
        // refresh job runs. `finish_full_scan_state` writes one durable sample
        // per user instead. The median itself is computed in Rust, shared with
        // the SQLite backend, so the two cannot drift.
        let rows = sqlx_core::query::query(
            "SELECT value FROM scan_state WHERE key = $1
             ORDER BY updated_at DESC, user_did DESC LIMIT 20",
        )
        .bind(super::traits::LAST_FULL_SCAN_DURATION_KEY)
        .fetch_all(&self.pool)
        .await?;
        let median = super::traits::median_scan_duration_secs(
            rows.into_iter().map(|r| r.get::<String, _>(0)),
        );

        let eta_seconds = eta_seconds(&status, position, concurrency_limit, median);

        Ok(Some(ScanQueueEntry {
            user_did: user_did.to_string(),
            status,
            position,
            eta_seconds,
            enqueued_at,
        }))
    }

    async fn list_scan_queue(&self) -> Result<Vec<ScanQueueRow>> {
        // Position counts on the SAME `(enqueued_at, user_did)` pair the
        // ORDER BY sorts on, so a tie renders 1st/2nd/3rd AND numbers 1/2/3.
        // Counting `enqueued_at <=` alone gave all three the same number
        // (#271).
        let rows = sqlx_core::query::query(
            "SELECT user_did, status, enqueued_at, started_at, finished_at, last_error,
                    (SELECT COUNT(*) FROM scan_queue q2
                      WHERE q2.status = 'queued'
                        AND (q2.enqueued_at, q2.user_did)
                            <= (q.enqueued_at, q.user_did)) AS position,
                    kind, full_requested_at, completion
             FROM scan_queue q
             ORDER BY q.enqueued_at ASC, q.user_did ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        // `?` on every row rather than a lossy default: an unrecognised
        // `kind`/`completion` is a row this binary cannot interpret, and
        // rendering it as an ordinary full scan hides it from the operator —
        // the exact blindness #288 removed.
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let raw_kind: String = row.get(7);
            let kind = ScanKind::from_str(&raw_kind)
                .with_context(|| format!("scan_queue.kind holds an unknown value {raw_kind:?}"))?;
            let completion = match row.get::<Option<String>, _>(9) {
                None => None,
                Some(c) => Some(FinishCompletion::from_str(&c).with_context(|| {
                    format!("scan_queue.completion holds an unknown value {c:?}")
                })?),
            };
            out.push({
                let status: String = row.get(1);
                let raw_position: i64 = row.get(6);
                ScanQueueRow {
                    user_did: row.get::<String, _>(0),
                    // Non-queued rows hold a slot or are finished; neither has
                    // a place in line. Same rule as `scan_queue_entry`.
                    position: if status == "queued" { raw_position } else { 0 },
                    status,
                    // TIMESTAMPTZ here vs TEXT in SQLite. `::TEXT` would
                    // render "2026-08-06 00:36:25.231997-07" — a different
                    // separator and offset format, varying with the
                    // connection's TimeZone and rejected by
                    // DateTime::parse_from_rfc3339. Normalise the way
                    // list_scan_skips does.
                    enqueued_at: row.get::<chrono::DateTime<chrono::Utc>, _>(2).to_rfc3339(),
                    started_at: row
                        .get::<Option<chrono::DateTime<chrono::Utc>>, _>(3)
                        .map(|t| t.to_rfc3339()),
                    finished_at: row
                        .get::<Option<chrono::DateTime<chrono::Utc>>, _>(4)
                        .map(|t| t.to_rfc3339()),
                    last_error: row.get::<Option<String>, _>(5),
                    kind,
                    full_requested_at: row
                        .get::<Option<chrono::DateTime<chrono::Utc>>, _>(8)
                        .map(|t| t.to_rfc3339()),
                    completion,
                }
            });
        }
        Ok(out)
    }

    // --- Refresh schedule (#343 §4.4, #344) ---

    async fn next_refresh_at(&self, user_did: &str) -> Result<Option<String>> {
        Ok(
            sqlx_core::query::query("SELECT next_refresh_at FROM users WHERE did = $1")
                .bind(user_did)
                .fetch_optional(&self.pool)
                .await?
                .and_then(|r| r.get::<Option<chrono::DateTime<chrono::Utc>>, _>(0))
                .map(|t| t.to_rfc3339()),
        )
    }

    async fn refreshed_generation(&self, user_did: &str) -> Result<Option<String>> {
        Ok(
            sqlx_core::query::query("SELECT refreshed_generation FROM users WHERE did = $1")
                .bind(user_did)
                .fetch_optional(&self.pool)
                .await?
                .and_then(|r| r.get::<Option<String>, _>(0)),
        )
    }

    async fn refresh_attempted_generation(&self, user_did: &str) -> Result<Option<String>> {
        Ok(
            sqlx_core::query::query(
                "SELECT refresh_attempted_generation FROM users WHERE did = $1",
            )
            .bind(user_did)
            .fetch_optional(&self.pool)
            .await?
            .and_then(|r| r.get::<Option<String>, _>(0)),
        )
    }

    async fn schedule_refresh(&self, user_did: &str, at_rfc3339: &str) -> Result<()> {
        sqlx_core::query::query(
            "UPDATE users SET next_refresh_at = $2::timestamptz WHERE did = $1",
        )
        .bind(user_did)
        .bind(at_rfc3339)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn schedule_retry_at(
        &self,
        user_did: &str,
        at_rfc3339: &str,
        attempted_generation: &str,
    ) -> Result<()> {
        // One statement for both facts (V4-01) — see the SQLite twin.
        sqlx_core::query::query(
            "UPDATE users
                SET next_refresh_at = $2::timestamptz, refresh_attempted_generation = $3
              WHERE did = $1",
        )
        .bind(user_did)
        .bind(at_rfc3339)
        .bind(attempted_generation)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn mark_refreshed_generation(&self, user_did: &str, generation: &str) -> Result<()> {
        // Proof sets BOTH columns (V3-04): a proven revision is an attempted
        // one, so the tick's revision clause stays quiet after a full scan.
        sqlx_core::query::query(
            "UPDATE users
                SET refreshed_generation = $2, refresh_attempted_generation = $2
              WHERE did = $1",
        )
        .bind(user_did)
        .bind(generation)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn mark_refresh_attempted_generation(
        &self,
        user_did: &str,
        generation: &str,
    ) -> Result<()> {
        sqlx_core::query::query(
            "UPDATE users SET refresh_attempted_generation = $2 WHERE did = $1",
        )
        .bind(user_did)
        .bind(generation)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn claim_and_enqueue_due_refreshes(
        &self,
        now_rfc3339: &str,
        next_rfc3339: &str,
        current_generation: &str,
        limit: usize,
    ) -> Result<Vec<String>> {
        // ONE transaction (R04). `FOR UPDATE OF u … SKIP LOCKED` means two
        // replicas ticking together partition the due set instead of fighting
        // over it: whatever the other one has already selected is skipped
        // rather than waited on.
        //
        // Lock order is `users` (FOR UPDATE) then `scan_queue`. `enqueue_scan`
        // and `finish_queued_scan` lock only `scan_queue`, and the schedulers
        // lock only `users` — no cycle.
        let mut tx = self.pool.begin().await?;
        let due: Vec<String> = sqlx_core::query::query(REFRESH_DUE_SQL)
            .bind(now_rfc3339)
            .bind(current_generation)
            .bind(limit as i64)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|r| r.get::<String, _>(0))
            .collect();

        let mut delivered = Vec::with_capacity(due.len());
        for did in due {
            let affected = sqlx_core::query::query(REFRESH_ENQUEUE_SQL)
                .bind(&did)
                .bind(now_rfc3339)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            if affected == 0 {
                // The row changed between the select and this write (a manual
                // enqueue or an admission committed in between). Leave the
                // schedule alone: whatever is running reschedules on
                // completion, and this user is reconsidered next tick (V2-02).
                continue;
            }
            sqlx_core::query::query(
                "UPDATE users
                    SET next_refresh_at = $2::timestamptz,
                        refresh_attempted_generation = $3
                  WHERE did = $1",
            )
            .bind(&did)
            .bind(next_rfc3339)
            .bind(current_generation)
            .execute(&mut *tx)
            .await?;
            delivered.push(did);
        }
        tx.commit().await?;
        Ok(delivered)
    }

    // --- Access requests (#309) ---

    async fn get_access_request(&self, did: &str) -> Result<Option<AccessRequestRow>> {
        let row = sqlx_core::query::query(
            "SELECT did, handle, status, requested_at, decided_at, decided_by
             FROM access_requests WHERE did = $1",
        )
        .bind(did)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| AccessRequestRow {
            did: r.get::<String, _>(0),
            handle: r.get::<String, _>(1),
            status: r.get::<String, _>(2),
            requested_at: r.get::<String, _>(3),
            decided_at: r.get::<Option<String>, _>(4),
            decided_by: r.get::<Option<String>, _>(5),
        }))
    }

    async fn upsert_access_request_pending(&self, did: &str, handle: &str) -> Result<()> {
        // ON CONFLICT refreshes the handle ONLY: a denied row stays denied and
        // an allowed row stays allowed — sign-in attempts never move the
        // state machine. Same rule as the SQLite side.
        sqlx_core::query::query(
            "INSERT INTO access_requests (did, handle, status, requested_at)
             VALUES ($1, $2, 'pending', $3)
             ON CONFLICT (did) DO UPDATE SET handle = EXCLUDED.handle",
        )
        .bind(did)
        .bind(handle)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn set_access_status(&self, did: &str, status: &str, decided_by: &str) -> Result<bool> {
        let result = sqlx_core::query::query(
            "UPDATE access_requests SET status = $2, decided_at = $3, decided_by = $4
             WHERE did = $1",
        )
        .bind(did)
        .bind(status)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(decided_by)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn grant_access(&self, did: &str, handle: &str, decided_by: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx_core::query::query(
            "INSERT INTO access_requests (did, handle, status, requested_at, decided_at, decided_by)
             VALUES ($1, $2, 'allowed', $3, $3, $4)
             ON CONFLICT (did) DO UPDATE SET status = 'allowed', handle = EXCLUDED.handle,
                 decided_at = EXCLUDED.decided_at, decided_by = EXCLUDED.decided_by",
        )
        .bind(did)
        .bind(handle)
        .bind(now)
        .bind(decided_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_access_requests(&self) -> Result<Vec<AccessRequestRow>> {
        let rows = sqlx_core::query::query(
            "SELECT did, handle, status, requested_at, decided_at, decided_by
             FROM access_requests ORDER BY requested_at ASC, did ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| AccessRequestRow {
                did: r.get::<String, _>(0),
                handle: r.get::<String, _>(1),
                status: r.get::<String, _>(2),
                requested_at: r.get::<String, _>(3),
                decided_at: r.get::<Option<String>, _>(4),
                decided_by: r.get::<Option<String>, _>(5),
            })
            .collect())
    }

    // --- OAuth write sessions (#315) ---

    async fn get_oauth_session(&self, user_did: &str) -> Result<Option<OauthSessionRow>> {
        let row = sqlx_core::query::query(
            "SELECT user_did, pds_url, scope, access_token_enc, refresh_token_enc,
                    dpop_key_enc, access_expires_at, created_at, updated_at
             FROM oauth_sessions WHERE user_did = $1",
        )
        .bind(user_did)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| OauthSessionRow {
            user_did: r.get::<String, _>(0),
            pds_url: r.get::<String, _>(1),
            scope: r.get::<String, _>(2),
            access_token_enc: r.get::<Vec<u8>, _>(3),
            refresh_token_enc: r.get::<Vec<u8>, _>(4),
            dpop_key_enc: r.get::<Vec<u8>, _>(5),
            access_expires_at: r.get::<i64, _>(6),
            created_at: r.get::<String, _>(7),
            updated_at: r.get::<String, _>(8),
        }))
    }

    async fn upsert_oauth_session(&self, row: &OauthSessionRow) -> Result<()> {
        // created_at is deliberately absent from the DO UPDATE list: re-consent
        // rotates every secret but keeps the original connection date.
        sqlx_core::query::query(
            "INSERT INTO oauth_sessions (user_did, pds_url, scope, access_token_enc,
                 refresh_token_enc, dpop_key_enc, access_expires_at, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (user_did) DO UPDATE SET
                 pds_url = EXCLUDED.pds_url, scope = EXCLUDED.scope,
                 access_token_enc = EXCLUDED.access_token_enc,
                 refresh_token_enc = EXCLUDED.refresh_token_enc,
                 dpop_key_enc = EXCLUDED.dpop_key_enc,
                 access_expires_at = EXCLUDED.access_expires_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.user_did)
        .bind(&row.pds_url)
        .bind(&row.scope)
        .bind(&row.access_token_enc)
        .bind(&row.refresh_token_enc)
        .bind(&row.dpop_key_enc)
        .bind(row.access_expires_at)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_oauth_tokens(
        &self,
        user_did: &str,
        access_token_enc: &[u8],
        refresh_token_enc: &[u8],
        access_expires_at: i64,
        scope: &str,
        expected_updated_at: &str,
        new_updated_at: &str,
    ) -> Result<bool> {
        let result = sqlx_core::query::query(
            "UPDATE oauth_sessions
             SET access_token_enc = $2, refresh_token_enc = $3, access_expires_at = $4,
                 scope = $7, updated_at = $6
             WHERE user_did = $1 AND updated_at = $5",
        )
        .bind(user_did)
        .bind(access_token_enc)
        .bind(refresh_token_enc)
        .bind(access_expires_at)
        .bind(expected_updated_at)
        .bind(new_updated_at)
        .bind(scope)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn delete_oauth_session(&self, user_did: &str) -> Result<bool> {
        let result = sqlx_core::query::query("DELETE FROM oauth_sessions WHERE user_did = $1")
            .bind(user_did)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn delete_oauth_session_if_unchanged(
        &self,
        user_did: &str,
        expected_updated_at: &str,
    ) -> Result<bool> {
        let result = sqlx_core::query::query(
            "DELETE FROM oauth_sessions WHERE user_did = $1 AND updated_at = $2",
        )
        .bind(user_did)
        .bind(expected_updated_at)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    // --- Action batches (#315) ---

    async fn create_action_batch(
        &self,
        user_did: &str,
        kind: &str,
        source: &str,
        rows: &[NewAction],
    ) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        let now = chrono::Utc::now().to_rfc3339();
        let batch_id: i64 = sqlx_core::query::query(
            "INSERT INTO action_batches (user_did, kind, source, requested, status, created_at)
             VALUES ($1, $2, $3, $4, 'queued', $5) RETURNING id",
        )
        .bind(user_did)
        .bind(kind)
        .bind(source)
        .bind(rows.len() as i64)
        .bind(&now)
        .fetch_one(&mut *tx)
        .await?
        .get(0);
        for a in rows {
            sqlx_core::query::query(
                "INSERT INTO actions (batch_id, user_did, target_did, kind, status, undo_of,
                     score_at_action, tier_at_action)
                 VALUES ($1, $2, $3, $4, 'pending', $5, $6, $7)",
            )
            .bind(batch_id)
            .bind(user_did)
            .bind(&a.target_did)
            .bind(&a.kind)
            .bind(a.undo_of)
            .bind(a.score_at_action)
            .bind(&a.tier_at_action)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(batch_id)
    }

    async fn get_action_batch(&self, id: i64) -> Result<Option<ActionBatchRow>> {
        let row = sqlx_core::query::query(&format!(
            "SELECT {ACTION_BATCH_COLS} FROM action_batches WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(read_action_batch))
    }

    async fn list_action_batches(
        &self,
        user_did: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ActionBatchRow>> {
        let rows = sqlx_core::query::query(&format!(
            "SELECT {ACTION_BATCH_COLS} FROM action_batches WHERE user_did = $1
             ORDER BY created_at DESC, id DESC LIMIT $2 OFFSET $3"
        ))
        .bind(user_did)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(read_action_batch).collect())
    }

    async fn list_actions_for_batch(&self, batch_id: i64) -> Result<Vec<ActionRow>> {
        let rows = sqlx_core::query::query(&format!(
            "SELECT {ACTION_COLS} FROM actions WHERE batch_id = $1 ORDER BY id ASC"
        ))
        .bind(batch_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(read_action).collect())
    }

    async fn list_unfinished_batches(&self) -> Result<Vec<i64>> {
        let rows = sqlx_core::query::query(
            "SELECT id FROM action_batches WHERE status IN ('queued','running') ORDER BY id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get::<i64, _>(0)).collect())
    }

    async fn set_action_batch_status(
        &self,
        id: i64,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        // COALESCE keeps the first started_at across resumes; finished_at is
        // stamped on every terminal transition (a retry-to-queued clears it).
        sqlx_core::query::query(
            "UPDATE action_batches SET
                 status = $2,
                 error = $3,
                 started_at = CASE WHEN $2 = 'running' THEN COALESCE(started_at, $4) ELSE started_at END,
                 finished_at = CASE WHEN $2 IN ('done','partial','failed') THEN $4 ELSE NULL END
             WHERE id = $1",
        )
        .bind(id)
        .bind(status)
        .bind(error)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_action(
        &self,
        id: i64,
        status: &str,
        record_uri: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx_core::query::query(
            "UPDATE actions SET
                 status = $2,
                 record_uri = COALESCE($3, record_uri),
                 error = $4,
                 applied_at = CASE WHEN $2 IN ('applied','skipped_already_done') THEN $5 ELSE applied_at END,
                 undone_at = CASE WHEN $2 = 'undone' THEN $5 ELSE undone_at END
             WHERE id = $1",
        )
        .bind(id)
        .bind(status)
        .bind(record_uri)
        .bind(error)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_action(&self, id: i64) -> Result<Option<ActionRow>> {
        let row =
            sqlx_core::query::query(&format!("SELECT {ACTION_COLS} FROM actions WHERE id = $1"))
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.as_ref().map(read_action))
    }

    async fn active_actions(&self, user_did: &str) -> Result<Vec<ActionRow>> {
        let rows = sqlx_core::query::query(&format!(
            "SELECT {ACTION_COLS} FROM actions
             WHERE user_did = $1 AND status IN ('applied','skipped_already_done')
             ORDER BY id ASC"
        ))
        .bind(user_did)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(read_action).collect())
    }

    async fn list_score_snapshots(&self, user_did: &str) -> Result<Vec<ScoreSnapshot>> {
        let rows = sqlx_core::query::query(
            "SELECT did, handle, threat_score, threat_tier FROM account_scores WHERE user_did = $1",
        )
        .bind(user_did)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| ScoreSnapshot {
                did: r.get::<String, _>(0),
                handle: r.get::<String, _>(1),
                threat_score: r.get::<Option<f64>, _>(2),
                threat_tier: r.get::<Option<String>, _>(3),
            })
            .collect())
    }

    // --- Shared cache (#343 §4.1) ---

    async fn get_feed_snapshot(&self, did: &str) -> Result<Option<FeedSnapshot>> {
        let row = sqlx_core::query::query(
            "SELECT did, handle, posts_json, fetched_at, source
             FROM account_feed_snapshots WHERE did = $1",
        )
        .bind(did)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| FeedSnapshot {
            did: r.get::<String, _>(0),
            handle: r.get::<String, _>(1),
            posts_json: r.get::<String, _>(2),
            fetched_at: r.get::<String, _>(3),
            source: r.get::<String, _>(4),
        }))
    }

    async fn upsert_feed_snapshot(&self, s: &FeedSnapshot) -> Result<()> {
        sqlx_core::query::query(
            "INSERT INTO account_feed_snapshots (did, handle, posts_json, fetched_at, source)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (did) DO UPDATE SET
                 handle = EXCLUDED.handle,
                 posts_json = EXCLUDED.posts_json,
                 fetched_at = EXCLUDED.fetched_at,
                 source = EXCLUDED.source",
        )
        .bind(&s.did)
        .bind(&s.handle)
        .bind(&s.posts_json)
        .bind(&s.fetched_at)
        .bind(&s.source)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_onnx_scores(
        &self,
        model_id: &str,
        hashes: &[String],
    ) -> Result<std::collections::HashMap<String, f64>> {
        if hashes.is_empty() {
            return Ok(Default::default());
        }
        let rows = sqlx_core::query::query(
            "SELECT text_sha256, score FROM onnx_scores
             WHERE model_id = $1 AND text_sha256 = ANY($2)",
        )
        .bind(model_id)
        .bind(hashes.to_vec())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get::<String, _>(0), r.get::<f64, _>(1)))
            .collect())
    }

    async fn upsert_onnx_scores(&self, model_id: &str, rows: &[OnnxScoreRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        // One UNNEST round trip instead of one per row (CodeRabbit, PR #118),
        // the same shape as insert_amplification_events_batch. `model_id` and
        // `scored_at` are scalars broadcast to every row, so the statement
        // binds four parameters at any batch size.
        //
        // Deduplicating first is load-bearing, not tidiness: a single
        // INSERT ... ON CONFLICT DO UPDATE cannot touch the same row twice
        // ("command cannot affect row a second time"). The old per-row loop
        // simply let a later duplicate overwrite an earlier one, so collapse
        // to last-write-wins here and keep that behaviour. Today's callers
        // (CachedToxicityScorer) already pass distinct hashes; the trait is
        // public and the next one might not.
        let now = chrono::Utc::now().to_rfc3339();
        let mut last: std::collections::HashMap<&str, f64> =
            std::collections::HashMap::with_capacity(rows.len());
        for r in rows {
            last.insert(r.text_sha256.as_str(), r.score);
        }
        let (hashes, scores): (Vec<String>, Vec<f64>) = last
            .into_iter()
            .map(|(h, score)| (h.to_string(), score))
            .unzip();

        // Explicit ::text[] / ::float8[] casts so Postgres can type UNNEST's
        // output columns without inspecting values.
        sqlx_core::query::query(
            "INSERT INTO onnx_scores (text_sha256, model_id, score, scored_at)
             SELECT t.text_sha256, $1, t.score, $2
             FROM UNNEST($3::text[], $4::float8[]) AS t(text_sha256, score)
             ON CONFLICT (text_sha256, model_id) DO UPDATE SET
                 score = EXCLUDED.score, scored_at = EXCLUDED.scored_at",
        )
        .bind(model_id)
        .bind(&now)
        .bind(&hashes)
        .bind(&scores)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_classifier_verdicts(
        &self,
        model_id: &str,
        policy_version: &str,
        hashes: &[String],
    ) -> Result<std::collections::HashMap<String, ClassifierVerdictRow>> {
        if hashes.is_empty() {
            return Ok(Default::default());
        }
        let rows = sqlx_core::query::query(
            "SELECT text_sha256, toxic_token, confidence FROM classifier_verdicts
             WHERE model_id = $1 AND policy_version = $2 AND text_sha256 = ANY($3)",
        )
        .bind(model_id)
        .bind(policy_version)
        .bind(hashes.to_vec())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let h = r.get::<String, _>(0);
                (
                    h.clone(),
                    ClassifierVerdictRow {
                        text_sha256: h,
                        toxic_token: r.get::<bool, _>(1),
                        confidence: r.get::<f64, _>(2),
                    },
                )
            })
            .collect())
    }

    async fn upsert_classifier_verdicts(
        &self,
        model_id: &str,
        policy_version: &str,
        rows: &[ClassifierVerdictRow],
    ) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        // One UNNEST round trip, deduplicated last-write-wins — see
        // upsert_onnx_scores above for why both of those are required.
        let now = chrono::Utc::now().to_rfc3339();
        let mut last: std::collections::HashMap<&str, (bool, f64)> =
            std::collections::HashMap::with_capacity(rows.len());
        for r in rows {
            last.insert(r.text_sha256.as_str(), (r.toxic_token, r.confidence));
        }
        let mut hashes: Vec<String> = Vec::with_capacity(last.len());
        let mut toxic_tokens: Vec<bool> = Vec::with_capacity(last.len());
        let mut confidences: Vec<f64> = Vec::with_capacity(last.len());
        for (hash, (toxic, confidence)) in last {
            hashes.push(hash.to_string());
            toxic_tokens.push(toxic);
            confidences.push(confidence);
        }

        sqlx_core::query::query(
            "INSERT INTO classifier_verdicts
                 (text_sha256, model_id, policy_version, toxic_token, confidence, classified_at)
             SELECT t.text_sha256, $1, $2, t.toxic_token, t.confidence, $3
             FROM UNNEST($4::text[], $5::bool[], $6::float8[])
                  AS t(text_sha256, toxic_token, confidence)
             ON CONFLICT (text_sha256, model_id, policy_version) DO UPDATE SET
                 toxic_token = EXCLUDED.toxic_token,
                 confidence = EXCLUDED.confidence,
                 classified_at = EXCLUDED.classified_at",
        )
        .bind(model_id)
        .bind(policy_version)
        .bind(&now)
        .bind(&hashes)
        .bind(&toxic_tokens)
        .bind(&confidences)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn evict_stale_cache(
        &self,
        feed_cutoff: &str,
        score_cutoff: &str,
    ) -> Result<super::cache_retention::CacheEviction> {
        // One transaction for all three tables so a mid-sweep failure leaves
        // the cache internally consistent, matching the SQLite backend.
        //
        // The `<` comparisons are lexicographic on RFC3339 TEXT, sound only
        // because every writer uses `DateTime<Utc>::to_rfc3339()` (the ordering
        // argument lives on the trait doc). The v17 indexes keep this off a
        // sequential scan.
        let mut tx = self.pool.begin().await?;
        let feed_snapshots =
            sqlx_core::query::query("DELETE FROM account_feed_snapshots WHERE fetched_at < $1")
                .bind(feed_cutoff)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        let onnx_scores = sqlx_core::query::query("DELETE FROM onnx_scores WHERE scored_at < $1")
            .bind(score_cutoff)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let classifier_verdicts =
            sqlx_core::query::query("DELETE FROM classifier_verdicts WHERE classified_at < $1")
                .bind(score_cutoff)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        tx.commit().await?;
        Ok(super::cache_retention::CacheEviction {
            feed_snapshots,
            onnx_scores,
            classifier_verdicts,
        })
    }
}

/// Column list + row reader shared by every action_batches query (#315).
const ACTION_BATCH_COLS: &str =
    "id, user_did, kind, source, requested, status, error, created_at, started_at, finished_at";
const ACTION_COLS: &str = "id, batch_id, user_did, target_did, kind, status, record_uri, undo_of, \
     error, score_at_action, tier_at_action, applied_at, undone_at";

fn read_action_batch(r: &sqlx_postgres::PgRow) -> ActionBatchRow {
    ActionBatchRow {
        id: r.get::<i64, _>(0),
        user_did: r.get::<String, _>(1),
        kind: r.get::<String, _>(2),
        source: r.get::<String, _>(3),
        requested: r.get::<i64, _>(4),
        status: r.get::<String, _>(5),
        error: r.get::<Option<String>, _>(6),
        created_at: r.get::<String, _>(7),
        started_at: r.get::<Option<String>, _>(8),
        finished_at: r.get::<Option<String>, _>(9),
    }
}

fn read_action(r: &sqlx_postgres::PgRow) -> ActionRow {
    ActionRow {
        id: r.get::<i64, _>(0),
        batch_id: r.get::<i64, _>(1),
        user_did: r.get::<String, _>(2),
        target_did: r.get::<String, _>(3),
        kind: r.get::<String, _>(4),
        status: r.get::<String, _>(5),
        record_uri: r.get::<Option<String>, _>(6),
        undo_of: r.get::<Option<i64>, _>(7),
        error: r.get::<Option<String>, _>(8),
        score_at_action: r.get::<Option<f64>, _>(9),
        tier_at_action: r.get::<Option<String>, _>(10),
        applied_at: r.get::<Option<String>, _>(11),
        undone_at: r.get::<Option<String>, _>(12),
    }
}

// ── shared row-mapper ─────────────────────────────────────────────────────────

/// Map a `classification_queue` SELECT row into a `QueueRow` for the Postgres backend.
///
/// Expected column order (0-indexed):
///   0  account_did, 1  post_uri,    2  text,          3  context_text,
///   4  post_kind,   5  onnx_score,  6  status,
///   7  toxic_token (BOOLEAN nullable), 8  confidence (REAL nullable),
///   9  model_id,    10 policy_version
fn pg_map_queue_row(row: &sqlx_postgres::PgRow) -> QueueRow {
    QueueRow {
        account_did: row.get(0),
        post_uri: row.get(1),
        text: row.get(2),
        context_text: row.get(3),
        post_kind: row.get(4),
        onnx_score: row.get(5),
        status: row.get(6),
        toxic_token: row.get::<Option<bool>, _>(7),
        confidence: row.get::<Option<f32>, _>(8),
        model_id: row.get(9),
        policy_version: row.get(10),
    }
}
