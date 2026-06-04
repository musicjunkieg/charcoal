// Dry-run runner — count would-be Zentropi calls for a candidate's scan.
//
// This drives the *real* scoring function (`scoring::profile::build_profile` —
// the code that actually contains the Zentropi call site) over exactly the set
// of accounts a real scan would score: the candidate's amplifiers, plus the
// deduplicated followers of their quote/reply amplifiers. The only substitution
// is the scorer: a `CountingScorer` wrapping ONNX, which tallies would-be
// Zentropi calls at the two-stage gate instead of making them.
//
// Fidelity notes:
//   - NLI is disabled here (embedder/nli = None). That matches a scan run
//     without NLI. With NLI on, followers scoring above the Watch threshold get
//     re-scored in a second `build_profile` pass, which re-runs the toxicity
//     classification and roughly doubles their Zentropi calls — so the numbers
//     here are a lower bound for an NLI-enabled deployment.
//   - Follower dedup mirrors the pipeline's DB-staleness skip: each distinct
//     account (amplifier or follower) is scored at most once per run.
//   - Topic overlap uses TF-IDF (embedder = None), exactly as the pipeline's
//     fallback path does.

use std::collections::HashSet;

use futures::stream::{self, StreamExt};
use serde::Serialize;
use tracing::warn;

use crate::bluesky::client::PublicAtpClient;
use crate::bluesky::{followers, posts, profiles};
use crate::constellation::client::ConstellationClient;
use crate::discovery::counting_scorer::{CountSnapshot, CountingScorer};
use crate::discovery::engagement::{self, EngagementOptions, EngagementStratum};
use crate::scoring::profile::build_profile;
use crate::scoring::threat::ThreatWeights;
use crate::topics::fingerprint::TopicFingerprint;
use crate::topics::tfidf::TfIdfExtractor;
use crate::topics::traits::TopicExtractor;

/// Event types whose amplifiers trigger follower fan-out (and thus dominate cost).
const FANOUT_EVENT_TYPES: [&str; 2] = ["quote", "reply"];

/// How many of the candidate's own posts to fingerprint against.
const FINGERPRINT_POST_COUNT: usize = 200;

/// Options controlling a dry run.
#[derive(Debug, Clone)]
pub struct DryRunOptions {
    /// Engagement collection options (post window, likes, replies).
    pub engagement: EngagementOptions,
    /// Followers to fetch per quote/reply amplifier (the pipeline default is 50).
    pub max_followers: usize,
    /// Concurrent `build_profile` calls while scoring followers.
    pub concurrency: usize,
}

impl Default for DryRunOptions {
    fn default() -> Self {
        Self {
            engagement: EngagementOptions::default(),
            max_followers: 50,
            concurrency: 8,
        }
    }
}

/// Per-candidate dry-run result.
#[derive(Debug, Clone, Serialize)]
pub struct CandidateDryRun {
    pub did: String,
    pub handle: String,
    /// `Q`: distinct quote/reply amplifiers — the fan-out driver.
    pub fanout_amplifiers: usize,
    pub stratum: EngagementStratum,
    /// Accounts actually scored (amplifiers + deduped followers).
    pub amplifiers_scored: usize,
    pub followers_scored: usize,
    /// Counter delta attributed to this candidate's scan.
    pub counts: CountSnapshot,
}

impl CandidateDryRun {
    fn empty(did: &str, handle: &str) -> Self {
        Self {
            did: did.to_string(),
            handle: handle.to_string(),
            fanout_amplifiers: 0,
            stratum: EngagementStratum::None,
            amplifiers_scored: 0,
            followers_scored: 0,
            counts: CountSnapshot {
                posts_classified: 0,
                posts_cleared: 0,
                zentropi_calls: 0,
            },
        }
    }
}

/// Build the candidate's topic fingerprint from their recent posts (TF-IDF).
/// Returns `None` when there aren't enough posts to fingerprint — the same
/// condition under which a real scan can't proceed.
async fn build_candidate_fingerprint(
    client: &PublicAtpClient,
    handle: &str,
) -> Option<TopicFingerprint> {
    let posts = posts::fetch_recent_posts(client, handle, FINGERPRINT_POST_COUNT)
        .await
        .ok()?;
    if posts.is_empty() {
        return None;
    }
    let texts: Vec<String> = posts.iter().map(|p| p.text.clone()).collect();
    TfIdfExtractor::default().extract(&texts).ok()
}

/// Score one account through the real `build_profile`, with NLI/embeddings off.
/// Counting happens as a side effect inside the `CountingScorer`.
async fn score_one(
    client: &PublicAtpClient,
    scorer: &CountingScorer,
    handle: &str,
    did: &str,
    fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
    pile_on: &HashSet<String>,
) {
    if let Err(e) = build_profile(
        client,
        scorer,
        handle,
        did,
        fingerprint,
        weights,
        None, // embedder → TF-IDF overlap
        None, // protected_embedding
        0.0,  // median_engagement
        pile_on,
        None, // nli_scorer
        None, // protected_posts_with_embeddings
        None, // direct_pairs
        None, // data_dir (no audit logging)
        None, // graph_distance
    )
    .await
    {
        warn!(handle, error = %e, "dry-run build_profile failed, skipping account");
    }
}

