// Profile filter — drop non-viable candidates before the expensive stages.
//
// Harvesting (jetstream + seeds) casts a wide net and catches bots, abandoned
// eggs, and brand-new accounts alongside real people. This stage fetches each
// candidate's profile via `app.bsky.actor.getProfiles` (batched, 25 at a time)
// and applies cheap activity filters so the later engagement-stratification and
// dry-run stages only spend effort on accounts that could actually run Charcoal
// and be scored.
//
// The filter is deliberately conservative: its job is to remove accounts that
// *can't be scored*, not to bias the engagement distribution. Charcoal itself
// treats accounts with fewer than 5 posts as "Insufficient Data", so that's the
// principled default floor. Follower and account-age gates default to off.
//
// As usual, the pure decision logic (`evaluate`) is split from the network I/O
// (`fetch_profile_stats`, `filter_candidates`) so it can be unit-tested.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tracing::{info, warn};

use crate::bluesky::client::PublicAtpClient;

/// The subset of profile fields we need to assess viability. Kept free of
/// atrium types so the filter logic is testable without API fixtures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileStats {
    pub did: String,
    pub handle: String,
    pub posts_count: Option<i64>,
    pub followers_count: Option<i64>,
    pub follows_count: Option<i64>,
    /// Account creation time as an RFC 3339 string, when the API reports it.
    pub created_at: Option<String>,
}

/// Tunable viability thresholds.
#[derive(Debug, Clone)]
pub struct FilterThresholds {
    /// Minimum post count. Defaults to 5 — below this Charcoal can't score the
    /// account reliably anyway.
    pub min_posts: i64,
    /// Minimum follower count. Defaults to 0 (no follower gate) so the sample
    /// isn't biased away from small but real accounts.
    pub min_followers: i64,
    /// Minimum account age in days. Defaults to 0 (no age gate). Brand-new
    /// accounts have unstable fingerprints; raise this to exclude them.
    pub min_account_age_days: i64,
}

impl Default for FilterThresholds {
    fn default() -> Self {
        Self {
            min_posts: 5,
            min_followers: 0,
            min_account_age_days: 0,
        }
    }
}

/// Why a candidate was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// Profile lacked a post count — usually deactivated/suspended/unresolvable.
    MissingData,
    /// Fewer posts than `min_posts`.
    TooFewPosts,
    /// Fewer followers than `min_followers`.
    TooFewFollowers,
    /// Account younger than `min_account_age_days`.
    TooNew,
}

impl RejectReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            RejectReason::MissingData => "missing_data",
            RejectReason::TooFewPosts => "too_few_posts",
            RejectReason::TooFewFollowers => "too_few_followers",
            RejectReason::TooNew => "too_new",
        }
    }
}

/// Parse an RFC 3339 timestamp into UTC, returning `None` on any parse failure.
fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Decide whether a candidate passes the filter.
///
/// Returns `None` if the account is viable, or `Some(reason)` describing why it
/// was rejected. `now` is passed in (rather than read from the clock) so the
/// age check is deterministic and testable. A missing `created_at` does *not*
/// reject the account — we only fail the age check when we have a date and it's
/// too recent.
pub fn evaluate(
    stats: &ProfileStats,
    thresholds: &FilterThresholds,
    now: DateTime<Utc>,
) -> Option<RejectReason> {
    let posts = match stats.posts_count {
        Some(p) => p,
        None => return Some(RejectReason::MissingData),
    };
    if posts < thresholds.min_posts {
        return Some(RejectReason::TooFewPosts);
    }

    if thresholds.min_followers > 0 && stats.followers_count.unwrap_or(0) < thresholds.min_followers
    {
        return Some(RejectReason::TooFewFollowers);
    }

    if thresholds.min_account_age_days > 0 {
        if let Some(created) = stats.created_at.as_deref().and_then(parse_rfc3339) {
            let age_days = (now - created).num_days();
            if age_days < thresholds.min_account_age_days {
                return Some(RejectReason::TooNew);
            }
        }
    }

    None
}

/// Outcome of filtering a candidate set.
#[derive(Debug, Default)]
pub struct FilterReport {
    /// Number of candidate DIDs submitted.
    pub requested: usize,
    /// DIDs `getProfiles` didn't return (invalid/suspended/deactivated).
    pub not_found: usize,
    /// Surviving candidates with their fetched stats.
    pub kept: Vec<ProfileStats>,
    /// Count of rejections keyed by reason string.
    pub rejected_by_reason: BTreeMap<&'static str, usize>,
}

impl FilterReport {
    /// Total number of profiles rejected by the filter.
    pub fn rejected_count(&self) -> usize {
        self.rejected_by_reason.values().sum()
    }
}

/// Convert an atrium detailed profile into our plain `ProfileStats`.
fn to_stats(profile: &atrium_api::app::bsky::actor::defs::ProfileViewDetailed) -> ProfileStats {
    ProfileStats {
        did: profile.did.as_str().to_string(),
        handle: profile.handle.as_str().to_string(),
        posts_count: profile.posts_count,
        followers_count: profile.followers_count,
        follows_count: profile.follows_count,
        created_at: profile.created_at.as_ref().map(|d| d.as_str().to_string()),
    }
}

