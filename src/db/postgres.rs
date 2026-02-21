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

use super::models::{AccountScore, AmplificationEvent, ThreatTier, ToxicPost};
use super::traits::Database;

/// Type alias for the PostgreSQL connection pool.
pub type PgPool = Pool<Postgres>;

pub struct PgDatabase {
    pool: PgPool,
}

impl PgDatabase {
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
        let migration_result: Result<()> = async {
            // Ensure schema_version table exists (idempotent DDL, no transaction needed)
            sqlx_core::query::query(
                "CREATE TABLE IF NOT EXISTS schema_version (
                    version INTEGER PRIMARY KEY,
                    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                )",
            )
            .execute(&self.pool)
            .await?;

            let migrations = [
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
            ];

            for (version, sql) in migrations {
                let applied: bool = sqlx_core::query::query(
                    "SELECT COUNT(*) > 0 FROM schema_version WHERE version = $1",
                )
                .bind(version)
                .fetch_one(&self.pool)
                .await
                .map(|row| row.get::<bool, _>(0))
                .unwrap_or(false);

                if !applied {
                    if version == 1 {
                        // Migration 1 contains CREATE EXTENSION which cannot run inside a
                        // transaction. All statements use IF NOT EXISTS so they are safe
                        // to retry if the process is interrupted partway through.
                        sqlx_core::raw_sql::raw_sql(sql).execute(&self.pool).await?;
                    } else {
                        // Migrations 2+ are wrapped in a transaction so the schema change
                        // and schema_version insert are committed or rolled back together.
                        let mut tx = self.pool.begin().await?;
                        sqlx_core::raw_sql::raw_sql(sql).execute(&mut *tx).await?;
                        tx.commit().await?;
                    }
                }
            }

