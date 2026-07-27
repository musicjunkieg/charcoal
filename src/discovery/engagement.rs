// Engagement stratification — measure the per-candidate cost driver for free.
//
// The dominant term in Charcoal's Zentropi cost is the follower fan-out from
// quote/reply amplifiers: each such amplifier drags in up to 50 follower scans.
// So the single most important number to know about a candidate is `Q`, the
// count of *distinct quote/reply amplifiers* their posts attract. This stage
// measures it using only free, public reads — Constellation backlinks plus
// (optionally) reply threads — with no toxicity scoring and no Zentropi calls.
//
// Engagement is heavily power-law distributed: a handful of viral/heavily-
// targeted accounts dwarf everyone else. Bucketing candidates into strata by Q
// lets the final network estimate sample *within* strata and reweight, so the
// expensive tail is represented instead of being averaged away.
//
// As elsewhere, the pure aggregation (`summarize`) and bucketing (`assign_stratum`)
// are split from the network I/O (`collect_engagement`) for unit testing.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;
use tracing::warn;

use crate::bluesky::amplification::AmplificationNotification;
use crate::bluesky::client::PublicAtpClient;
use crate::bluesky::{posts, replies};
use crate::constellation::client::ConstellationClient;

/// Event types that trigger follower fan-out in the real scan pipeline, and
/// therefore drive the dominant Zentropi cost term. Quotes are the primary
/// harassment vector; replies are the other direct-engagement fan-out trigger.
const FANOUT_EVENT_TYPES: [&str; 2] = ["quote", "reply"];

/// A candidate's engagement profile — the inputs to its cost stratum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngagementProfile {
    pub did: String,
    pub handle: String,
    /// Number of the candidate's recent posts examined for backlinks.
    pub post_count: usize,
    /// `A`: distinct amplifier DIDs across all collected event types.
    pub total_amplifiers: usize,
    /// `Q`: distinct amplifier DIDs from fan-out events (quotes + replies) —
    /// the dominant cost driver.
    pub fanout_amplifiers: usize,
    /// Raw event counts keyed by event type (quote/repost/like/reply).
    pub events_by_type: BTreeMap<String, usize>,
    /// The cost stratum this candidate falls into, based on `fanout_amplifiers`.
    pub stratum: EngagementStratum,
}

/// Cost strata keyed on the fan-out amplifier count `Q`. Boundaries are chosen
/// to separate the long flat body of the distribution from the expensive tail,
/// which is where sampling needs the most resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EngagementStratum {
    /// Q = 0 — no fan-out; contributes ~0 Zentropi calls.
    None,
    /// Q in 1..=9.
    Low,
    /// Q in 10..=49.
    Medium,
    /// Q in 50..=199.
    High,
    /// Q >= 200 — the viral/heavily-targeted tail.
    Viral,
}

impl EngagementStratum {
    pub fn as_str(&self) -> &'static str {
        match self {
            EngagementStratum::None => "none",
            EngagementStratum::Low => "low",
            EngagementStratum::Medium => "medium",
            EngagementStratum::High => "high",
            EngagementStratum::Viral => "viral",
        }
    }
}

/// Assign a cost stratum from the fan-out amplifier count.
pub fn assign_stratum(fanout_amplifiers: usize) -> EngagementStratum {
    match fanout_amplifiers {
        0 => EngagementStratum::None,
        1..=9 => EngagementStratum::Low,
        10..=49 => EngagementStratum::Medium,
        50..=199 => EngagementStratum::High,
        _ => EngagementStratum::Viral,
    }
}