/// Run a dry-run scan for a single candidate and return the would-be Zentropi
/// call count (plus supporting counters), attributed via a before/after
/// snapshot of the shared `CountingScorer`.
pub async fn dry_run_candidate(
    client: &PublicAtpClient,
    constellation: &ConstellationClient,
    scorer: &CountingScorer,
    did: &str,
    handle: &str,
    opts: &DryRunOptions,
) -> CandidateDryRun {
    let stats = scorer.stats();
    let before = stats.snapshot();

    // A scan can't proceed without a fingerprint; mirror that.
    let fingerprint = match build_candidate_fingerprint(client, handle).await {
        Some(fp) => fp,
        None => {
            warn!(handle, "No fingerprint for candidate, dry-run yields zero");
            return CandidateDryRun::empty(did, handle);
        }
    };

    let (_post_count, events) =
        engagement::collect_events(client, constellation, did, handle, &opts.engagement).await;
    let summary = engagement::summarize(did, handle, 0, &events);

    // Resolve amplifier DIDs to handles (Constellation returns DID-as-handle).
    let amplifier_dids: Vec<String> = {
        let mut seen = HashSet::new();
        events
            .iter()
            .map(|e| e.amplifier_did.clone())
            .filter(|d| seen.insert(d.clone()))
            .collect()
    };
    let handles = profiles::resolve_dids_to_handles(client, &amplifier_dids)
        .await
        .unwrap_or_default();

    let weights = ThreatWeights::default();
    let empty_pile: HashSet<String> = HashSet::new();

    // `scored` mirrors the pipeline's DB-staleness dedup: an account scored once
    // (as an amplifier or a follower) is never re-scored in the same run. Seed it
    // with the candidate's own DID so they're excluded from their own report.
    let mut scored: HashSet<String> = HashSet::new();
    scored.insert(did.to_string());

    // Phase 1: score the amplifiers themselves.
    let mut amplifiers_scored = 0usize;
    for amp_did in &amplifier_dids {
        let Some(amp_handle) = handles.get(amp_did) else {
            continue; // unresolved → can't fetch posts to score
        };
        if amp_handle == handle {
            continue; // self
        }
        if scored.insert(amp_did.clone()) {
            score_one(
                client,
                scorer,
                amp_handle,
                amp_did,
                &fingerprint,
                &weights,
                &empty_pile,
            )
            .await;
            amplifiers_scored += 1;
        }
    }

    // Phase 2: gather deduped followers of the quote/reply amplifiers.
    let fanout_amp_dids: Vec<&String> = amplifier_dids
        .iter()
        .filter(|d| {
            events.iter().any(|e| {
                &&e.amplifier_did == d && FANOUT_EVENT_TYPES.contains(&e.event_type.as_str())
            })
        })
        .collect();

    let mut to_score: Vec<followers::Follower> = Vec::new();
    for amp_did in fanout_amp_dids {
        let Some(amp_handle) = handles.get(amp_did) else {
            continue;
        };
        match followers::fetch_followers(client, amp_handle, opts.max_followers).await {
            Ok(list) => {
                for f in list {
                    if f.handle != handle && scored.insert(f.did.clone()) {
                        to_score.push(f);
                    }
                }
            }
            Err(e) => {
                warn!(handle = amp_handle, error = %e, "dry-run failed to fetch followers");
            }
        }
    }

    let followers_scored = to_score.len();

    // Score followers concurrently; the shared atomic counters make order
    // irrelevant to the totals.
    let concurrency = opts.concurrency.max(1);
    stream::iter(to_score.iter())
        .map(|f| {
            score_one(
                client,
                scorer,
                &f.handle,
                &f.did,
                &fingerprint,
                &weights,
                &empty_pile,
            )
        })
        .buffer_unordered(concurrency)
        .collect::<Vec<()>>()
        .await;

    let after = stats.snapshot();
    CandidateDryRun {
        did: did.to_string(),
        handle: handle.to_string(),
        fanout_amplifiers: summary.fanout_amplifiers,
        stratum: summary.stratum,
        amplifiers_scored,
        followers_scored,
        counts: after.delta_from(&before),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_result_has_zero_counts() {
        let r = CandidateDryRun::empty("did:x", "x.bsky.social");
        assert_eq!(r.counts.zentropi_calls, 0);
        assert_eq!(r.amplifiers_scored, 0);
        assert_eq!(r.followers_scored, 0);
        assert_eq!(r.stratum, EngagementStratum::None);
    }

    #[test]
    fn default_options_match_pipeline() {
        let o = DryRunOptions::default();
        assert_eq!(o.max_followers, 50);
        assert_eq!(o.concurrency, 8);
    }
}