            Ok(())
        }
        .await;

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

    async fn get_scan_state(&self, key: &str) -> Result<Option<String>> {
        let row = sqlx_core::query::query("SELECT value FROM scan_state WHERE key = $1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<String, _>(0)))
    }

    async fn set_scan_state(&self, key: &str, value: &str) -> Result<()> {
        sqlx_core::query::query(
            "INSERT INTO scan_state (key, value, updated_at)
             VALUES ($1, $2, NOW())
             ON CONFLICT(key) DO UPDATE SET value = $2, updated_at = NOW()",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn save_fingerprint(&self, fingerprint_json: &str, post_count: u32) -> Result<()> {
        sqlx_core::query::query(
            "INSERT INTO topic_fingerprint (id, fingerprint_json, post_count, updated_at)
             VALUES (1, $1, $2, NOW())
             ON CONFLICT(id) DO UPDATE SET
                fingerprint_json = $1,
                post_count = $2,
                updated_at = NOW()",
        )
        .bind(fingerprint_json)
        .bind(i32::try_from(post_count).context("post_count exceeds i32 range")?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn save_embedding(&self, embedding: &[f64]) -> Result<()> {
        // Convert f64 to f32 for pgvector (which uses 32-bit floats)
        let floats: Vec<f32> = embedding.iter().map(|&v| v as f32).collect();
        let vector = pgvector::Vector::from(floats);
        let result = sqlx_core::query::query(
            "UPDATE topic_fingerprint SET embedding_vector = $1, updated_at = NOW() WHERE id = 1",
        )
        .bind(vector)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            anyhow::bail!(
                "save_embedding: no fingerprint row found — run `charcoal fingerprint` first"
            );
        }
        Ok(())
    }

    async fn get_fingerprint(&self) -> Result<Option<(String, u32, String)>> {
        let row = sqlx_core::query::query(
            "SELECT fingerprint_json, post_count,
                    to_char(updated_at, 'YYYY-MM-DD HH24:MI:SS') as updated_at
             FROM topic_fingerprint WHERE id = 1",
        )
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

    async fn get_embedding(&self) -> Result<Option<Vec<f64>>> {
        let row =
            sqlx_core::query::query("SELECT embedding_vector FROM topic_fingerprint WHERE id = 1")
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

    async fn upsert_account_score(&self, score: &AccountScore) -> Result<()> {
        let top_posts_json = serde_json::to_value(&score.top_toxic_posts)?;
        let behavioral_json: Option<serde_json::Value> = score
            .behavioral_signals
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok());

        sqlx_core::query::query(
            "INSERT INTO account_scores
                (did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
                 posts_analyzed, top_toxic_posts, scored_at, behavioral_signals)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW(), $9)
             ON CONFLICT(did) DO UPDATE SET
                handle = $2,
                toxicity_score = $3,
                topic_overlap = $4,
                threat_score = $5,
                threat_tier = $6,
                posts_analyzed = $7,
                top_toxic_posts = $8,
                scored_at = NOW(),
                behavioral_signals = $9",
        )
        .bind(&score.did)
        .bind(&score.handle)
        .bind(score.toxicity_score)
        .bind(score.topic_overlap)
        .bind(score.threat_score)
        .bind(&score.threat_tier)
        .bind(score.posts_analyzed as i32)
        .bind(&top_posts_json)
        .bind(&behavioral_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_ranked_threats(&self, min_score: f64) -> Result<Vec<AccountScore>> {
        let rows = sqlx_core::query::query(
            "SELECT did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
                    posts_analyzed, top_toxic_posts,
                    to_char(scored_at, 'YYYY-MM-DD HH24:MI:SS') as scored_at,
                    behavioral_signals
             FROM account_scores
             WHERE threat_score >= $1
             ORDER BY threat_score DESC",
        )
        .bind(min_score)
        .fetch_all(&self.pool)
        .await?;

        let mut accounts = Vec::new();
        for row in rows {
            let top_posts_json: serde_json::Value = row.get(7);
            let top_toxic_posts: Vec<ToxicPost> =
                serde_json::from_value(top_posts_json).unwrap_or_default();

            // Recalculate tier from stored score so threshold changes
            // take effect without rescanning.
            let threat_score: Option<f64> = row.get(4);
            let threat_tier = threat_score.map(|s| ThreatTier::from_score(s).to_string());

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
            });
        }
        Ok(accounts)
    }

    async fn is_score_stale(&self, did: &str, max_age_days: i64) -> Result<bool> {
        // Use make_interval(days => $2) with a bound i32 instead of string
        // concatenation — avoids SQL injection risk and type ambiguity.
        let row = sqlx_core::query::query(
            "SELECT scored_at < NOW() - make_interval(days => $2)
             FROM account_scores WHERE did = $1",
        )
        .bind(did)
        .bind(max_age_days as i32)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            None => Ok(true), // No score exists — treat as stale
            Some(r) => Ok(r.get::<bool, _>(0)),
        }
    }

    async fn insert_amplification_event(
        &self,
        event_type: &str,
        amplifier_did: &str,
        amplifier_handle: &str,
        original_post_uri: &str,
        amplifier_post_uri: Option<&str>,
        amplifier_text: Option<&str>,
    ) -> Result<i64> {
        let row = sqlx_core::query::query(
            "INSERT INTO amplification_events
                (event_type, amplifier_did, amplifier_handle, original_post_uri,
                 amplifier_post_uri, amplifier_text)
             VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING id",
        )
        .bind(event_type)
        .bind(amplifier_did)
        .bind(amplifier_handle)
        .bind(original_post_uri)
        .bind(amplifier_post_uri)
        .bind(amplifier_text)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>(0))
    }

    async fn get_recent_events(&self, limit: u32) -> Result<Vec<AmplificationEvent>> {
        // Cap at i32::MAX before casting to avoid overflow — PostgreSQL LIMIT
        // accepts i64 but sqlx binds integers as i32 here. Values above i32::MAX
        // are effectively unlimited for any realistic dataset.
        let rows = sqlx_core::query::query(
            "SELECT id, event_type, amplifier_did, amplifier_handle, original_post_uri,
                    amplifier_post_uri, amplifier_text,
                    to_char(detected_at, 'YYYY-MM-DD HH24:MI:SS') as detected_at,
                    followers_fetched, followers_scored
             FROM amplification_events
             ORDER BY detected_at DESC
             LIMIT $1",
        )
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
            });
        }
        Ok(events)
    }

    async fn get_events_for_pile_on(&self) -> Result<Vec<(String, String, String)>> {
        let rows = sqlx_core::query::query(
            "SELECT amplifier_did, original_post_uri,
                    to_char(detected_at, 'YYYY-MM-DD HH24:MI:SS') as detected_at
             FROM amplification_events
             ORDER BY original_post_uri, detected_at",
        )
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

    async fn get_median_engagement(&self) -> Result<f64> {
        // Use percentile_cont for a true median calculation
        let row = sqlx_core::query::query(
            "SELECT COALESCE(
                percentile_cont(0.5) WITHIN GROUP (
                    ORDER BY (behavioral_signals->>'avg_engagement')::double precision
                ),
                0.0
             )
             FROM account_scores
             WHERE behavioral_signals IS NOT NULL
               AND behavioral_signals->>'avg_engagement' IS NOT NULL",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<f64, _>(0))
    }

    async fn get_all_scan_state(&self) -> Result<Vec<(String, String)>> {
        let rows = sqlx_core::query::query("SELECT key, value FROM scan_state")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<String, _>(0), r.get::<String, _>(1)))
            .collect())
    }

    async fn insert_amplification_event_raw(&self, event: &AmplificationEvent) -> Result<i64> {
        // Insert with the original detected_at so migrated events keep their
        // real timestamps. Pile-on detection depends on accurate timestamps.
        let row = sqlx_core::query::query(
            "INSERT INTO amplification_events
                (event_type, amplifier_did, amplifier_handle, original_post_uri,
                 amplifier_post_uri, amplifier_text, detected_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7::timestamptz)
             RETURNING id",
        )
        .bind(&event.event_type)
        .bind(&event.amplifier_did)
        .bind(&event.amplifier_handle)
        .bind(&event.original_post_uri)
        .bind(&event.amplifier_post_uri)
        .bind(&event.amplifier_text)
        .bind(&event.detected_at)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>(0))
    }
}