/// Aggregate collected amplification events into an engagement profile.
///
/// `A` counts distinct amplifier DIDs across every event type; `Q` counts
/// distinct amplifier DIDs from fan-out events (quotes + replies) only. Event
/// counts are raw (one post quoted twice by the same account counts as two
/// quote events but one distinct amplifier).
pub fn summarize(
    did: &str,
    handle: &str,
    post_count: usize,
    events: &[AmplificationNotification],
) -> EngagementProfile {
    let mut all_amplifiers: HashSet<&str> = HashSet::new();
    let mut fanout_amplifiers: HashSet<&str> = HashSet::new();
    let mut events_by_type: BTreeMap<String, usize> = BTreeMap::new();

    for event in events {
        all_amplifiers.insert(event.amplifier_did.as_str());
        if FANOUT_EVENT_TYPES.contains(&event.event_type.as_str()) {
            fanout_amplifiers.insert(event.amplifier_did.as_str());
        }
        *events_by_type.entry(event.event_type.clone()).or_insert(0) += 1;
    }

    let fanout = fanout_amplifiers.len();
    EngagementProfile {
        did: did.to_string(),
        handle: handle.to_string(),
        post_count,
        total_amplifiers: all_amplifiers.len(),
        fanout_amplifiers: fanout,
        events_by_type,
        stratum: assign_stratum(fanout),
    }
}

/// What to collect when measuring engagement. Constellation quotes + reposts are
/// always fetched (cheap, 2 calls/post). Likes and replies are opt-in because
/// they add API cost: likes inflate `A` but not `Q`; replies require per-post
/// thread fetches plus the candidate's follow graph for drive-by filtering.
#[derive(Debug, Clone)]
pub struct EngagementOptions {
    /// How many recent posts to examine for backlinks.
    pub max_posts: usize,
    /// Also query Constellation for likes (affects `A`, not `Q`).
    pub include_likes: bool,
    /// Also detect drive-by replies (affects `Q` — more accurate, more calls).
    pub include_replies: bool,
}

impl Default for EngagementOptions {
    fn default() -> Self {
        Self {
            max_posts: 50,
            include_likes: false,
            include_replies: false,
        }
    }
}

/// Measure a candidate's engagement using free public reads.
///
/// Fetches the candidate's recent posts, queries Constellation for quote/repost
/// (and optionally like) backlinks, optionally detects drive-by replies, and
/// summarizes the result into an `EngagementProfile`. Performs no toxicity
/// scoring and makes no Zentropi calls.
pub async fn collect_engagement(
    client: &PublicAtpClient,
    constellation: &ConstellationClient,
    did: &str,
    handle: &str,
    opts: &EngagementOptions,
) -> EngagementProfile {
    let (post_count, events) = collect_events(client, constellation, did, handle, opts).await;
    summarize(did, handle, post_count, &events)
}

