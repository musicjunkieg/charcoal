# Topic-First Discovery & Adaptive Sampling Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the graph-first sweep with topic-first discovery, fix post sampling to include replies, add adaptive sampling with early stopping, fix the context score double-application bug, and prepare for Zentropi integration.

**Architecture:** The current `build_profile` fetches 50 original posts (no replies) and scores them all with ONNX. This plan changes the fetch to include replies, partitions posts for different consumers (fingerprinting vs toxicity), adds adaptive sampling stages, adds topic-based search discovery via `searchPosts`, and fixes the context score double-application bug. Each phase is independently shippable.

**Tech Stack:** Rust, AT Protocol public API (`getAuthorFeed`, `getPosts`, `searchPosts`), ONNX (existing), Zentropi CoPE API (Phase 5, future)

**Design docs:**
- Architecture briefing: `docs/2026-04-02-architecture-correction-briefing.md`
- Full design: `docs/2026-04-02-topic-first-discovery-and-sampling.md`

---

## Chunk 1: Phase 0 — Zentropi CoPE Spike + Phase 1a — Post Sampling

### Phase 0: Zentropi CoPE API Spike

**Goal:** Validate that Zentropi CoPE can serve as the binary toxicity classifier before building the architecture around it. This is a time-boxed research spike, not a production integration.

**What to test:**
1. Can we hit the API and get a response?
2. Does the policy prompt correctly distinguish ally vs hostile use of identity terms?
3. What's the throughput? Can the free tier handle 200-500 classifications per scan?
4. What's the latency per request?

#### Task 0.1: Zentropi API Client Spike

**Files:**
- Create: `src/toxicity/zentropi_spike.rs` (temporary — will be deleted or promoted after spike)
- Modify: `src/toxicity/mod.rs` (add module declaration)

- [ ] **Step 1: Write a minimal Zentropi client**

```rust
// src/toxicity/zentropi_spike.rs
//
// Spike: test Zentropi CoPE API for binary toxicity classification.
// This is temporary code for validation — will be promoted to production
// or deleted based on spike results.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const ZENTROPI_API_URL: &str = "https://api.zentropi.ai/v1/label";

#[derive(Serialize)]
struct ZentropiRequest {
    content_text: String,
    criteria_text: String,
}

#[derive(Deserialize, Debug)]
pub struct ZentropiResponse {
    pub label: String,
    pub confidence: f64,
    pub compute_time: f64,
}

impl ZentropiResponse {
    pub fn is_toxic(&self) -> bool {
        self.label == "1"
    }
}

/// Policy prompt for Charcoal's content moderation.
/// Distinguishes hostile intent from ally use of identity terms.
const MODERATION_POLICY: &str = r#"
## Overview
Classify social media posts as toxic (1) or not toxic (0) in the context
of online harassment and community safety.

## Definition of Terms
- "Toxic content" means posts that attack, demean, threaten, or belittle
  individuals or groups based on identity, appearance, or beliefs.
- "Reclaimed language" means identity terms used positively or neutrally
  by members of that community (e.g., "fat liberation", "queer joy").

## Label: Toxic (1)
### Includes
- Direct personal attacks, insults, or name-calling
- Dehumanizing language about identity groups
- Threats of violence or harm
- Mockery or contempt directed at specific people or groups
- Patronizing dismissal ("you people don't understand")
- Passive-aggressive hostility disguised as concern
- Sealioning or bad-faith questioning designed to exhaust

### Excludes
- Legitimate disagreement or criticism of ideas (not people)
- Profanity used for emphasis without targeting anyone ("fuck yeah!")
- Reclaimed identity language used positively ("fat liberation is important")
- Academic or journalistic discussion of sensitive topics
- Humor that isn't targeting a person or group
- Venting about systems or institutions (not individuals)
- Supportive or affirming use of identity terms
"#;

pub struct ZentropiSpike {
    client: reqwest::Client,
    api_key: String,
}

impl ZentropiSpike {
    pub fn new(api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
        }
    }

    /// Classify a single text. Returns the raw API response.
    pub async fn classify(&self, text: &str) -> Result<ZentropiResponse> {
        let request = ZentropiRequest {
            content_text: text.to_string(),
            criteria_text: MODERATION_POLICY.to_string(),
        };

        let response = self
            .client
            .post(ZENTROPI_API_URL)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await
            .context("Failed to reach Zentropi API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Zentropi API error {}: {}", status, body);
        }

        response
            .json::<ZentropiResponse>()
            .await
            .context("Failed to parse Zentropi response")
    }

    /// Classify a reply in the context of its parent post.
    /// Concatenates parent + reply into a single content field.
    pub async fn classify_pair(
        &self,
        parent_text: &str,
        reply_text: &str,
    ) -> Result<ZentropiResponse> {
        let combined = format!(
            "[Parent post]: {}\n\n[Reply]: {}",
            parent_text, reply_text
        );
        self.classify(&combined).await
    }

    /// Run the spike validation suite.
    /// Tests the policy with known ally vs hostile examples.
    pub async fn run_validation(&self) -> Result<SpikeResults> {
        let test_cases = vec![
            // Ally content — should be NOT toxic (label: 0)
            ("Fuck yeah, fat liberation!", false),
            ("Fat people deserve respect and dignity", false),
            ("As a queer person, I love seeing this representation", false),
            ("Trans rights are human rights, full stop", false),
            ("This is such a powerful discussion about body politics", false),
            // Hostile content — should be toxic (label: 1)
            ("Fat people are disgusting and lazy", true),
            ("You people are mentally ill and need help", true),
            ("Imagine being this stupid about basic biology", true),
            ("Another day, another delusional take from the woke mob", true),
            ("I bet your parents are so disappointed in what you became", true),
            // Ambiguous / edge cases
            ("I disagree with this policy approach entirely", false),
            ("This take is problematic and here's why...", false),
            ("You're wrong and this is harmful to the community", true),
        ];

        let mut results = SpikeResults::default();
        for (text, expected_toxic) in &test_cases {
            match self.classify(text).await {
                Ok(response) => {
                    let actual_toxic = response.is_toxic();
                    let correct = *expected_toxic == actual_toxic;
                    results.total += 1;
                    if correct {
                        results.correct += 1;
                    }
                    results.details.push(SpikeDetail {
                        text: text.to_string(),
                        expected_toxic: *expected_toxic,
                        actual_toxic,
                        confidence: response.confidence,
                        compute_time: response.compute_time,
                        correct,
                    });
                }
                Err(e) => {
                    results.errors.push(format!("Failed on '{}': {}", text, e));
                }
            }
        }
        results.accuracy = if results.total > 0 {
            results.correct as f64 / results.total as f64
        } else {
            0.0
        };
        Ok(results)
    }
}

#[derive(Debug, Default)]
pub struct SpikeResults {
    pub total: usize,
    pub correct: usize,
    pub accuracy: f64,
    pub errors: Vec<String>,
    pub details: Vec<SpikeDetail>,
}

#[derive(Debug)]
pub struct SpikeDetail {
    pub text: String,
    pub expected_toxic: bool,
    pub actual_toxic: bool,
    pub confidence: f64,
    pub compute_time: f64,
    pub correct: bool,
}
```

- [ ] **Step 2: Add module to mod.rs**

In `src/toxicity/mod.rs`, add:
```rust
pub mod zentropi_spike;
```

- [ ] **Step 3: Add a CLI command to run the spike**

Add a `zentropi-spike` subcommand to `src/main.rs` (or wherever CLI commands are defined). It should:
1. Read `ZENTROPI_API_KEY` from env
2. Run `ZentropiSpike::run_validation()`
3. Print results: accuracy, per-case results, average latency, any errors
4. Print a go/no-go recommendation based on accuracy >= 0.85 and avg latency < 2s

- [ ] **Step 4: Run the spike manually**

```bash
ZENTROPI_API_KEY=<key> cargo run -- zentropi-spike
```

Document results in a markdown file at `docs/spike-results/2026-04-XX-zentropi-spike.md`.

**Go/no-go criteria:**
- Accuracy >= 85% on the test suite (especially the ally cases)
- Average latency < 2 seconds per classification
- No rate limit errors on 13 sequential requests
- If no-go: document why, evaluate self-hosted CoPE-A-9B or Llama Guard 4

- [ ] **Step 5: Commit**

```bash
git add src/toxicity/zentropi_spike.rs src/toxicity/mod.rs src/main.rs
git commit -m 'feat: add Zentropi CoPE API spike for binary toxicity classification'
```

---

### Phase 1a: Reply-Inclusive Post Sampling

**Goal:** Change post fetching to include replies, partition posts into originals/replies/quotes, and compute reply ratio from the same data (eliminating the separate `fetch_reply_ratio` call).

**Key insight from design doc:** `fetch_recent_posts` uses `posts_no_replies` filter, which excludes replies — the exact posts where hostile behavior manifests. A person can post wholesome original content and be vicious in replies.

#### Task 1a.1: Add PostSample and ReplyPost Types

**Files:**
- Modify: `src/bluesky/posts.rs:16-25` (add new types after existing `Post` struct)

- [ ] **Step 1: Write the failing test**

Create test in `tests/unit_sampling.rs`:

