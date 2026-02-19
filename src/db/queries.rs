// Database queries — CRUD operations for all tables.
//
// Every database interaction goes through this module. This keeps SQL
// contained in one place and gives the rest of the app clean Rust interfaces.

use anyhow::Result;
use rusqlite::{params, Connection};

use super::models::{AccountScore, AmplificationEvent, ThreatTier, ToxicPost};

// --- Scan state ---

/// Get a scan state value by key (e.g., "notifications_cursor").
pub fn get_scan_state(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM scan_state WHERE key = ?1")?;
    let result = stmt.query_row(params![key], |row| row.get(0)).optional()?;
    Ok(result)
}

/// Set a scan state value (upsert).
pub fn set_scan_state(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO scan_state (key, value, updated_at)
         VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = datetime('now')",
        params![key, value],
    )?;
    Ok(())
}

// --- Topic fingerprint ---

/// Store the topic fingerprint (singleton — always id=1).
pub fn save_fingerprint(conn: &Connection, fingerprint_json: &str, post_count: u32) -> Result<()> {
    conn.execute(
        "INSERT INTO topic_fingerprint (id, fingerprint_json, post_count, updated_at)
         VALUES (1, ?1, ?2, datetime('now'))
         ON CONFLICT(id) DO UPDATE SET
            fingerprint_json = ?1,
            post_count = ?2,
            updated_at = datetime('now')",
        params![fingerprint_json, post_count],
    )?;
    Ok(())
}

/// Store the protected user's mean embedding vector alongside the fingerprint.
/// The vector is stored as a JSON array of floats.
pub fn save_embedding(conn: &Connection, embedding_json: &str) -> Result<()> {
    conn.execute(
        "UPDATE topic_fingerprint SET embedding_vector = ?1, updated_at = datetime('now') WHERE id = 1",
        params![embedding_json],
    )?;
    Ok(())
}

/// Load the stored fingerprint JSON and metadata.
pub fn get_fingerprint(conn: &Connection) -> Result<Option<(String, u32, String)>> {
    let mut stmt = conn.prepare(
        "SELECT fingerprint_json, post_count, updated_at FROM topic_fingerprint WHERE id = 1",
    )?;
    let result = stmt
        .query_row([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .optional()?;
    Ok(result)
}

/// Load the stored embedding vector (if one exists).
pub fn get_embedding(conn: &Connection) -> Result<Option<Vec<f64>>> {
    let mut stmt = conn.prepare("SELECT embedding_vector FROM topic_fingerprint WHERE id = 1")?;
    let result: Option<Option<String>> = stmt.query_row([], |row| row.get(0)).optional()?;

    match result.flatten() {
        Some(json) => {
            let vec: Vec<f64> = serde_json::from_str(&json)?;
            Ok(Some(vec))
        }
        None => Ok(None),
    }
}

// --- Account scores ---

/// Save or update an account's scores.
pub fn upsert_account_score(conn: &Connection, score: &AccountScore) -> Result<()> {
    let top_posts_json = serde_json::to_string(&score.top_toxic_posts)?;
    conn.execute(
        "INSERT INTO account_scores (did, handle, toxicity_score, topic_overlap, threat_score, threat_tier, posts_analyzed, top_toxic_posts, scored_at, behavioral_signals)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'), ?9)
         ON CONFLICT(did) DO UPDATE SET
            handle = ?2,
            toxicity_score = ?3,
            topic_overlap = ?4,
            threat_score = ?5,
            threat_tier = ?6,
            posts_analyzed = ?7,
            top_toxic_posts = ?8,
            scored_at = datetime('now'),
            behavioral_signals = ?9",
        params![
            score.did,
            score.handle,
            score.toxicity_score,
            score.topic_overlap,
            score.threat_score,
            score.threat_tier,
            score.posts_analyzed,
            top_posts_json,
            score.behavioral_signals,
        ],
    )?;
    Ok(())
}

/// Get all scored accounts, ranked by threat score descending.
pub fn get_ranked_threats(conn: &Connection, min_score: f64) -> Result<Vec<AccountScore>> {
    let mut stmt = conn.prepare(
        "SELECT did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
                posts_analyzed, top_toxic_posts, scored_at, behavioral_signals
         FROM account_scores
         WHERE threat_score >= ?1
         ORDER BY threat_score DESC",
    )?;

    let rows = stmt.query_map(params![min_score], |row| {
        let top_posts_json: String = row.get(7)?;
        let top_toxic_posts: Vec<ToxicPost> =
            serde_json::from_str(&top_posts_json).unwrap_or_default();
        // Recalculate tier from stored score so threshold changes
        // take effect without rescanning.
        let threat_score: Option<f64> = row.get(4)?;
        let threat_tier = threat_score.map(|s| ThreatTier::from_score(s).to_string());
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
        })
    })?;

    let mut accounts = Vec::new();
    for row in rows {
        accounts.push(row?);
    }
    Ok(accounts)
}

/// Check if an account's score is stale (older than the given number of days).
pub fn is_score_stale(conn: &Connection, did: &str, max_age_days: i64) -> Result<bool> {
    let mut stmt = conn.prepare("SELECT scored_at FROM account_scores WHERE did = ?1")?;
    let result: Option<String> = stmt.query_row(params![did], |row| row.get(0)).optional()?;

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

// --- Amplification events ---

/// Record a new amplification event.
pub fn insert_amplification_event(
    conn: &Connection,
    event_type: &str,
    amplifier_did: &str,
    amplifier_handle: &str,
    original_post_uri: &str,
    amplifier_post_uri: Option<&str>,
    amplifier_text: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO amplification_events
            (event_type, amplifier_did, amplifier_handle, original_post_uri,
             amplifier_post_uri, amplifier_text)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event_type,
            amplifier_did,
            amplifier_handle,
            original_post_uri,
            amplifier_post_uri,
            amplifier_text,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get recent amplification events.
pub fn get_recent_events(conn: &Connection, limit: u32) -> Result<Vec<AmplificationEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, event_type, amplifier_did, amplifier_handle, original_post_uri,
                amplifier_post_uri, amplifier_text, detected_at, followers_fetched, followers_scored
         FROM amplification_events
         ORDER BY detected_at DESC
         LIMIT ?1",
    )?;

    let rows = stmt.query_map(params![limit], |row| {
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
        })
    })?;

    let mut events = Vec::new();
    for row in rows {
        events.push(row?);
    }
    Ok(events)
}

