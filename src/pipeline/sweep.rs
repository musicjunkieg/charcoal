// Background sweep pipeline: score followers-of-followers (Mode 2).
//
// Scans the protected user's second-degree network for accounts that are
// both topically proximate and behaviorally hostile — the "haven't collided
// yet but probably will" pool.
//
// Strategy: fetch the protected user's followers, then each follower's
// followers, deduplicate, filter by topic overlap, and score survivors
// for toxicity. This is expensive, so we cap at each level and skip
// accounts already scored recently.

use anyhow::Result;
use futures::stream::{self, StreamExt};
use futures::FutureExt;
use indicatif::{ProgressBar, ProgressStyle};
use rusqlite::Connection;
use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use tracing::{info, warn};

use crate::bluesky::followers;
use crate::db::queries;
use crate::scoring::profile;
use crate::scoring::threat::ThreatWeights;
use crate::topics::fingerprint::TopicFingerprint;
use crate::toxicity::traits::ToxicityScorer;

use bsky_sdk::BskyAgent;

/// Run the background sweep pipeline.
///
/// Scans followers-of-followers of the protected user, filtered by topic
/// overlap. Returns the number of second-degree accounts found and scored.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    agent: &BskyAgent,
    scorer: &dyn ToxicityScorer,
    conn: &Connection,
    protected_handle: &str,
    protected_fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
    max_first_degree: usize,
    max_second_degree_per: usize,
    concurrency: usize,
) -> Result<(usize, usize)> {
    // Step 1: Fetch the protected user's followers
    println!("Fetching your followers (up to {max_first_degree})...");
    let first_degree = followers::fetch_followers(agent, protected_handle, max_first_degree).await?;
    info!(count = first_degree.len(), "First-degree followers fetched");

    // Step 2: Fetch second-degree followers (followers of your followers)
    println!(
        "Scanning second-degree network ({} followers, up to {} each)...",
        first_degree.len(),
        max_second_degree_per,
    );

    let mut seen: HashSet<String> = HashSet::new();
    // Exclude the protected user and all first-degree followers
    seen.insert(protected_handle.to_string());
    for f in &first_degree {
        seen.insert(f.did.clone());
    }

    let mut second_degree_pool = Vec::new();

    let pb = ProgressBar::new(first_degree.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("  Network [{bar:30}] {pos}/{len} ({eta})")
            .unwrap(),
    );

    for follower in &first_degree {
        match followers::fetch_followers(agent, &follower.handle, max_second_degree_per).await {
            Ok(their_followers) => {
                for f in their_followers {
                    if seen.insert(f.did.clone()) {
                        second_degree_pool.push(f);
                    }
                }
            }
            Err(e) => {
                warn!(
                    handle = follower.handle,
                    error = %e,
                    "Failed to fetch followers, skipping"
                );
            }
        }
        pb.inc(1);
    }
    pb.finish_and_clear();

    println!(
        "  Found {} unique second-degree accounts",
        second_degree_pool.len(),
    );

    // Step 3: Filter to accounts with stale or missing scores
    let stale: Vec<_> = second_degree_pool
        .iter()
        .filter(|f| queries::is_score_stale(conn, &f.did, 7).unwrap_or(true))
        .collect();

    if stale.is_empty() {
        println!("  All second-degree accounts have recent scores.");
        return Ok((second_degree_pool.len(), 0));
    }

    println!(
        "  {} need scoring ({} concurrent)...",
        stale.len(),
        concurrency,
    );

    // Step 4: Score in parallel (same pattern as amplification pipeline)
    let pb = ProgressBar::new(stale.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("  Scoring [{bar:30}] {pos}/{len} ({eta})")
            .unwrap(),
    );

    let mut stream = stream::iter(stale.into_iter().map(|follower| {
        let handle_for_panic = follower.handle.clone();
        async move {
            AssertUnwindSafe(profile::build_profile(
                agent,
                scorer,
                &follower.handle,
                &follower.did,
                protected_fingerprint,
                weights,
            ))
            .catch_unwind()
            .await
            .unwrap_or_else(|_| {
                Err(anyhow::anyhow!("Panic while scoring @{}", handle_for_panic))
            })
        }
    }))
    .buffer_unordered(concurrency);

    // Step 5: Write results to DB incrementally — each score is persisted
    // as it arrives so a crash doesn't lose everything scored so far
    let mut accounts_scored = 0;
    while let Some(result) = stream.next().await {
        match result {
            Ok(score) => {
                queries::upsert_account_score(conn, &score)?;
                accounts_scored += 1;
            }
            Err(e) => {
                warn!(error = %e, "Failed to score account, skipping");
            }
        }
        pb.inc(1);
    }
    pb.finish_and_clear();

    Ok((second_degree_pool.len(), accounts_scored))
}
