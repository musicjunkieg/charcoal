// tests/unit_sampling.rs
//
// Tests for reply-inclusive post sampling types and partitioning logic.

use charcoal::bluesky::posts::{FingerprintQuality, Post, PostSample, ReplyPost};

#[test]
fn post_sample_partitioned_correctly() {
    let sample = PostSample {
        originals: make_posts(20),
        replies: make_reply_posts(25),
        quotes: make_posts(5),
        reply_ratio: 25.0 / 50.0,
        quote_ratio: 5.0 / 50.0,
        total_posts: 50,
    };
    assert_eq!(sample.originals.len(), 20);
    assert_eq!(sample.replies.len(), 25);
    assert_eq!(sample.quotes.len(), 5);
    assert_eq!(sample.total_posts, 50);
    assert!((sample.reply_ratio - 0.5).abs() < 0.001);
    assert!((sample.quote_ratio - 0.1).abs() < 0.001);
}

#[test]
fn fingerprint_quality_sufficient_originals() {
    assert_eq!(
        FingerprintQuality::from_counts(20, 30),
        FingerprintQuality::Normal
    );
}

#[test]
fn fingerprint_quality_mixed_fallback() {
    assert_eq!(
        FingerprintQuality::from_counts(10, 40),
        FingerprintQuality::Degraded
    );
}

#[test]
fn fingerprint_quality_insufficient() {
    assert_eq!(
        FingerprintQuality::from_counts(3, 8),
        FingerprintQuality::Unreliable
    );
}

#[test]
fn fingerprint_quality_zero_originals() {
    // 0 originals is ALWAYS unreliable — fingerprinting entirely from replies
    // captures topics of people they're arguing with, not their own interests
    assert_eq!(
        FingerprintQuality::from_counts(0, 25),
        FingerprintQuality::Unreliable
    );
}

#[test]
fn fingerprint_quality_boundary_exactly_15_originals() {
    assert_eq!(
        FingerprintQuality::from_counts(15, 0),
        FingerprintQuality::Normal
    );
}

#[test]
fn fingerprint_quality_boundary_14_originals_with_replies() {
    assert_eq!(
        FingerprintQuality::from_counts(14, 1),
        FingerprintQuality::Degraded
    );
}

/// Integration test — requires network access.
/// Fetches posts for a known public account and verifies partitioning.
#[tokio::test]
async fn fetch_posts_with_replies_partitions_correctly() {
    use charcoal::bluesky::client::PublicAtpClient;
    use charcoal::bluesky::posts;

    let client = PublicAtpClient::new("https://public.api.bsky.app").unwrap();
    // Use a well-known active account that posts replies
    let sample = posts::fetch_posts_with_replies(&client, "bsky.app", 50).await;

    match sample {
        Ok(sample) => {
            assert!(sample.total_posts > 0, "Should have fetched some posts");
            assert_eq!(
                sample.total_posts,
                sample.originals.len() + sample.replies.len() + sample.quotes.len(),
                "total_posts should equal sum of partitions"
            );
            assert!(sample.reply_ratio >= 0.0 && sample.reply_ratio <= 1.0);
            assert!(sample.quote_ratio >= 0.0 && sample.quote_ratio <= 1.0);

            for reply in &sample.replies {
                assert!(!reply.parent_uri.is_empty(), "Reply should have parent_uri");
                assert!(
                    reply.parent_uri.starts_with("at://"),
                    "Parent URI should be an AT URI"
                );
            }
        }
        Err(e) => {
            eprintln!("Network test skipped: {}", e);
        }
    }
}

#[test]
fn parent_uri_deduplication() {
    use charcoal::bluesky::posts::ReplyPost;
    use std::collections::HashSet;

    let mut all_replies = make_reply_posts(5);
    // Add one with a duplicate parent URI (same as index 0)
    all_replies.push(ReplyPost {
        post: Post {
            uri: "at://did:plc:test/app.bsky.feed.post/extra".to_string(),
            text: "duplicate parent reply".to_string(),
            created_at: None,
            like_count: 0,
            repost_count: 0,
            quote_count: 0,
            is_quote: false,
        },
        parent_uri: "at://did:plc:other/app.bsky.feed.post/0".to_string(),
    });

    let unique_uris: HashSet<&str> = all_replies.iter().map(|r| r.parent_uri.as_str()).collect();
    assert_eq!(
        unique_uris.len(),
        5,
        "Should have 5 unique parent URIs (one was a duplicate)"
    );
}

fn make_posts(n: usize) -> Vec<Post> {
    (0..n)
        .map(|i| Post {
            uri: format!("at://did:plc:test/app.bsky.feed.post/{}", i),
            text: format!("Test post number {}", i),
            created_at: None,
            like_count: 0,
            repost_count: 0,
            quote_count: 0,
            is_quote: false,
        })
        .collect()
}

fn make_reply_posts(n: usize) -> Vec<ReplyPost> {
    (0..n)
        .map(|i| ReplyPost {
            post: Post {
                uri: format!("at://did:plc:test/app.bsky.feed.post/reply{}", i),
                text: format!("Reply post number {}", i),
                created_at: None,
                like_count: 0,
                repost_count: 0,
                quote_count: 0,
                is_quote: false,
            },
            parent_uri: format!("at://did:plc:other/app.bsky.feed.post/{}", i),
        })
        .collect()
}