/// Collect the raw amplification events for a candidate (the I/O behind
/// `collect_engagement`). Returns `(post_count, events)`. Shared with the
/// dry-run stage, which needs the individual events — not just the summary — to
/// drive per-amplifier scoring.
pub async fn collect_events(
    client: &PublicAtpClient,
    constellation: &ConstellationClient,
    did: &str,
    handle: &str,
    opts: &EngagementOptions,
) -> (usize, Vec<AmplificationNotification>) {
    let posts = match posts::fetch_recent_posts(client, handle, opts.max_posts).await {
        Ok(p) => p,
        Err(e) => {
            warn!(handle, error = %e, "Failed to fetch posts for engagement, treating as empty");
            return (0, Vec::new());
        }
    };

    let post_uris: Vec<String> = posts.iter().map(|p| p.uri.clone()).collect();

    // Quotes + reposts (always). Constellation does 2 backlink calls per URI.
    let mut events = constellation.find_amplification_events(&post_uris).await;

    if opts.include_likes {
        events.extend(constellation.find_likers(&post_uris).await);
    }

    if opts.include_replies {
        // The candidate is the protected user here, so drive-by filtering uses
        // the candidate's own follow graph. This is the API-heavy path: one
        // thread fetch per post plus paginated follows.
        let follows = replies::fetch_follows_set(client, did)
            .await
            .unwrap_or_default();
        for post in &posts {
            match replies::fetch_replies_to_post(client, &post.uri).await {
                Ok(reps) => {
                    let reply_dids: Vec<String> = reps.iter().map(|(d, _, _)| d.clone()).collect();
                    let drive_by: HashSet<String> =
                        replies::filter_drive_by_replies_excluding_self(&reply_dids, &follows, did)
                            .into_iter()
                            .collect();
                    for (reply_did, _text, reply_uri) in &reps {
                        if drive_by.contains(reply_did) {
                            events.push(AmplificationNotification {
                                event_type: "reply".to_string(),
                                amplifier_did: reply_did.clone(),
                                amplifier_handle: reply_did.clone(),
                                original_post_uri: Some(post.uri.clone()),
                                amplifier_post_uri: reply_uri.clone(),
                                indexed_at: String::new(),
                            });
                        }
                    }
                }
                Err(e) => {
                    warn!(uri = post.uri, error = %e, "Failed to fetch replies for engagement");
                }
            }
        }
    }

    (posts.len(), events)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(event_type: &str, did: &str) -> AmplificationNotification {
        AmplificationNotification {
            event_type: event_type.to_string(),
            amplifier_did: did.to_string(),
            amplifier_handle: did.to_string(),
            original_post_uri: Some("at://orig".to_string()),
            amplifier_post_uri: format!("at://{did}/post"),
            indexed_at: String::new(),
        }
    }

    #[test]
    fn stratum_boundaries() {
        assert_eq!(assign_stratum(0), EngagementStratum::None);
        assert_eq!(assign_stratum(1), EngagementStratum::Low);
        assert_eq!(assign_stratum(9), EngagementStratum::Low);
        assert_eq!(assign_stratum(10), EngagementStratum::Medium);
        assert_eq!(assign_stratum(49), EngagementStratum::Medium);
        assert_eq!(assign_stratum(50), EngagementStratum::High);
        assert_eq!(assign_stratum(199), EngagementStratum::High);
        assert_eq!(assign_stratum(200), EngagementStratum::Viral);
    }

    #[test]
    fn summarize_counts_distinct_amplifiers() {
        let events = vec![
            event("quote", "did:a"),
            event("repost", "did:b"),
            event("like", "did:c"),
        ];
        let p = summarize("did:user", "user.bsky.social", 50, &events);
        assert_eq!(p.total_amplifiers, 3);
        // Only the quote counts toward fan-out.
        assert_eq!(p.fanout_amplifiers, 1);
        assert_eq!(p.stratum, EngagementStratum::Low);
    }

    #[test]
    fn fanout_includes_quotes_and_replies_only() {
        let events = vec![
            event("quote", "did:a"),
            event("reply", "did:b"),
            event("repost", "did:c"),
            event("like", "did:d"),
        ];
        let p = summarize("did:user", "user", 50, &events);
        assert_eq!(p.total_amplifiers, 4);
        assert_eq!(p.fanout_amplifiers, 2); // quote + reply
    }

    #[test]
    fn summarize_dedups_same_amplifier_across_events() {
        // Same account quotes two different posts → 1 distinct amplifier, 2 events.
        let events = vec![event("quote", "did:a"), event("quote", "did:a")];
        let p = summarize("did:user", "user", 50, &events);
        assert_eq!(p.total_amplifiers, 1);
        assert_eq!(p.fanout_amplifiers, 1);
        assert_eq!(p.events_by_type.get("quote"), Some(&2));
    }

    #[test]
    fn summarize_empty_is_none_stratum() {
        let p = summarize("did:user", "user", 0, &[]);
        assert_eq!(p.total_amplifiers, 0);
        assert_eq!(p.fanout_amplifiers, 0);
        assert_eq!(p.stratum, EngagementStratum::None);
        assert!(p.events_by_type.is_empty());
    }

    #[test]
    fn events_by_type_tallies_each_type() {
        let events = vec![
            event("quote", "did:a"),
            event("quote", "did:b"),
            event("like", "did:c"),
        ];
        let p = summarize("did:user", "user", 50, &events);
        assert_eq!(p.events_by_type.get("quote"), Some(&2));
        assert_eq!(p.events_by_type.get("like"), Some(&1));
        assert_eq!(p.events_by_type.get("repost"), None);
    }
}
