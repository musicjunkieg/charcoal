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