```rust
// tests/unit_sampling.rs
//
// Tests for reply-inclusive post sampling types and partitioning logic.

use charcoal::bluesky::posts::{FingerprintQuality, Post, PostSample, ReplyPost};

#[test]
fn post_sample_reply_ratio_all_replies() {
    let sample = PostSample {
        originals: vec![],
        replies: vec![ReplyPost {
            post: Post {
                uri: "at://did:plc:test/app.bsky.feed.post/1".to_string(),
                text: "you're wrong".to_string(),
                created_at: None,
                like_count: 0,
                repost_count: 0,
                quote_count: 0,
                is_quote: false,
            },
            parent_uri: "at://did:plc:other/app.bsky.feed.post/2".to_string(),
        }],
        quotes: vec![],
        reply_ratio: 0.0,
        quote_ratio: 0.0,
        total_posts: 0,
    };
    // total_posts should be originals + replies + quotes
    assert_eq!(sample.originals.len(), 0);
    assert_eq!(sample.replies.len(), 1);
}

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
    assert_eq!(FingerprintQuality::from_counts(20, 30), FingerprintQuality::Normal);
}

#[test]
fn fingerprint_quality_mixed_fallback() {
    assert_eq!(FingerprintQuality::from_counts(10, 40), FingerprintQuality::Degraded);
}

#[test]
fn fingerprint_quality_insufficient() {
    assert_eq!(FingerprintQuality::from_counts(3, 8), FingerprintQuality::Unreliable);
}

#[test]
fn fingerprint_quality_zero_originals() {
    assert_eq!(FingerprintQuality::from_counts(0, 25), FingerprintQuality::Unreliable);
}

#[test]
fn fingerprint_quality_boundary_exactly_15_originals() {
    assert_eq!(FingerprintQuality::from_counts(15, 0), FingerprintQuality::Normal);
}

#[test]
fn fingerprint_quality_boundary_14_originals_with_replies() {
    // 14 originals + 1 reply = 15 total, but originals < 15 → Degraded
    assert_eq!(FingerprintQuality::from_counts(14, 1), FingerprintQuality::Degraded);
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
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --test unit_sampling -- --nocapture
```
Expected: FAIL — `PostSample`, `ReplyPost`, `FingerprintQuality` not defined

- [ ] **Step 3: Add the new types to posts.rs**

Add after the existing `Post` struct in `src/bluesky/posts.rs`:

```rust
/// A reply post with its parent URI for context pair formation.
#[derive(Debug, Clone)]
pub struct ReplyPost {
    pub post: Post,
    /// AT URI of the post being replied to (for fetching parent text)
    pub parent_uri: String,
}

/// Partitioned post sample from an account's feed.
///
/// Separates posts into originals, replies, and quotes so different
/// consumers can use the appropriate subset:
/// - Topic fingerprinting: originals (chosen topics, not inherited from arguments)
/// - Toxicity scoring: all posts, with replies weighted 70%
/// - Context pairs: replies with parent URIs for NLI/Zentropi pair scoring
#[derive(Debug, Clone)]
pub struct PostSample {
    /// Original posts (not replies, not quotes)
    pub originals: Vec<Post>,
    /// Reply posts with parent URI for context pair fetching
    pub replies: Vec<ReplyPost>,
    /// Quote posts (embed another post)
    pub quotes: Vec<Post>,
    /// Computed reply ratio (replies / total non-repost posts)
    pub reply_ratio: f64,
    /// Computed quote ratio (quotes / total non-repost posts)
    pub quote_ratio: f64,
    /// Total non-repost posts seen (denominator for ratios)
    pub total_posts: usize,
}

/// Quality of a topic fingerprint based on data availability.
///
/// When an account is reply-heavy, fingerprinting from originals alone
/// may produce unreliable results. This flag lets downstream scoring
/// account for fingerprint confidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FingerprintQuality {
    /// >= 15 originals — fingerprint from originals only
    Normal,
    /// < 15 originals but >= 15 total — fingerprint from all posts
    Degraded,
    /// < 15 total posts — fingerprint is unreliable
    Unreliable,
}

impl FingerprintQuality {
    /// Determine fingerprint quality from post counts.
    ///
    /// Special case: 0 originals is always Unreliable even if total >= 15,
    /// because fingerprinting entirely from replies captures the topics of
    /// people they're arguing with, not their own interests.
    pub fn from_counts(originals: usize, replies_and_quotes: usize) -> Self {
        if originals >= 15 {
            FingerprintQuality::Normal
        } else if originals == 0 {
            FingerprintQuality::Unreliable
        } else if originals + replies_and_quotes >= 15 {
            FingerprintQuality::Degraded
        } else {
            FingerprintQuality::Unreliable
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            FingerprintQuality::Normal => "normal",
            FingerprintQuality::Degraded => "degraded",
            FingerprintQuality::Unreliable => "unreliable",
        }
    }
}
```

Add `use serde::{Deserialize, Serialize};` to the imports in `posts.rs` if not already present.

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test --test unit_sampling -- --nocapture
```
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/bluesky/posts.rs tests/unit_sampling.rs
git commit -m 'feat: add PostSample, ReplyPost, and FingerprintQuality types for reply-inclusive sampling'
```

#### Task 1a.2: Implement fetch_posts_with_replies

**Files:**
- Modify: `src/bluesky/posts.rs` (add new function after `fetch_recent_posts`)

- [ ] **Step 1: Write the integration test**

This function hits the AT Protocol API, so it needs a live test. Add to `tests/unit_sampling.rs`:

```rust
/// Integration test — requires network access.
/// Fetches posts for a known public account and verifies partitioning.
#[tokio::test]
async fn fetch_posts_with_replies_partitions_correctly() {
    use charcoal::bluesky::client::PublicAtpClient;
    use charcoal::bluesky::posts;

    let client = PublicAtpClient::new(None);
    // Use a well-known active account that posts replies
    let sample = posts::fetch_posts_with_replies(&client, "bsky.app", 50).await;

    match sample {
        Ok(sample) => {
            // Basic structural assertions
            assert!(sample.total_posts > 0, "Should have fetched some posts");
            assert_eq!(
                sample.total_posts,
                sample.originals.len() + sample.replies.len() + sample.quotes.len(),
                "total_posts should equal sum of partitions"
            );
            assert!(sample.reply_ratio >= 0.0 && sample.reply_ratio <= 1.0);
            assert!(sample.quote_ratio >= 0.0 && sample.quote_ratio <= 1.0);

            // Every reply should have a non-empty parent_uri
            for reply in &sample.replies {
                assert!(
                    !reply.parent_uri.is_empty(),
                    "Reply should have parent_uri"
                );
                assert!(
                    reply.parent_uri.starts_with("at://"),
                    "Parent URI should be an AT URI"
                );
            }
        }
        Err(e) => {
            // Network tests can fail in CI — don't panic, just warn
            eprintln!("Network test skipped: {}", e);
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --test unit_sampling fetch_posts_with_replies -- --nocapture
```
Expected: FAIL — `fetch_posts_with_replies` not defined

- [ ] **Step 3: Implement fetch_posts_with_replies**

Add to `src/bluesky/posts.rs` after `fetch_recent_posts`:

