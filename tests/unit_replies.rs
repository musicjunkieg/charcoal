//! Tests for drive-by reply detection.

use charcoal::bluesky::replies::{filter_drive_by_replies, filter_drive_by_replies_excluding_self};
use std::collections::HashSet;

#[test]
fn filters_out_followed_accounts() {
    let follows: HashSet<String> = ["did:plc:friend1", "did:plc:friend2"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    let reply_dids = vec![
        "did:plc:friend1".to_string(),  // followed — should be filtered
        "did:plc:stranger".to_string(), // not followed — drive-by
        "did:plc:friend2".to_string(),  // followed — should be filtered
        "did:plc:rando".to_string(),    // not followed — drive-by
    ];

    let drive_bys = filter_drive_by_replies(&reply_dids, &follows);
    assert_eq!(drive_bys.len(), 2);
    assert!(drive_bys.contains(&"did:plc:stranger".to_string()));
    assert!(drive_bys.contains(&"did:plc:rando".to_string()));
}

#[test]
fn empty_follows_treats_all_as_drive_by() {
    let follows: HashSet<String> = HashSet::new();
    let reply_dids = vec!["did:plc:a".to_string(), "did:plc:b".to_string()];
    let drive_bys = filter_drive_by_replies(&reply_dids, &follows);
    assert_eq!(drive_bys.len(), 2);
}

#[test]
fn empty_replies_returns_empty() {
    let follows: HashSet<String> = ["did:plc:friend"].iter().map(|s| s.to_string()).collect();
    let reply_dids: Vec<String> = vec![];
    let drive_bys = filter_drive_by_replies(&reply_dids, &follows);
    assert!(drive_bys.is_empty());
}

#[test]
fn filters_out_protected_user_self_replies() {
    let follows: HashSet<String> = HashSet::new();
    let protected_did = "did:plc:protected";
    let reply_dids = vec![
        "did:plc:protected".to_string(), // self-reply — exclude
        "did:plc:stranger".to_string(),  // drive-by
    ];

    let drive_bys = filter_drive_by_replies_excluding_self(&reply_dids, &follows, protected_did);
    assert_eq!(drive_bys.len(), 1);
    assert_eq!(drive_bys[0], "did:plc:stranger");
}

#[test]
fn self_reply_filter_also_excludes_follows() {
    let follows: HashSet<String> = ["did:plc:friend"].iter().map(|s| s.to_string()).collect();
    let protected_did = "did:plc:protected";
    let reply_dids = vec![
        "did:plc:protected".to_string(), // self — exclude
        "did:plc:friend".to_string(),    // followed — exclude
        "did:plc:stranger".to_string(),  // drive-by — keep
    ];

    let drive_bys = filter_drive_by_replies_excluding_self(&reply_dids, &follows, protected_did);
    assert_eq!(drive_bys.len(), 1);
    assert_eq!(drive_bys[0], "did:plc:stranger");
}

#[test]
fn all_replies_from_follows_returns_empty() {
    let follows: HashSet<String> = ["did:plc:a", "did:plc:b"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let reply_dids = vec!["did:plc:a".to_string(), "did:plc:b".to_string()];
    let drive_bys = filter_drive_by_replies(&reply_dids, &follows);
    assert!(drive_bys.is_empty());
}

#[test]
fn thread_response_parses_replies() {
    // Test deserialization of getPostThread response structure
    use charcoal::bluesky::replies::extract_reply_dids_from_thread;

    let thread_json = serde_json::json!({
        "thread": {
            "post": {
                "uri": "at://did:plc:user/app.bsky.feed.post/abc",
                "author": { "did": "did:plc:user", "handle": "user.bsky.social" },
                "record": { "text": "original post" }
            },
            "replies": [
                {
                    "post": {
                        "uri": "at://did:plc:replier1/app.bsky.feed.post/def",
                        "author": { "did": "did:plc:replier1", "handle": "replier1.bsky.social" },
                        "record": { "text": "hostile reply" }
                    }
                },
                {
                    "post": {
                        "uri": "at://did:plc:replier2/app.bsky.feed.post/ghi",
                        "author": { "did": "did:plc:replier2", "handle": "replier2.bsky.social" },
                        "record": { "text": "another reply" }
                    }
                }
            ]
        }
    });

    let replies = extract_reply_dids_from_thread(&thread_json);
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0].0, "did:plc:replier1");
    assert_eq!(replies[0].1, "hostile reply");
    assert_eq!(replies[0].2, "at://did:plc:replier1/app.bsky.feed.post/def");
    assert_eq!(replies[1].0, "did:plc:replier2");
}

#[test]
fn thread_response_handles_empty_replies() {
    use charcoal::bluesky::replies::extract_reply_dids_from_thread;

    let thread_json = serde_json::json!({
        "thread": {
            "post": {
                "uri": "at://did:plc:user/app.bsky.feed.post/abc",
                "author": { "did": "did:plc:user" },
                "record": { "text": "a post with no replies" }
            }
        }
    });

    let replies = extract_reply_dids_from_thread(&thread_json);
    assert!(replies.is_empty());
}

#[test]
fn thread_response_skips_entries_missing_did() {
    use charcoal::bluesky::replies::extract_reply_dids_from_thread;

    let thread_json = serde_json::json!({
        "thread": {
            "post": {
                "uri": "at://did:plc:user/app.bsky.feed.post/abc",
                "author": { "did": "did:plc:user" },
                "record": { "text": "original" }
            },
            "replies": [
                {
                    "post": {
                        "uri": "at://did:plc:r1/app.bsky.feed.post/def",
                        "author": { "did": "did:plc:r1" },
                        "record": { "text": "valid reply" }
                    }
                },
                {
                    "post": {
                        "uri": "",
                        "author": {},
                        "record": { "text": "missing did" }
                    }
                }
            ]
        }
    });

    let replies = extract_reply_dids_from_thread(&thread_json);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].0, "did:plc:r1");
}
