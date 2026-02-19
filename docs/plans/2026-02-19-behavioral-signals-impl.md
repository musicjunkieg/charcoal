# Behavioral Signals Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add behavioral signals (quote ratio, reply ratio, pile-on detection) to the threat scoring pipeline as a gate + multiplier hybrid that reduces false positives for allies and amplifies scores for hostile behavioral patterns.

**Architecture:** New `src/scoring/behavioral.rs` module with pure functions for signal computation, gate logic, and boost calculation. `Post` struct gains `is_quote` field. New `fetch_reply_ratio()` in posts module. Pile-on detection queries the existing `amplification_events` table. DB migration v3 adds `behavioral_signals` JSON column. `AccountScore` gains `behavioral_signals` field. `build_profile()` wired to compute and store behavioral data.

**Tech Stack:** Rust, rusqlite (parameterized queries), serde_json, chrono (timestamp parsing for pile-on windows), atrium-api types (embed detection)

**Design doc:** `docs/plans/2026-02-19-behavioral-signals-design.md`

---

### Task 1: BehavioralSignals struct and serialization

**Files:**
- Create: `src/scoring/behavioral.rs`
- Modify: `src/scoring/mod.rs`
- Test: `tests/unit_behavioral.rs`

**Step 1: Write the failing test**

Create `tests/unit_behavioral.rs`:

```rust
use charcoal::scoring::behavioral::BehavioralSignals;

#[test]
fn behavioral_signals_default_is_neutral() {
    let signals = BehavioralSignals::default();
    assert_eq!(signals.quote_ratio, 0.0);
    assert_eq!(signals.reply_ratio, 0.0);
    assert_eq!(signals.avg_engagement, 0.0);
    assert!(!signals.pile_on);
    assert!(!signals.benign_gate);
    assert_eq!(signals.behavioral_boost, 1.0);
}

#[test]
fn behavioral_signals_json_roundtrip() {
    let signals = BehavioralSignals {
        quote_ratio: 0.35,
        reply_ratio: 0.45,
        avg_engagement: 12.5,
        pile_on: true,
        benign_gate: false,
        behavioral_boost: 1.22,
    };
    let json = serde_json::to_string(&signals).unwrap();
    let deserialized: BehavioralSignals = serde_json::from_str(&json).unwrap();
    assert!((deserialized.quote_ratio - 0.35).abs() < f64::EPSILON);
    assert!(deserialized.pile_on);
    assert!((deserialized.behavioral_boost - 1.22).abs() < f64::EPSILON);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test unit_behavioral -v`
Expected: FAIL — `behavioral` module doesn't exist

**Step 3: Write minimal implementation**

Create `src/scoring/behavioral.rs`:

```rust
// Behavioral signals — post-pattern analysis for scoring adjustment.
//
// Computes behavioral signals (quote ratio, reply ratio, engagement,
// pile-on participation) and uses them as a gate + multiplier hybrid:
// - Benign gate: caps score at 12.0 for clearly non-threatening accounts
// - Hostile multiplier: boosts score by 1.0-1.5x for hostile patterns

use serde::{Deserialize, Serialize};

/// Behavioral signals computed from an account's posting patterns.
///
/// Stored as JSON in the `behavioral_signals` column of `account_scores`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehavioralSignals {
    /// Fraction of posts that are quote-posts (0.0-1.0)
    pub quote_ratio: f64,
    /// Fraction of posts that are replies (0.0-1.0)
    pub reply_ratio: f64,
    /// Mean likes + reposts received per post
    pub avg_engagement: f64,
    /// Whether this account participated in a detected pile-on
    pub pile_on: bool,
    /// Whether the benign gate was applied (for transparency in reports)
    pub benign_gate: bool,
    /// The computed behavioral boost multiplier (1.0 = neutral)
    pub behavioral_boost: f64,
}

impl Default for BehavioralSignals {
    fn default() -> Self {
        Self {
            quote_ratio: 0.0,
            reply_ratio: 0.0,
            avg_engagement: 0.0,
            pile_on: false,
            benign_gate: false,
            behavioral_boost: 1.0,
        }
    }
}
```

Add to `src/scoring/mod.rs`:

```rust
pub mod behavioral;
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test unit_behavioral -v`
Expected: PASS (2 tests)

**Step 5: Commit**

```bash
git add src/scoring/behavioral.rs src/scoring/mod.rs tests/unit_behavioral.rs
git commit -m "feat: add BehavioralSignals struct with serde serialization"
```

---

### Task 2: Behavioral boost computation

**Files:**
- Modify: `src/scoring/behavioral.rs`
- Test: `tests/unit_behavioral.rs`

**Step 1: Write the failing tests**

Append to `tests/unit_behavioral.rs`:

```rust
use charcoal::scoring::behavioral::compute_behavioral_boost;

#[test]
fn boost_all_zeros_is_one() {
    let boost = compute_behavioral_boost(0.0, 0.0, false);
    assert!((boost - 1.0).abs() < f64::EPSILON);
}

#[test]
fn boost_max_is_1_5() {
    // 100% quotes + 100% replies + pile-on = 1.0 + 0.20 + 0.15 + 0.15 = 1.50
    let boost = compute_behavioral_boost(1.0, 1.0, true);
    assert!((boost - 1.5).abs() < f64::EPSILON);
}

#[test]
fn boost_quote_only() {
    // 50% quotes = 1.0 + 0.5 * 0.20 = 1.10
    let boost = compute_behavioral_boost(0.5, 0.0, false);
    assert!((boost - 1.1).abs() < f64::EPSILON);
}

#[test]
fn boost_reply_only() {
    // 80% replies = 1.0 + 0.8 * 0.15 = 1.12
    let boost = compute_behavioral_boost(0.0, 0.8, false);
    assert!((boost - 1.12).abs() < f64::EPSILON);
}

#[test]
fn boost_pile_on_only() {
    let boost = compute_behavioral_boost(0.0, 0.0, true);
    assert!((boost - 1.15).abs() < f64::EPSILON);
}

#[test]
fn boost_typical_hostile() {
    // 40% quotes, 30% replies, no pile-on = 1.0 + 0.08 + 0.045 = 1.125
    let boost = compute_behavioral_boost(0.4, 0.3, false);
    assert!((boost - 1.125).abs() < 0.001);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test unit_behavioral compute_behavioral_boost -v`
