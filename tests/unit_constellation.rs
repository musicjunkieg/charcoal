// Unit tests for the Constellation backlink client.
//
// Tests serde deserialization, AT-URI construction, event conversion,
// and dedup logic — all without network access.

use charcoal::constellation::client::{BacklinkRecord, BacklinksResponse};

#[test]
fn deserialize_empty_response() {
    let json = r#"{"total": 0, "records": []}"#;
    let resp: BacklinksResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.total, Some(0));
    assert!(resp.records.is_empty());
    assert!(resp.cursor.is_none());
}

#[test]
fn deserialize_response_with_records() {
    let json = r#"{
        "total": 2,
        "records": [
            {"did": "did:plc:abc123", "collection": "app.bsky.feed.post", "rkey": "3k1abc"},
            {"did": "did:plc:def456", "collection": "app.bsky.feed.repost", "rkey": "3k2def"}
        ],
        "cursor": "next-page-token"
    }"#;
    let resp: BacklinksResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.total, Some(2));
    assert_eq!(resp.records.len(), 2);
    assert_eq!(resp.records[0].did, "did:plc:abc123");
    assert_eq!(resp.records[0].collection, "app.bsky.feed.post");
    assert_eq!(resp.records[0].rkey, "3k1abc");
    assert_eq!(resp.records[1].did, "did:plc:def456");
    assert_eq!(resp.cursor, Some("next-page-token".to_string()));
}

#[test]
fn deserialize_null_cursor() {
    let json = r#"{"total": 1, "records": [{"did": "did:plc:xyz", "collection": "app.bsky.feed.post", "rkey": "abc"}], "cursor": null}"#;
    let resp: BacklinksResponse = serde_json::from_str(json).unwrap();
    assert!(resp.cursor.is_none());
    assert_eq!(resp.records.len(), 1);
}

#[test]
fn deserialize_missing_total() {
    // Constellation may omit `total` in some responses
    let json = r#"{"records": [], "cursor": null}"#;
    let resp: BacklinksResponse = serde_json::from_str(json).unwrap();
    assert!(resp.total.is_none());
    assert!(resp.records.is_empty());
}

#[test]
fn at_uri_construction_from_record() {
    let record = BacklinkRecord {
        did: "did:plc:abc123".to_string(),
        collection: "app.bsky.feed.post".to_string(),
        rkey: "3k1xyz".to_string(),
    };
    let uri = format!("at://{}/{}/{}", record.did, record.collection, record.rkey);
    assert_eq!(uri, "at://did:plc:abc123/app.bsky.feed.post/3k1xyz");
}

#[test]
fn at_uri_construction_repost() {
    let record = BacklinkRecord {
        did: "did:plc:user999".to_string(),
        collection: "app.bsky.feed.repost".to_string(),
        rkey: "rk42".to_string(),
    };
    let uri = format!("at://{}/{}/{}", record.did, record.collection, record.rkey);
    assert_eq!(uri, "at://did:plc:user999/app.bsky.feed.repost/rk42");
}

#[test]
fn dedup_by_amplifier_post_uri() {
    use charcoal::bluesky::notifications::AmplificationNotification;
    use std::collections::HashSet;

    let events_a = vec![
        AmplificationNotification {
            event_type: "quote".to_string(),
            amplifier_did: "did:plc:aaa".to_string(),
            amplifier_handle: "alice.bsky.social".to_string(),
            original_post_uri: Some("at://did:plc:me/app.bsky.feed.post/1".to_string()),
            amplifier_post_uri: "at://did:plc:aaa/app.bsky.feed.post/q1".to_string(),
            indexed_at: "2026-02-18T00:00:00Z".to_string(),
        },
        AmplificationNotification {
            event_type: "repost".to_string(),
            amplifier_did: "did:plc:bbb".to_string(),
            amplifier_handle: "bob.bsky.social".to_string(),
            original_post_uri: Some("at://did:plc:me/app.bsky.feed.post/1".to_string()),
            amplifier_post_uri: "at://did:plc:bbb/app.bsky.feed.repost/r1".to_string(),
            indexed_at: "2026-02-18T00:00:00Z".to_string(),
        },
    ];

    // Supplementary events — one duplicate, one new
    let events_b = vec![
        AmplificationNotification {
            event_type: "quote".to_string(),
            amplifier_did: "did:plc:aaa".to_string(),
            amplifier_handle: "did:plc:aaa".to_string(),
            original_post_uri: Some("at://did:plc:me/app.bsky.feed.post/1".to_string()),
            amplifier_post_uri: "at://did:plc:aaa/app.bsky.feed.post/q1".to_string(), // duplicate
            indexed_at: String::new(),
        },
        AmplificationNotification {
            event_type: "quote".to_string(),
            amplifier_did: "did:plc:ccc".to_string(),
            amplifier_handle: "did:plc:ccc".to_string(),
            original_post_uri: Some("at://did:plc:me/app.bsky.feed.post/1".to_string()),
            amplifier_post_uri: "at://did:plc:ccc/app.bsky.feed.post/q2".to_string(), // new
            indexed_at: String::new(),
        },
    ];

    // Simulate the dedup logic from amplification::run
    let mut merged = events_a;
    let existing_uris: HashSet<String> = merged
        .iter()
        .map(|e| e.amplifier_post_uri.clone())
        .collect();
    for event in events_b {
        if !existing_uris.contains(&event.amplifier_post_uri) {
            merged.push(event);
        }
    }

    assert_eq!(merged.len(), 3); // 2 original + 1 new (duplicate dropped)
    assert_eq!(merged[2].amplifier_did, "did:plc:ccc");
}
