// Behavioral signals — post-pattern analysis for scoring adjustment.
//
// Computes behavioral signals (quote ratio, reply ratio, engagement,
// pile-on participation) and uses them as a gate + multiplier hybrid:
// - Benign gate: caps score at 12.0 for clearly non-threatening accounts
// - Hostile multiplier: boosts score by 1.0-1.5x for hostile patterns

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::bluesky::posts::Post;

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

/// Compute the mean engagement (likes + reposts) received per post.
pub fn compute_avg_engagement(posts: &[Post]) -> f64 {
    if posts.is_empty() {
        return 0.0;
    }
    let total: f64 = posts
        .iter()
        .map(|p| (p.like_count + p.repost_count) as f64)
        .sum();
    total / posts.len() as f64
}

/// Compute the fraction of posts that are quote-posts.
/// Returns 0.0 if total_posts is 0.
pub fn compute_quote_ratio(quote_count: usize, total_posts: usize) -> f64 {
    if total_posts == 0 {
        return 0.0;
    }
    quote_count as f64 / total_posts as f64
}

/// Compute the fraction of posts that are replies.
/// Returns 0.0 if total_posts is 0.
pub fn compute_reply_ratio(reply_count: usize, total_posts: usize) -> f64 {
    if total_posts == 0 {
        return 0.0;
    }
    reply_count as f64 / total_posts as f64
}

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

/// Benign gate thresholds
const BENIGN_QUOTE_RATIO_MAX: f64 = 0.15;
const BENIGN_REPLY_RATIO_MAX: f64 = 0.30;
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
pub fn detect_pile_on_participants(events: &[(&str, &str, &str)]) -> HashSet<String> {
    let mut result = HashSet::new();

    // Group events by original_post_uri
    let mut by_post: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
    for &(did, uri, ts) in events {
        by_post.entry(uri).or_default().push((did, ts));
    }

    for (_uri, mut post_events) in by_post {
        // Sort by timestamp
        post_events.sort_by_key(|&(_, ts)| ts.to_string());

        // Parse timestamps and collect (did, timestamp_secs) pairs
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

            for &(did, ts) in parsed.iter().skip(i) {
                if ts > window_end {
                    break;
                }
                unique_dids.insert(did);
            }

            if unique_dids.len() >= PILE_ON_THRESHOLD {
                for did in unique_dids {
                    result.insert(did.to_string());
                }
            }
        }
    }

    result
}