Expected: FAIL — function doesn't exist

**Step 3: Write minimal implementation**

Add to `src/scoring/behavioral.rs`:

```rust
/// Compute the behavioral boost multiplier from posting patterns.
///
/// Range: 1.0 (neutral) to 1.5 (maximum hostile pattern).
/// - quote_ratio * 0.20: accounts that mostly quote-dunk get up to +0.20
/// - reply_ratio * 0.15: reply-heavy accounts get up to +0.15
/// - pile_on: +0.15 if the account participated in a detected pile-on
pub fn compute_behavioral_boost(quote_ratio: f64, reply_ratio: f64, pile_on: bool) -> f64 {
    let mut boost = 1.0;
    boost += quote_ratio * 0.20;
    boost += reply_ratio * 0.15;
    if pile_on {
        boost += 0.15;
    }
    boost
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test unit_behavioral -v`
Expected: PASS (8 tests)

**Step 5: Commit**

```bash
git add src/scoring/behavioral.rs tests/unit_behavioral.rs
git commit -m "feat: add compute_behavioral_boost multiplier function"
```

---

### Task 3: Benign gate logic

**Files:**
- Modify: `src/scoring/behavioral.rs`
- Test: `tests/unit_behavioral.rs`

**Step 1: Write the failing tests**

Append to `tests/unit_behavioral.rs`:

```rust
use charcoal::scoring::behavioral::{is_behaviorally_benign, apply_behavioral_modifier};

#[test]
fn benign_gate_all_conditions_met() {
    // Low quote ratio, low reply ratio, no pile-on, above-median engagement
    assert!(is_behaviorally_benign(0.10, 0.20, false, 15.0, 10.0));
}

#[test]
fn benign_gate_fails_high_quote_ratio() {
    assert!(!is_behaviorally_benign(0.20, 0.20, false, 15.0, 10.0));
}

#[test]
fn benign_gate_fails_high_reply_ratio() {
    assert!(!is_behaviorally_benign(0.10, 0.35, false, 15.0, 10.0));
}

#[test]
fn benign_gate_fails_pile_on() {
    assert!(!is_behaviorally_benign(0.10, 0.20, true, 15.0, 10.0));
}

#[test]
fn benign_gate_fails_low_engagement() {
    // Engagement (5.0) below median (10.0)
    assert!(!is_behaviorally_benign(0.10, 0.20, false, 5.0, 10.0));
}

#[test]
fn benign_gate_exact_thresholds() {
    // Exactly at thresholds — 0.15 is NOT < 0.15, so should fail
    assert!(!is_behaviorally_benign(0.15, 0.20, false, 15.0, 10.0));
    // 0.30 is NOT < 0.30, so should fail
    assert!(!is_behaviorally_benign(0.10, 0.30, false, 15.0, 10.0));
}

#[test]
fn modifier_benign_caps_at_12() {
    // Raw score of 50.0, benign gate active -> capped at 12.0
    let (score, benign) = apply_behavioral_modifier(50.0, 0.05, 0.10, false, 15.0, 10.0);
    assert!(benign);
    assert!((score - 12.0).abs() < f64::EPSILON);
}

#[test]
fn modifier_benign_passes_through_low_score() {
    // Raw score of 5.0, benign gate active -> stays at 5.0 (below cap)
    let (score, benign) = apply_behavioral_modifier(5.0, 0.05, 0.10, false, 15.0, 10.0);
    assert!(benign);
    assert!((score - 5.0).abs() < f64::EPSILON);
}

#[test]
fn modifier_hostile_applies_boost() {
    // Not benign (high quote ratio), boost = 1.0 + 0.8*0.20 = 1.16
    let (score, benign) = apply_behavioral_modifier(50.0, 0.80, 0.10, false, 15.0, 10.0);
    assert!(!benign);
    // 50.0 * 1.16 = 58.0
    assert!((score - 58.0).abs() < 0.1);
}

#[test]
fn modifier_no_behavioral_data_is_neutral() {
    // Default signals: all zeros, no pile-on, engagement below any median
    // Not benign (engagement 0.0 <= median 10.0), boost = 1.0
    let (score, benign) = apply_behavioral_modifier(50.0, 0.0, 0.0, false, 0.0, 10.0);
    assert!(!benign);
    assert!((score - 50.0).abs() < f64::EPSILON);
}

#[test]
fn modifier_clamped_to_100() {
    // High raw score * boost should still clamp to 100
    let (score, _) = apply_behavioral_modifier(90.0, 1.0, 1.0, true, 0.0, 10.0);
    // 90.0 * 1.5 = 135 -> clamped to 100
    assert!((score - 100.0).abs() < f64::EPSILON);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test unit_behavioral benign -v`
Expected: FAIL — functions don't exist

**Step 3: Write minimal implementation**

Add to `src/scoring/behavioral.rs`:

