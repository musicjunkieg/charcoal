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
    use charcoal::bluesky::amplification::AmplificationNotification;
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

// ============================================================
// Task 2 (#213): the discovery loops parallelize their network fetches, then
// run a PURE dedup fold over results sorted back into URI order. These tests
// pin the fold's ordering + dedup so the parallel path is byte-identical to
// the old serial loop.
// ============================================================

use charcoal::constellation::client::{dedup_amplification_events, dedup_liker_events};

fn resp(records: Vec<BacklinkRecord>) -> BacklinksResponse {
    BacklinksResponse {
        total: Some(records.len() as u64),
        records,
        cursor: None,
    }
}

fn rec(did: &str, collection: &str, rkey: &str) -> BacklinkRecord {
    BacklinkRecord {
        did: did.to_string(),
        collection: collection.to_string(),
        rkey: rkey.to_string(),
    }
}

#[test]
fn dedup_amplification_events_preserves_serial_order_and_dedups() {
    // Two URIs, each with a quote response and a repost response, supplied in
    // URI order (as buffer_unordered + sort_by_key guarantees). Expected output
    // order matches the old serial loop: for each URI, quotes then reposts.
    let uri_a = "at://did:plc:me/app.bsky.feed.post/A".to_string();
    let uri_b = "at://did:plc:me/app.bsky.feed.post/B".to_string();

    let fetched: Vec<(
        String,
        anyhow::Result<BacklinksResponse>,
        anyhow::Result<BacklinksResponse>,
    )> = vec![
        (
            uri_a.clone(),
            Ok(resp(vec![rec("did:plc:q1", "app.bsky.feed.post", "qa")])),
            Ok(resp(vec![rec("did:plc:r1", "app.bsky.feed.repost", "ra")])),
        ),
        (
            uri_b.clone(),
            // A duplicate of uri_a's quote (same amp_uri) must be dropped.
            Ok(resp(vec![
                rec("did:plc:q1", "app.bsky.feed.post", "qa"),
                rec("did:plc:q2", "app.bsky.feed.post", "qb"),
            ])),
            Ok(resp(vec![])),
        ),
    ];

    let events = dedup_amplification_events(fetched);

    // Serial order: A-quote, A-repost, B-quote(new only). The B duplicate drops.
    let uris: Vec<&str> = events
        .iter()
        .map(|e| e.amplifier_post_uri.as_str())
        .collect();
    assert_eq!(
        uris,
        vec![
            "at://did:plc:q1/app.bsky.feed.post/qa",
            "at://did:plc:r1/app.bsky.feed.repost/ra",
            "at://did:plc:q2/app.bsky.feed.post/qb",
        ]
    );
    assert_eq!(events[0].event_type, "quote");
    assert_eq!(events[1].event_type, "repost");
    assert_eq!(events[2].event_type, "quote");
    assert_eq!(events[0].original_post_uri, Some(uri_a));
    assert_eq!(events[2].original_post_uri, Some(uri_b));
}

#[test]
fn dedup_amplification_events_skips_errored_uris_and_keeps_order() {
    let fetched: Vec<(
        String,
        anyhow::Result<BacklinksResponse>,
        anyhow::Result<BacklinksResponse>,
    )> = vec![
        (
            "at://did:plc:me/app.bsky.feed.post/A".to_string(),
            Err(anyhow::anyhow!("quote fetch failed")),
            Ok(resp(vec![rec("did:plc:r1", "app.bsky.feed.repost", "ra")])),
        ),
        (
            "at://did:plc:me/app.bsky.feed.post/B".to_string(),
            Ok(resp(vec![rec("did:plc:q2", "app.bsky.feed.post", "qb")])),
            Err(anyhow::anyhow!("repost fetch failed")),
        ),
    ];

    let events = dedup_amplification_events(fetched);

    // A's quote errored (skipped), A's repost ok; B's quote ok, B's repost errored.
    let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(types, vec!["repost", "quote"]);
}

#[test]
fn dedup_liker_events_preserves_order_and_dedups_by_did_and_uri() {
    let uri_a = "at://did:plc:me/app.bsky.feed.post/A".to_string();
    let uri_b = "at://did:plc:me/app.bsky.feed.post/B".to_string();

    let fetched: Vec<(String, anyhow::Result<BacklinksResponse>)> = vec![
        (
            uri_a.clone(),
            Ok(resp(vec![
                rec("did:plc:liker1", "app.bsky.feed.like", "l1"),
                rec("did:plc:liker1", "app.bsky.feed.like", "l2"), // same did+uri → dropped
            ])),
        ),
        (
            uri_b.clone(),
            Ok(resp(vec![rec(
                "did:plc:liker1",
                "app.bsky.feed.like",
                "l3",
            )])), // same did, diff uri → kept
        ),
    ];

    let events = dedup_liker_events(fetched);

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].amplifier_did, "did:plc:liker1");
    assert_eq!(events[0].original_post_uri, Some(uri_a));
    assert_eq!(events[1].original_post_uri, Some(uri_b));
    assert!(events.iter().all(|e| e.event_type == "like"));
    assert!(events.iter().all(|e| e.amplifier_post_uri.is_empty()));
}
