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
    AccountScore, AccuracyMetrics, AmplificationEvent, InferredPair, NewAmplificationEvent,
    ThreatTier, ToxicPost, UserLabel, UserRow,
};
use super::traits::Database;
use crate::pipeline::scan_phases::staging::{QueueRow, VerdictRow};

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

    async fn get_fingerprint(&self, user_did: &str) -> Result<Option<(String, u32, String)>> {
        let row = sqlx_core::query::query(
            "SELECT fingerprint_json, post_count,
                    to_char(updated_at, 'YYYY-MM-DD HH24:MI:SS') as updated_at
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

        sqlx_core::query::query(
            "INSERT INTO account_scores
                (user_did, did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
                 posts_analyzed, top_toxic_posts, scored_at, behavioral_signals, context_score, graph_distance,
                 fingerprint_quality, scoring_confidence)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW(), $10, $11, $12, $13, $14)
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
                scoring_confidence = $14",
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
                    fingerprint_quality, scoring_confidence, graph_distance
             FROM account_scores
             WHERE user_did = $1 AND threat_score >= $2
             ORDER BY threat_score DESC",
        )
        .bind(user_did)
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
                context_score: row.get(10),
                graph_distance: row.get(13),
                fingerprint_quality: row.get(11),
                scoring_confidence: row.get(12),
            });
        }
        Ok(accounts)
    }

    async fn is_score_stale(&self, user_did: &str, did: &str, max_age_days: i64) -> Result<bool> {
        // Use make_interval(days => $3) with a bound i32 instead of string
        // concatenation — avoids SQL injection risk and type ambiguity.
        let row = sqlx_core::query::query(
            "SELECT scored_at < NOW() - make_interval(days => $3)
             FROM account_scores WHERE user_did = $1 AND did = $2",
        )
        .bind(user_did)
        .bind(did)
        .bind(i32::try_from(max_age_days).context("max_age_days exceeds i32 range")?)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            None => Ok(true), // No score exists — treat as stale
            Some(r) => Ok(r.get::<bool, _>(0)),
        }
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
                    fingerprint_quality, scoring_confidence, graph_distance
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
            let threat_tier = threat_score.map(|s| ThreatTier::from_score(s).to_string());
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
            }
        }))
    }

    async fn get_account_by_did(&self, user_did: &str, did: &str) -> Result<Option<AccountScore>> {
        let row = sqlx_core::query::query(
            "SELECT did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
                    posts_analyzed, top_toxic_posts,
                    to_char(scored_at, 'YYYY-MM-DD HH24:MI:SS') as scored_at,
                    behavioral_signals, context_score,
                    fingerprint_quality, scoring_confidence, graph_distance
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
            let threat_tier = threat_score.map(|s| ThreatTier::from_score(s).to_string());
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
                    a.fingerprint_quality, a.scoring_confidence, a.graph_distance
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
                context_score: row.get(10),
                graph_distance: row.get(13),
                fingerprint_quality: row.get(11),
                scoring_confidence: row.get(12),
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