```rust
/// Benign gate thresholds. An account must meet ALL conditions to be
/// considered behaviorally benign (which caps their threat score).
const BENIGN_QUOTE_RATIO_MAX: f64 = 0.15;
const BENIGN_REPLY_RATIO_MAX: f64 = 0.30;

/// Maximum threat score when the benign gate is active.
/// Just below Watch (8.0) -> Elevated (15.0) boundary, ensuring benign
/// accounts never reach Elevated or High tier.
const BENIGN_GATE_CAP: f64 = 12.0;

/// Check whether an account's behavioral signals indicate benign posting patterns.
///
/// ALL conditions must be true:
/// - Quote ratio below threshold (they rarely quote-dunk)
/// - Reply ratio below threshold (they don't mostly reply to strangers)
/// - Not involved in any pile-on
/// - Average engagement above median (they're a creator, not just a reactor)
pub fn is_behaviorally_benign(
    quote_ratio: f64,
    reply_ratio: f64,
    pile_on: bool,
    avg_engagement: f64,
    median_engagement: f64,
) -> bool {
    quote_ratio < BENIGN_QUOTE_RATIO_MAX
        && reply_ratio < BENIGN_REPLY_RATIO_MAX
        && !pile_on
        && avg_engagement > median_engagement
}

/// Apply the behavioral modifier to a raw threat score.
///
/// Gate + Multiplier Hybrid:
/// - If the account is behaviorally benign, cap the score at 12.0
/// - Otherwise, multiply the score by the behavioral boost (1.0-1.5x)
///
/// Returns (modified_score, benign_gate_applied).
pub fn apply_behavioral_modifier(
    raw_score: f64,
    quote_ratio: f64,
    reply_ratio: f64,
    pile_on: bool,
    avg_engagement: f64,
    median_engagement: f64,
) -> (f64, bool) {
    let benign = is_behaviorally_benign(
        quote_ratio,
        reply_ratio,
        pile_on,
        avg_engagement,
        median_engagement,
    );

    if benign {
        (raw_score.min(BENIGN_GATE_CAP), true)
    } else {
        let boost = compute_behavioral_boost(quote_ratio, reply_ratio, pile_on);
        let score = (raw_score * boost).clamp(0.0, 100.0);
        (score, false)
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test unit_behavioral -v`
Expected: PASS (19 tests)

**Step 5: Run full test suite for backward compatibility**

Run: `cargo test --all-targets`
Expected: All 139 + 19 = 158 tests pass

**Step 6: Commit**

```bash
git add src/scoring/behavioral.rs tests/unit_behavioral.rs
git commit -m "feat: add benign gate and behavioral modifier logic"
```

---

### Task 4: Quote detection in Post struct

**Files:**
- Modify: `src/bluesky/posts.rs`
- Test: `tests/unit_behavioral.rs`

**Step 1: Write the failing test**

Append to `tests/unit_behavioral.rs`:

```rust
use charcoal::scoring::behavioral::compute_quote_ratio;

#[test]
fn quote_ratio_no_posts() {
    assert!((compute_quote_ratio(0, 0) - 0.0).abs() < f64::EPSILON);
}

#[test]
fn quote_ratio_no_quotes() {
    assert!((compute_quote_ratio(0, 10) - 0.0).abs() < f64::EPSILON);
}

#[test]
fn quote_ratio_all_quotes() {
    assert!((compute_quote_ratio(10, 10) - 1.0).abs() < f64::EPSILON);
}

#[test]
fn quote_ratio_half() {
    assert!((compute_quote_ratio(5, 10) - 0.5).abs() < f64::EPSILON);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test unit_behavioral quote_ratio -v`
Expected: FAIL — function doesn't exist

**Step 3: Write implementation**

Add to `src/scoring/behavioral.rs`:

```rust
/// Compute the fraction of posts that are quote-posts.
///
/// Returns 0.0 if total_posts is 0 (avoids division by zero).
pub fn compute_quote_ratio(quote_count: usize, total_posts: usize) -> f64 {
    if total_posts == 0 {
        return 0.0;
    }
    quote_count as f64 / total_posts as f64
}
```

Add `is_quote` field to `Post` in `src/bluesky/posts.rs`:

```rust
pub struct Post {
    pub uri: String,
    pub text: String,
    pub created_at: Option<String>,
    pub like_count: i64,
    pub repost_count: i64,
    pub quote_count: i64,
    /// Whether this post quotes another post (has embed.record or embed.recordWithMedia)
    pub is_quote: bool,
}
```

Update the post construction in `fetch_recent_posts` (around line 80):

```rust
// Detect quote-posts by checking the embed type.
// A quote has embed.record (quote only) or embed.recordWithMedia
// (quote + image/link). The embed field is on PostView.
let is_quote = post_view.embed.as_ref().map_or(false, |embed| {
    use atrium_api::types::Union;
    matches!(
        embed,
        Union::Refs(
            atrium_api::app::bsky::feed::defs::PostViewEmbedRefs::AppBskyEmbedRecordView(_)
                | atrium_api::app::bsky::feed::defs::PostViewEmbedRefs::AppBskyEmbedRecordWithMediaView(_)
        )
    )
});

posts.push(Post {
    uri: post_view.uri.clone(),
    text,
    created_at: Some(post_view.indexed_at.as_ref().to_string()),
    like_count: post_view.like_count.unwrap_or(0),
    repost_count: post_view.repost_count.unwrap_or(0),
    quote_count: post_view.quote_count.unwrap_or(0),
    is_quote,
});
```

Note: The exact enum variant names come from the atrium-api crate. Before writing, verify the actual type names by checking:
- `atrium_api::app::bsky::feed::defs::PostViewEmbedRefs` variants
- The `Union` wrapper type from `atrium_api::types`

If the variant names differ, adjust accordingly. Use `cargo check` to verify.

**Step 4: Run test to verify it passes**

