// Database queries — CRUD operations for all tables.
//
// Every database interaction goes through this module. This keeps SQL
// contained in one place and gives the rest of the app clean Rust interfaces.
//
// All query functions take a `user_did` parameter to scope data to a specific
// protected user. This enables multi-user support where each user's threat
// data is isolated.

use anyhow::Result;
use rusqlite::{params, Connection};

use super::models::{AccountScore, AmplificationEvent, ThreatTier, ToxicPost};

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

// --- Account scores ---

/// Save or update an account's scores for a specific user.
pub fn upsert_account_score(conn: &Connection, user_did: &str, score: &AccountScore) -> Result<()> {
    let top_posts_json = serde_json::to_string(&score.top_toxic_posts)?;
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, toxicity_score, topic_overlap, threat_score, threat_tier, posts_analyzed, top_toxic_posts, scored_at, behavioral_signals)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, datetime('now'), ?10)
         ON CONFLICT(user_did, did) DO UPDATE SET
            handle = ?3,
            toxicity_score = ?4,
            topic_overlap = ?5,
            threat_score = ?6,
            threat_tier = ?7,
            posts_analyzed = ?8,
            top_toxic_posts = ?9,
            scored_at = datetime('now'),
            behavioral_signals = ?10",
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
                posts_analyzed, top_toxic_posts, scored_at, behavioral_signals
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
             amplifier_post_uri, amplifier_text, detected_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            user_did,
            event.event_type,
            event.amplifier_did,
            event.amplifier_handle,
            event.original_post_uri,
            event.amplifier_post_uri,
            event.amplifier_text,
            event.detected_at,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Record a new amplification event for a specific user.
pub fn insert_amplification_event(
    conn: &Connection,
    user_did: &str,
    event_type: &str,
    amplifier_did: &str,
    amplifier_handle: &str,
    original_post_uri: &str,
    amplifier_post_uri: Option<&str>,
    amplifier_text: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO amplification_events
            (user_did, event_type, amplifier_did, amplifier_handle, original_post_uri,
             amplifier_post_uri, amplifier_text)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            user_did,
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

/// Get recent amplification events for a specific user.
pub fn get_recent_events(
    conn: &Connection,
    user_did: &str,
    limit: u32,
) -> Result<Vec<AmplificationEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, event_type, amplifier_did, amplifier_handle, original_post_uri,
                amplifier_post_uri, amplifier_text, detected_at, followers_fetched, followers_scored
         FROM amplification_events
         WHERE user_did = ?1
         ORDER BY detected_at DESC
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
                posts_analyzed, top_toxic_posts, scored_at, behavioral_signals
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
                posts_analyzed, top_toxic_posts, scored_at, behavioral_signals
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
        })
        .optional()?;
    Ok(result)
}

// rusqlite's optional() helper — converts "no rows" into None
use rusqlite::OptionalExtension;

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
            threat_score: Some(65.0),
            threat_tier: Some("Elevated".to_string()),
            posts_analyzed: 20,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
        };
        upsert_account_score(&conn, TEST_USER, &score).unwrap();

        let ranked = get_ranked_threats(&conn, TEST_USER, 0.0).unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].handle, "test.bsky.social");
        assert_eq!(ranked[0].threat_score, Some(65.0));
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
            threat_score: Some(30.0),
            threat_tier: Some("Watch".to_string()),
            posts_analyzed: 10,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
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
            threat_score: Some(30.0),
            threat_tier: Some("Watch".to_string()),
            posts_analyzed: 10,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
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
            threat_score: Some(30.0),
            threat_tier: Some("Watch".to_string()),
            posts_analyzed: 10,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
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
                threat_score: Some(30.0),
                threat_tier: Some("Watch".to_string()),
                posts_analyzed: 10,
                top_toxic_posts: vec![],
                scored_at: String::new(),
                behavioral_signals: Some(format!(r#"{{"avg_engagement":{eng}}}"#)),
            };
            upsert_account_score(&conn, TEST_USER, &score).unwrap();
        }

        let median = get_median_engagement(&conn, TEST_USER).unwrap();
        assert!((median - 20.0).abs() < f64::EPSILON);
    }
}