/// Fetch detailed profile stats for a batch of DIDs via `getProfiles`.
///
/// Requests are chunked into groups of 25 (the API maximum). DIDs that fail to
/// resolve are simply omitted from the result; batch-level errors are logged and
/// skipped so one bad batch doesn't sink the whole fetch.
pub async fn fetch_profile_stats(client: &PublicAtpClient, dids: &[String]) -> Vec<ProfileStats> {
    use atrium_api::app::bsky::actor::get_profiles;

    let mut out = Vec::new();
    for chunk in dids.chunks(25) {
        let params: Vec<(&str, &str)> = chunk.iter().map(|d| ("actors", d.as_str())).collect();
        if params.is_empty() {
            continue;
        }
        match client
            .xrpc_get::<get_profiles::Output>("app.bsky.actor.getProfiles", &params)
            .await
        {
            Ok(output) => {
                for profile in &output.profiles {
                    out.push(to_stats(profile));
                }
            }
            Err(e) => {
                warn!(error = %e, batch_size = chunk.len(), "getProfiles batch failed, skipping");
            }
        }
    }
    out
}

/// Fetch profiles for the candidate DIDs and apply the viability filter.
pub async fn filter_candidates(
    client: &PublicAtpClient,
    dids: &[String],
    thresholds: &FilterThresholds,
) -> FilterReport {
    let now = Utc::now();
    let stats = fetch_profile_stats(client, dids).await;
    let not_found = dids.len().saturating_sub(stats.len());

    let mut report = FilterReport {
        requested: dids.len(),
        not_found,
        ..Default::default()
    };

    for s in stats {
        match evaluate(&s, thresholds, now) {
            None => report.kept.push(s),
            Some(reason) => {
                *report
                    .rejected_by_reason
                    .entry(reason.as_str())
                    .or_insert(0) += 1;
            }
        }
    }

    info!(
        requested = report.requested,
        kept = report.kept.len(),
        rejected = report.rejected_count(),
        not_found = report.not_found,
        "Profile filter complete"
    );
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(posts: Option<i64>, followers: Option<i64>, created: Option<&str>) -> ProfileStats {
        ProfileStats {
            did: "did:plc:test".to_string(),
            handle: "test.bsky.social".to_string(),
            posts_count: posts,
            followers_count: followers,
            follows_count: Some(10),
            created_at: created.map(|s| s.to_string()),
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-04T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn passes_with_default_thresholds() {
        let t = FilterThresholds::default();
        assert_eq!(evaluate(&stats(Some(50), Some(100), None), &t, now()), None);
    }

    #[test]
    fn rejects_too_few_posts() {
        let t = FilterThresholds::default();
        assert_eq!(
            evaluate(&stats(Some(3), Some(100), None), &t, now()),
            Some(RejectReason::TooFewPosts)
        );
    }

    #[test]
    fn rejects_missing_post_count_as_missing_data() {
        let t = FilterThresholds::default();
        assert_eq!(
            evaluate(&stats(None, Some(100), None), &t, now()),
            Some(RejectReason::MissingData)
        );
    }

    #[test]
    fn follower_gate_off_by_default() {
        // 0 followers passes under defaults (min_followers = 0).
        let t = FilterThresholds::default();
        assert_eq!(evaluate(&stats(Some(50), Some(0), None), &t, now()), None);
    }

    #[test]
    fn rejects_too_few_followers_when_gate_set() {
        let t = FilterThresholds {
            min_followers: 10,
            ..Default::default()
        };
        assert_eq!(
            evaluate(&stats(Some(50), Some(3), None), &t, now()),
            Some(RejectReason::TooFewFollowers)
        );
    }

    #[test]
    fn rejects_too_new_when_age_gate_set() {
        let t = FilterThresholds {
            min_account_age_days: 30,
            ..Default::default()
        };
        // Created 5 days before `now` → too new.
        let s = stats(Some(50), Some(100), Some("2026-05-30T00:00:00Z"));
        assert_eq!(evaluate(&s, &t, now()), Some(RejectReason::TooNew));
    }

    #[test]
    fn passes_age_gate_when_old_enough() {
        let t = FilterThresholds {
            min_account_age_days: 30,
            ..Default::default()
        };
        let s = stats(Some(50), Some(100), Some("2026-01-01T00:00:00Z"));
        assert_eq!(evaluate(&s, &t, now()), None);
    }

    #[test]
    fn missing_created_at_does_not_reject_on_age() {
        let t = FilterThresholds {
            min_account_age_days: 30,
            ..Default::default()
        };
        // No created_at → age check can't fail it.
        assert_eq!(evaluate(&stats(Some(50), Some(100), None), &t, now()), None);
    }

    #[test]
    fn report_rejected_count_sums_reasons() {
        let mut r = FilterReport::default();
        r.rejected_by_reason.insert("too_few_posts", 3);
        r.rejected_by_reason.insert("missing_data", 2);
        assert_eq!(r.rejected_count(), 5);
    }
}