Run: `cargo test --all-targets`
Expected: All tests pass (existing tests unaffected since `is_quote` is a new field and existing `Post` construction in tests doesn't use it)

**Step 5: Commit**

```bash
git add src/bluesky/posts.rs src/scoring/behavioral.rs tests/unit_behavioral.rs
git commit -m "feat: add is_quote field to Post struct with embed detection"
```

---

### Task 5: Reply ratio fetching

**Files:**
- Modify: `src/bluesky/posts.rs`
- Modify: `src/scoring/behavioral.rs`
- Test: `tests/unit_behavioral.rs`

**Step 1: Write the failing test for the ratio helper**

Append to `tests/unit_behavioral.rs`:

```rust
use charcoal::scoring::behavioral::compute_reply_ratio;

#[test]
fn reply_ratio_no_posts() {
    assert!((compute_reply_ratio(0, 0) - 0.0).abs() < f64::EPSILON);
}

#[test]
fn reply_ratio_no_replies() {
    assert!((compute_reply_ratio(0, 20) - 0.0).abs() < f64::EPSILON);
}

#[test]
fn reply_ratio_all_replies() {
    assert!((compute_reply_ratio(20, 20) - 1.0).abs() < f64::EPSILON);
}

#[test]
fn reply_ratio_mixed() {
    assert!((compute_reply_ratio(15, 50) - 0.3).abs() < f64::EPSILON);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test unit_behavioral reply_ratio -v`
Expected: FAIL — function doesn't exist

**Step 3: Write implementation**

Add to `src/scoring/behavioral.rs`:

```rust
/// Compute the fraction of posts that are replies.
///
/// Returns 0.0 if total_posts is 0 (avoids division by zero).
pub fn compute_reply_ratio(reply_count: usize, total_posts: usize) -> f64 {
    if total_posts == 0 {
        return 0.0;
    }
    reply_count as f64 / total_posts as f64
}
```

Add to `src/bluesky/posts.rs`:

```rust
/// Fetch the reply ratio for an account by sampling one page of posts.
///
/// Makes a single API call with `posts_and_author_threads` filter (which
/// includes replies), then counts how many have a `reply` field set.
/// Returns (reply_count, total_count) so the caller can compute the ratio.
pub async fn fetch_reply_ratio(
    client: &PublicAtpClient,
    handle: &str,
) -> Result<(usize, usize)> {
    let params: Vec<(&str, &str)> = vec![
        ("actor", handle),
        ("filter", "posts_and_author_threads"),
        ("limit", "50"),
    ];

    let output: get_author_feed::Output = client
        .xrpc_get("app.bsky.feed.getAuthorFeed", &params)
        .await
        .with_context(|| format!("Failed to fetch reply ratio for @{}", handle))?;

    let mut reply_count = 0;
    let mut total = 0;

    for feed_item in &output.feed {
        // Skip reposts
        if feed_item.reason.is_some() {
            continue;
        }
        total += 1;
        if feed_item.reply.is_some() {
            reply_count += 1;
        }
    }

    debug!(
        handle = handle,
        replies = reply_count,
        total = total,
        "Reply ratio sample"
    );

    Ok((reply_count, total))
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --all-targets`
Expected: All tests pass

**Step 5: Commit**

```bash
git add src/bluesky/posts.rs src/scoring/behavioral.rs tests/unit_behavioral.rs
git commit -m "feat: add reply ratio fetching and computation"
```

---

### Task 6: Pile-on detection

**Files:**
- Modify: `src/scoring/behavioral.rs`
- Modify: `src/db/queries.rs`
- Test: `tests/unit_behavioral.rs`

**Step 1: Write the failing tests**

Append to `tests/unit_behavioral.rs`:

```rust
use charcoal::scoring::behavioral::detect_pile_on_participants;

#[test]
fn pile_on_below_threshold_not_detected() {
    // 4 events in 24h -> not a pile-on
    let events = vec![
        ("did:plc:a", "at://post/1", "2026-02-19T10:00:00Z"),
        ("did:plc:b", "at://post/1", "2026-02-19T11:00:00Z"),
        ("did:plc:c", "at://post/1", "2026-02-19T12:00:00Z"),
        ("did:plc:d", "at://post/1", "2026-02-19T13:00:00Z"),
    ];
    let participants = detect_pile_on_participants(&events);
    assert!(participants.is_empty());
}

#[test]
fn pile_on_at_threshold_detected() {
    // 5 distinct amplifiers on same post within 24h -> pile-on
    let events = vec![
        ("did:plc:a", "at://post/1", "2026-02-19T10:00:00Z"),
        ("did:plc:b", "at://post/1", "2026-02-19T11:00:00Z"),
        ("did:plc:c", "at://post/1", "2026-02-19T12:00:00Z"),
        ("did:plc:d", "at://post/1", "2026-02-19T13:00:00Z"),
        ("did:plc:e", "at://post/1", "2026-02-19T14:00:00Z"),
    ];
    let participants = detect_pile_on_participants(&events);
    assert_eq!(participants.len(), 5);
    assert!(participants.contains("did:plc:a"));
    assert!(participants.contains("did:plc:e"));
}

#[test]
fn pile_on_outside_window_not_detected() {
    // 5 amplifiers but spread across 48h -> no pile-on
    let events = vec![
        ("did:plc:a", "at://post/1", "2026-02-18T10:00:00Z"),
        ("did:plc:b", "at://post/1", "2026-02-18T22:00:00Z"),
        ("did:plc:c", "at://post/1", "2026-02-19T10:00:00Z"),
        ("did:plc:d", "at://post/1", "2026-02-19T22:00:00Z"),
        ("did:plc:e", "at://post/1", "2026-02-20T10:00:00Z"),
    ];
    let participants = detect_pile_on_participants(&events);
    assert!(participants.is_empty());
}

#[test]
fn pile_on_deduplicates_same_amplifier() {
    // Same DID amplifying twice counts as 1
    let events = vec![
        ("did:plc:a", "at://post/1", "2026-02-19T10:00:00Z"),
        ("did:plc:a", "at://post/1", "2026-02-19T10:30:00Z"), // duplicate
        ("did:plc:b", "at://post/1", "2026-02-19T11:00:00Z"),
        ("did:plc:c", "at://post/1", "2026-02-19T12:00:00Z"),
        ("did:plc:d", "at://post/1", "2026-02-19T13:00:00Z"),
    ];
    // Only 4 unique DIDs -> not a pile-on
    let participants = detect_pile_on_participants(&events);
    assert!(participants.is_empty());
}

#[test]
fn pile_on_multiple_posts_independent() {
    // 3 on post/1 + 3 on post/2 -> neither reaches threshold
    let events = vec![
        ("did:plc:a", "at://post/1", "2026-02-19T10:00:00Z"),
        ("did:plc:b", "at://post/1", "2026-02-19T11:00:00Z"),
        ("did:plc:c", "at://post/1", "2026-02-19T12:00:00Z"),
        ("did:plc:d", "at://post/2", "2026-02-19T10:00:00Z"),
        ("did:plc:e", "at://post/2", "2026-02-19T11:00:00Z"),
        ("did:plc:f", "at://post/2", "2026-02-19T12:00:00Z"),
    ];
    let participants = detect_pile_on_participants(&events);
    assert!(participants.is_empty());
}

#[test]
fn pile_on_sliding_window_catches_late_cluster() {
    // First 2 are early, then 5 more cluster in a 3-hour window
    let events = vec![
        ("did:plc:a", "at://post/1", "2026-02-18T08:00:00Z"),
        ("did:plc:b", "at://post/1", "2026-02-18T09:00:00Z"),
        ("did:plc:c", "at://post/1", "2026-02-19T10:00:00Z"),
        ("did:plc:d", "at://post/1", "2026-02-19T11:00:00Z"),
        ("did:plc:e", "at://post/1", "2026-02-19T12:00:00Z"),
        ("did:plc:f", "at://post/1", "2026-02-19T12:30:00Z"),
        ("did:plc:g", "at://post/1", "2026-02-19T13:00:00Z"),
    ];
    let participants = detect_pile_on_participants(&events);
    // The cluster of c,d,e,f,g is 5 in 3h -> pile-on
    assert!(participants.len() >= 5);
    assert!(participants.contains("did:plc:c"));
    assert!(participants.contains("did:plc:g"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test unit_behavioral pile_on -v`
Expected: FAIL — function doesn't exist

**Step 3: Write implementation**

Add to `src/scoring/behavioral.rs`:

```rust
use std::collections::{HashMap, HashSet};

/// Minimum number of distinct amplifiers in a 24-hour window to trigger
/// pile-on detection. Below this threshold, it's normal engagement.
const PILE_ON_THRESHOLD: usize = 5;

/// Duration of the pile-on sliding window in seconds (24 hours).
const PILE_ON_WINDOW_SECS: i64 = 24 * 60 * 60;

/// Detect pile-on participants from amplification events.
///
/// Takes a slice of (amplifier_did, original_post_uri, detected_at_iso)
/// tuples. Groups by post URI, then uses a sliding 24-hour window to find
/// clusters of 5+ distinct amplifiers. Returns the set of DIDs that
/// participated in any detected pile-on.
pub fn detect_pile_on_participants(
    events: &[(&str, &str, &str)],
) -> HashSet<String> {
    let mut result = HashSet::new();

    // Group events by original_post_uri
    let mut by_post: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
    for &(did, uri, ts) in events {
        by_post.entry(uri).or_default().push((did, ts));
    }

    for (_uri, mut post_events) in by_post {
        // Sort by timestamp
        post_events.sort_by_key(|&(_, ts)| ts.to_string());

        // Parse timestamps and deduplicate DIDs within window
        let parsed: Vec<(&str, i64)> = post_events
            .iter()
            .filter_map(|&(did, ts)| {
                chrono::DateTime::parse_from_rfc3339(ts)
                    .ok()
                    .map(|dt| (did, dt.timestamp()))
            })
            .collect();

        if parsed.len() < PILE_ON_THRESHOLD {
            continue;
        }

        // Sliding window: for each event, look forward 24h and count unique DIDs
        for i in 0..parsed.len() {
            let window_start = parsed[i].1;
            let window_end = window_start + PILE_ON_WINDOW_SECS;

            let mut unique_dids: HashSet<&str> = HashSet::new();
            let mut window_indices = Vec::new();

            for (j, &(did, ts)) in parsed.iter().enumerate().skip(i) {
                if ts > window_end {
                    break;
                }
                unique_dids.insert(did);
                window_indices.push(j);
            }

            if unique_dids.len() >= PILE_ON_THRESHOLD {
                // All unique DIDs in this window are pile-on participants
                for did in unique_dids {
                    result.insert(did.to_string());
                }
            }
        }
    }

    result
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --test unit_behavioral -v`
Expected: All tests pass

**Step 5: Add a DB query to fetch events for pile-on detection**

Add to `src/db/queries.rs`:

```rust
/// Get amplification events grouped by the data needed for pile-on detection.
///
/// Returns (amplifier_did, original_post_uri, detected_at) tuples for all
/// events. The caller uses these with `detect_pile_on_participants()`.
pub fn get_events_for_pile_on(conn: &Connection) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT amplifier_did, original_post_uri, detected_at
         FROM amplification_events
         ORDER BY original_post_uri, detected_at",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;

    let mut events = Vec::new();
    for row in rows {
        events.push(row?);
    }
    Ok(events)
}
```

**Step 6: Run full test suite**

Run: `cargo test --all-targets`
Expected: All tests pass

**Step 7: Commit**

```bash
git add src/scoring/behavioral.rs src/db/queries.rs tests/unit_behavioral.rs
git commit -m "feat: add pile-on detection with 24-hour sliding window"
```

---

### Task 7: DB migration v3 — behavioral_signals column

**Files:**
- Modify: `src/db/schema.rs`
- Modify: `src/db/models.rs`
- Modify: `src/db/queries.rs`
- Test: `src/db/schema.rs` (inline tests)

**Step 1: Write the failing test**

Add to the `tests` module in `src/db/schema.rs`:

```rust
#[test]
fn test_migration_v3_adds_behavioral_signals_column() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();

    // Insert an account score first
    conn.execute(
        "INSERT INTO account_scores (did, handle, posts_analyzed)
         VALUES ('did:plc:test', 'test.bsky.social', 10)",
        [],
    )
    .unwrap();

    // Update behavioral_signals
    conn.execute(
        "UPDATE account_scores SET behavioral_signals = ?1 WHERE did = 'did:plc:test'",
        rusqlite::params![r#"{"quote_ratio":0.5}"#],
    )
    .unwrap();

    let result: String = conn
        .query_row(
            "SELECT behavioral_signals FROM account_scores WHERE did = 'did:plc:test'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(result, r#"{"quote_ratio":0.5}"#);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test test_migration_v3 -v`
Expected: FAIL — column doesn't exist

**Step 3: Write implementation**

Add migration v3 in `src/db/schema.rs` after the v2 migration (after line 92):

```rust
// Migration v3: add behavioral_signals column to account_scores.
// Stores a JSON object with quote_ratio, reply_ratio, avg_engagement,
// pile_on, benign_gate, and behavioral_boost.
run_migration(conn, 3, |c| {
    c.execute_batch("ALTER TABLE account_scores ADD COLUMN behavioral_signals TEXT;")
})?;
```

Add `behavioral_signals` field to `AccountScore` in `src/db/models.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountScore {
    pub did: String,
    pub handle: String,
    pub toxicity_score: Option<f64>,
    pub topic_overlap: Option<f64>,
    pub threat_score: Option<f64>,
    pub threat_tier: Option<String>,
    pub posts_analyzed: u32,
    pub top_toxic_posts: Vec<ToxicPost>,
    pub scored_at: String,
    /// Behavioral signals (JSON-serialized), present when behavioral analysis ran
    pub behavioral_signals: Option<String>,
}
```

Update `upsert_account_score` in `src/db/queries.rs` to include `behavioral_signals`:

```rust
pub fn upsert_account_score(conn: &Connection, score: &AccountScore) -> Result<()> {
    let top_posts_json = serde_json::to_string(&score.top_toxic_posts)?;
    conn.execute(
        "INSERT INTO account_scores (did, handle, toxicity_score, topic_overlap, threat_score, threat_tier, posts_analyzed, top_toxic_posts, behavioral_signals, scored_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, datetime('now'))
         ON CONFLICT(did) DO UPDATE SET
            handle = ?2,
            toxicity_score = ?3,
            topic_overlap = ?4,
            threat_score = ?5,
            threat_tier = ?6,
            posts_analyzed = ?7,
            top_toxic_posts = ?8,
            behavioral_signals = ?9,
            scored_at = datetime('now')",
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
```

Update `get_ranked_threats` to read `behavioral_signals`:

```rust
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
```

**Important:** Update ALL places that construct `AccountScore` to include `behavioral_signals: None` for backward compatibility. Search for `AccountScore {` in:
- `src/scoring/profile.rs` (2 occurrences — insufficient data + full score)
- `tests/composition.rs` (`make_account` helper)
- `src/pipeline/validate.rs` (if it exists)

**Step 4: Run tests to verify they pass**

Run: `cargo test --all-targets`
Expected: All tests pass (including migration test + all existing tests with the new field)

**Step 5: Commit**

```bash
git add src/db/schema.rs src/db/models.rs src/db/queries.rs src/scoring/profile.rs tests/composition.rs
git commit -m "feat: add behavioral_signals column (migration v3) and update AccountScore"
```

---

### Task 8: Wire behavioral signals into build_profile

**Files:**
- Modify: `src/scoring/profile.rs`
- Modify: `src/scoring/behavioral.rs`

**Step 1: Add avg_engagement helper to behavioral.rs**

```rust
/// Compute the mean engagement (likes + reposts) received per post.
pub fn compute_avg_engagement(posts: &[crate::bluesky::posts::Post]) -> f64 {
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

**Step 2: Update build_profile signature and logic**

Update `build_profile` in `src/scoring/profile.rs` to:
1. Accept a `median_engagement: f64` parameter (computed externally from all scored accounts)
2. Accept a `pile_on_dids: &HashSet<String>` parameter (pre-computed)
3. Compute quote ratio from `is_quote` field
4. Call `fetch_reply_ratio`
5. Compute avg engagement
6. Apply `apply_behavioral_modifier` to the raw score
7. Populate `behavioral_signals` field on `AccountScore`

The updated function gains two new parameters:

```rust
pub async fn build_profile(
    client: &PublicAtpClient,
    scorer: &dyn ToxicityScorer,
    target_handle: &str,
    target_did: &str,
    protected_fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
    embedder: Option<&SentenceEmbedder>,
    protected_embedding: Option<&[f64]>,
    median_engagement: f64,
    pile_on_dids: &std::collections::HashSet<String>,
) -> Result<AccountScore> {
    // ... existing steps 1-4 unchanged ...

    // Step 4b: Compute behavioral signals
    let quote_count = target_posts.iter().filter(|p| p.is_quote).count();
    let quote_ratio = behavioral::compute_quote_ratio(quote_count, target_posts.len());

    let (reply_count, reply_total) = posts::fetch_reply_ratio(client, target_handle)
        .await
        .unwrap_or((0, 0));
    let reply_ratio = behavioral::compute_reply_ratio(reply_count, reply_total);

    let avg_engagement = behavioral::compute_avg_engagement(&target_posts);
    let pile_on = pile_on_dids.contains(target_did);

    // Step 5: Compute the raw threat score (same formula as before)
    let (raw_score, _) = threat::compute_threat_score(avg_toxicity, topic_overlap, weights);

    // Step 6: Apply behavioral modifier (gate or multiplier)
    let (final_score, benign_gate) = behavioral::apply_behavioral_modifier(
        raw_score,
        quote_ratio,
        reply_ratio,
        pile_on,
        avg_engagement,
        median_engagement,
    );

    let tier = crate::db::models::ThreatTier::from_score(final_score);

    let behavioral_boost = behavioral::compute_behavioral_boost(quote_ratio, reply_ratio, pile_on);
    let signals = behavioral::BehavioralSignals {
        quote_ratio,
        reply_ratio,
        avg_engagement,
        pile_on,
        benign_gate,
        behavioral_boost,
    };
    let signals_json = serde_json::to_string(&signals)?;

    // ... logging updated to include behavioral info ...

    Ok(AccountScore {
        did: target_did.to_string(),
        handle: target_handle.to_string(),
        toxicity_score: Some(avg_toxicity),
        topic_overlap: Some(topic_overlap),
        threat_score: Some(final_score),
        threat_tier: Some(tier.to_string()),
        posts_analyzed: target_posts.len() as u32,
        top_toxic_posts,
        scored_at: String::new(),
        behavioral_signals: Some(signals_json),
    })
}
```

**Step 3: Run full test suite**

Run: `cargo test --all-targets`
Expected: Compilation errors in callers (amplification.rs, sweep.rs, main.rs) — they need the new parameters. Fix in Task 9.

**Step 4: Commit** (after Task 9 fixes callers)

---

### Task 9: Update pipeline callers with new parameters

**Files:**
- Modify: `src/pipeline/amplification.rs`
- Modify: `src/pipeline/sweep.rs`
- Modify: `src/main.rs`

**Step 1: Add median engagement query to queries.rs**

Add to `src/db/queries.rs`:

```rust
/// Get the median engagement across all scored accounts.
///
/// Returns 0.0 if no accounts have behavioral signals yet.
/// Used as the threshold for the benign gate's engagement condition.
pub fn get_median_engagement(conn: &Connection) -> Result<f64> {
    let mut stmt = conn.prepare(
        "SELECT behavioral_signals FROM account_scores WHERE behavioral_signals IS NOT NULL"
    )?;
    let mut engagements: Vec<f64> = stmt.query_map([], |row| {
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
    if engagements.len() % 2 == 0 {
        Ok((engagements[mid - 1] + engagements[mid]) / 2.0)
    } else {
        Ok(engagements[mid])
    }
}
```

**Step 2: Update amplification pipeline**

Add `median_engagement` and `pile_on_dids` parameters to the `run()` function in `src/pipeline/amplification.rs`. The caller (main.rs) computes these before calling the pipeline:

```rust
pub async fn run(
    // ... existing params ...
    median_engagement: f64,
    pile_on_dids: &std::collections::HashSet<String>,
) -> Result<(usize, usize)> {
    // ... existing code, but update the build_profile call to pass new params ...
}
```

**Step 3: Update sweep pipeline**

Same pattern for `src/pipeline/sweep.rs`.

**Step 4: Update main.rs**

In the `Score`, `Scan`, and `Sweep` commands, compute `median_engagement` and `pile_on_dids` before calling the pipeline:

```rust
// Before calling build_profile or pipeline::run
let median_engagement = charcoal::db::queries::get_median_engagement(&conn)?;
let pile_on_events = charcoal::db::queries::get_events_for_pile_on(&conn)?;
let pile_on_refs: Vec<(&str, &str, &str)> = pile_on_events
    .iter()
    .map(|(d, u, t)| (d.as_str(), u.as_str(), t.as_str()))
    .collect();
let pile_on_dids = charcoal::scoring::behavioral::detect_pile_on_participants(&pile_on_refs);
```

**Step 5: Run full test suite**

Run: `cargo test --all-targets`
Expected: All tests pass

**Step 6: Commit Tasks 8 + 9 together**

```bash
git add src/scoring/profile.rs src/scoring/behavioral.rs src/pipeline/amplification.rs src/pipeline/sweep.rs src/main.rs src/db/queries.rs
git commit -m "feat: wire behavioral signals into scoring pipeline and all callers"
```

---

### Task 10: Real-world persona tests

**Files:**
- Modify: `tests/unit_behavioral.rs`

**Step 1: Write the persona tests**

```rust
// ============================================================
// Real-world persona scenarios
// ============================================================

use charcoal::scoring::threat::{compute_threat_score, ThreatWeights};

/// The Quote-Dunker: 80% quotes, moderate toxicity, high overlap
/// Should get behavioral boost pushing from Watch toward Elevated
#[test]
fn persona_the_quote_dunker() {
    let weights = ThreatWeights::default();
    let toxicity = 0.15;
    let overlap = 0.40;

    // Raw score: 0.15 * 70 * (1 + 0.40 * 1.5) = 10.5 * 1.6 = 16.8 (Elevated)
    let (raw_score, raw_tier) = compute_threat_score(toxicity, overlap, &weights);
    assert!((raw_score - 16.8).abs() < 0.1);

    // With behavioral boost: quote_ratio=0.80, reply_ratio=0.30, no pile-on
    // boost = 1.0 + 0.80*0.20 + 0.30*0.15 = 1.0 + 0.16 + 0.045 = 1.205
    let (final_score, benign) = apply_behavioral_modifier(
        raw_score, 0.80, 0.30, false, 20.0, 10.0,
    );
    assert!(!benign);
    // 16.8 * 1.205 = 20.244
    assert!(final_score > raw_score, "Boost should increase score");
    assert!((final_score - 20.244).abs() < 0.1);
}

/// The Supportive Ally: 5% quotes, low toxicity, high overlap
/// Should trigger benign gate, capping at 12.0
#[test]
fn persona_the_supportive_ally() {
    let weights = ThreatWeights::default();
    let toxicity = 0.10;
    let overlap = 0.70;

    // Raw score: 0.10 * 70 * (1 + 0.70 * 1.5) = 7.0 * 2.05 = 14.35 (Watch)
    let (raw_score, _) = compute_threat_score(toxicity, overlap, &weights);
    assert!((raw_score - 14.35).abs() < 0.1);

    // Benign: quote=0.05 (<0.15), reply=0.10 (<0.30), no pile-on, engagement 25 > median 10
    let (final_score, benign) = apply_behavioral_modifier(
        raw_score, 0.05, 0.10, false, 25.0, 10.0,
    );
    assert!(benign, "Ally should trigger benign gate");
    assert!((final_score - 12.0).abs() < f64::EPSILON, "Should be capped at 12.0");
    let tier = charcoal::db::models::ThreatTier::from_score(final_score);
    assert_eq!(tier, ThreatTier::Watch, "Ally should stay at Watch");
}

/// The Pile-On Participant: moderate toxicity, part of a 7-account pile-on
/// Should get pile-on boost pushing into Elevated
#[test]
fn persona_the_pile_on_participant() {
    let weights = ThreatWeights::default();
    let toxicity = 0.20;
    let overlap = 0.35;

    // Raw: 0.20 * 70 * (1 + 0.35 * 1.5) = 14.0 * 1.525 = 21.35 (Elevated)
    let (raw_score, _) = compute_threat_score(toxicity, overlap, &weights);

    // With pile-on: quote=0.30, reply=0.20, pile_on=true
    // boost = 1.0 + 0.30*0.20 + 0.20*0.15 + 0.15 = 1.0 + 0.06 + 0.03 + 0.15 = 1.24
    let (final_score, benign) = apply_behavioral_modifier(
        raw_score, 0.30, 0.20, true, 8.0, 10.0,
    );
    assert!(!benign);
    // 21.35 * 1.24 = 26.474
    assert!((final_score - 26.474).abs() < 0.1);
    let tier = charcoal::db::models::ThreatTier::from_score(final_score);
    assert_eq!(tier, ThreatTier::Elevated);
}

/// The Lurker Reposter: low post count, low engagement, few quotes
/// Doesn't trigger benign gate (engagement too low), gets small boost
#[test]
fn persona_the_lurker_reposter() {
    let weights = ThreatWeights::default();
    let toxicity = 0.25;
    let overlap = 0.30;

    // Raw: 0.25 * 70 * (1 + 0.30 * 1.5) = 17.5 * 1.45 = 25.375 (Elevated)
    let (raw_score, _) = compute_threat_score(toxicity, overlap, &weights);

    // Low engagement (2.0 < median 10.0) blocks benign gate even with low ratios
    // quote=0.05, reply=0.15, no pile-on
    // boost = 1.0 + 0.05*0.20 + 0.15*0.15 = 1.0 + 0.01 + 0.0225 = 1.0325
    let (final_score, benign) = apply_behavioral_modifier(
        raw_score, 0.05, 0.15, false, 2.0, 10.0,
    );
    assert!(!benign, "Low engagement should block benign gate");
    // 25.375 * 1.0325 = 26.2
    assert!((final_score - 26.2).abs() < 0.5);
}

/// High toxicity + benign behavior: gate should prevent High tier
#[test]
fn persona_high_tox_benign_behavior() {
    let weights = ThreatWeights::default();
    let toxicity = 0.50;
    let overlap = 0.50;

    // Raw: 0.50 * 70 * (1 + 0.50 * 1.5) = 35 * 1.75 = 61.25 (High!)
    let (raw_score, raw_tier) = compute_threat_score(toxicity, overlap, &weights);
    assert_eq!(raw_tier, ThreatTier::High);

    // But benign behavior caps at 12.0
    let (final_score, benign) = apply_behavioral_modifier(
        raw_score, 0.05, 0.10, false, 30.0, 10.0,
    );
    assert!(benign);
    assert!((final_score - 12.0).abs() < f64::EPSILON);
    let tier = charcoal::db::models::ThreatTier::from_score(final_score);
    assert_eq!(tier, ThreatTier::Watch, "Benign gate prevents High tier");
}
```

**Step 2: Run tests**

Run: `cargo test --test unit_behavioral persona -v`
Expected: All 5 persona tests pass

**Step 3: Run full test suite**

Run: `cargo test --all-targets`
Expected: All tests pass (139 original + new behavioral tests)

**Step 4: Commit**

```bash
git add tests/unit_behavioral.rs
git commit -m "test: add real-world persona scenarios for behavioral signals"
```

---

### Task 11: Final cleanup — cargo fmt, clippy, full test suite

**Files:** All modified files

**Step 1: Format**

Run: `cargo fmt`

**Step 2: Clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Fix any warnings.

**Step 3: Full test suite**

Run: `cargo test --all-targets`
Expected: All tests pass.

**Step 4: Commit any formatting/clippy fixes**

```bash
git add -u
git commit -m "style: apply cargo fmt and fix clippy warnings"
```

---

## Summary of all files changed

| File | Action | Purpose |
|------|--------|---------|
| `src/scoring/behavioral.rs` | Create | BehavioralSignals struct, boost/gate/ratio functions, pile-on detection |
| `src/scoring/mod.rs` | Modify | Add `pub mod behavioral;` |
| `src/bluesky/posts.rs` | Modify | Add `is_quote` field, `fetch_reply_ratio()` function |
| `src/db/schema.rs` | Modify | Migration v3: `behavioral_signals TEXT` column |
| `src/db/models.rs` | Modify | Add `behavioral_signals: Option<String>` to `AccountScore` |
| `src/db/queries.rs` | Modify | Update upsert/read queries, add `get_events_for_pile_on()`, `get_median_engagement()` |
| `src/scoring/profile.rs` | Modify | Wire behavioral computation into `build_profile()` |
| `src/pipeline/amplification.rs` | Modify | Pass new params through to `build_profile()` |
| `src/pipeline/sweep.rs` | Modify | Pass new params through to `build_profile()` |
| `src/main.rs` | Modify | Compute pile-on/median before calling pipelines |
| `tests/unit_behavioral.rs` | Create | All behavioral signal unit tests + persona scenarios |
| `tests/composition.rs` | Modify | Update `make_account` helper with `behavioral_signals` field |