```rust
/// Fetch recent posts with replies included, partitioned into a PostSample.
///
/// Uses the `posts_with_replies` filter to get both original posts and replies
/// from the same API call. Partitions into originals, replies (with parent URIs),
/// and quotes. Computes reply and quote ratios from the same data.
///
/// This replaces the pattern of calling `fetch_recent_posts` + `fetch_reply_ratio`
/// separately — one API call yields both toxicity-scoreable text AND behavioral ratios.
pub async fn fetch_posts_with_replies(
    client: &PublicAtpClient,
    handle: &str,
    max_posts: usize,
) -> Result<PostSample> {
    let mut originals = Vec::new();
    let mut replies = Vec::new();
    let mut quotes = Vec::new();
    let mut cursor: Option<String> = None;
    let mut total_seen = 0usize;

    let page_size = max_posts.min(100).to_string();

    loop {
        let mut params: Vec<(&str, &str)> = vec![
            ("actor", handle),
            ("filter", "posts_with_replies"),
            ("limit", &page_size),
        ];
        if let Some(ref c) = cursor {
            params.push(("cursor", c));
        }

        let output: get_author_feed::Output = client
            .xrpc_get("app.bsky.feed.getAuthorFeed", &params)
            .await
            .with_context(|| format!("Failed to fetch feed for @{}", handle))?;

        for feed_item in &output.feed {
            // Skip reposts — they aren't authored by this account
            if feed_item.reason.is_some() {
                continue;
            }

            let post_view = &feed_item.post;

            // Decode the record to get post text and reply metadata
            let record = match atrium_api::app::bsky::feed::post::Record::try_from_unknown(
                post_view.record.clone(),
            ) {
                Ok(r) => r,
                Err(_) => continue,
            };

            let text = record.data.text.clone();

            // Skip very short posts (likely just links/images)
            if text.chars().count() < 15 {
                continue;
            }

            // Detect quote-posts by checking the embed type
            let is_quote = post_view.embed.as_ref().is_some_and(|embed| {
                use atrium_api::types::Union;
                matches!(
                    embed,
                    Union::Refs(
                        atrium_api::app::bsky::feed::defs::PostViewEmbedRefs::AppBskyEmbedRecordView(_)
                            | atrium_api::app::bsky::feed::defs::PostViewEmbedRefs::AppBskyEmbedRecordWithMediaView(_)
                    )
                )
            });

            let post = Post {
                uri: post_view.uri.clone(),
                text,
                created_at: Some(post_view.indexed_at.as_ref().to_string()),
                like_count: post_view.like_count.unwrap_or(0),
                repost_count: post_view.repost_count.unwrap_or(0),
                quote_count: post_view.quote_count.unwrap_or(0),
                is_quote,
            };

            // Check if this is a reply by examining the reply field
            let is_reply = feed_item.reply.is_some();

            // Extract parent URI from the reply reference
            let parent_uri = if is_reply {
                record
                    .data
                    .reply
                    .as_ref()
                    .map(|r| r.parent.uri.clone())
            } else {
                None
            };

            total_seen += 1;

            if is_reply {
                if let Some(parent) = parent_uri {
                    replies.push(ReplyPost {
                        post,
                        parent_uri: parent,
                    });
                } else {
                    // Reply without parent URI — treat as original
                    originals.push(post);
                }
            } else if is_quote {
                quotes.push(post);
            } else {
                originals.push(post);
            }

            if total_seen >= max_posts {
                break;
            }
        }

        debug!(
            page_posts = output.feed.len(),
            originals = originals.len(),
            replies = replies.len(),
            quotes = quotes.len(),
            total = total_seen,
            "Fetched page of posts (with replies) for @{}",
            handle
        );

        if total_seen >= max_posts {
            break;
        }

        cursor = output.data.cursor.clone();
        if cursor.is_none() || output.feed.is_empty() {
            break;
        }
    }

    let total_posts = originals.len() + replies.len() + quotes.len();
    let reply_ratio = if total_posts > 0 {
        replies.len() as f64 / total_posts as f64
    } else {
        0.0
    };
    let quote_ratio = if total_posts > 0 {
        quotes.len() as f64 / total_posts as f64
    } else {
        0.0
    };

    info!(
        handle,
        originals = originals.len(),
        replies = replies.len(),
        quotes = quotes.len(),
        reply_ratio = format!("{:.2}", reply_ratio),
        "Collected partitioned posts"
    );

    Ok(PostSample {
        originals,
        replies,
        quotes,
        reply_ratio,
        quote_ratio,
        total_posts,
    })
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test --test unit_sampling -- --nocapture
```
Expected: PASS (network test may skip in offline environments)

- [ ] **Step 5: Run clippy**

```bash
cargo clippy --all-targets --features web
```
Expected: No new warnings

- [ ] **Step 6: Commit**

```bash
git add src/bluesky/posts.rs tests/unit_sampling.rs
git commit -m 'feat: add fetch_posts_with_replies for reply-inclusive post sampling'
```

#### Task 1a.3: Add Batch Parent Post Fetching

**Files:**
- Modify: `src/bluesky/posts.rs` (add `fetch_parent_posts` function)

- [ ] **Step 1: Write the test**

Add to `tests/unit_sampling.rs`:

```rust
#[test]
fn parent_uri_deduplication() {
    // Verify that collecting parent URIs from replies deduplicates correctly
    use std::collections::HashSet;

    let replies = make_reply_posts(5);
    // Add some with duplicate parent URIs
    let mut all_replies = replies;
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
        parent_uri: "at://did:plc:other/app.bsky.feed.post/0".to_string(), // same as first
    });

    let unique_uris: HashSet<&str> = all_replies.iter().map(|r| r.parent_uri.as_str()).collect();
    assert_eq!(unique_uris.len(), 5, "Should have 5 unique parent URIs (one was a duplicate)");
}
```

- [ ] **Step 2: Run test to verify it passes** (this is a pure logic test)

```bash
cargo test --test unit_sampling parent_uri -- --nocapture
```

- [ ] **Step 3: Implement fetch_parent_posts**

Add to `src/bluesky/posts.rs`:

```rust
/// Batch-fetch parent post texts for reply context pair formation.
///
/// Given a set of AT URIs, fetches the post texts via `getPosts` (up to 25
/// per call, as per API limit). Returns a map from URI to text.
///
/// Used to form (parent_text, reply_text) pairs for Zentropi or NLI scoring.
pub async fn fetch_parent_posts(
    client: &PublicAtpClient,
    parent_uris: &[String],
) -> Result<std::collections::HashMap<String, String>> {
    use std::collections::HashMap;

    let mut result = HashMap::new();
    if parent_uris.is_empty() {
        return Ok(result);
    }

    // Deduplicate URIs
    let unique_uris: Vec<&str> = {
        let mut seen = std::collections::HashSet::new();
        parent_uris
            .iter()
            .filter(|u| seen.insert(u.as_str()))
            .map(|u| u.as_str())
            .collect()
    };

    // getPosts accepts up to 25 URIs per call
    for chunk in unique_uris.chunks(25) {
        let uris_param = chunk.join(",");
        // getPosts takes multiple `uris` params, not a comma-separated list.
        // Build the query params manually.
        let params: Vec<(&str, &str)> = chunk.iter().map(|uri| ("uris", *uri)).collect();

        match client
            .xrpc_get::<get_posts::Output>("app.bsky.feed.getPosts", &params)
            .await
        {
            Ok(output) => {
                for post_view in &output.posts {
                    if let Ok(record) =
                        atrium_api::app::bsky::feed::post::Record::try_from_unknown(
                            post_view.record.clone(),
                        )
                    {
                        result.insert(post_view.uri.clone(), record.data.text.clone());
                    }
                }
            }
            Err(e) => {
                warn!(
                    chunk_size = chunk.len(),
                    error = %e,
                    "Failed to fetch parent posts batch, skipping"
                );
            }
        }
    }

    debug!(
        requested = parent_uris.len(),
        fetched = result.len(),
        "Fetched parent posts for context pairs"
    );

    Ok(result)
}
```

- [ ] **Step 4: Run full test suite**

```bash
cargo test --features web
```
Expected: All existing tests pass, new tests pass

- [ ] **Step 5: Commit**

```bash
git add src/bluesky/posts.rs tests/unit_sampling.rs
git commit -m 'feat: add batch parent post fetching for reply context pairs'
```

---

## Chunk 2: Phase 1b — Scoring Formula Changes

### Phase 1b: Reply-Weighted Toxicity, Fingerprint Quality, Context Score Fix

**Goal:** Update `build_profile` to use the new reply-inclusive fetch, implement reply-weighted toxicity, originals-first fingerprinting with quality tracking, fix the context score double-application bug, and add schema migration for new fields.

#### Task 1b.1: Fix Context Score Double-Application Bug

**Files:**
- Modify: `src/scoring/behavioral.rs:149-179` (change `apply_behavioral_modifier_contextual` return type)
- Modify: `src/scoring/profile.rs:298-318` (fix double application)
- Modify: `tests/unit_context.rs` (add regression test)

- [ ] **Step 1: Write the failing test**

Add to `tests/unit_context.rs`:

```rust
#[test]
fn context_score_no_double_application() {
    use charcoal::scoring::behavioral;

    // Concern troll: context_score = 0.8, behaviorally benign
    // The gate bypass should consume the context signal — don't multiply again
    let raw_score = 20.0;
    let quote_ratio = 0.05; // benign
    let reply_ratio = 0.10; // benign
    let pile_on = false;
    let avg_engagement = 50.0; // above median
    let median_engagement = 10.0;
    let context_score = Some(0.8);

    let (score, _benign_gate, gate_was_bypassed) =
        behavioral::apply_behavioral_modifier_contextual(
            raw_score,
            quote_ratio,
            reply_ratio,
            pile_on,
            avg_engagement,
            median_engagement,
            context_score,
        );

    // Gate was bypassed due to context_score >= 0.5
    assert!(gate_was_bypassed, "Gate should have been bypassed");

    // Score should NOT then be multiplied by context_multiplier
    // With the bug: score = 20 * 1.0 (boost) * 1.4 (context) = 28.0
    // Without the bug: score = 20 * 1.0 (boost) = 20.0 (gate bypass consumed context)
    assert!(
        score < 25.0,
        "Score should not be double-amplified by context, got {}",
        score
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --test unit_context context_score_no_double -- --nocapture
```
Expected: FAIL — `apply_behavioral_modifier_contextual` returns `(f64, bool)`, not `(f64, bool, bool)`

- [ ] **Step 3: Update apply_behavioral_modifier_contextual**

In `src/scoring/behavioral.rs`, change the function to return a 3-tuple:

```rust
/// Apply the behavioral modifier with optional contextual override.
///
/// When context_score >= 0.5, the benign gate is bypassed — an account that
/// looks benign in isolation but is hostile in direct interactions with the
/// protected user's content is exactly the concern troll this system detects.
///
/// Returns (modified_score, benign_gate_applied, gate_was_bypassed_by_context).
pub fn apply_behavioral_modifier_contextual(
    raw_score: f64,
    quote_ratio: f64,
    reply_ratio: f64,
    pile_on: bool,
    avg_engagement: f64,
    median_engagement: f64,
    context_score: Option<f64>,
) -> (f64, bool, bool) {
    let context_overrides_gate = context_score.map(|cs| cs >= 0.5).unwrap_or(false);

    if context_overrides_gate {
        // Skip benign gate check, but still apply hostile multiplier
        let boost = compute_behavioral_boost(quote_ratio, reply_ratio, pile_on);
        ((raw_score * boost).clamp(0.0, 100.0), false, true)
    } else {
        let (score, benign_gate) = apply_behavioral_modifier(
            raw_score,
            quote_ratio,
            reply_ratio,
            pile_on,
            avg_engagement,
            median_engagement,
        );
        (score, benign_gate, false)
    }
}
```

