// Database queries — CRUD operations for all tables.
//
// Every database interaction goes through this module. This keeps SQL
// contained in one place and gives the rest of the app clean Rust interfaces.
//
// All query functions take a `user_did` parameter to scope data to a specific
// protected user. This enables multi-user support where each user's threat
// data is isolated.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use super::models::{
    AccountScore, AccuracyMetrics, AmplificationEvent, InferredPair, NewAmplificationEvent,
    ThreatTier, ToxicPost, UserLabel, UserRow,
};
use super::traits::{ScanClaim, ScanQueueDepth, ScanQueueEntry, ScanQueueRow, ScanSkip};

// --- Users ---

/// Create or update a user record.
pub fn upsert_user(conn: &Connection, did: &str, handle: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO users (did, handle) VALUES (?1, ?2)
         ON CONFLICT(did) DO UPDATE SET handle = ?2",
        params![did, handle],
    )?;
    Ok(())
}

/// Look up a user's handle by DID. Returns None if the user is not registered.
pub fn get_user_handle(conn: &Connection, did: &str) -> Result<Option<String>> {
    let handle = conn
        .query_row(
            "SELECT handle FROM users WHERE did = ?1",
            params![did],
            |row| row.get(0),
        )
        .optional()?;
    Ok(handle)
}

// --- Scan state ---

/// Get a scan state value by key (e.g., "notifications_cursor") for a specific user.
pub fn get_scan_state(conn: &Connection, user_did: &str, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM scan_state WHERE user_did = ?1 AND key = ?2")?;
    let result = stmt
        .query_row(params![user_did, key], |row| row.get(0))
        .optional()?;
    Ok(result)
}