/// Get amplification events for pile-on detection.
/// Returns (amplifier_did, original_post_uri, detected_at) tuples.
pub fn get_events_for_pile_on(conn: &Connection) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT amplifier_did, original_post_uri, detected_at
         FROM amplification_events
         ORDER BY original_post_uri, detected_at",
    )?;

    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;

    let mut events = Vec::new();
    for row in rows {
        events.push(row?);
    }
    Ok(events)
}

/// Get the median engagement across all scored accounts with behavioral data.
pub fn get_median_engagement(conn: &Connection) -> Result<f64> {
    let mut stmt = conn.prepare(
        "SELECT behavioral_signals FROM account_scores WHERE behavioral_signals IS NOT NULL",
    )?;
    let mut engagements: Vec<f64> = stmt
        .query_map([], |row| {
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

// rusqlite's optional() helper — converts "no rows" into None
use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::create_tables;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn test_scan_state_roundtrip() {
        let conn = test_db();
        assert_eq!(get_scan_state(&conn, "cursor").unwrap(), None);

        set_scan_state(&conn, "cursor", "abc123").unwrap();
        assert_eq!(
            get_scan_state(&conn, "cursor").unwrap(),
            Some("abc123".to_string())
        );

        // Upsert overwrites
        set_scan_state(&conn, "cursor", "def456").unwrap();
        assert_eq!(
            get_scan_state(&conn, "cursor").unwrap(),
            Some("def456".to_string())
        );
    }

    #[test]
    fn test_fingerprint_roundtrip() {
        let conn = test_db();
        assert!(get_fingerprint(&conn).unwrap().is_none());

        save_fingerprint(&conn, r#"{"topics": []}"#, 100).unwrap();
        let (json, count, _updated) = get_fingerprint(&conn).unwrap().unwrap();
        assert_eq!(json, r#"{"topics": []}"#);
        assert_eq!(count, 100);

        // Upsert replaces
        save_fingerprint(&conn, r#"{"topics": ["a"]}"#, 200).unwrap();
        let (json, count, _) = get_fingerprint(&conn).unwrap().unwrap();
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
            threat_score: Some(65.0),
            threat_tier: Some("Elevated".to_string()),
            posts_analyzed: 20,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
        };
        upsert_account_score(&conn, &score).unwrap();

        let ranked = get_ranked_threats(&conn, 0.0).unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].handle, "test.bsky.social");
        assert_eq!(ranked[0].threat_score, Some(65.0));
    }

    #[test]
    fn test_embedding_roundtrip() {
        let conn = test_db();

        // No embedding initially
        assert!(get_embedding(&conn).unwrap().is_none());

        // Must have a fingerprint row first (embedding is a column on it)
        save_fingerprint(&conn, r#"{"clusters":[]}"#, 50).unwrap();

        // Still no embedding until explicitly saved
        assert!(get_embedding(&conn).unwrap().is_none());

        // Save an embedding vector
        let embedding = vec![0.1, 0.2, 0.3, -0.5];
        let emb_json = serde_json::to_string(&embedding).unwrap();
        save_embedding(&conn, &emb_json).unwrap();

        // Retrieve it
        let loaded = get_embedding(&conn).unwrap().unwrap();
        assert_eq!(loaded.len(), 4);
        assert!((loaded[0] - 0.1).abs() < f64::EPSILON);
        assert!((loaded[3] - -0.5).abs() < f64::EPSILON);

        // Overwrite with a new embedding
        let new_embedding = vec![1.0, 2.0];
        let new_json = serde_json::to_string(&new_embedding).unwrap();
        save_embedding(&conn, &new_json).unwrap();

        let reloaded = get_embedding(&conn).unwrap().unwrap();
        assert_eq!(reloaded.len(), 2);
        assert!((reloaded[0] - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_embedding_survives_fingerprint_update() {
        let conn = test_db();
        save_fingerprint(&conn, r#"{"clusters":[]}"#, 50).unwrap();

        let embedding = vec![0.1, 0.2, 0.3];
        save_embedding(&conn, &serde_json::to_string(&embedding).unwrap()).unwrap();

        // Update the fingerprint — embedding should survive (different column)
        save_fingerprint(&conn, r#"{"clusters":["new"]}"#, 100).unwrap();

        let loaded = get_embedding(&conn).unwrap().unwrap();
        assert_eq!(loaded.len(), 3);
        assert!((loaded[0] - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn test_amplification_event() {
        let conn = test_db();

        let id = insert_amplification_event(
            &conn,
            "quote",
            "did:plc:xyz",
            "troll.bsky.social",
            "at://did:plc:me/app.bsky.feed.post/abc",
            Some("at://did:plc:xyz/app.bsky.feed.post/def"),
            Some("lol look at this"),
        )
        .unwrap();
        assert!(id > 0);

        let events = get_recent_events(&conn, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "quote");
        assert_eq!(events[0].amplifier_handle, "troll.bsky.social");
    }
}