- [ ] **Step 4: Update build_profile to use the new return type and fix double application**

In `src/scoring/profile.rs`, update the call site (~line 298):

```rust
    let (score_with_behavioral, benign_gate, gate_was_bypassed) =
        behavioral::apply_behavioral_modifier_contextual(
            raw_score,
            quote_ratio,
            reply_ratio,
            pile_on,
            avg_engagement,
            median_engagement,
            context_score,
        );

    // Only apply context multiplier if gate wasn't bypassed by context.
    // When the gate is bypassed due to context_score >= 0.5, context has
    // already done its work — don't multiply again on top of it.
    let context_multiplier = match (context_score, gate_was_bypassed) {
        (Some(ctx), false) => 1.0 + (ctx * 0.5), // normal: context boosts
        (Some(_), true) => 1.0,                   // gate bypass consumed context
        (None, _) => 1.0,
    };
```

- [ ] **Step 5: Fix all existing callers and tests that use the old 2-tuple return**

Search for all uses of `apply_behavioral_modifier_contextual`:

```bash
cargo test --features web 2>&1 | head -50
```

Fix any compilation errors in test files that destructure the old 2-tuple.

- [ ] **Step 6: Run all tests**

```bash
cargo test --features web
```
Expected: All tests pass including the new regression test

- [ ] **Step 7: Commit**

```bash
git add src/scoring/behavioral.rs src/scoring/profile.rs tests/unit_context.rs
git commit -m 'fix: prevent context score double-application in concern troll scoring'
```

#### Task 1b.2: Schema Migration v8 — Add fingerprint_quality and scoring_confidence

**Files:**
- Modify: `src/db/schema.rs` (add migration v8)
- Modify: `src/db/models.rs` (add fields to AccountScore)

- [ ] **Step 1: Write the test**

Add to `tests/unit_scoring.rs`:

```rust
#[test]
fn scoring_confidence_ordering() {
    use charcoal::db::models::ScoringConfidence;

    assert_eq!(ScoringConfidence::Low.staleness_days(), 3);
    assert_eq!(ScoringConfidence::Standard.staleness_days(), 7);
    assert_eq!(ScoringConfidence::High.staleness_days(), 14);
}

#[test]
fn fingerprint_quality_serialization() {
    use charcoal::bluesky::posts::FingerprintQuality;

    assert_eq!(FingerprintQuality::Normal.as_str(), "normal");
    assert_eq!(FingerprintQuality::Degraded.as_str(), "degraded");
    assert_eq!(FingerprintQuality::Unreliable.as_str(), "unreliable");
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --test unit_scoring scoring_confidence -- --nocapture
```
Expected: FAIL — `ScoringConfidence` not defined

- [ ] **Step 3: Add ScoringConfidence to models.rs**

Add to `src/db/models.rs`:

```rust
/// Confidence level of a scoring result based on data volume.
///
/// Used to prioritize re-scoring: Low confidence accounts are re-scored
/// sooner (3 days) than High confidence accounts (14 days).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScoringConfidence {
    /// < 25 posts analyzed, early exit
    Low,
    /// 25-50 posts, standard sampling
    Standard,
    /// 50+ posts, full analysis with context pairs
    High,
}

impl ScoringConfidence {
    /// Number of days before this score is considered stale.
    pub fn staleness_days(&self) -> i64 {
        match self {
            ScoringConfidence::Low => 3,
            ScoringConfidence::Standard => 7,
            ScoringConfidence::High => 14,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ScoringConfidence::Low => "low",
            ScoringConfidence::Standard => "standard",
            ScoringConfidence::High => "high",
        }
    }
}
```

Add new fields to `AccountScore`:

```rust
pub struct AccountScore {
    // ... existing fields ...
    /// Quality of the topic fingerprint used for overlap scoring
    pub fingerprint_quality: Option<String>,
    /// Confidence level of this scoring result
    pub scoring_confidence: Option<String>,
}
```

- [ ] **Step 4: Add schema migration v8**

Add to `src/db/schema.rs` after the v7 migration:

```rust
    // Migration v8: add fingerprint_quality and scoring_confidence to account_scores.
    // fingerprint_quality tracks whether the fingerprint was built from originals only
    // (normal), mixed (degraded), or insufficient data (unreliable).
    // scoring_confidence tracks the depth of analysis (low/standard/high).
    run_migration(conn, 8, |c| {
        c.execute_batch(
            "
            ALTER TABLE account_scores ADD COLUMN fingerprint_quality TEXT;
            ALTER TABLE account_scores ADD COLUMN scoring_confidence TEXT;
            ",
        )
    })?;
```

- [ ] **Step 5: Update all AccountScore construction sites**

Every place that builds an `AccountScore` needs the two new fields. Search for `AccountScore {` and add:
```rust
    fingerprint_quality: None,
    scoring_confidence: None,
```

This includes:
- `src/scoring/profile.rs` (the main builder — will be updated to set real values in Task 1b.3)
- `src/db/sqlite.rs` (query result mapping)
- Any test helpers

- [ ] **Step 6: Update SQLite read/write queries**

In `src/db/sqlite.rs`, update the `upsert_account_score` INSERT/UPDATE to include the new columns, and update the SELECT in `get_ranked_threats` and similar queries.

- [ ] **Step 7: Update Postgres queries** (if applicable)

Check `src/db/postgres.rs` for similar changes needed.

- [ ] **Step 8: Run all tests**

```bash
cargo test --features web
```
Expected: All tests pass

- [ ] **Step 9: Commit**

```bash
git add src/db/schema.rs src/db/models.rs src/db/sqlite.rs src/scoring/profile.rs
git commit -m 'feat: add fingerprint_quality and scoring_confidence fields (schema v8)'
```

#### Task 1b.3: Update build_profile to Use Reply-Inclusive Fetch

**Files:**
- Modify: `src/scoring/profile.rs` (major refactor of build_profile)

- [ ] **Step 1: Write tests for reply-weighted toxicity**

Add to `tests/unit_scoring.rs`:

```rust
#[test]
fn reply_weighted_toxicity_hostile_replies_clean_originals() {
    use charcoal::scoring::profile::compute_reply_weighted_toxicity;

    // 12/30 replies toxic, 0/20 originals toxic
    let result = compute_reply_weighted_toxicity(12, 30, 0, 20);
    // reply_tox_rate = 12/30 = 0.40
    // original_tox_rate = 0/20 = 0.0
    // weighted = 0.40 * 0.7 + 0.0 * 0.3 = 0.28
    assert!((result - 0.28).abs() < 0.001, "Expected 0.28, got {}", result);
}

#[test]
fn reply_weighted_toxicity_falls_back_when_few_replies() {
    use charcoal::scoring::profile::compute_reply_weighted_toxicity;

    // Only 3 replies — below threshold of 5, falls back to flat rate
    let result = compute_reply_weighted_toxicity(2, 3, 4, 20);
    // flat rate = (2 + 4) / (3 + 20) = 6/23 ≈ 0.2609
    assert!((result - 6.0 / 23.0).abs() < 0.001, "Expected flat rate, got {}", result);
}

#[test]
fn reply_weighted_toxicity_zero_posts() {
    use charcoal::scoring::profile::compute_reply_weighted_toxicity;

    let result = compute_reply_weighted_toxicity(0, 0, 0, 0);
    assert!((result - 0.0).abs() < 0.001);
}

#[test]
fn reply_weighted_toxicity_all_replies_toxic() {
    use charcoal::scoring::profile::compute_reply_weighted_toxicity;

    let result = compute_reply_weighted_toxicity(20, 20, 5, 10);
    // reply_tox_rate = 20/20 = 1.0
    // original_tox_rate = 5/10 = 0.5
    // weighted = 1.0 * 0.7 + 0.5 * 0.3 = 0.85
    assert!((result - 0.85).abs() < 0.001, "Expected 0.85, got {}", result);
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --test unit_scoring reply_weighted -- --nocapture
```
Expected: FAIL — `compute_reply_weighted_toxicity` not defined

- [ ] **Step 3: Add compute_reply_weighted_toxicity to profile.rs**