/// Get all scan state key-value pairs for a specific user.
pub fn get_all_scan_state(conn: &Connection, user_did: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("SELECT key, value FROM scan_state WHERE user_did = ?1")?;
    let rows = stmt.query_map(params![user_did], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Set a scan state value (upsert) for a specific user.
pub fn set_scan_state(conn: &Connection, user_did: &str, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO scan_state (user_did, key, value, updated_at)
         VALUES (?1, ?2, ?3, datetime('now'))
         ON CONFLICT(user_did, key) DO UPDATE SET value = ?3, updated_at = datetime('now')",
        params![user_did, key, value],
    )?;
    Ok(())
}

// --- Topic fingerprint ---

/// Store the topic fingerprint for a specific user.
pub fn save_fingerprint(
    conn: &Connection,
    user_did: &str,
    fingerprint_json: &str,
    post_count: u32,
) -> Result<()> {
    conn.execute(
        "INSERT INTO topic_fingerprint (user_did, fingerprint_json, post_count, updated_at)
         VALUES (?1, ?2, ?3, datetime('now'))
         ON CONFLICT(user_did) DO UPDATE SET
            fingerprint_json = ?2,
            post_count = ?3,
            updated_at = datetime('now')",
        params![user_did, fingerprint_json, post_count],
    )?;
    Ok(())
}

/// Store the protected user's mean embedding vector alongside the fingerprint.
/// The vector is stored as a JSON array of floats.
///
/// Returns an error if no fingerprint row exists for this user (i.e.,
/// `charcoal fingerprint` has not been run yet). An UPDATE that matches zero
/// rows would silently succeed otherwise, losing the embedding.
pub fn save_embedding(conn: &Connection, user_did: &str, embedding_json: &str) -> Result<()> {
    let rows = conn.execute(
        "UPDATE topic_fingerprint SET embedding_vector = ?1, updated_at = datetime('now') WHERE user_did = ?2",
        params![embedding_json, user_did],
    )?;
    if rows == 0 {
        anyhow::bail!(
            "save_embedding: no fingerprint row found — run `charcoal fingerprint` first"
        );
    }
    Ok(())
}

/// Load the stored fingerprint JSON and metadata for a specific user.
pub fn get_fingerprint(conn: &Connection, user_did: &str) -> Result<Option<(String, u32, String)>> {
    let mut stmt = conn.prepare(
        "SELECT fingerprint_json, post_count, updated_at FROM topic_fingerprint WHERE user_did = ?1",
    )?;
    let result = stmt
        .query_row(params![user_did], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    Ok(result)
}

/// Load the stored embedding vector for a specific user (if one exists).
pub fn get_embedding(conn: &Connection, user_did: &str) -> Result<Option<Vec<f64>>> {
    let mut stmt =
        conn.prepare("SELECT embedding_vector FROM topic_fingerprint WHERE user_did = ?1")?;
    let result: Option<Option<String>> = stmt
        .query_row(params![user_did], |row| row.get(0))
        .optional()?;

    match result.flatten() {
        Some(json) => {
            let vec: Vec<f64> = serde_json::from_str(&json)?;
            Ok(Some(vec))
        }
        None => Ok(None),
    }
}

/// Persist a complete fingerprint generation in one transaction (#302).
/// `unchecked_transaction` for the same reason as delete_user_data: these
/// free functions take &Connection, and rusqlite's transaction() needs &mut.
pub fn save_fingerprint_bundle(
    conn: &Connection,
    user_did: &str,
    fingerprint_json: &str,
    post_count: u32,
    embedding: Option<&str>, // pre-serialized JSON array, like save_embedding
    clusters: &[crate::db::models::ClusterCentroid],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO topic_fingerprint (user_did, fingerprint_json, post_count, embedding_vector, updated_at)
         VALUES (?1, ?2, ?3, ?4, datetime('now'))
         ON CONFLICT(user_did) DO UPDATE SET
            fingerprint_json = ?2,
            post_count = ?3,
            embedding_vector = ?4,
            updated_at = datetime('now')",
        params![user_did, fingerprint_json, post_count, embedding],
    )?;
    tx.execute(
        "DELETE FROM topic_clusters WHERE user_did = ?1",
        params![user_did],
    )?;
    for (i, cluster) in clusters.iter().enumerate() {
        let centroid_json = serde_json::to_string(&cluster.centroid)?;
        tx.execute(
            "INSERT INTO topic_clusters (user_did, cluster_index, centroid, post_count)
             VALUES (?1, ?2, ?3, ?4)",
            params![user_did, i as i64, centroid_json, cluster.post_count],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Load topic centroids in cluster_index order. Empty = legacy/keyword-only.
pub fn get_topic_centroids(
    conn: &Connection,
    user_did: &str,
) -> Result<Vec<crate::db::models::ClusterCentroid>> {
    let mut stmt = conn.prepare(
        "SELECT centroid, post_count FROM topic_clusters
         WHERE user_did = ?1 ORDER BY cluster_index",
    )?;
    let rows = stmt.query_map(params![user_did], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (json, post_count) = row?;
        out.push(crate::db::models::ClusterCentroid {
            centroid: serde_json::from_str(&json)?,
            post_count,
        });
    }
    Ok(out)
}

// --- Account scores ---

/// Save or update an account's scores for a specific user.
pub fn upsert_account_score(conn: &Connection, user_did: &str, score: &AccountScore) -> Result<()> {
    let top_posts_json = serde_json::to_string(&score.top_toxic_posts)?;
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, toxicity_score, topic_overlap, threat_score, threat_tier, posts_analyzed, top_toxic_posts, scored_at, behavioral_signals, context_score, graph_distance, fingerprint_quality, scoring_confidence, overlap_legacy)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, datetime('now'), ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(user_did, did) DO UPDATE SET
            handle = ?3,
            toxicity_score = ?4,
            topic_overlap = ?5,
            threat_score = ?6,
            threat_tier = ?7,
            posts_analyzed = ?8,
            top_toxic_posts = ?9,
            scored_at = datetime('now'),
            behavioral_signals = ?10,
            context_score = ?11,
            graph_distance = ?12,
            fingerprint_quality = ?13,
            scoring_confidence = ?14,
            overlap_legacy = ?15",
        params![
            user_did,
            score.did,
            score.handle,
            score.toxicity_score,
            score.topic_overlap,
            score.threat_score,
            score.threat_tier,
            score.posts_analyzed,
            top_posts_json,
            score.behavioral_signals,
            score.context_score,
            score.graph_distance,
            score.fingerprint_quality,
            score.scoring_confidence,
            score.overlap_legacy,
        ],
    )?;
    Ok(())
}

/// Get all scored accounts for a specific user, ranked by threat score descending.
pub fn get_ranked_threats(
    conn: &Connection,
    user_did: &str,
    min_score: f64,
) -> Result<Vec<AccountScore>> {
    let mut stmt = conn.prepare(
        "SELECT did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
                posts_analyzed, top_toxic_posts, scored_at, behavioral_signals,
                graph_distance, fingerprint_quality, scoring_confidence, context_score,
                overlap_legacy
         FROM account_scores
         WHERE user_did = ?1 AND threat_score >= ?2
         ORDER BY threat_score DESC",
    )?;

    let rows = stmt.query_map(params![user_did, min_score], |row| {
        let top_posts_json: String = row.get(7)?;
        let top_toxic_posts: Vec<ToxicPost> =
            serde_json::from_str(&top_posts_json).unwrap_or_default();
        // Recalculate tier from stored score so threshold changes
        // take effect without rescanning.
        let threat_score: Option<f64> = row.get(4)?;
        // Preserve a stored NotAssessed tier (#222) instead of recomputing —
        // a NotAssessed row has a NULL score, which would otherwise resolve
        // to no tier at all (or "Low" for a real 0.0 score).
        let stored_tier: Option<String> = row.get(5)?;
        let threat_tier = match stored_tier.as_deref() {
            Some(s) if s == ThreatTier::NotAssessed.as_str() => Some(s.to_string()),
            _ => threat_score.map(|s| ThreatTier::from_score(s).to_string()),
        };
        Ok(AccountScore {
            did: row.get(0)?,
            handle: row.get(1)?,
            toxicity_score: row.get(2)?,
            topic_overlap: row.get(3)?,
            threat_score,
            threat_tier,
            posts_analyzed: row.get(6)?,
            top_toxic_posts,
            scored_at: row.get(8)?,
            behavioral_signals: row.get(9)?,
            context_score: row.get(13)?,
            graph_distance: row.get(10)?,
            fingerprint_quality: row.get(11)?,
            scoring_confidence: row.get(12)?,
            overlap_legacy: row.get(14)?,
        })
    })?;

    let mut accounts = Vec::new();
    for row in rows {
        accounts.push(row?);
    }
    Ok(accounts)
}

/// Check if an account's score is stale (older than the given number of days) for a specific user.
pub fn is_score_stale(
    conn: &Connection,
    user_did: &str,
    did: &str,
    max_age_days: i64,
) -> Result<bool> {
    let mut stmt =
        conn.prepare("SELECT scored_at FROM account_scores WHERE user_did = ?1 AND did = ?2")?;
    let result: Option<String> = stmt
        .query_row(params![user_did, did], |row| row.get(0))
        .optional()?;

    match result {
        None => Ok(true), // No score exists — treat as stale
        Some(scored_at) => {
            // Compare against current time minus max_age_days
            let stale: bool = conn.query_row(
                "SELECT datetime(?1) < datetime('now', ?2)",
                params![scored_at, format!("-{max_age_days} days")],
                |row| row.get(0),
            )?;
            Ok(stale)
        }
    }
}

/// Return the DIDs the user has scored within the last `max_age_days` — i.e.
/// the accounts a per-candidate `is_score_stale` check would call *fresh*
/// (row exists AND `scored_at >= now - max_age_days`).
///
/// Discovery loops fetch this set once and test membership in memory instead
/// of issuing one `is_score_stale` round-trip per candidate (#213). The cutoff
/// MUST match `is_score_stale` exactly, or candidates get silently re-scored or
/// skipped; `fresh_set_is_exactly_the_non_stale_dids` guards that.
pub fn get_fresh_scored_dids(
    conn: &Connection,
    user_did: &str,
    max_age_days: i64,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT did FROM account_scores
         WHERE user_did = ?1 AND datetime(scored_at) >= datetime('now', ?2)",
    )?;
    let dids = stmt
        .query_map(params![user_did, format!("-{max_age_days} days")], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(dids)
}

// --- Amplification events ---

/// Insert an amplification event with an explicit detected_at timestamp for a specific user.
/// Used by the migrate command to preserve original event timestamps.
pub fn insert_amplification_event_with_detected_at(
    conn: &Connection,
    user_did: &str,
    event: &super::models::AmplificationEvent,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO amplification_events
            (user_did, event_type, amplifier_did, amplifier_handle, original_post_uri,
             amplifier_post_uri, amplifier_text, detected_at, original_post_text, context_score)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            user_did,
            event.event_type,
            event.amplifier_did,
            event.amplifier_handle,
            event.original_post_uri,
            event.amplifier_post_uri,
            event.amplifier_text,
            event.detected_at,
            event.original_post_text,
            event.context_score,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Record a new amplification event for a specific user.
#[allow(clippy::too_many_arguments)]
pub fn insert_amplification_event(
    conn: &Connection,
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
    conn.execute(
        "INSERT INTO amplification_events
            (user_did, event_type, amplifier_did, amplifier_handle, original_post_uri,
             amplifier_post_uri, amplifier_text, original_post_text, context_score)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            user_did,
            event_type,
            amplifier_did,
            amplifier_handle,
            original_post_uri,
            amplifier_post_uri,
            amplifier_text,
            original_post_text,
            context_score,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Insert many amplification events in one transaction, batched into
/// multi-row `INSERT` statements.
///
/// SQLite binds a maximum of 999 parameters per statement on older builds and
/// each row uses 9, so rows are chunked at 100 (999/9 = 111, minus headroom).
/// All chunks share one transaction, so the batch is atomic and ids ascend in
/// slice order.
pub fn insert_amplification_events_batch(
    conn: &Connection,
    user_did: &str,
    events: &[NewAmplificationEvent],
) -> Result<usize> {
    if events.is_empty() {
        return Ok(0);
    }

    const ROWS_PER_STATEMENT: usize = 100;
    const COLS: usize = 9;

    let tx = conn.unchecked_transaction()?;
    let mut inserted = 0usize;

    for chunk in events.chunks(ROWS_PER_STATEMENT) {
        // Build "(?1,?2,...,?9),(?10,...)" with 1-based positional parameters.
        let placeholders: Vec<String> = (0..chunk.len())
            .map(|row| {
                let base = row * COLS;
                let slots: Vec<String> = (1..=COLS).map(|c| format!("?{}", base + c)).collect();
                format!("({})", slots.join(","))
            })
            .collect();

        let sql = format!(
            "INSERT INTO amplification_events
                (user_did, event_type, amplifier_did, amplifier_handle, original_post_uri,
                 amplifier_post_uri, amplifier_text, original_post_text, context_score)
             VALUES {}",
            placeholders.join(",")
        );

        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(chunk.len() * COLS);
        for e in chunk {
            params.push(Box::new(user_did.to_string()));
            params.push(Box::new(e.event_type.clone()));
            params.push(Box::new(e.amplifier_did.clone()));
            params.push(Box::new(e.amplifier_handle.clone()));
            params.push(Box::new(e.original_post_uri.clone()));
            params.push(Box::new(e.amplifier_post_uri.clone()));
            params.push(Box::new(e.amplifier_text.clone()));
            params.push(Box::new(e.original_post_text.clone()));
            params.push(Box::new(e.context_score));
        }

        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        inserted += tx.execute(&sql, param_refs.as_slice())?;
    }

    tx.commit()?;
    Ok(inserted)
}

/// Get recent amplification events for a specific user.
pub fn get_recent_events(
    conn: &Connection,
    user_did: &str,
    limit: u32,
) -> Result<Vec<AmplificationEvent>> {
    // Batched inserts (insert_amplification_events_batch) give every row in a
    // batch the SAME detected_at timestamp, so `ORDER BY detected_at DESC`
    // alone leaves same-batch rows in an arbitrary order. `id DESC` breaks the
    // tie deterministically — ids ascend in insertion order, so this keeps
    // "newest first" stable within a batch, not just across batches.
    let mut stmt = conn.prepare(
        "SELECT id, event_type, amplifier_did, amplifier_handle, original_post_uri,
                amplifier_post_uri, amplifier_text, detected_at, followers_fetched, followers_scored,
                original_post_text, context_score
         FROM amplification_events
         WHERE user_did = ?1
         ORDER BY detected_at DESC, id DESC
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(params![user_did, limit], |row| {
        Ok(AmplificationEvent {
            id: row.get(0)?,
            event_type: row.get(1)?,
            amplifier_did: row.get(2)?,
            amplifier_handle: row.get(3)?,
            original_post_uri: row.get(4)?,
            amplifier_post_uri: row.get(5)?,
            amplifier_text: row.get(6)?,
            detected_at: row.get(7)?,
            followers_fetched: row.get::<_, i32>(8)? != 0,
            followers_scored: row.get::<_, i32>(9)? != 0,
            original_post_text: row.get(10)?,
            context_score: row.get(11)?,
        })
    })?;

    let mut events = Vec::new();
    for row in rows {
        events.push(row?);
    }
    Ok(events)
}

/// Get amplification events for pile-on detection for a specific user.
/// Returns (amplifier_did, original_post_uri, detected_at) tuples.
pub fn get_events_for_pile_on(
    conn: &Connection,
    user_did: &str,
) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT amplifier_did, original_post_uri, detected_at
         FROM amplification_events
         WHERE user_did = ?1
         ORDER BY original_post_uri, detected_at",
    )?;

    let rows = stmt.query_map(params![user_did], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;

    let mut events = Vec::new();
    for row in rows {
        events.push(row?);
    }
    Ok(events)
}

/// Get all amplification events for a specific amplifier DID.
pub fn get_events_by_amplifier(
    conn: &Connection,
    user_did: &str,
    amplifier_did: &str,
) -> Result<Vec<AmplificationEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, event_type, amplifier_did, amplifier_handle, original_post_uri,
                amplifier_post_uri, amplifier_text, detected_at, followers_fetched,
                followers_scored, original_post_text, context_score
         FROM amplification_events
         WHERE user_did = ?1 AND amplifier_did = ?2
         ORDER BY detected_at DESC, id DESC",
    )?;
    let rows = stmt.query_map(params![user_did, amplifier_did], |row| {
        Ok(AmplificationEvent {
            id: row.get(0)?,
            event_type: row.get(1)?,
            amplifier_did: row.get(2)?,
            amplifier_handle: row.get(3)?,
            original_post_uri: row.get(4)?,
            amplifier_post_uri: row.get(5)?,
            amplifier_text: row.get(6)?,
            detected_at: row.get(7)?,
            followers_fetched: row.get::<_, i32>(8)? != 0,
            followers_scored: row.get::<_, i32>(9)? != 0,
            original_post_text: row.get(10)?,
            context_score: row.get(11)?,
        })
    })?;

    let mut events = Vec::new();
    for row in rows {
        events.push(row?);
    }
    Ok(events)
}

/// Get the median engagement across all scored accounts with behavioral data for a specific user.
pub fn get_median_engagement(conn: &Connection, user_did: &str) -> Result<f64> {
    let mut stmt = conn.prepare(
        "SELECT behavioral_signals FROM account_scores WHERE user_did = ?1 AND behavioral_signals IS NOT NULL",
    )?;
    let mut engagements: Vec<f64> = stmt
        .query_map(params![user_did], |row| {
            let json: String = row.get(0)?;
            Ok(json)
        })?
        .filter_map(|r| r.ok())
        .filter_map(|json| {
            serde_json::from_str::<serde_json::Value>(&json)
                .ok()
                .and_then(|v| v.get("avg_engagement")?.as_f64())
        })
        .collect();

    if engagements.is_empty() {
        return Ok(0.0);
    }

    engagements.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = engagements.len() / 2;
    if engagements.len().is_multiple_of(2) {
        Ok((engagements[mid - 1] + engagements[mid]) / 2.0)
    } else {
        Ok(engagements[mid])
    }
}

/// Get a single account score by exact handle (case-insensitive) for a specific user.
pub fn get_account_by_handle(
    conn: &Connection,
    user_did: &str,
    handle: &str,
) -> Result<Option<AccountScore>> {
    let mut stmt = conn.prepare(
        "SELECT did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
                posts_analyzed, top_toxic_posts, scored_at, behavioral_signals,
                fingerprint_quality, scoring_confidence, context_score, graph_distance,
                overlap_legacy
         FROM account_scores
         WHERE user_did = ?1 AND lower(handle) = lower(?2)
         LIMIT 1",
    )?;
    let result = stmt
        .query_row(params![user_did, handle], |row| {
            let top_posts_json: String = row.get(7)?;
            let top_toxic_posts: Vec<ToxicPost> =
                serde_json::from_str(&top_posts_json).unwrap_or_default();
            let threat_score: Option<f64> = row.get(4)?;
            // Preserve a stored NotAssessed tier (#222) instead of
            // recomputing from the (NULL) score.
            let stored_tier: Option<String> = row.get(5)?;
            let threat_tier = match stored_tier.as_deref() {
                Some(s) if s == ThreatTier::NotAssessed.as_str() => Some(s.to_string()),
                _ => threat_score.map(|s| ThreatTier::from_score(s).to_string()),
            };
            Ok(AccountScore {
                did: row.get(0)?,
                handle: row.get(1)?,
                toxicity_score: row.get(2)?,
                topic_overlap: row.get(3)?,
                threat_score,
                threat_tier,
                posts_analyzed: row.get(6)?,
                top_toxic_posts,
                scored_at: row.get(8)?,
                behavioral_signals: row.get(9)?,
                context_score: row.get(12)?,
                graph_distance: row.get(13)?,
                fingerprint_quality: row.get(10)?,
                scoring_confidence: row.get(11)?,
                overlap_legacy: row.get(14)?,
            })
        })
        .optional()?;
    Ok(result)
}

/// Get a single account score by DID for a specific user.
pub fn get_account_by_did(
    conn: &Connection,
    user_did: &str,
    did: &str,
) -> Result<Option<AccountScore>> {
    let mut stmt = conn.prepare(
        "SELECT did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
                posts_analyzed, top_toxic_posts, scored_at, behavioral_signals,
                fingerprint_quality, scoring_confidence, context_score, graph_distance,
                overlap_legacy
         FROM account_scores
         WHERE user_did = ?1 AND did = ?2
         LIMIT 1",
    )?;
    let result = stmt
        .query_row(params![user_did, did], |row| {
            let top_posts_json: String = row.get(7)?;
            let top_toxic_posts: Vec<ToxicPost> =
                serde_json::from_str(&top_posts_json).unwrap_or_default();
            let threat_score: Option<f64> = row.get(4)?;
            // Preserve a stored NotAssessed tier (#222) instead of
            // recomputing from the (NULL) score.
            let stored_tier: Option<String> = row.get(5)?;
            let threat_tier = match stored_tier.as_deref() {
                Some(s) if s == ThreatTier::NotAssessed.as_str() => Some(s.to_string()),
                _ => threat_score.map(|s| ThreatTier::from_score(s).to_string()),
            };
            Ok(AccountScore {
                did: row.get(0)?,
                handle: row.get(1)?,
                toxicity_score: row.get(2)?,
                topic_overlap: row.get(3)?,
                threat_score,
                threat_tier,
                posts_analyzed: row.get(6)?,
                top_toxic_posts,
                scored_at: row.get(8)?,
                behavioral_signals: row.get(9)?,
                context_score: row.get(12)?,
                graph_distance: row.get(13)?,
                fingerprint_quality: row.get(10)?,
                scoring_confidence: row.get(11)?,
                overlap_legacy: row.get(14)?,
            })
        })
        .optional()?;
    Ok(result)
}

// --- User labels ---

/// Create or update a user-provided label for a target account.
pub fn upsert_user_label(
    conn: &Connection,
    user_did: &str,
    target_did: &str,
    label: &str,
    notes: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO user_labels (user_did, target_did, label, labeled_at, notes)
         VALUES (?1, ?2, ?3, datetime('now'), ?4)
         ON CONFLICT(user_did, target_did) DO UPDATE SET
            label = ?3, labeled_at = datetime('now'), notes = ?4",
        params![user_did, target_did, label, notes],
    )?;
    Ok(())
}

/// Get the user-provided label for a target account, if one exists.
pub fn get_user_label(
    conn: &Connection,
    user_did: &str,
    target_did: &str,
) -> Result<Option<UserLabel>> {
    let mut stmt = conn.prepare(
        "SELECT user_did, target_did, label, labeled_at, notes
         FROM user_labels
         WHERE user_did = ?1 AND target_did = ?2",
    )?;
    let result = stmt
        .query_row(params![user_did, target_did], |row| {
            Ok(UserLabel {
                user_did: row.get(0)?,
                target_did: row.get(1)?,
                label: row.get(2)?,
                labeled_at: row.get(3)?,
                notes: row.get(4)?,
            })
        })
        .optional()?;
    Ok(result)
}

/// Get scored accounts that have no user label, sorted by threat_score DESC.
pub fn get_unlabeled_accounts(
    conn: &Connection,
    user_did: &str,
    limit: i64,
) -> Result<Vec<AccountScore>> {
    let mut stmt = conn.prepare(
        "SELECT a.did, a.handle, a.toxicity_score, a.topic_overlap, a.threat_score, a.threat_tier,
                a.posts_analyzed, a.top_toxic_posts, a.scored_at, a.behavioral_signals,
                a.context_score, a.fingerprint_quality, a.scoring_confidence,
                a.overlap_legacy
         FROM account_scores a
         LEFT JOIN user_labels ul ON a.user_did = ul.user_did AND a.did = ul.target_did
         WHERE a.user_did = ?1 AND ul.target_did IS NULL AND a.threat_score IS NOT NULL
         ORDER BY a.threat_score DESC
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(params![user_did, limit], |row| {
        let top_posts_json: String = row.get(7)?;
        let top_toxic_posts: Vec<ToxicPost> =
            serde_json::from_str(&top_posts_json).unwrap_or_default();
        let threat_score: Option<f64> = row.get(4)?;
        // Preserve a stored NotAssessed tier (#222) instead of recomputing
        // from the (NULL) score.
        let stored_tier: Option<String> = row.get(5)?;
        let threat_tier = match stored_tier.as_deref() {
            Some(s) if s == ThreatTier::NotAssessed.as_str() => Some(s.to_string()),
            _ => threat_score.map(|s| ThreatTier::from_score(s).to_string()),
        };
        Ok(AccountScore {
            did: row.get(0)?,
            handle: row.get(1)?,
            toxicity_score: row.get(2)?,
            topic_overlap: row.get(3)?,
            threat_score,
            threat_tier,
            posts_analyzed: row.get(6)?,
            top_toxic_posts,
            scored_at: row.get(8)?,
            behavioral_signals: row.get(9)?,
            context_score: row.get(10)?,
            graph_distance: None,
            fingerprint_quality: row.get(11)?,
            scoring_confidence: row.get(12)?,
            overlap_legacy: row.get(13)?,
        })
    })?;

    let mut accounts = Vec::new();
    for row in rows {
        accounts.push(row?);
    }
    Ok(accounts)
}

/// Compute accuracy metrics comparing predicted tiers to user labels.
///
/// Tier ordering for comparison: high=3, elevated=2, watch=1, low=0, safe=0.
pub fn get_accuracy_metrics(conn: &Connection, user_did: &str) -> Result<AccuracyMetrics> {
    // Helper to convert a tier/label string to a numeric rank for comparison.
    fn tier_rank(tier: &str) -> i64 {
        match tier {
            "high" => 3,
            "elevated" => 2,
            "watch" => 1,
            _ => 0, // "low" and "safe" both rank 0
        }
    }

    let mut stmt = conn.prepare(
        "SELECT lower(a.threat_tier), lower(ul.label)
         FROM user_labels ul
         INNER JOIN account_scores a ON a.user_did = ul.user_did AND a.did = ul.target_did
         WHERE ul.user_did = ?1",
    )?;

    let rows = stmt.query_map(params![user_did], |row| {
        let tier: String = row.get::<_, Option<String>>(0)?.unwrap_or_default();
        let label: String = row.get(1)?;
        Ok((tier, label))
    })?;

    let mut total_labeled: i64 = 0;
    let mut exact_matches: i64 = 0;
    let mut overscored: i64 = 0;
    let mut underscored: i64 = 0;

    for row in rows {
        let (tier, label) = row?;
        total_labeled += 1;

        let predicted = tier_rank(&tier);
        let actual = tier_rank(&label);

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

// --- Inferred pairs ---

/// Delete all inferred pairs for a target account (before re-inferring).
pub fn delete_inferred_pairs(conn: &Connection, user_did: &str, target_did: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM inferred_pairs WHERE user_did = ?1 AND target_did = ?2",
        params![user_did, target_did],
    )?;
    Ok(())
}

/// Insert a topic-matched post pair for NLI scoring.
#[allow(clippy::too_many_arguments)]
pub fn insert_inferred_pair(
    conn: &Connection,
    user_did: &str,
    target_did: &str,
    target_post_text: &str,
    target_post_uri: &str,
    user_post_text: &str,
    user_post_uri: &str,
    similarity: f64,
    context_score: Option<f64>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO inferred_pairs
            (user_did, target_did, target_post_text, target_post_uri,
             user_post_text, user_post_uri, similarity, context_score)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(user_did, target_did, target_post_uri, user_post_uri)
         DO UPDATE SET similarity = ?7, context_score = ?8",
        params![
            user_did,
            target_did,
            target_post_text,
            target_post_uri,
            user_post_text,
            user_post_uri,
            similarity,
            context_score,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get all inferred pairs for a target account.
pub fn get_inferred_pairs(
    conn: &Connection,
    user_did: &str,
    target_did: &str,
) -> Result<Vec<InferredPair>> {
    let mut stmt = conn.prepare(
        "SELECT id, user_did, target_did, target_post_text, target_post_uri,
                user_post_text, user_post_uri, similarity, context_score, created_at
         FROM inferred_pairs
         WHERE user_did = ?1 AND target_did = ?2
         ORDER BY similarity DESC",
    )?;

    let rows = stmt.query_map(params![user_did, target_did], |row| {
        Ok(InferredPair {
            id: row.get(0)?,
            user_did: row.get(1)?,
            target_did: row.get(2)?,
            target_post_text: row.get(3)?,
            target_post_uri: row.get(4)?,
            user_post_text: row.get(5)?,
            user_post_uri: row.get(6)?,
            similarity: row.get(7)?,
            context_score: row.get(8)?,
            created_at: row.get(9)?,
        })
    })?;

    let mut pairs = Vec::new();
    for row in rows {
        pairs.push(row?);
    }
    Ok(pairs)
}

// --- Admin dashboard ---

/// List all users in the system, ordered by creation date descending.
pub fn list_users(conn: &Connection) -> Result<Vec<UserRow>> {
    let mut stmt = conn.prepare(
        "SELECT did, handle, created_at, last_login_at FROM users ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(UserRow {
            did: row.get(0)?,
            handle: row.get(1)?,
            created_at: row.get(2)?,
            last_login_at: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Count scored accounts for a user (only those with a non-null threat_score).
pub fn get_scored_account_count(conn: &Connection, user_did: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM account_scores WHERE user_did = ?1 AND threat_score IS NOT NULL",
        params![user_did],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Check if a topic fingerprint exists for a user.
pub fn has_fingerprint(conn: &Connection, user_did: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM topic_fingerprint WHERE user_did = ?1",
        params![user_did],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Delete all data for a user (cascade across all user-scoped tables).
pub fn delete_user_data(conn: &Connection, user_did: &str) -> Result<()> {
    // One transaction for the whole sequence, matching the Postgres backend.
    // Without it, a failure partway through leaves the account half-deleted —
    // e.g. `scan_skips` cleared while the `users` row survives, or the reverse:
    // the account gone but its scanned-account DIDs and error text still on
    // disk. For a deletion path the partial outcome is the dangerous one, so it
    // has to be all-or-nothing.
    //
    // `unchecked_transaction` because this takes `&Connection`; rusqlite's
    // `transaction()` needs `&mut`, which would ripple through every caller for
    // no behavioural gain.
    let tx = conn.unchecked_transaction()?;

    // Staging tables first (#208) — they have no FK to the data tables, but
    // clearing them up front keeps the delete in dependency order and ensures
    // a user's queued classification work doesn't outlive the account itself.
    tx.execute(
        "DELETE FROM classification_queue WHERE user_did = ?1",
        params![user_did],
    )?;
    tx.execute(
        "DELETE FROM scan_account_input WHERE user_did = ?1",
        params![user_did],
    )?;
    // #234: scan_skips holds the user's DID, the DIDs of accounts scanned on
    // their behalf, and raw error text. It is user-scoped like everything else
    // here and must not outlive the account.
    tx.execute(
        "DELETE FROM scan_skips WHERE user_did = ?1",
        params![user_did],
    )?;
    // #257: scan_queue holds the user's admission state; a queued or running
    // row must not outlive the account.
    tx.execute(
        "DELETE FROM scan_queue WHERE user_did = ?1",
        params![user_did],
    )?;
    tx.execute(
        "DELETE FROM inferred_pairs WHERE user_did = ?1",
        params![user_did],
    )?;
    tx.execute(
        "DELETE FROM user_labels WHERE user_did = ?1",
        params![user_did],
    )?;
    tx.execute(
        "DELETE FROM amplification_events WHERE user_did = ?1",
        params![user_did],
    )?;
    tx.execute(
        "DELETE FROM account_scores WHERE user_did = ?1",
        params![user_did],
    )?;
    tx.execute(
        "DELETE FROM scan_state WHERE user_did = ?1",
        params![user_did],
    )?;
    tx.execute(
        "DELETE FROM topic_fingerprint WHERE user_did = ?1",
        params![user_did],
    )?;
    // #302: topic_clusters holds one row per centroid of the user's latest
    // fingerprint generation; it must not outlive the account either.
    tx.execute(
        "DELETE FROM topic_clusters WHERE user_did = ?1",
        params![user_did],
    )?;
    tx.execute("DELETE FROM users WHERE did = ?1", params![user_did])?;
    tx.commit()?;
    Ok(())
}

/// Update last_login_at timestamp for a user.
pub fn update_last_login(conn: &Connection, did: &str) -> Result<()> {
    conn.execute(
        "UPDATE users SET last_login_at = datetime('now') WHERE did = ?1",
        params![did],
    )?;
    Ok(())
}

/// Get all DIDs that have been scored for a user.
pub fn get_all_scored_dids(conn: &Connection, user_did: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT did FROM account_scores WHERE user_did = ?1")?;
    let dids = stmt
        .query_map(params![user_did], |row| row.get(0))?
        .collect::<std::result::Result<Vec<String>, _>>()?;
    Ok(dids)
}

// --- Classification staging (#208) ---

use crate::pipeline::scan_phases::staging::{QueueRow, VerdictRow};

/// Enqueue a batch of classifier work-queue rows for a user.
///
/// Uses UPSERT so Phase A is idempotent — the same `(user_did, account_did,
/// post_uri)` triple can be written twice and will result in a single row.
/// All fields (including pre-filled clean-pass verdicts) are written.
pub fn enqueue_classifications(conn: &Connection, user_did: &str, rows: &[QueueRow]) -> Result<()> {
    // Batch all inserts inside one transaction and one prepared statement to
    // avoid re-acquiring the Mutex for every row.
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO classification_queue
                 (user_did, account_did, post_uri, text, context_text,
                  post_kind, onnx_score, status,
                  toxic_token, confidence, model_id, policy_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(user_did, account_did, post_uri) DO UPDATE SET
                 text           = excluded.text,
                 context_text   = excluded.context_text,
                 post_kind      = excluded.post_kind,
                 onnx_score     = excluded.onnx_score,
                 status         = CASE WHEN classification_queue.status = 'done'
                                       THEN classification_queue.status
                                       ELSE excluded.status END,
                 toxic_token    = CASE WHEN classification_queue.status = 'done'
                                       THEN classification_queue.toxic_token
                                       ELSE excluded.toxic_token END,
                 confidence     = CASE WHEN classification_queue.status = 'done'
                                       THEN classification_queue.confidence
                                       ELSE excluded.confidence END,
                 model_id       = CASE WHEN classification_queue.status = 'done'
                                       THEN classification_queue.model_id
                                       ELSE excluded.model_id END,
                 policy_version = CASE WHEN classification_queue.status = 'done'
                                       THEN classification_queue.policy_version
                                       ELSE excluded.policy_version END",
        )?;
        for row in rows {
            let toxic_token_int: Option<i64> = row.toxic_token.map(|b| if b { 1 } else { 0 });
            let confidence_real: Option<f64> = row.confidence.map(f64::from);
            stmt.execute(params![
                user_did,
                row.account_did,
                row.post_uri,
                row.text,
                row.context_text,
                row.post_kind,
                row.onnx_score,
                row.status,
                toxic_token_int,
                confidence_real,
                row.model_id,
                row.policy_version,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Stash a serialised `AccountInput` blob for an account (UPSERT).
pub fn stash_account_input(
    conn: &Connection,
    user_did: &str,
    account_did: &str,
    payload_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO scan_account_input (user_did, account_did, payload_json)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(user_did, account_did) DO UPDATE SET payload_json = excluded.payload_json",
        params![user_did, account_did, payload_json],
    )?;
    Ok(())
}

/// Fetch up to `limit` pending queue rows for a user.
pub fn fetch_pending_classifications(
    conn: &Connection,
    user_did: &str,
    limit: i64,
) -> Result<Vec<QueueRow>> {
    let mut stmt = conn.prepare(
        "SELECT account_did, post_uri, text, context_text, post_kind,
                onnx_score, status, toxic_token, confidence, model_id, policy_version
         FROM classification_queue
         WHERE user_did = ?1 AND status = 'pending'
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![user_did, limit], map_queue_row)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Record a batch of completed classifier verdicts.
///
/// Updates each matching row to `status='done'` and fills in all verdict
/// fields.  All updates are batched inside a single transaction.
pub fn record_classification_verdicts(
    conn: &Connection,
    user_did: &str,
    verdicts: &[VerdictRow],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "UPDATE classification_queue
             SET status = 'done',
                 toxic_token    = ?1,
                 confidence     = ?2,
                 model_id       = ?3,
                 policy_version = ?4
             WHERE user_did = ?5 AND account_did = ?6 AND post_uri = ?7",
        )?;
        for v in verdicts {
            let toxic_token_int: i64 = if v.toxic_token { 1 } else { 0 };
            let confidence_real: f64 = f64::from(v.confidence);
            stmt.execute(params![
                toxic_token_int,
                confidence_real,
                v.model_id,
                v.policy_version,
                user_did,
                v.account_did,
                v.post_uri,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// List distinct `account_did` values in the classification queue for a user.
pub fn list_scan_accounts(conn: &Connection, user_did: &str) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT DISTINCT account_did FROM classification_queue WHERE user_did = ?1")?;
    let dids = stmt
        .query_map(params![user_did], |row| row.get(0))?
        .collect::<std::result::Result<Vec<String>, _>>()?;
    Ok(dids)
}

/// Fetch all queue rows (any status) for a specific account.
pub fn fetch_account_verdicts(
    conn: &Connection,
    user_did: &str,
    account_did: &str,
) -> Result<Vec<QueueRow>> {
    let mut stmt = conn.prepare(
        "SELECT account_did, post_uri, text, context_text, post_kind,
                onnx_score, status, toxic_token, confidence, model_id, policy_version
         FROM classification_queue
         WHERE user_did = ?1 AND account_did = ?2",
    )?;
    let rows = stmt.query_map(params![user_did, account_did], map_queue_row)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Retrieve the stashed `AccountInput` JSON blob for an account, if any.
pub fn fetch_account_input(
    conn: &Connection,
    user_did: &str,
    account_did: &str,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT payload_json FROM scan_account_input WHERE user_did = ?1 AND account_did = ?2",
    )?;
    let result = stmt
        .query_row(params![user_did, account_did], |row| row.get(0))
        .optional()?;
    Ok(result)
}

/// Count rows with `status='pending'` in the classification queue for a user.
pub fn count_pending_classifications(conn: &Connection, user_did: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM classification_queue WHERE user_did = ?1 AND status = 'pending'",
        params![user_did],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Delete all staging data for a user from `classification_queue` and
/// `scan_account_input`.  Does NOT touch `scan_state`.
pub fn clear_scan_staging(conn: &Connection, user_did: &str) -> Result<()> {
    // Both deletes in one transaction so a crash can't leave half-cleared
    // staging (mirrors the batched-write pattern used elsewhere in this file).
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM classification_queue WHERE user_did = ?1",
        params![user_did],
    )?;
    tx.execute(
        "DELETE FROM scan_account_input WHERE user_did = ?1",
        params![user_did],
    )?;
    tx.commit()?;
    Ok(())
}

/// Delete the staging data for a single account from `classification_queue` and
/// `scan_account_input`.  Does NOT touch `scan_state`.
pub fn clear_account_staging(conn: &Connection, user_did: &str, account_did: &str) -> Result<()> {
    // Both deletes in one transaction so a crash can't leave half-cleared
    // staging (mirrors the batched-write pattern used elsewhere in this file).
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM classification_queue WHERE user_did = ?1 AND account_did = ?2",
        params![user_did, account_did],
    )?;
    tx.execute(
        "DELETE FROM scan_account_input WHERE user_did = ?1 AND account_did = ?2",
        params![user_did, account_did],
    )?;
    tx.commit()?;
    Ok(())
}

// ── shared row-mapper ─────────────────────────────────────────────────────────

/// Map a `classification_queue` SELECT row into a `QueueRow`.
///
/// Expected column order (0-indexed):
///   0  account_did, 1  post_uri,    2  text,          3  context_text,
///   4  post_kind,   5  onnx_score,  6  status,
///   7  toxic_token (INTEGER nullable), 8  confidence (REAL nullable),
///   9  model_id,    10 policy_version
fn map_queue_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueueRow> {
    let toxic_int: Option<i64> = row.get(7)?;
    let confidence_real: Option<f64> = row.get(8)?;
    Ok(QueueRow {
        account_did: row.get(0)?,
        post_uri: row.get(1)?,
        text: row.get(2)?,
        context_text: row.get(3)?,
        post_kind: row.get(4)?,
        onnx_score: row.get(5)?,
        status: row.get(6)?,
        toxic_token: toxic_int.map(|i| i != 0),
        confidence: confidence_real.map(|f| f as f32),
        model_id: row.get(9)?,
        policy_version: row.get(10)?,
    })
}

// ── scan_skips (#226) ────────────────────────────────────────────────────────
//
// A skipped account is a real gap in a scan's coverage. Before this table the
// only evidence was a WARN log line, and Railway drops log messages by RATE
// (not severity) once a replica exceeds 500/sec — so those counts were a floor,
// not a total. See #220 for the diagnosis this would have made trivial.

/// Record (or update) one skipped account.
///
/// Upserts on (user_did, account_did, phase): a re-gather that fails again
/// refreshes the row instead of adding one, so the count keeps meaning
/// "accounts missing from this scan".
pub fn record_scan_skip(
    conn: &Connection,
    user_did: &str,
    account_did: &str,
    phase: &str,
    error: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO scan_skips (user_did, account_did, phase, error, skipped_at)
         VALUES (?1, ?2, ?3, ?4, datetime('now'))
         ON CONFLICT(user_did, account_did, phase) DO UPDATE SET
             error = excluded.error,
             skipped_at = excluded.skipped_at",
        params![user_did, account_did, phase, error],
    )?;
    Ok(())
}

/// Number of accounts skipped in the current scan for a user.
pub fn count_scan_skips(conn: &Connection, user_did: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM scan_skips WHERE user_did = ?1",
        params![user_did],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Count accounts whose `threat_tier` is `'NotAssessed'` for a user (#222).
///
/// `get_ranked_threats` filters on `threat_score >= ?`, which always excludes
/// NULL-score NotAssessed rows, so this can't be derived from that result
/// set — it needs its own query.
pub fn count_not_assessed(conn: &Connection, user_did: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM account_scores WHERE user_did = ?1 AND threat_tier = 'NotAssessed'",
        params![user_did],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// All skips recorded for a user, newest first.
pub fn list_scan_skips(conn: &Connection, user_did: &str) -> Result<Vec<ScanSkip>> {
    let mut stmt = conn.prepare(
        "SELECT account_did, phase, error, skipped_at
         FROM scan_skips WHERE user_did = ?1
         ORDER BY skipped_at DESC, account_did ASC",
    )?;
    let rows = stmt
        .query_map(params![user_did], |row| {
            Ok(ScanSkip {
                account_did: row.get(0)?,
                phase: row.get(1)?,
                error: row.get(2)?,
                skipped_at: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Delete a user's recorded skips. Called at gather entry so the count always
/// describes the current scan.
pub fn clear_scan_skips(conn: &Connection, user_did: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM scan_skips WHERE user_did = ?1",
        params![user_did],
    )?;
    Ok(())
}

// --- Scan admission queue (#257) ---
//
// This mirrors PgDatabase's semantics in src/db/postgres.rs, kept minimal
// because #263 deletes this backend entirely. Written to be removed, not
// maintained.
//
// On the admission race Postgres needs an advisory lock for: it cannot happen
// here as wired. `SqliteDatabase` holds one `Mutex<Connection>` and keeps the
// guard for the whole `claim_next_scan` call, so every admitter in the process
// is already single-file. Across processes the connection runs in WAL mode
// (see db::mod), where a DEFERRED transaction's count-then-update *could*
// over-admit, so the transaction below is BEGIN IMMEDIATE: the second admitter
// gets SQLITE_BUSY — a loud error — instead of quietly admitting past the cap.

/// Add a user to the scan queue. Idempotent — a second call while queued or
/// running is a no-op; a finished ('done'/'failed') row is reset so the user
/// can scan again. Mirrors PgDatabase::enqueue_scan.
pub fn enqueue_scan(conn: &Connection, user_did: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO scan_queue (user_did, status, enqueued_at)
         VALUES (?1, 'queued', ?2)
         ON CONFLICT(user_did) DO UPDATE SET
             status = 'queued', enqueued_at = ?2,
             started_at = NULL, finished_at = NULL,
             lease_expires = NULL, last_error = NULL,
             claim_id = NULL
         WHERE status IN ('done', 'failed')",
        params![user_did, now],
    )?;
    Ok(())
}

/// Claim the oldest queued scan if fewer than `limit` are running.
/// Mirrors PgDatabase::claim_next_scan.
pub fn claim_next_scan(
    conn: &Connection,
    limit: usize,
    lease_secs: i64,
) -> Result<Option<ScanClaim>> {
    // `new_unchecked` because this takes `&Connection`, matching the rest of
    // this module (see delete_user_data above for why). Immediate rather than
    // the default Deferred so the write lock is taken before the count.
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;

    let running: i64 = tx.query_row(
        "SELECT COUNT(*) FROM scan_queue WHERE status = 'running'",
        [],
        |row| row.get(0),
    )?;
    if running >= limit as i64 {
        tx.commit()?;
        return Ok(None);
    }

    // `(enqueued_at, user_did)`, not `enqueued_at` alone: RFC3339 TEXT ties
    // whenever two enqueues land in the same millisecond, and among tied rows
    // a bare `ORDER BY enqueued_at` falls back to rowid — so the row admitted
    // here would not be the row `list_scan_queue` displays as next. One total
    // order for display, position, and admission (#271).
    let did: Option<String> = tx
        .query_row(
            "SELECT user_did FROM scan_queue
             WHERE status = 'queued'
             ORDER BY enqueued_at, user_did
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;

    let Some(did) = did else {
        tx.commit()?;
        return Ok(None);
    };

    let started_at = chrono::Utc::now();
    let lease_expires = started_at + chrono::Duration::seconds(lease_secs);
    // randomblob(16) is SQLite's built-in CSPRNG — the fencing-token equivalent
    // of Postgres's gen_random_uuid(), with no new Rust dependency.
    let claim_id: String = tx.query_row(
        "UPDATE scan_queue
         SET status = 'running', started_at = ?2, lease_expires = ?3,
             claim_id = lower(hex(randomblob(16)))
         WHERE user_did = ?1
         RETURNING claim_id",
        params![did, started_at.to_rfc3339(), lease_expires.to_rfc3339()],
        |row| row.get(0),
    )?;

    tx.commit()?;
    Ok(Some(ScanClaim {
        user_did: did,
        claim_id,
    }))
}

/// Extend a running scan's lease, only if `claim_id` still owns the row.
/// Returns false when it does not. Mirrors PgDatabase::heartbeat_scan.
pub fn heartbeat_scan(
    conn: &Connection,
    user_did: &str,
    claim_id: &str,
    lease_secs: i64,
) -> Result<bool> {
    let lease_expires = chrono::Utc::now() + chrono::Duration::seconds(lease_secs);
    let changed = conn.execute(
        "UPDATE scan_queue SET lease_expires = ?3
         WHERE user_did = ?1 AND status = 'running' AND claim_id = ?2",
        params![user_did, claim_id, lease_expires.to_rfc3339()],
    )?;
    Ok(changed > 0)
}

/// Mark a scan done (error None) or failed (error Some), releasing its slot.
/// Only the holder of `claim_id` may do so — see PgDatabase::finish_queued_scan
/// for why. Returns false when nothing matched. Mirrors
/// PgDatabase::finish_queued_scan.
pub fn finish_queued_scan(
    conn: &Connection,
    user_did: &str,
    claim_id: &str,
    error: Option<&str>,
) -> Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let status = if error.is_none() { "done" } else { "failed" };
    let changed = conn.execute(
        "UPDATE scan_queue
         SET status = ?3, finished_at = ?4, lease_expires = NULL, last_error = ?5
         WHERE user_did = ?1 AND status = 'running' AND claim_id = ?2",
        params![user_did, claim_id, status, now, error],
    )?;
    Ok(changed > 0)
}

/// Return running rows whose lease has lapsed to 'queued'. Timestamps are
/// RFC3339 in UTC, so lexicographic comparison is correct. A running row with
/// a NULL lease is reclaimed too — otherwise nothing ever would, and the slot
/// would stay occupied forever. Mirrors PgDatabase::reclaim_expired_scans.
pub fn reclaim_expired_scans(conn: &Connection) -> Result<usize> {
    let now = chrono::Utc::now().to_rfc3339();
    let changed = conn.execute(
        "UPDATE scan_queue
         SET status = 'queued', started_at = NULL, lease_expires = NULL,
             claim_id = NULL
         WHERE status = 'running'
           AND (lease_expires IS NULL OR lease_expires < ?1)",
        params![now],
    )?;
    Ok(changed)
}

/// Count queued and running rows in one round-trip.
/// Mirrors PgDatabase::scan_queue_depth.
pub fn scan_queue_depth(conn: &Connection) -> Result<ScanQueueDepth> {
    let (queued, running): (i64, i64) = conn.query_row(
        "SELECT COUNT(*) FILTER (WHERE status = 'queued'),
                COUNT(*) FILTER (WHERE status = 'running')
         FROM scan_queue",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(ScanQueueDepth {
        queued: queued as usize,
        running: running as usize,
    })
}

/// Every scan_queue row, oldest first. Mirrors PgDatabase::list_scan_queue.
///
/// `position` is computed exactly as `scan_queue_entry` computes it — a
/// correlated COUNT over queued rows enqueued at-or-before this one, forced to
/// 0 for any non-queued status. Two formulas for one number is how the queue
/// panel and the per-user column start disagreeing.
///
/// `user_did` breaks ties on `enqueued_at` so the order is total: SQLite's
/// TEXT timestamps compare lexicographically, and two rows enqueued inside the
/// same millisecond would otherwise come back in whatever order the scan
/// happens to produce.
///
/// The position predicate uses the SAME pair — `(enqueued_at, user_did) <=
/// (…)`, a row-value comparison SQLite has supported since 3.15. Counting on
/// `enqueued_at <=` alone gave every tied row the same number while the
/// ORDER BY still rendered them 1st, 2nd, 3rd, so three rows each announced
/// they were "3rd in line" (#271).
pub fn list_scan_queue(conn: &Connection) -> Result<Vec<ScanQueueRow>> {
    let mut stmt = conn.prepare(
        "SELECT user_did, status, enqueued_at, started_at, finished_at, last_error,
                (SELECT COUNT(*) FROM scan_queue q2
                  WHERE q2.status = 'queued'
                    AND (q2.enqueued_at, q2.user_did) <= (q.enqueued_at, q.user_did))
         FROM scan_queue q
         ORDER BY q.enqueued_at ASC, q.user_did ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        let status: String = row.get(1)?;
        let raw_position: i64 = row.get(6)?;
        Ok(ScanQueueRow {
            user_did: row.get(0)?,
            position: if status == "queued" { raw_position } else { 0 },
            status,
            enqueued_at: row.get(2)?,
            started_at: row.get(3)?,
            finished_at: row.get(4)?,
            last_error: row.get(5)?,
        })
    })?;
    // Collected with `?` rather than `filter_map(ok)`: a row that fails to map
    // would otherwise vanish from an operator's view of the queue, which is
    // the exact blindness #288 is removing.
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// A user's queue entry with position and ETA, or None if the user has never
/// been enqueued. Mirrors PgDatabase::scan_queue_entry.
pub fn scan_queue_entry(
    conn: &Connection,
    user_did: &str,
    concurrency_limit: usize,
) -> Result<Option<ScanQueueEntry>> {
    // Same `(enqueued_at, user_did)` row-value predicate as `list_scan_queue`
    // and the same total order `claim_next_scan` admits by — see #271.
    let row: Option<(String, String, i64)> = conn
        .query_row(
            "SELECT status, enqueued_at,
                    (SELECT COUNT(*) FROM scan_queue q2
                      WHERE q2.status = 'queued'
                        AND (q2.enqueued_at, q2.user_did) <= (q.enqueued_at, q.user_did))
             FROM scan_queue q WHERE user_did = ?1",
            params![user_did],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;

    let Some((status, enqueued_at, raw_position)) = row else {
        return Ok(None);
    };
    let position = if status == "queued" { raw_position } else { 0 };

    // Rolling median over the last 20 completed scans. None until any finish,
    // so ETA is absent rather than fabricated on a fresh install.
    //
    // Seconds are kept FRACTIONAL, matching the Postgres backend's
    // `EXTRACT(EPOCH FROM (finished_at - started_at))`. This used to be
    // `num_seconds()`, which truncates, so the same scan history quoted a
    // different ETA either side of a backend switch. Truncating is not a
    // harmless rounding difference once `eta_seconds` multiplies the median by
    // the batch count: a 90.6s median eight batches out is 724s here and 720s
    // there. Rounding both would have to throw away precision Postgres already
    // has, so the SQLite side gains it instead.
    //
    // Errors propagate with `?` rather than being dropped by `filter_map(ok)`:
    // a corrupt row or an unparseable timestamp would otherwise silently shrink
    // the sample and skew the median instead of surfacing.
    let mut durations: Vec<f64> = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT started_at, finished_at FROM scan_queue
         WHERE status = 'done' AND started_at IS NOT NULL
         ORDER BY finished_at DESC LIMIT 20",
    )?;
    let rows = stmt.query_map([], |row| {
        let started_at: String = row.get(0)?;
        let finished_at: String = row.get(1)?;
        Ok((started_at, finished_at))
    })?;
    for row in rows {
        let (s, f) = row?;
        let s = chrono::DateTime::parse_from_rfc3339(&s)
            .with_context(|| format!("scan_queue.started_at is not RFC3339: {s}"))?;
        let f = chrono::DateTime::parse_from_rfc3339(&f)
            .with_context(|| format!("scan_queue.finished_at is not RFC3339: {f}"))?;
        durations.push((f - s).as_seconds_f64());
    }

    let median = if durations.is_empty() {
        None
    } else {
        // `total_cmp`, not `partial_cmp().unwrap()`: durations are finite by
        // construction, but a total order needs no unwrap to say so.
        durations.sort_by(f64::total_cmp);
        let mid = durations.len() / 2;
        // Averaging the two middle values on an even sample is what
        // PERCENTILE_CONT(0.5) does, so the backends agree here too.
        Some(if durations.len().is_multiple_of(2) {
            (durations[mid - 1] + durations[mid]) / 2.0
        } else {
            durations[mid]
        })
    };

    let eta_seconds = super::traits::eta_seconds(&status, position, concurrency_limit, median);

    Ok(Some(ScanQueueEntry {
        user_did: user_did.to_string(),
        status,
        position,
        eta_seconds,
        enqueued_at,
    }))
}

// --- Access requests (#309) ---

pub fn get_access_request(
    conn: &Connection,
    did: &str,
) -> Result<Option<crate::db::traits::AccessRequestRow>> {
    let mut stmt = conn.prepare(
        "SELECT did, handle, status, requested_at, decided_at, decided_by
         FROM access_requests WHERE did = ?1",
    )?;
    let row = stmt
        .query_row([did], |r| {
            Ok(crate::db::traits::AccessRequestRow {
                did: r.get(0)?,
                handle: r.get(1)?,
                status: r.get(2)?,
                requested_at: r.get(3)?,
                decided_at: r.get(4)?,
                decided_by: r.get(5)?,
            })
        })
        .optional()?;
    Ok(row)
}

pub fn upsert_access_request_pending(conn: &Connection, did: &str, handle: &str) -> Result<()> {
    // ON CONFLICT refreshes the handle ONLY: a denied row stays denied and an
    // allowed row stays allowed — sign-in attempts never move the state machine.
    conn.execute(
        "INSERT INTO access_requests (did, handle, status, requested_at)
         VALUES (?1, ?2, 'pending', ?3)
         ON CONFLICT (did) DO UPDATE SET handle = excluded.handle",
        rusqlite::params![did, handle, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

pub fn set_access_status(
    conn: &Connection,
    did: &str,
    status: &str,
    decided_by: &str,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE access_requests SET status = ?2, decided_at = ?3, decided_by = ?4
         WHERE did = ?1",
        rusqlite::params![did, status, chrono::Utc::now().to_rfc3339(), decided_by],
    )?;
    Ok(n > 0)
}

pub fn grant_access(conn: &Connection, did: &str, handle: &str, decided_by: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO access_requests (did, handle, status, requested_at, decided_at, decided_by)
         VALUES (?1, ?2, 'allowed', ?3, ?3, ?4)
         ON CONFLICT (did) DO UPDATE SET status = 'allowed', handle = excluded.handle,
             decided_at = excluded.decided_at, decided_by = excluded.decided_by",
        rusqlite::params![did, handle, now, decided_by],
    )?;
    Ok(())
}

pub fn list_access_requests(conn: &Connection) -> Result<Vec<crate::db::traits::AccessRequestRow>> {
    let mut stmt = conn.prepare(
        "SELECT did, handle, status, requested_at, decided_at, decided_by
         FROM access_requests ORDER BY requested_at ASC, did ASC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(crate::db::traits::AccessRequestRow {
                did: r.get(0)?,
                handle: r.get(1)?,
                status: r.get(2)?,
                requested_at: r.get(3)?,
                decided_at: r.get(4)?,
                decided_by: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::create_tables;

    const TEST_USER: &str = "did:plc:testuser000000000000";

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn test_upsert_user() {
        let conn = test_db();
        upsert_user(&conn, "did:plc:abc", "alice.bsky.social").unwrap();

        let handle: String = conn
            .query_row(
                "SELECT handle FROM users WHERE did = 'did:plc:abc'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(handle, "alice.bsky.social");

        // Update handle
        upsert_user(&conn, "did:plc:abc", "alice-new.bsky.social").unwrap();
        let handle: String = conn
            .query_row(
                "SELECT handle FROM users WHERE did = 'did:plc:abc'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(handle, "alice-new.bsky.social");
    }

    #[test]
    fn test_scan_state_roundtrip() {
        let conn = test_db();
        assert_eq!(get_scan_state(&conn, TEST_USER, "cursor").unwrap(), None);

        set_scan_state(&conn, TEST_USER, "cursor", "abc123").unwrap();
        assert_eq!(
            get_scan_state(&conn, TEST_USER, "cursor").unwrap(),
            Some("abc123".to_string())
        );

        // Upsert overwrites
        set_scan_state(&conn, TEST_USER, "cursor", "def456").unwrap();
        assert_eq!(
            get_scan_state(&conn, TEST_USER, "cursor").unwrap(),
            Some("def456".to_string())
        );
    }

    #[test]
    fn test_scan_state_user_isolation() {
        let conn = test_db();
        let user_a = "did:plc:usera";
        let user_b = "did:plc:userb";

        set_scan_state(&conn, user_a, "cursor", "aaa").unwrap();
        set_scan_state(&conn, user_b, "cursor", "bbb").unwrap();

        assert_eq!(
            get_scan_state(&conn, user_a, "cursor").unwrap(),
            Some("aaa".to_string())
        );
        assert_eq!(
            get_scan_state(&conn, user_b, "cursor").unwrap(),
            Some("bbb".to_string())
        );

        let all_a = get_all_scan_state(&conn, user_a).unwrap();
        assert_eq!(all_a.len(), 1);
        assert_eq!(all_a[0], ("cursor".to_string(), "aaa".to_string()));
    }

    #[test]
    fn test_fingerprint_roundtrip() {
        let conn = test_db();
        assert!(get_fingerprint(&conn, TEST_USER).unwrap().is_none());

        save_fingerprint(&conn, TEST_USER, r#"{"topics": []}"#, 100).unwrap();
        let (json, count, _updated) = get_fingerprint(&conn, TEST_USER).unwrap().unwrap();
        assert_eq!(json, r#"{"topics": []}"#);
        assert_eq!(count, 100);

        // Upsert replaces
        save_fingerprint(&conn, TEST_USER, r#"{"topics": ["a"]}"#, 200).unwrap();
        let (json, count, _) = get_fingerprint(&conn, TEST_USER).unwrap().unwrap();
        assert_eq!(json, r#"{"topics": ["a"]}"#);
        assert_eq!(count, 200);
    }

    #[test]
    fn test_account_score_upsert_and_rank() {
        let conn = test_db();

        let score = AccountScore {
            did: "did:plc:abc".to_string(),
            handle: "test.bsky.social".to_string(),
            toxicity_score: Some(0.8),
            topic_overlap: Some(0.3),
            overlap_legacy: None,
            threat_score: Some(65.0),
            threat_tier: Some("Elevated".to_string()),
            posts_analyzed: 20,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
            fingerprint_quality: None,
            scoring_confidence: None,
        };
        upsert_account_score(&conn, TEST_USER, &score).unwrap();

        let ranked = get_ranked_threats(&conn, TEST_USER, 0.0).unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].handle, "test.bsky.social");
        assert_eq!(ranked[0].threat_score, Some(65.0));
    }

    /// #222: a NotAssessed row (NULL score, tier "NotAssessed") must survive
    /// the read path's tier-recompute unchanged — it must never be recomputed
    /// to "Low" the way a real NULL/0.0 score would be.
    #[test]
    fn not_assessed_row_survives_read_recompute() {
        let conn = test_db();

        let score = AccountScore {
            did: "did:plc:notassessed".to_string(),
            handle: "unassessed.bsky.social".to_string(),
            toxicity_score: None,
            topic_overlap: None,
            overlap_legacy: None,
            threat_score: None,
            threat_tier: Some(ThreatTier::NotAssessed.as_str().to_string()),
            posts_analyzed: 5,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
            fingerprint_quality: None,
            scoring_confidence: None,
        };
        upsert_account_score(&conn, TEST_USER, &score).unwrap();

        // The write path must store SQL NULL, not coerce None to 0.0.
        let stored_score: Option<f64> = conn
            .query_row(
                "SELECT threat_score FROM account_scores WHERE user_did = ?1 AND did = ?2",
                params![TEST_USER, "did:plc:notassessed"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            stored_score, None,
            "threat_score must persist as NULL, not 0.0"
        );

        // get_account_by_did recomputes the tier from the stored score on
        // every read; it must preserve a stored NotAssessed tier instead.
        let fetched = get_account_by_did(&conn, TEST_USER, "did:plc:notassessed")
            .unwrap()
            .expect("account should be found");
        assert_eq!(fetched.threat_score, None);
        assert_eq!(
            fetched.threat_tier.as_deref(),
            Some(ThreatTier::NotAssessed.as_str()),
            "NotAssessed tier must not be recomputed to Low"
        );
        assert_ne!(fetched.threat_tier.as_deref(), Some("Low"));

        // Same guarantee via the handle lookup.
        let fetched_by_handle = get_account_by_handle(&conn, TEST_USER, "unassessed.bsky.social")
            .unwrap()
            .expect("account should be found by handle");
        assert_eq!(
            fetched_by_handle.threat_tier.as_deref(),
            Some(ThreatTier::NotAssessed.as_str())
        );
    }

    #[test]
    fn test_save_embedding_fails_without_fingerprint_row() {
        let conn = test_db();
        // No fingerprint row — save_embedding must return an error, not silently succeed
        let result = save_embedding(&conn, TEST_USER, r#"[0.1, 0.2]"#);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("no fingerprint row"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn test_embedding_roundtrip() {
        let conn = test_db();

        // No embedding initially
        assert!(get_embedding(&conn, TEST_USER).unwrap().is_none());

        // Must have a fingerprint row first (embedding is a column on it)
        save_fingerprint(&conn, TEST_USER, r#"{"clusters":[]}"#, 50).unwrap();

        // Still no embedding until explicitly saved
        assert!(get_embedding(&conn, TEST_USER).unwrap().is_none());

        // Save an embedding vector
        let embedding = vec![0.1, 0.2, 0.3, -0.5];
        let emb_json = serde_json::to_string(&embedding).unwrap();
        save_embedding(&conn, TEST_USER, &emb_json).unwrap();

        // Retrieve it
        let loaded = get_embedding(&conn, TEST_USER).unwrap().unwrap();
        assert_eq!(loaded.len(), 4);
        assert!((loaded[0] - 0.1).abs() < f64::EPSILON);
        assert!((loaded[3] - -0.5).abs() < f64::EPSILON);

        // Overwrite with a new embedding
        let new_embedding = vec![1.0, 2.0];
        let new_json = serde_json::to_string(&new_embedding).unwrap();
        save_embedding(&conn, TEST_USER, &new_json).unwrap();

        let reloaded = get_embedding(&conn, TEST_USER).unwrap().unwrap();
        assert_eq!(reloaded.len(), 2);
        assert!((reloaded[0] - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_embedding_survives_fingerprint_update() {
        let conn = test_db();
        save_fingerprint(&conn, TEST_USER, r#"{"clusters":[]}"#, 50).unwrap();

        let embedding = vec![0.1, 0.2, 0.3];
        save_embedding(
            &conn,
            TEST_USER,
            &serde_json::to_string(&embedding).unwrap(),
        )
        .unwrap();

        // Update the fingerprint — embedding should survive (different column)
        save_fingerprint(&conn, TEST_USER, r#"{"clusters":["new"]}"#, 100).unwrap();

        let loaded = get_embedding(&conn, TEST_USER).unwrap().unwrap();
        assert_eq!(loaded.len(), 3);
        assert!((loaded[0] - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn test_amplification_event() {
        let conn = test_db();

        let id = insert_amplification_event(
            &conn,
            TEST_USER,
            "quote",
            "did:plc:xyz",
            "troll.bsky.social",
            "at://did:plc:me/app.bsky.feed.post/abc",
            Some("at://did:plc:xyz/app.bsky.feed.post/def"),
            Some("lol look at this"),
            None,
            None,
        )
        .unwrap();
        assert!(id > 0);

        let events = get_recent_events(&conn, TEST_USER, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "quote");
        assert_eq!(events[0].amplifier_handle, "troll.bsky.social");
    }

    #[test]
    fn test_account_by_handle() {
        let conn = test_db();

        let score = AccountScore {
            did: "did:plc:abc".to_string(),
            handle: "Test.Bsky.Social".to_string(),
            toxicity_score: Some(0.5),
            topic_overlap: Some(0.2),
            overlap_legacy: None,
            threat_score: Some(30.0),
            threat_tier: Some("Watch".to_string()),
            posts_analyzed: 10,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
            fingerprint_quality: None,
            scoring_confidence: None,
        };
        upsert_account_score(&conn, TEST_USER, &score).unwrap();

        // Case-insensitive lookup
        let found = get_account_by_handle(&conn, TEST_USER, "test.bsky.social")
            .unwrap()
            .unwrap();
        assert_eq!(found.did, "did:plc:abc");

        // Not found for different user
        let not_found = get_account_by_handle(&conn, "did:plc:other", "test.bsky.social").unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn test_account_by_did() {
        let conn = test_db();

        let score = AccountScore {
            did: "did:plc:abc".to_string(),
            handle: "test.bsky.social".to_string(),
            toxicity_score: Some(0.5),
            topic_overlap: Some(0.2),
            overlap_legacy: None,
            threat_score: Some(30.0),
            threat_tier: Some("Watch".to_string()),
            posts_analyzed: 10,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
            fingerprint_quality: None,
            scoring_confidence: None,
        };
        upsert_account_score(&conn, TEST_USER, &score).unwrap();

        let found = get_account_by_did(&conn, TEST_USER, "did:plc:abc")
            .unwrap()
            .unwrap();
        assert_eq!(found.handle, "test.bsky.social");

        // Not found for different user
        let not_found = get_account_by_did(&conn, "did:plc:other", "did:plc:abc").unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn test_is_score_stale() {
        let conn = test_db();

        // No score — should be stale
        assert!(is_score_stale(&conn, TEST_USER, "did:plc:abc", 7).unwrap());

        let score = AccountScore {
            did: "did:plc:abc".to_string(),
            handle: "test.bsky.social".to_string(),
            toxicity_score: Some(0.5),
            topic_overlap: Some(0.2),
            overlap_legacy: None,
            threat_score: Some(30.0),
            threat_tier: Some("Watch".to_string()),
            posts_analyzed: 10,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
            fingerprint_quality: None,
            scoring_confidence: None,
        };
        upsert_account_score(&conn, TEST_USER, &score).unwrap();

        // Just scored — should not be stale
        assert!(!is_score_stale(&conn, TEST_USER, "did:plc:abc", 7).unwrap());
    }

    #[test]
    fn test_pile_on_events() {
        let conn = test_db();

        insert_amplification_event(
            &conn,
            TEST_USER,
            "quote",
            "did:plc:a",
            "a.bsky.social",
            "at://did:plc:me/app.bsky.feed.post/1",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        insert_amplification_event(
            &conn,
            TEST_USER,
            "quote",
            "did:plc:b",
            "b.bsky.social",
            "at://did:plc:me/app.bsky.feed.post/1",
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let events = get_events_for_pile_on(&conn, TEST_USER).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_median_engagement() {
        let conn = test_db();

        // No data — returns 0.0
        assert!((get_median_engagement(&conn, TEST_USER).unwrap() - 0.0).abs() < f64::EPSILON);

        // Insert accounts with behavioral signals
        for (i, eng) in [10.0, 20.0, 30.0].iter().enumerate() {
            let score = AccountScore {
                did: format!("did:plc:eng{i}"),
                handle: format!("eng{i}.bsky.social"),
                toxicity_score: Some(0.5),
                topic_overlap: Some(0.2),
                overlap_legacy: None,
                threat_score: Some(30.0),
                threat_tier: Some("Watch".to_string()),
                posts_analyzed: 10,
                top_toxic_posts: vec![],
                scored_at: String::new(),
                behavioral_signals: Some(format!(r#"{{"avg_engagement":{eng}}}"#)),
                context_score: None,
                graph_distance: None,
                fingerprint_quality: None,
                scoring_confidence: None,
            };
            upsert_account_score(&conn, TEST_USER, &score).unwrap();
        }

        let median = get_median_engagement(&conn, TEST_USER).unwrap();
        assert!((median - 20.0).abs() < f64::EPSILON);
    }

    // --- Scan admission queue (#257 / #270) ---
    //
    // A reviewer hand-verified these behaviors against the SQLite backend by
    // executing the statements directly and found no divergence from
    // Postgres (which has coverage in tests/db_postgres.rs). These tests
    // close the regression gap: nothing previously caught this backend
    // drifting from that behavior.

    const QUEUE_USER_A: &str = "did:plc:queuea0000000000000a";
    const QUEUE_USER_B: &str = "did:plc:queueb0000000000000b";
    const QUEUE_USER_C: &str = "did:plc:queuec0000000000000c";

    /// A double-click enqueue must not send the user to the back of the
    /// FIFO. `user_did` is the primary key, so a second `enqueue_scan` call
    /// always leaves exactly one row — asserting the row count cannot catch
    /// the `WHERE status IN ('done','failed')` guard being deleted. The
    /// timestamp is the only thing that can.
    #[test]
    fn enqueue_is_idempotent_and_preserves_position() {
        let conn = test_db();
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();
        let first = scan_queue_entry(&conn, QUEUE_USER_A, 1).unwrap().unwrap();

        // A real gap so a reset enqueued_at would be visibly different.
        std::thread::sleep(std::time::Duration::from_millis(20));
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();
        let second = scan_queue_entry(&conn, QUEUE_USER_A, 1).unwrap().unwrap();

        assert_eq!(second.status, "queued");
        assert_eq!(second.position, 1, "one row — not two");
        assert_eq!(
            second.enqueued_at, first.enqueued_at,
            "a re-enqueue while still queued must not move the user's place in line"
        );
    }

    #[test]
    fn claim_mints_a_token_and_persists_running_status() {
        let conn = test_db();
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();
        let claim = claim_next_scan(&conn, 1, 120).unwrap().expect("claim");
        assert_eq!(claim.user_did, QUEUE_USER_A);
        assert_eq!(claim.claim_id.len(), 32, "claim_id: {}", claim.claim_id);
        assert!(
            claim
                .claim_id
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "claim_id must be lowercase hex: {}",
            claim.claim_id
        );

        // Read back directly rather than trusting the return value.
        let (status, stored_claim_id): (String, String) = conn
            .query_row(
                "SELECT status, claim_id FROM scan_queue WHERE user_did = ?1",
                params![QUEUE_USER_A],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "running");
        assert_eq!(stored_claim_id, claim.claim_id);
    }

    /// A worker whose lease lapsed must not be able to free or extend the
    /// slot that was handed to someone else — the claim_id fencing token
    /// exists for exactly this.
    #[test]
    fn stale_claim_cannot_heartbeat_or_finish() {
        let conn = test_db();
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();

        // Worker A claims with an already-expired lease, gets reclaimed, and
        // worker B claims the freed row — the sequence a redeploy produces.
        let a = claim_next_scan(&conn, 2, -1).unwrap().expect("A claims");
        assert_eq!(reclaim_expired_scans(&conn).unwrap(), 1);
        let b = claim_next_scan(&conn, 2, 120).unwrap().expect("B claims");
        assert_ne!(a.claim_id, b.claim_id, "the reclaim must invalidate A");

        assert!(
            !heartbeat_scan(&conn, QUEUE_USER_A, &a.claim_id, 120).unwrap(),
            "a stale claim must not extend the new owner's lease"
        );
        assert!(
            !finish_queued_scan(&conn, QUEUE_USER_A, &a.claim_id, None).unwrap(),
            "a stale claim must not finish the new owner's scan"
        );
        assert_eq!(
            scan_queue_entry(&conn, QUEUE_USER_A, 1)
                .unwrap()
                .unwrap()
                .status,
            "running",
            "the row must still belong to B"
        );

        // B, holding the live token, succeeds on both surfaces.
        assert!(heartbeat_scan(&conn, QUEUE_USER_A, &b.claim_id, 120).unwrap());
        assert!(finish_queued_scan(&conn, QUEUE_USER_A, &b.claim_id, None).unwrap());
    }

    /// The admitter's only way to tell an idle queue from a wedged one —
    /// `claim_next_scan` returns `None` for both.
    #[test]
    fn depth_counts_queued_and_running_separately() {
        let conn = test_db();
        assert_eq!(
            scan_queue_depth(&conn).unwrap(),
            ScanQueueDepth {
                queued: 0,
                running: 0
            },
            "an empty table must report zero, not fail"
        );

        for did in [QUEUE_USER_A, QUEUE_USER_B, QUEUE_USER_C] {
            enqueue_scan(&conn, did).unwrap();
        }
        assert_eq!(
            scan_queue_depth(&conn).unwrap(),
            ScanQueueDepth {
                queued: 3,
                running: 0
            }
        );

        let claim = claim_next_scan(&conn, 1, 120).unwrap().expect("claim");
        assert_eq!(
            scan_queue_depth(&conn).unwrap(),
            ScanQueueDepth {
                queued: 2,
                running: 1
            },
            "a claim must move the row from queued to running, not double-count it"
        );

        // Finished rows are neither waiting nor holding a slot.
        finish_queued_scan(&conn, &claim.user_did, &claim.claim_id, None).unwrap();
        assert_eq!(
            scan_queue_depth(&conn).unwrap(),
            ScanQueueDepth {
                queued: 2,
                running: 0
            },
            "a finished row must not keep counting against the cap"
        );
    }

    #[test]
    fn double_finish_returns_false_second_time() {
        let conn = test_db();
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();
        let claim = claim_next_scan(&conn, 1, 120).unwrap().unwrap();
        assert!(finish_queued_scan(&conn, QUEUE_USER_A, &claim.claim_id, None).unwrap());
        assert!(
            !finish_queued_scan(&conn, QUEUE_USER_A, &claim.claim_id, None).unwrap(),
            "the row is no longer running, so a second finish must be a no-op"
        );
    }

    #[test]
    fn re_enqueue_after_done_resets_the_row() {
        let conn = test_db();
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();
        let claim = claim_next_scan(&conn, 1, 120).unwrap().unwrap();
        finish_queued_scan(&conn, QUEUE_USER_A, &claim.claim_id, None).unwrap();
        let done_at = scan_queue_entry(&conn, QUEUE_USER_A, 1)
            .unwrap()
            .unwrap()
            .enqueued_at;

        std::thread::sleep(std::time::Duration::from_millis(20));
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();

        let entry = scan_queue_entry(&conn, QUEUE_USER_A, 1).unwrap().unwrap();
        assert_eq!(entry.status, "queued");
        assert_ne!(
            entry.enqueued_at, done_at,
            "re-enqueue after done must set a fresh timestamp"
        );

        let claim_id: Option<String> = conn
            .query_row(
                "SELECT claim_id FROM scan_queue WHERE user_did = ?1",
                params![QUEUE_USER_A],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(claim_id, None, "claim_id must be cleared on re-enqueue");
    }

    /// A running row with a NULL lease is otherwise unrecoverable — nothing
    /// would ever reclaim it and the slot stays occupied forever.
    #[test]
    fn null_lease_is_reclaimed() {
        let conn = test_db();
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();
        claim_next_scan(&conn, 1, 120).unwrap().unwrap();

        // The state a crash between claim and heartbeat can leave behind.
        conn.execute(
            "UPDATE scan_queue SET lease_expires = NULL WHERE user_did = ?1",
            params![QUEUE_USER_A],
        )
        .unwrap();

        assert_eq!(
            reclaim_expired_scans(&conn).unwrap(),
            1,
            "a running row with a NULL lease must be reclaimed, not stranded"
        );
        assert_eq!(
            scan_queue_entry(&conn, QUEUE_USER_A, 1)
                .unwrap()
                .unwrap()
                .status,
            "queued"
        );
    }

    #[test]
    fn expired_lease_is_reclaimed_and_invalidates_the_claim_id() {
        let conn = test_db();
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();
        let claim = claim_next_scan(&conn, 1, -1).unwrap().unwrap();

        assert_eq!(reclaim_expired_scans(&conn).unwrap(), 1);
        assert_eq!(
            scan_queue_entry(&conn, QUEUE_USER_A, 1)
                .unwrap()
                .unwrap()
                .status,
            "queued"
        );

        let claim_id: Option<String> = conn
            .query_row(
                "SELECT claim_id FROM scan_queue WHERE user_did = ?1",
                params![QUEUE_USER_A],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            claim_id, None,
            "reclaim must null the claim_id so the previous holder is invalidated"
        );
        // The old token no longer holds anything, on either surface.
        assert!(!heartbeat_scan(&conn, QUEUE_USER_A, &claim.claim_id, 120).unwrap());
    }

    #[test]
    fn cap_respected_returns_none() {
        let conn = test_db();
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();
        enqueue_scan(&conn, QUEUE_USER_B).unwrap();

        assert!(claim_next_scan(&conn, 1, 120).unwrap().is_some());
        assert!(
            claim_next_scan(&conn, 1, 120).unwrap().is_none(),
            "cap of 1 already met — a second claim must be refused even though a queued row remains"
        );
    }

    #[test]
    fn claims_come_back_in_fifo_order() {
        let conn = test_db();
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        enqueue_scan(&conn, QUEUE_USER_B).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        enqueue_scan(&conn, QUEUE_USER_C).unwrap();

        let first = claim_next_scan(&conn, 3, 120).unwrap().unwrap();
        let second = claim_next_scan(&conn, 3, 120).unwrap().unwrap();
        let third = claim_next_scan(&conn, 3, 120).unwrap().unwrap();

        assert_eq!(
            first.user_did, QUEUE_USER_A,
            "oldest enqueued must claim first"
        );
        assert_eq!(second.user_did, QUEUE_USER_B, "second-oldest claims second");
        assert_eq!(third.user_did, QUEUE_USER_C, "youngest claims last");
    }

    #[test]
    fn position_counts_queued_rows_ahead() {
        let conn = test_db();
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        enqueue_scan(&conn, QUEUE_USER_B).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        enqueue_scan(&conn, QUEUE_USER_C).unwrap();

        let entry_b = scan_queue_entry(&conn, QUEUE_USER_B, 10).unwrap().unwrap();
        assert_eq!(entry_b.position, 2, "A and B are queued at-or-ahead of B");
        let entry_c = scan_queue_entry(&conn, QUEUE_USER_C, 10).unwrap().unwrap();
        assert_eq!(entry_c.position, 3);
    }

    /// `eta_seconds` is None for TWO independent reasons — a non-'queued'
    /// status, and no median yet. Testing the status gate while no median
    /// exists would pass for the wrong reason, so this seeds a finished scan
    /// first to guarantee a median, then checks a *running* row still gets
    /// None.
    #[test]
    fn eta_is_none_for_non_queued_status_even_with_a_median_available() {
        let conn = test_db();

        // Seed a finished scan so a median exists.
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();
        let claim = claim_next_scan(&conn, 1, 120).unwrap().unwrap();
        finish_queued_scan(&conn, QUEUE_USER_A, &claim.claim_id, None).unwrap();

        // A running row, with the median now available, must still report
        // no ETA — a running scan's remaining time is unknown.
        enqueue_scan(&conn, QUEUE_USER_B).unwrap();
        claim_next_scan(&conn, 1, 120).unwrap().unwrap();
        let running = scan_queue_entry(&conn, QUEUE_USER_B, 1).unwrap().unwrap();
        assert_eq!(running.status, "running");
        assert_eq!(
            running.eta_seconds, None,
            "a running scan's remaining time is unknown, so this must be None — not Some(0)"
        );
    }

    #[test]
    fn eta_is_none_when_no_median_exists_yet() {
        let conn = test_db();
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();
        let entry = scan_queue_entry(&conn, QUEUE_USER_A, 1).unwrap().unwrap();
        assert_eq!(entry.status, "queued");
        assert_eq!(
            entry.eta_seconds, None,
            "no scan has ever finished, so there is no median to compute an ETA from"
        );
    }

    /// Fractional seconds in the completed-scan history must survive into the
    /// median, because the Postgres backend's `EXTRACT(EPOCH FROM ...)` keeps
    /// them. `num_seconds()` truncated, so the same history quoted a different
    /// ETA either side of a backend switch.
    ///
    /// The position is deliberately 2, not 1: at one batch the multiplication
    /// hides the difference (90.5 and 90.0 both truncate to 90). At two
    /// batches the truncating version answers 180 and the correct one 181,
    /// which is exactly how the divergence reaches a user.
    ///
    /// `tests/db_postgres.rs::test_pg_eta_matches_sqlite_for_fractional_durations`
    /// is the other half of this — it asserts the two backends agree on the
    /// same history rather than asserting this side in isolation.
    #[test]
    fn the_median_keeps_fractional_seconds() {
        let conn = test_db();
        seed_completed_scan(&conn, "did:plc:done000000000000000", 90.5);

        enqueue_scan(&conn, QUEUE_USER_A).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        enqueue_scan(&conn, QUEUE_USER_B).unwrap();

        let entry = scan_queue_entry(&conn, QUEUE_USER_B, 1).unwrap().unwrap();
        assert_eq!(entry.position, 2);
        assert_eq!(
            entry.eta_seconds,
            Some(181),
            "a 90.5s median over two batches is 181s; 180 means the half-second \
             was truncated away and the backends disagree"
        );
    }

    /// Insert a finished `scan_queue` row whose duration is exactly
    /// `duration_secs`. Written directly rather than via
    /// `claim_next_scan`/`finish_queued_scan` because those stamp wall-clock
    /// times, and this needs a sub-second duration it can name.
    fn seed_completed_scan(conn: &Connection, user_did: &str, duration_secs: f64) {
        let started = chrono::DateTime::parse_from_rfc3339("2026-08-06T00:00:00+00:00").unwrap();
        let finished =
            started + chrono::TimeDelta::nanoseconds((duration_secs * 1e9).round() as i64);
        conn.execute(
            "INSERT INTO scan_queue (user_did, status, enqueued_at, started_at, finished_at)
             VALUES (?1, 'done', ?2, ?2, ?3)",
            params![user_did, started.to_rfc3339(), finished.to_rfc3339()],
        )
        .unwrap();
    }

    #[test]
    fn delete_user_data_clears_scan_queue() {
        let conn = test_db();
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();
        assert!(scan_queue_entry(&conn, QUEUE_USER_A, 1).unwrap().is_some());

        delete_user_data(&conn, QUEUE_USER_A).unwrap();

        assert_eq!(
            scan_queue_entry(&conn, QUEUE_USER_A, 1).unwrap(),
            None,
            "scan_queue row must not survive delete_user_data"
        );
    }

    // ── list_scan_queue (#288) ────────────────────────────────────────────

    /// Insert a queued row with an enqueued_at this test chooses.
    ///
    /// `enqueue_scan` stamps wall-clock time, so rows inserted through it come
    /// out in insertion order *and* in enqueued_at order — a query with no
    /// ORDER BY at all would satisfy an ordering assertion built that way,
    /// because SQLite returns rowid order. Naming the timestamps lets the three
    /// candidate orderings (insertion, alphabetical, enqueued_at) disagree, so
    /// only the right one passes.
    fn seed_queued_at(conn: &Connection, user_did: &str, enqueued_at: &str) {
        conn.execute(
            "INSERT INTO scan_queue (user_did, status, enqueued_at)
             VALUES (?1, 'queued', ?2)",
            params![user_did, enqueued_at],
        )
        .unwrap();
    }

    /// Seed A, B, C in that order — alphabetical AND insertion order — with
    /// enqueued_at running backwards, so the correct answer is C, B, A and
    /// nothing else.
    fn seed_reverse_chronological_queue(conn: &Connection) {
        seed_queued_at(conn, QUEUE_USER_A, "2026-08-09T00:00:03+00:00");
        seed_queued_at(conn, QUEUE_USER_B, "2026-08-09T00:00:02+00:00");
        seed_queued_at(conn, QUEUE_USER_C, "2026-08-09T00:00:01+00:00");
    }

    /// The admin dashboard renders this list top-to-bottom as the queue, so
    /// the ORDER BY is load-bearing, not cosmetic: whoever is listed first is
    /// who the operator believes runs next. Asserting `len()` alone would pass
    /// against an unordered query, which is exactly the bug that matters.
    #[test]
    fn list_scan_queue_is_ordered_oldest_first() {
        let conn = test_db();
        seed_reverse_chronological_queue(&conn);

        let rows = list_scan_queue(&conn).unwrap();
        let dids: Vec<&str> = rows.iter().map(|r| r.user_did.as_str()).collect();
        assert_eq!(
            dids,
            vec![QUEUE_USER_C, QUEUE_USER_B, QUEUE_USER_A],
            "rows must come back in enqueued_at order, oldest first — insertion \
             order and alphabetical order both say A, B, C here"
        );
    }

    /// Position is what the dashboard shows as "3rd in line". A row behind
    /// another queued row is 2, never 1 — and it is numbered by enqueued_at,
    /// not by whatever order the rows happen to arrive in.
    #[test]
    fn list_scan_queue_numbers_queued_rows_from_one() {
        let conn = test_db();
        seed_reverse_chronological_queue(&conn);

        let rows = list_scan_queue(&conn).unwrap();
        let positions: Vec<(&str, i64)> = rows
            .iter()
            .map(|r| (r.user_did.as_str(), r.position))
            .collect();
        assert_eq!(
            positions,
            vec![(QUEUE_USER_C, 1), (QUEUE_USER_B, 2), (QUEUE_USER_A, 3)],
            "queued rows are numbered 1..n by enqueued_at; the FIRST-inserted \
             row is last in line here, so a rowid-ordered count would say 1"
        );
    }

    /// Seed three rows sharing ONE `enqueued_at`, inserted in reverse
    /// `user_did` order.
    ///
    /// SQLite stores `enqueued_at` as RFC3339 TEXT, so two enqueues inside the
    /// same millisecond produce the identical string — a real tie, not a
    /// contrived one. Inserting C, B, A makes rowid order (which an
    /// unordered SQLite scan returns, and which `ORDER BY enqueued_at` alone
    /// falls back to among ties) the exact reverse of the expected answer.
    const TIED_ENQUEUED_AT: &str = "2026-08-09T00:00:01+00:00";

    fn seed_tied_queue(conn: &Connection) {
        seed_queued_at(conn, QUEUE_USER_C, TIED_ENQUEUED_AT);
        seed_queued_at(conn, QUEUE_USER_B, TIED_ENQUEUED_AT);
        seed_queued_at(conn, QUEUE_USER_A, TIED_ENQUEUED_AT);
    }

    /// `enqueued_at` alone is a PARTIAL order. When rows tie on it, a position
    /// counted as `enqueued_at <= ` hands every tied row the SAME number while
    /// the display `ORDER BY` still renders them in a definite sequence — so
    /// three rows render 1st, 2nd, 3rd and all three claim to be "3rd in line".
    /// `(enqueued_at, user_did)` is the total order both must use (#271).
    #[test]
    fn list_scan_queue_breaks_enqueued_at_ties_by_user_did() {
        let conn = test_db();
        seed_tied_queue(&conn);

        let rows = list_scan_queue(&conn).unwrap();
        let listed: Vec<(&str, i64)> = rows
            .iter()
            .map(|r| (r.user_did.as_str(), r.position))
            .collect();
        assert_eq!(
            listed,
            vec![(QUEUE_USER_A, 1), (QUEUE_USER_B, 2), (QUEUE_USER_C, 3)],
            "tied rows must get DISTINCT positions, in the order they are \
             displayed; counting `enqueued_at <=` alone gives all three 3"
        );
    }

    /// The per-user column has to agree with the queue panel row-for-row —
    /// two formulas for one number is how they start disagreeing.
    #[test]
    fn scan_queue_entry_breaks_enqueued_at_ties_by_user_did() {
        let conn = test_db();
        seed_tied_queue(&conn);

        for (did, expected) in [(QUEUE_USER_A, 1), (QUEUE_USER_B, 2), (QUEUE_USER_C, 3)] {
            let entry = scan_queue_entry(&conn, did, 1).unwrap().unwrap();
            assert_eq!(
                entry.position, expected,
                "{did} must be told the same position the queue panel shows"
            );
        }
    }

    /// The point of the fix: whoever the dashboard lists first must be
    /// whoever admission actually takes. With ties broken only for display,
    /// `claim_next_scan`'s `ORDER BY enqueued_at` falls back to rowid and
    /// admits the row shown LAST.
    #[test]
    fn claim_follows_the_order_the_queue_displays() {
        let conn = test_db();
        seed_tied_queue(&conn);

        let displayed_first = list_scan_queue(&conn).unwrap()[0].user_did.clone();
        let claim = claim_next_scan(&conn, 1, 120).unwrap().expect("claim");
        assert_eq!(
            claim.user_did, displayed_first,
            "admission must take the row the dashboard shows as next"
        );
        assert_eq!(
            claim.user_did, QUEUE_USER_A,
            "the total order is (enqueued_at, user_did), so A goes first"
        );
    }

    /// A running row occupies a slot rather than a place in line, so its
    /// position is 0 — and, critically, claiming it must RENUMBER everyone
    /// behind it. If position were computed over all rows regardless of
    /// status, B would still read 2 after A starts running.
    #[test]
    fn list_scan_queue_zeroes_position_for_non_queued_rows() {
        let conn = test_db();
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        enqueue_scan(&conn, QUEUE_USER_B).unwrap();
        claim_next_scan(&conn, 1, 120).unwrap().unwrap();

        let rows = list_scan_queue(&conn).unwrap();
        let by_did = |did: &str| {
            rows.iter()
                .find(|r| r.user_did == did)
                .unwrap_or_else(|| panic!("{did} must be listed"))
                .clone()
        };

        let a = by_did(QUEUE_USER_A);
        assert_eq!(a.status, "running");
        assert_eq!(a.position, 0, "a running row holds a slot, not a place");
        assert!(
            a.started_at.is_some(),
            "claiming stamps started_at, and the dashboard reads it"
        );

        let b = by_did(QUEUE_USER_B);
        assert_eq!(b.status, "queued");
        assert_eq!(
            b.position, 1,
            "B is now first in line — a position that ignored status would say 2"
        );
    }

    /// The whole point of #288: "the last scan failed" and "this user has
    /// never scanned" were indistinguishable on the old ScanManager-derived
    /// column. A failed row must carry its error and its finished_at.
    #[test]
    fn list_scan_queue_surfaces_failure_details() {
        let conn = test_db();
        enqueue_scan(&conn, QUEUE_USER_A).unwrap();
        let claim = claim_next_scan(&conn, 1, 120).unwrap().unwrap();
        finish_queued_scan(
            &conn,
            QUEUE_USER_A,
            &claim.claim_id,
            Some("gather exploded"),
        )
        .unwrap();

        let rows = list_scan_queue(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.status, "failed");
        assert_eq!(row.position, 0);
        assert_eq!(row.last_error.as_deref(), Some("gather exploded"));
        assert!(row.started_at.is_some());
        assert!(
            row.finished_at.is_some(),
            "a finished row must carry when it finished"
        );

        // Every timestamp on this trait is RFC3339 — the SPA parses them and
        // the Postgres backend must render the same shape.
        for ts in [
            Some(row.enqueued_at.clone()),
            row.started_at.clone(),
            row.finished_at.clone(),
        ]
        .into_iter()
        .flatten()
        {
            chrono::DateTime::parse_from_rfc3339(&ts)
                .unwrap_or_else(|e| panic!("{ts} is not RFC3339: {e}"));
        }
    }

    #[test]
    fn list_scan_queue_is_empty_when_nothing_was_ever_enqueued() {
        let conn = test_db();
        assert_eq!(list_scan_queue(&conn).unwrap(), Vec::new());
    }
}
