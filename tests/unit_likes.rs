//! Tests for like detection via Constellation backlinks.

#[test]
fn likes_source_path_is_correct() {
    // The Constellation source path for likes should be:
    // app.bsky.feed.like:subject.uri
    assert_eq!(
        charcoal::constellation::client::LIKES_SOURCE,
        "app.bsky.feed.like:subject.uri"
    );
}

#[test]
fn like_events_have_no_amplifier_post_uri() {
    // Likes don't create a new post — amplifier_post_uri should be None
    use charcoal::bluesky::amplification::AmplificationNotification;

    let like = AmplificationNotification {
        event_type: "like".to_string(),
        amplifier_did: "did:plc:liker".to_string(),
        amplifier_handle: "did:plc:liker".to_string(),
        original_post_uri: Some("at://did:plc:user/app.bsky.feed.post/abc".to_string()),
        amplifier_post_uri: String::new(), // no post URI for likes
        indexed_at: String::new(),
    };
    assert_eq!(like.event_type, "like");
    assert!(like.amplifier_post_uri.is_empty());
}

#[test]
fn likes_api_fallback_response_parses() {
    // Test deserialization of app.bsky.feed.getLikes response
    use charcoal::bluesky::likes::LikesResponse;

    let json = r#"{
        "uri": "at://did:plc:user/app.bsky.feed.post/abc",
        "likes": [
            {
                "indexedAt": "2026-03-19T12:00:00Z",
                "createdAt": "2026-03-19T11:55:00Z",
                "actor": {
                    "did": "did:plc:liker1",
                    "handle": "liker1.bsky.social"
                }
            },
            {
                "indexedAt": "2026-03-19T12:01:00Z",
                "createdAt": "2026-03-19T11:56:00Z",
                "actor": {
                    "did": "did:plc:liker2",
                    "handle": "liker2.bsky.social"
                }
            }
        ],
        "cursor": "next-page"
    }"#;

    let resp: LikesResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.likes.len(), 2);
    assert_eq!(resp.likes[0].actor.did, "did:plc:liker1");
    assert_eq!(resp.likes[1].actor.did, "did:plc:liker2");
    assert_eq!(resp.cursor, Some("next-page".to_string()));
}

#[test]
fn likes_api_response_handles_no_cursor() {
    use charcoal::bluesky::likes::LikesResponse;

    let json = r#"{
        "uri": "at://did:plc:user/app.bsky.feed.post/abc",
        "likes": [],
        "cursor": null
    }"#;

    let resp: LikesResponse = serde_json::from_str(json).unwrap();
    assert!(resp.likes.is_empty());
    assert!(resp.cursor.is_none());
}

#[test]
fn likes_api_response_handles_missing_cursor() {
    use charcoal::bluesky::likes::LikesResponse;

    let json = r#"{
        "uri": "at://did:plc:user/app.bsky.feed.post/abc",
        "likes": []
    }"#;

    let resp: LikesResponse = serde_json::from_str(json).unwrap();
    assert!(resp.likes.is_empty());
    assert!(resp.cursor.is_none());
}