```rust
/// Minimum number of replies to use reply-weighted toxicity.
/// Below this, falls back to flat rate across all posts.
const MIN_REPLIES_FOR_WEIGHTING: usize = 5;

/// Compute reply-weighted toxicity rate.
///
/// Reply toxicity is weighted 70% and original toxicity 30%, because
/// hostile behavior manifests in replies — not original posts. An account
/// can post wholesome original content and be vicious in replies.
///
/// Falls back to flat rate when there are fewer than 5 replies (insufficient
/// interactive data to weight reliably).
///
/// Arguments are counts of toxic posts by type, not continuous scores.
/// When using ONNX only (pre-Zentropi), these counts come from
/// `weighted_toxicity()` exceeding 0.5 (the category-weighted threshold).
/// This is imperfect for identity-adjacent content but is the best we
/// can do without Zentropi. When Zentropi is active (Phase 5), counts
/// come from Zentropi binary labels instead.
pub fn compute_reply_weighted_toxicity(
    toxic_replies: usize,
    total_replies: usize,
    toxic_originals: usize,
    total_originals: usize,
) -> f64 {
    let total = total_replies + total_originals;
    if total == 0 {
        return 0.0;
    }

    if total_replies < MIN_REPLIES_FOR_WEIGHTING {
        // Flat rate fallback
        let toxic_total = toxic_replies + toxic_originals;
        return toxic_total as f64 / total as f64;
    }

    let reply_tox_rate = if total_replies > 0 {
        toxic_replies as f64 / total_replies as f64
    } else {
        0.0
    };

    let original_tox_rate = if total_originals > 0 {
        toxic_originals as f64 / total_originals as f64
    } else {
        0.0
    };

    reply_tox_rate * 0.7 + original_tox_rate * 0.3
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test --test unit_scoring reply_weighted -- --nocapture
```
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/scoring/profile.rs tests/unit_scoring.rs
git commit -m 'feat: add reply-weighted toxicity computation'
```

#### Task 1b.4: Refactor build_profile to Use PostSample

**Files:**
- Modify: `src/scoring/profile.rs` (refactor to use `fetch_posts_with_replies`)
- Modify: `src/pipeline/amplification.rs` (update call site)
- Modify: `src/pipeline/sweep.rs` (update call site)

This is the integration task that wires the new sampling into the scoring pipeline. The `build_profile` function signature changes to accept a `PostSample` instead of fetching posts internally, or it fetches via the new function.

- [ ] **Step 1: Update build_profile to use fetch_posts_with_replies**

Replace the post-fetching section of `build_profile` (~lines 55-78):

```rust
    // Step 1: Fetch the target's posts with replies included
    let sample = posts::fetch_posts_with_replies(client, target_handle, 50).await?;

    if sample.total_posts < 5 {
        info!(
            handle = target_handle,
            post_count = sample.total_posts,
            "Insufficient posts for reliable scoring"
        );
        return Ok(AccountScore {
            did: target_did.to_string(),
            handle: target_handle.to_string(),
            toxicity_score: None,
            topic_overlap: None,
            threat_score: None,
            threat_tier: Some("Insufficient Data".to_string()),
            posts_analyzed: sample.total_posts as u32,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
            fingerprint_quality: None,
            scoring_confidence: None,
        });
    }

    // Step 2: Determine fingerprint quality and select posts for fingerprinting
    let fp_quality = FingerprintQuality::from_counts(
        sample.originals.len(),
        sample.replies.len() + sample.quotes.len(),
    );

    let fingerprint_posts: Vec<String> = if sample.originals.len() >= 15 {
        // Enough originals — fingerprint from chosen topics only
        sample.originals.iter().map(|p| p.text.clone()).collect()
    } else {
        // Fall back to all posts for fingerprinting
        sample.originals.iter().map(|p| p.text.clone())
            .chain(sample.replies.iter().map(|r| r.post.text.clone()))
            .chain(sample.quotes.iter().map(|p| p.text.clone()))
            .collect()
    };

    // All posts go to toxicity scoring — replies weighted more heavily
    let all_post_texts: Vec<String> = sample.replies.iter().map(|r| r.post.text.clone())
        .chain(sample.quotes.iter().map(|p| p.text.clone()))
        .chain(sample.originals.iter().map(|p| p.text.clone()))
        .collect();
```

Then update the toxicity scoring section to use `all_post_texts` instead of `post_texts`, and fingerprinting to use `fingerprint_posts`.

Update the reply ratio computation to use the pre-computed values from `PostSample` instead of calling `fetch_reply_ratio` separately:

```rust
    // Reply and quote ratios come from the PostSample — no separate API call needed
    let reply_ratio = sample.reply_ratio;
    let quote_ratio = sample.quote_ratio;
```

Remove the `fetch_reply_ratio` call block (~lines 146-158).

Set the new fields on the returned `AccountScore`:

```rust
    fingerprint_quality: Some(fp_quality.as_str().to_string()),
    scoring_confidence: Some("standard".to_string()), // Phase 2 will make this adaptive
```

- [ ] **Step 2: Update the topic overlap computation to use fingerprint_posts**

```rust
    let topic_overlap = if let (Some(emb), Some(protected_emb)) = (embedder, protected_embedding) {
        let target_embeddings = emb.embed_batch(&fingerprint_posts).await?;
        let target_mean = embeddings::mean_embedding(&target_embeddings);
        embeddings::cosine_similarity_embeddings(protected_emb, &target_mean)
    } else {
        let topic_extractor = TfIdfExtractor {
            top_n_keywords: 40,
            max_clusters: 7,
        };
        let target_fingerprint = topic_extractor.extract(&fingerprint_posts)?;
        overlap::cosine_similarity(protected_fingerprint, &target_fingerprint)
    };
```

- [ ] **Step 3: Update all_posts reference for engagement computation**

Build a combined `Vec<&Post>` for engagement computation:

```rust
    // Collect all posts for engagement computation
    let all_posts_for_engagement: Vec<&Post> = sample.originals.iter()
        .chain(sample.replies.iter().map(|r| &r.post))
        .chain(sample.quotes.iter())
        .collect();
    let avg_engagement = behavioral::compute_avg_engagement_refs(&all_posts_for_engagement);
```

Add `compute_avg_engagement_refs` to `behavioral.rs`:

```rust
/// Compute mean engagement from post references (avoids cloning).
pub fn compute_avg_engagement_refs(posts: &[&Post]) -> f64 {
    if posts.is_empty() {
        return 0.0;
    }
    let total: f64 = posts
        .iter()
        .map(|p| (p.like_count + p.repost_count) as f64)
        .sum();
    total / posts.len() as f64
}
```

- [ ] **Step 4: Run all tests**

```bash
cargo test --features web
```

Fix any compilation errors from the refactored `build_profile`.

- [ ] **Step 5: Commit**

```bash
git add src/scoring/profile.rs src/scoring/behavioral.rs src/bluesky/posts.rs src/pipeline/amplification.rs src/pipeline/sweep.rs
git commit -m 'feat: refactor build_profile to use reply-inclusive sampling and originals-first fingerprinting'
```

#### Task 1b.5: Composition Tests for New Scoring Personas

**Files:**
- Create: `tests/unit_sampling_personas.rs`

- [ ] **Step 1: Write persona tests**

```rust
// tests/unit_sampling_personas.rs
//
// Persona-based composition tests for the new sampling and scoring pipeline.
// These test the scenarios described in the design doc.

use charcoal::bluesky::posts::{FingerprintQuality, Post, PostSample, ReplyPost};
use charcoal::scoring::profile::compute_reply_weighted_toxicity;

/// The Hidden Hostile: wholesome originals, vicious replies
#[test]
fn persona_hidden_hostile() {
    // 0/20 originals toxic, 12/30 replies toxic
    let tox_rate = compute_reply_weighted_toxicity(12, 30, 0, 20);
    // reply_tox = 0.40, original_tox = 0.0
    // weighted = 0.40 * 0.7 + 0.0 * 0.3 = 0.28
    assert!(
        (tox_rate - 0.28).abs() < 0.01,
        "Hidden hostile should have weighted tox ~0.28, got {}",
        tox_rate
    );
    // Compare to flat rate: 12/50 = 0.24 — reply weighting surfaces the threat
    let flat_rate = 12.0 / 50.0;
    assert!(
        tox_rate > flat_rate,
        "Reply-weighted rate should be higher than flat rate for hidden hostiles"
    );
}

/// The Reply-Heavy Account: 4 originals about cooking, 46 hostile replies on fat lib topics
#[test]
fn persona_reply_heavy_fingerprinting() {
    let fp_quality = FingerprintQuality::from_counts(4, 46);
    assert_eq!(
        fp_quality,
        FingerprintQuality::Degraded,
        "4 originals + 46 replies should use degraded fingerprint"
    );
}

/// Account with zero originals — all replies
#[test]
fn persona_all_replies() {
    let fp_quality = FingerprintQuality::from_counts(0, 35);
    assert_eq!(
        fp_quality,
        FingerprintQuality::Unreliable,
        "0 originals should always be unreliable regardless of reply count"
    );
}

/// The Efficient Clean Account: early exit candidate
#[test]
fn persona_clean_account_would_early_exit() {
    // 25 posts, all ONNX < 0.10, overlap 0.08
    // In Phase 2, this account exits at Stage 1
    // For now, verify the flat rate is 0.0 (no toxic posts)
    let tox_rate = compute_reply_weighted_toxicity(0, 10, 0, 15);
    assert!((tox_rate - 0.0).abs() < 0.001);
}

/// The Borderline Concern Troll: context_score 0.8, benign behavioral signals
#[test]
fn persona_concern_troll_no_double_count() {
    use charcoal::scoring::behavioral;

    // Concern troll: benign behavior but high context score
    let raw_score = 20.0;
    let (score, _benign_gate, gate_bypassed) =
        behavioral::apply_behavioral_modifier_contextual(
            raw_score,
            0.05,  // low quote ratio (benign)
            0.10,  // low reply ratio (benign)
            false, // no pile-on
            50.0,  // above median engagement
            10.0,  // median
            Some(0.8),
        );

    assert!(gate_bypassed, "Gate should be bypassed for context >= 0.5");

    // After gate bypass, context multiplier should NOT apply
    // So the final score should be raw_score * behavioral_boost (≈1.0)
    // NOT raw_score * behavioral_boost * context_multiplier
    assert!(
        score < 25.0,
        "Score should not be double-amplified, got {}",
        score
    );
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --test unit_sampling_personas -- --nocapture
```
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add tests/unit_sampling_personas.rs
git commit -m 'test: add persona-based composition tests for new sampling pipeline'
```

---

## Chunk 3: Phase 2 — Adaptive Sampling

### Phase 2: Three-Stage Adaptive Sampling with Early Stopping

**Goal:** Reduce scoring cost by 50-60% on clean accounts through early stopping. Accounts that are clearly clean and topically irrelevant exit after 25 posts. Borderline accounts get extended analysis.

**Dependency:** Phase 1a+1b (needs PostSample and reply-inclusive fetch)

#### Task 2.1: Add Adaptive Sampling Logic

**Files:**
- Modify: `src/scoring/profile.rs` (add staged sampling)
- Modify: `tests/unit_scoring.rs` (add stage tests)

- [ ] **Step 1: Write the failing test**

Add to `tests/unit_scoring.rs`:

```rust
#[test]
fn early_exit_clean_and_irrelevant() {
    use charcoal::scoring::profile::should_early_exit_stage1;

    // All ONNX scores < 0.10 and overlap < 0.15 → exit
    let onnx_scores = vec![0.02, 0.05, 0.03, 0.01, 0.08];
    let topic_overlap = 0.08;
    assert!(should_early_exit_stage1(&onnx_scores, topic_overlap, 0.15));
}

#[test]
fn no_early_exit_if_any_onnx_above_threshold() {
    use charcoal::scoring::profile::should_early_exit_stage1;

    let onnx_scores = vec![0.02, 0.05, 0.15, 0.01, 0.08];
    let topic_overlap = 0.08;
    assert!(!should_early_exit_stage1(&onnx_scores, topic_overlap, 0.15));
}

#[test]
fn no_early_exit_if_overlap_above_gate() {
    use charcoal::scoring::profile::should_early_exit_stage1;

    let onnx_scores = vec![0.02, 0.05, 0.03, 0.01, 0.08];
    let topic_overlap = 0.20;
    assert!(!should_early_exit_stage1(&onnx_scores, topic_overlap, 0.15));
}

#[test]
fn stage2_resolves_when_clear_signal() {
    use charcoal::scoring::profile::should_continue_to_stage3;

    // Score is 22.0, not near any tier boundary (8, 15, 35) ± 5
    assert!(!should_continue_to_stage3(22.0));
}

#[test]
fn stage2_continues_when_near_boundary() {
    use charcoal::scoring::profile::should_continue_to_stage3;

    // Score is 13.0, within ±5 of Watch/Elevated boundary at 15.0
    assert!(should_continue_to_stage3(13.0));

    // Score is 6.0, within ±5 of Low/Watch boundary at 8.0
    assert!(should_continue_to_stage3(6.0));

    // Score is 37.0, within ±5 of Elevated/High boundary at 35.0
    assert!(should_continue_to_stage3(37.0));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --test unit_scoring early_exit -- --nocapture
cargo test --test unit_scoring stage2 -- --nocapture
```
Expected: FAIL — functions not defined

- [ ] **Step 3: Implement the stage decision functions**

Add to `src/scoring/profile.rs`:

```rust
/// ONNX clean-pass threshold. Posts below this are genuinely clean — no
/// identity terms, no hostility. Posts at or above need secondary classification.
const ONNX_CLEAN_THRESHOLD: f64 = 0.10;

/// Check if an account can exit early at Stage 1 (25 posts).
///
/// Exits when ALL ONNX scores are below the clean threshold AND topic
/// overlap is below the gate threshold. This catches the ~50-60% of
/// sweep accounts that are clearly clean and topically irrelevant.
///
/// ONNX is ONLY reliable for low scores. A low score genuinely means
/// no hostile language or identity terms. High scores are NOT trustworthy
/// (keyword triggering on identity terms).
pub fn should_early_exit_stage1(
    onnx_scores: &[f64],
    topic_overlap: f64,
    overlap_gate_threshold: f64,
) -> bool {
    topic_overlap < overlap_gate_threshold && onnx_scores.iter().all(|&s| s < ONNX_CLEAN_THRESHOLD)
}

/// Tier boundary proximity thresholds.
const TIER_BOUNDARIES: [f64; 3] = [8.0, 15.0, 35.0]; // Watch, Elevated, High
const BOUNDARY_MARGIN: f64 = 5.0;

/// Check if a Stage 2 score is near a tier boundary and needs Stage 3.
///
/// Returns true if the score is within ±5 points of any tier boundary,
/// meaning more data could change the tier classification.
pub fn should_continue_to_stage3(score: f64) -> bool {
    TIER_BOUNDARIES
        .iter()
        .any(|&boundary| (score - boundary).abs() <= BOUNDARY_MARGIN)
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test --test unit_scoring early_exit -- --nocapture
cargo test --test unit_scoring stage -- --nocapture
```
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/scoring/profile.rs tests/unit_scoring.rs
git commit -m 'feat: add adaptive sampling stage decision functions'
```

#### Task 2.2: Wire Adaptive Sampling into build_profile

**Files:**
- Modify: `src/scoring/profile.rs` (restructure into staged sampling)

This is a significant refactor of `build_profile`. The function changes from "fetch 50 posts, score all" to "fetch 25, check early exit, optionally fetch more."

- [ ] **Step 1: Refactor build_profile into staged execution**

The key changes:
1. First fetch: 25 posts via `fetch_posts_with_replies(client, handle, 25)`
2. Compute preliminary ONNX scores and topic overlap
3. If `should_early_exit_stage1` → return Low with `ScoringConfidence::Low`
4. Second fetch: 50 posts total (fetch another 25)
5. Full scoring with current formula
6. If `should_continue_to_stage3` → fetch 100+ posts, add NLI pairs
7. Set `ScoringConfidence` based on final stage

The implementation should preserve the existing function signature for backward compatibility. The staged behavior is internal.

```rust
    // Stage 1: Initial sample (25 posts)
    let initial_sample = posts::fetch_posts_with_replies(client, target_handle, 25).await?;

    if initial_sample.total_posts < 5 {
        // ... return Insufficient Data (existing logic) ...
    }

    // Quick ONNX scoring for early exit check
    let initial_texts: Vec<String> = /* all texts from initial_sample */;
    let initial_onnx = scorer.score_batch(&initial_texts).await?;
    let initial_onnx_scores: Vec<f64> = initial_onnx.iter().map(|r| r.toxicity).collect();

    // Compute preliminary topic overlap
    let preliminary_overlap = /* compute from initial_sample.originals or all posts */;

    // Early exit check
    if should_early_exit_stage1(&initial_onnx_scores, preliminary_overlap) {
        return Ok(AccountScore {
            // ... Low tier, scoring_confidence: "low" ...
        });
    }

    // Stage 2: Extended sample (50 posts)
    let full_sample = posts::fetch_posts_with_replies(client, target_handle, 50).await?;
    // ... full scoring pipeline with reply-weighted toxicity ...

    let preliminary_score = /* compute threat score */;

    if should_continue_to_stage3(preliminary_score) {
        // Stage 3: Deep analysis (100+ posts, NLI pairs)
        let deep_sample = posts::fetch_posts_with_replies(client, target_handle, 100).await?;
        // ... extended scoring with context pairs ...
        // scoring_confidence: "high"
    } else {
        // scoring_confidence: "standard"
    }
```

**Important:** Stage 1 early exit uses ONNX scores only for the clean-pass check. It does NOT use ONNX high scores to identify toxic accounts — that would produce false positives on ally content.

- [ ] **Step 2: Update is_score_stale to use confidence-aware staleness**

In `src/db/sqlite.rs` (and `postgres.rs`), update `is_score_stale` to check the `scoring_confidence` column and use the appropriate staleness period:

```rust
// If scoring_confidence is stored, use it for staleness
// Low: 3 days, Standard: 7 days, High: 14 days
// Default to 7 days if not set (backward compatibility)
```

- [ ] **Step 3: Run all tests**

```bash
cargo test --features web
```

- [ ] **Step 4: Commit**

```bash
git add src/scoring/profile.rs src/db/sqlite.rs
git commit -m 'feat: implement three-stage adaptive sampling with early stopping in build_profile'
```

---

## Chunk 4: Phase 3 — Topic-First Discovery

### Phase 3: Topic-First Discovery Pipeline

**Goal:** Replace graph-first sweep with topic-based search discovery. Use `searchPosts` to find accounts posting about the protected user's topics, then score them. Graph expansion remains as a targeted secondary pass from High/Elevated accounts.

#### Task 3.1: Topic Search Module

**Files:**
- Create: `src/discovery/mod.rs`
- Create: `src/discovery/topic_search.rs`
- Modify: `src/lib.rs` (add `discovery` module)

- [ ] **Step 1: Write the failing test**

Create `tests/unit_discovery.rs`:

```rust
// tests/unit_discovery.rs
//
// Tests for topic-first discovery pipeline.

use charcoal::discovery::topic_search;

#[test]
fn extract_search_keywords_from_fingerprint() {
    use charcoal::topics::fingerprint::{TopicCluster, TopicFingerprint};

    let fingerprint = TopicFingerprint {
        clusters: vec![
            TopicCluster {
                label: "fat liberation".to_string(),
                keywords: vec!["fat".to_string(), "liberation".to_string(), "body".to_string()],
                weight: 0.8,
            },
            TopicCluster {
                label: "queer identity".to_string(),
                keywords: vec!["queer".to_string(), "identity".to_string(), "trans".to_string()],
                weight: 0.6,
            },
            TopicCluster {
                label: "community".to_string(),
                keywords: vec!["community".to_string(), "governance".to_string()],
                weight: 0.3,
            },
        ],
        post_count: 50,
    };

    let search_terms = topic_search::extract_search_keywords(&fingerprint, 3);
    // Should return top keywords from the highest-weight clusters
    assert!(!search_terms.is_empty());
    assert!(search_terms.len() <= 3);
    // The top cluster's keywords should be represented
    assert!(search_terms.iter().any(|k| k.contains("fat") || k.contains("liberation")));
}

#[test]
fn deduplicate_author_dids() {
    let raw_dids = vec![
        "did:plc:aaa".to_string(),
        "did:plc:bbb".to_string(),
        "did:plc:aaa".to_string(), // duplicate
        "did:plc:ccc".to_string(),
    ];
    let already_scored = vec!["did:plc:bbb".to_string()]
        .into_iter()
        .collect::<std::collections::HashSet<_>>();

    let new_dids = topic_search::deduplicate_dids(&raw_dids, &already_scored);
    assert_eq!(new_dids.len(), 2); // aaa and ccc (bbb already scored)
    assert!(new_dids.contains(&"did:plc:aaa".to_string()));
    assert!(new_dids.contains(&"did:plc:ccc".to_string()));
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --test unit_discovery -- --nocapture
```
Expected: FAIL — module not found

- [ ] **Step 3: Implement topic_search module**

Create `src/discovery/mod.rs`:
```rust
pub mod topic_search;
```

Create `src/discovery/topic_search.rs`:
```rust
// Topic-first discovery — find accounts via searchPosts by topic keywords.
//
// The primary discovery mechanism for predictive defense. Instead of walking
// the follower graph (expensive, mostly irrelevant), search for posts about
// the protected user's topics and extract author DIDs.

use anyhow::{Context, Result};
use std::collections::HashSet;
use tracing::{debug, info};

use crate::bluesky::client::PublicAtpClient;
use crate::topics::fingerprint::TopicFingerprint;

/// Extract top N search keywords from a topic fingerprint.
///
/// Takes the first keyword from each cluster, sorted by cluster weight
/// (highest weight first). Clusters represent the user's primary topic
/// areas, and the first keyword in each is the most representative term.
/// Filters out very short keywords (< 3 chars).
pub fn extract_search_keywords(fingerprint: &TopicFingerprint, top_n: usize) -> Vec<String> {
    let mut clusters_sorted: Vec<_> = fingerprint.clusters.iter().collect();
    clusters_sorted.sort_by(|a, b| b.weight.partial_cmp(&a.weight).unwrap_or(std::cmp::Ordering::Equal));

    clusters_sorted
        .iter()
        .flat_map(|cluster| {
            cluster.keywords.iter()
                .filter(|k| k.chars().count() >= 3)
                .take(1) // Top keyword per cluster
        })
        .take(top_n)
        .cloned()
        .collect()
}

/// Deduplicate author DIDs against already-scored accounts.
pub fn deduplicate_dids(raw_dids: &[String], already_scored: &HashSet<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    raw_dids
        .iter()
        .filter(|did| !already_scored.contains(did.as_str()) && seen.insert(did.clone()))
        .cloned()
        .collect()
}

/// Search for posts matching a keyword and extract unique author DIDs.
///
/// Uses `app.bsky.feed.searchPosts` to find posts about a given topic,
/// then collects the author DIDs. Handles pagination up to max_results.
pub async fn search_posts_for_authors(
    client: &PublicAtpClient,
    query: &str,
    max_results: usize,
) -> Result<Vec<String>> {
    let mut author_dids = Vec::new();
    let mut cursor: Option<String> = None;
    let limit = max_results.min(100).to_string();

    loop {
        let mut params: Vec<(&str, &str)> = vec![
            ("q", query),
            ("limit", &limit),
        ];
        if let Some(ref c) = cursor {
            params.push(("cursor", c));
        }

        // searchPosts returns SearchPostsOutput with posts array
        let output: atrium_api::app::bsky::feed::search_posts::Output = client
            .xrpc_get("app.bsky.feed.searchPosts", &params)
            .await
            .with_context(|| format!("searchPosts failed for query: {}", query))?;

        for post_view in &output.posts {
            author_dids.push(post_view.author.did.to_string());
        }

        debug!(
            query,
            page_results = output.posts.len(),
            total_authors = author_dids.len(),
            "searchPosts page"
        );

        if author_dids.len() >= max_results {
            break;
        }

        cursor = output.data.cursor.clone();
        if cursor.is_none() || output.posts.is_empty() {
            break;
        }
    }

    info!(
        query,
        unique_authors = author_dids.len(),
        "Collected author DIDs from search"
    );

    Ok(author_dids)
}

/// Run a topic-first discovery cycle.
///
/// Searches for posts matching top keywords from the fingerprint,
/// deduplicates against already-scored accounts, and returns new
/// author DIDs to score.
pub async fn discover_by_topic(
    client: &PublicAtpClient,
    fingerprint: &TopicFingerprint,
    already_scored: &HashSet<String>,
    keywords_per_cycle: usize,
    results_per_keyword: usize,
) -> Result<Vec<String>> {
    let keywords = extract_search_keywords(fingerprint, keywords_per_cycle);

    info!(
        keywords = ?keywords,
        "Running topic-first discovery cycle"
    );

    let mut all_dids = Vec::new();
    for keyword in &keywords {
        match search_posts_for_authors(client, keyword, results_per_keyword).await {
            Ok(dids) => all_dids.extend(dids),
            Err(e) => {
                tracing::warn!(keyword, error = %e, "searchPosts failed, skipping keyword");
            }
        }
    }

    let new_dids = deduplicate_dids(&all_dids, already_scored);
    info!(
        raw = all_dids.len(),
        new = new_dids.len(),
        "Topic discovery: found new accounts to score"
    );

    Ok(new_dids)
}
```

Add `pub mod discovery;` to `src/lib.rs`.

- [ ] **Step 4: Run tests**

```bash
cargo test --test unit_discovery -- --nocapture
```
Expected: PASS for unit tests (search integration test needs network)

- [ ] **Step 5: Commit**

```bash
git add src/discovery/mod.rs src/discovery/topic_search.rs src/lib.rs tests/unit_discovery.rs
git commit -m 'feat: add topic-first discovery via searchPosts'
```

#### Task 3.2: Threat-Graph Expansion Module

**Files:**
- Create: `src/discovery/threat_expansion.rs`

- [ ] **Step 1: Write the test**

Add to `tests/unit_discovery.rs`:

```rust
#[test]
fn only_expand_from_high_and_elevated() {
    use charcoal::discovery::threat_expansion::filter_expansion_candidates;
    use charcoal::db::models::ThreatTier;

    let accounts = vec![
        ("did:plc:high1", ThreatTier::High),
        ("did:plc:elevated1", ThreatTier::Elevated),
        ("did:plc:watch1", ThreatTier::Watch),
        ("did:plc:low1", ThreatTier::Low),
    ];

    let candidates = filter_expansion_candidates(&accounts);
    assert_eq!(candidates.len(), 2);
    assert!(candidates.contains(&"did:plc:high1"));
    assert!(candidates.contains(&"did:plc:elevated1"));
}
```

- [ ] **Step 2: Implement threat_expansion module**

Create `src/discovery/threat_expansion.rs`:

```rust
// Threat-graph expansion — targeted follower walk from known-hostile accounts.
//
// Secondary discovery mechanism. When an account scores High or Elevated,
// their followers are higher-signal than random second-degree followers.
// Hostile accounts cluster: a person who follows three known-High accounts
// is more likely to be a threat.

use crate::db::models::ThreatTier;

/// Filter accounts to only those worth expanding (High or Elevated tier).
pub fn filter_expansion_candidates<'a>(
    accounts: &'a [(&'a str, ThreatTier)],
) -> Vec<&'a str> {
    accounts
        .iter()
        .filter(|(_, tier)| matches!(tier, ThreatTier::High | ThreatTier::Elevated))
        .map(|(did, _)| *did)
        .collect()
}
```

Update `src/discovery/mod.rs`:
```rust
pub mod topic_search;
pub mod threat_expansion;
```

- [ ] **Step 3: Run tests**

```bash
cargo test --test unit_discovery -- --nocapture
```

- [ ] **Step 4: Commit**

```bash
git add src/discovery/threat_expansion.rs src/discovery/mod.rs tests/unit_discovery.rs
git commit -m 'feat: add threat-graph expansion module for targeted follower walk'
```

#### Task 3.3: Wire Topic Discovery into CLI

**Files:**
- Modify: `src/main.rs` (add `--sweep-mode` CLI flag to sweep subcommand)
- Modify: `src/pipeline/sweep.rs` (add topic-first path alongside existing graph walk)

The `sweep` subcommand is defined via `clap` in `src/main.rs`. The pipeline logic is in `src/pipeline/sweep.rs`.

- [ ] **Step 1: Add SweepMode enum and CLI flag**

In `src/main.rs`, add to the sweep subcommand args:

```rust
/// How to discover accounts for scoring
#[arg(long, default_value = "topic")]
sweep_mode: SweepMode,

#[derive(Debug, Clone, clap::ValueEnum)]
enum SweepMode {
    /// Search for accounts by topic keywords (primary, recommended)
    Topic,
    /// Walk follower graph (legacy, expensive)
    Graph,
    /// Both topic search + graph walk
    Both,
}
```

- [ ] **Step 2: Add topic-first sweep function**

Add `pub async fn run_topic_first(...)` to `src/pipeline/sweep.rs`:

```rust
/// Run topic-first discovery sweep.
///
/// 1. Extract search keywords from the protected user's fingerprint
/// 2. Search for posts matching those keywords via searchPosts
/// 3. Deduplicate against already-scored accounts
/// 4. Score new accounts via build_profile
/// 5. For any High/Elevated results, run threat-graph expansion
pub async fn run_topic_first(
    client: &PublicAtpClient,
    scorer: &dyn ToxicityScorer,
    db: &Arc<dyn Database>,
    user_did: &str,
    protected_fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
    concurrency: usize,
    embedder: Option<&SentenceEmbedder>,
    protected_embedding: Option<&[f64]>,
    median_engagement: f64,
    pile_on_dids: &std::collections::HashSet<String>,
    data_dir: Option<&std::path::Path>,
    keywords_per_cycle: usize,   // default: 5
    results_per_keyword: usize,  // default: 100
) -> Result<(usize, usize)> {
    // Step 1: Get already-scored DIDs for deduplication
    let scored_dids: HashSet<String> = db
        .get_all_scored_dids(user_did)
        .await?
        .into_iter()
        .collect();

    // Step 2: Discover new accounts via topic search
    println!("Running topic-first discovery...");
    let new_dids = crate::discovery::topic_search::discover_by_topic(
        client,
        protected_fingerprint,
        &scored_dids,
        keywords_per_cycle,
        results_per_keyword,
    )
    .await?;

    println!("  Found {} new accounts to score", new_dids.len());

    if new_dids.is_empty() {
        return Ok((0, 0));
    }

    // Step 3: Resolve DIDs to handles and score
    // Use getProfiles for batch resolution (25 per call)
    let mut accounts_scored = 0;
    let discovered = new_dids.len();

    // ... (batch resolve DIDs to handles via getProfiles,
    //      then score each via build_profile with concurrency,
    //      same pattern as existing sweep.rs::run)

    // Step 4: Threat-graph expansion from High/Elevated results
    // (Query DB for newly scored High/Elevated, fetch their followers,
    //  deduplicate, score — same pattern as existing graph sweep but
    //  only from hostile accounts, not all followers)

    Ok((discovered, accounts_scored))
}
```

Note: The `get_all_scored_dids` method may need to be added to the `Database` trait. Check if an equivalent exists.

- [ ] **Step 3: Route CLI sweep command to the correct implementation**

In `src/main.rs`, in the sweep command handler:

```rust
match sweep_mode {
    SweepMode::Topic => {
        sweep::run_topic_first(/* params */).await?;
    }
    SweepMode::Graph => {
        // Existing sweep::run() — unchanged
        sweep::run(/* params */).await?;
    }
    SweepMode::Both => {
        sweep::run_topic_first(/* params */).await?;
        sweep::run(/* params */).await?;
    }
}
```

- [ ] **Step 4: Add Database trait method if needed**

If `get_all_scored_dids` doesn't exist, add to `src/db/traits.rs`:

```rust
/// Get all DIDs that have been scored for a user (for deduplication).
async fn get_all_scored_dids(&self, user_did: &str) -> Result<Vec<String>>;
```

And implement in `sqlite.rs` and `postgres.rs`:

```rust
async fn get_all_scored_dids(&self, user_did: &str) -> Result<Vec<String>> {
    let conn = self.conn.lock().await;
    let mut stmt = conn.prepare(
        "SELECT did FROM account_scores WHERE user_did = ?1"
    )?;
    let dids = stmt.query_map(params![user_did], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(dids)
}
```

- [ ] **Step 5: Run tests and validate**

```bash
cargo test --features web
```

- [ ] **Step 6: Commit**

```bash
git add src/pipeline/sweep.rs src/main.rs src/db/traits.rs src/db/sqlite.rs
git commit -m 'feat: wire topic-first discovery as primary sweep pipeline'
```

---

## Chunk 5: Phase 4 — Firehose Monitoring (Deferred) & Phase 5 — Zentropi Integration (After Spike)

### Phase 4: Firehose Monitoring

**Status:** Deferred — this phase depends on Jetstream WebSocket integration which is a separate workstream. The design is documented but implementation is blocked until we validate the event stream approach (issue #59).

**When to implement:** After Phase 3 is stable and deployed to staging. The firehose catches anyone Layers 1-2 missed at the moment of collision — it's the real-time safety net.

**Key files to create:**
- `src/firehose/mod.rs`
- `src/firehose/jetstream.rs` — WebSocket client filtered to `app.bsky.feed.post`
- Filter for posts referencing protected user's DID in reply/embed
- Score-on-arrival for unscored strangers
- Alert pathway for already-scored High accounts

### Phase 5: Zentropi Integration

**Status:** Blocked on Phase 0 spike results. Do not implement until the spike validates:
1. API availability and rate limits
2. Policy prompt accuracy on ally vs hostile content
3. Throughput sustainability for production scan volumes

**When to implement:** After Phase 0 spike passes go/no-go criteria AND Phases 1a+1b are deployed.

**Key changes (contingent on spike):**
- Add `src/toxicity/zentropi.rs` — production Zentropi client
- ONNX becomes clean-pass filter only (< 0.10 = cleared, everything else → Zentropi)
- Toxicity rate computed from Zentropi binary labels, not ONNX continuous scores
- `weighted_toxicity()` removed from scoring path (kept as ONNX-only fallback)
- Reply pairs sent as `(parent_text, reply_text)` to Zentropi
- Groq Safeguard removed (if still present)
- Remove ensemble scorer's dependency on OpenAI Moderation

**Fallback if Zentropi no-go:**
- Self-host CoPE-A-9B via llama.cpp/MLX on Mac Studio
- Or evaluate Llama Guard 4 12B as alternative
- Or keep current ONNX + weighted_toxicity with documented caveats

---

## Important Notes

**Spec discrepancy — High threshold:** The design doc says "40.0 High" but the codebase uses 35.0 (`ThreatTier::from_score` in `src/db/models.rs:119`). The plan follows the code. If the threshold should change, that's a separate issue.

**Shared scoring schema constraint:** Per the design doc's "Future: Shared Scoring Schema" section, do NOT tightly couple toxicity computation with per-user threat scoring in new code. Keep them as distinct steps in `build_profile` so the eventual `account_profiles` / `user_threat_assessments` split is clean.

**Fingerprint quality and staleness:** `ScoringConfidence` drives staleness via `is_score_stale`. `FingerprintQuality::Unreliable` should also trigger sooner re-scoring — when implementing Task 2.2, also check fingerprint quality in the staleness logic (e.g., Unreliable fingerprint → treat as `ScoringConfidence::Low` for staleness purposes).

---

## Implementation Order Summary

```
Phase 0: Zentropi spike (1-2 hours, can run in parallel with 1a)
  └── Go/no-go decision for Phase 5

Phase 1a: Reply-inclusive post sampling (independent)
  ├── Task 1a.1: PostSample/ReplyPost/FingerprintQuality types
  ├── Task 1a.2: fetch_posts_with_replies
  └── Task 1a.3: Batch parent post fetching

Phase 1b: Scoring formula changes (depends on 1a)
  ├── Task 1b.1: Fix context score double-application bug
  ├── Task 1b.2: Schema migration v8 (fingerprint_quality, scoring_confidence)
  ├── Task 1b.3: Reply-weighted toxicity computation
  ├── Task 1b.4: Refactor build_profile to use PostSample
  └── Task 1b.5: Persona composition tests

Phase 2: Adaptive sampling (depends on 1a+1b)
  ├── Task 2.1: Stage decision functions
  └── Task 2.2: Wire into build_profile

Phase 3: Topic-first discovery (can start after 1a, full value after 1b)
  ├── Task 3.1: Topic search module
  ├── Task 3.2: Threat-graph expansion module
  └── Task 3.3: Wire into CLI sweep command

Phase 4: Firehose monitoring (deferred — separate workstream)

Phase 5: Zentropi integration (blocked on Phase 0 results)
```
