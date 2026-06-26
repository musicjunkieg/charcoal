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
//
// Both entry points (`run` and `run_topic_first`) build a candidate set and
// hand it to the three-phase `run_phased_scan` orchestrator (#208), which
// drives the same `stage1_outcome` / `score_from_sample` cores the old
// `build_profile` call did — only now staged through Gather → Burst →
// Finalize. The candidate filters (staleness for `run`, scored-DID dedup for
// `run_topic_first`) are preserved exactly, so the produced `AccountScore`s
// are identical to the pre-rewire behaviour.

use anyhow::Result;
use futures::StreamExt;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{info, warn};

use crate::bluesky::client::PublicAtpClient;
use crate::bluesky::followers;
use crate::db::Database;
use crate::pipeline::scan_phases::burst;
// `TwoStageToxicityScorer` impls both `ToxicityScorer` and `CleanPassScorer`
// (the gather seam). The phased pipeline needs both views plus the classifier,
// so sweep takes the concrete scorer and coerces it into the two `&dyn` views.
use crate::pipeline::scan_phases::gather::{AtpPostFetcher, CleanPassScorer};
use crate::pipeline::scan_phases::{run_phased_scan, CandidateInput, PhasedScanDeps};
use crate::scoring::threat::ThreatWeights;
use crate::topics::embeddings::SentenceEmbedder;
use crate::topics::fingerprint::TopicFingerprint;
use crate::toxicity::ensemble::TwoStageToxicityScorer;
use crate::toxicity::traits::ToxicityScorer;

/// Run the background sweep pipeline.
///
/// Scans followers-of-followers of the protected user, filtered by topic
/// overlap. Returns `(second_degree_pool_size, accounts_scored, degraded)` —
/// `degraded` is true when the scan is incomplete: either the cost ceiling was
/// hit, or one or more accounts were skipped due to fetch/score errors. Re-run
/// to resume.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    client: &PublicAtpClient,
    scorer: &TwoStageToxicityScorer,
    db: &Arc<dyn Database>,
    user_did: &str,
    protected_handle: &str,
    protected_fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
    max_first_degree: usize,
    max_second_degree_per: usize,
    concurrency: usize,
    embedder: Option<&SentenceEmbedder>,
    protected_embedding: Option<&[f64]>,
    median_engagement: f64,
    pile_on_dids: &std::collections::HashSet<String>,
    data_dir: Option<&std::path::Path>,
) -> Result<(usize, usize, bool)> {
    // Step 1: Fetch the protected user's followers
    println!("Fetching your followers (up to {max_first_degree})...");
    let first_degree =
        followers::fetch_followers(client, protected_handle, max_first_degree).await?;
    info!(count = first_degree.len(), "First-degree followers fetched");

    // Step 2: Fetch second-degree followers (followers of your followers)
    println!(
        "Scanning second-degree network ({} followers, up to {} each)...",
        first_degree.len(),
        max_second_degree_per,
    );

    let mut seen: HashSet<String> = HashSet::new();
    // Exclude the protected user (by DID, since dedupe keys are DIDs) and all first-degree followers
    seen.insert(user_did.to_string());
    for f in &first_degree {
        seen.insert(f.did.clone());
    }

    // Fetch each first-degree follower's followers concurrently. This network
    // fan-out is the dominant cost of onboarding (#207): the old serial loop
    // awaited one `fetch_followers` at a time, so the ~2h pre-burst wall-clock
    // was almost entirely these sequential awaits. Dedup stays sequential
    // afterward (a HashSet can't be shared across the in-flight futures), but
    // dedup is cheap — all the time was in the I/O. Index by `usize` (not
    // `&first_degree` items) so the mapped future carries no higher-ranked
    // borrow; this future is held across the web background-scan's
    // `tokio::spawn` boundary, the same reason `run_gather` indexes by usize.
    let mut fetch_results: Vec<(usize, String, Result<Vec<followers::Follower>>)> =
        futures::stream::iter(0..first_degree.len())
            .map(|i| {
                let handle = first_degree[i].handle.clone();
                async move {
                    let result =
                        followers::fetch_followers(client, &handle, max_second_degree_per).await;
                    (i, handle, result)
                }
            })
            .buffer_unordered(concurrency.clamp(1, 64))
            .collect()
            .await;

    // `buffer_unordered` yields in completion (network-timing) order. Sort back
    // to first-degree order before the fold so the deduped pool — and the
    // downstream `stale` / `candidates` vecs it feeds — is deterministic and
    // byte-identical to the old serial loop's output. (Dedup correctness doesn't
    // need this; reproducibility does.)
    fetch_results.sort_by_key(|(i, _, _)| *i);
    let ordered: Vec<(String, Result<Vec<followers::Follower>>)> = fetch_results
        .into_iter()
        .map(|(_, handle, result)| (handle, result))
        .collect();

    let second_degree_pool = dedup_second_degree(&mut seen, ordered);

    println!(
        "  Found {} unique second-degree accounts",
        second_degree_pool.len(),
    );

    // Step 3: Filter to accounts with stale or missing scores (candidate set).
    // Same staleness gate as before the phased-pipeline rewire.
    let mut stale = Vec::new();
    for f in &second_degree_pool {
        if db.is_score_stale(user_did, &f.did, 7).await.unwrap_or(true) {
            stale.push(f);
        }
    }

    if stale.is_empty() {
        println!("  All second-degree accounts have recent scores.");
        return Ok((second_degree_pool.len(), 0, false));
    }

    println!(
        "  {} need scoring ({} concurrent)...",
        stale.len(),
        concurrency,
    );

    // Build the candidate set. Sweep accounts carry no NLI/amplifier pairs and
    // no graph distance — exactly the `None` arguments the old `build_profile`
    // call passed for direct_pairs / nli / graph_distance.
    let candidates: Vec<CandidateInput> = stale
        .iter()
        .map(|f| CandidateInput {
            account_did: f.did.clone(),
            account_handle: f.handle.clone(),
            is_pile_on: pile_on_dids.contains(&f.did),
            direct_pairs: None,
            graph_distance: None,
        })
        .collect();

    let summary = run_sweep_phased(
        client,
        scorer,
        db,
        user_did,
        protected_fingerprint,
        weights,
        embedder,
        protected_embedding,
        median_engagement,
        data_dir,
        concurrency,
        &candidates,
    )
    .await?;

    let (scored, degraded) = summary;
    Ok((second_degree_pool.len(), scored, degraded))
}

/// Run topic-first discovery sweep.
///
/// Instead of walking the follower graph, searches for posts matching the
/// protected user's topic fingerprint via searchPosts. Deduplicates against
/// already-scored accounts and scores new discoveries. Returns
/// `(discovered, accounts_scored, degraded)` — `degraded` is true when the scan
/// was cost-capped and left resumable.
#[allow(clippy::too_many_arguments)]
pub async fn run_topic_first(
    client: &PublicAtpClient,
    scorer: &TwoStageToxicityScorer,
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
    keywords_per_cycle: usize,
    results_per_keyword: usize,
) -> Result<(usize, usize, bool)> {
    // Step 1: Get already-scored DIDs for deduplication (candidate filter).
    let scored_dids: HashSet<String> = db
        .get_all_scored_dids(user_did)
        .await?
        .into_iter()
        .collect();

    println!(
        "  {} accounts already scored, searching for new discoveries...",
        scored_dids.len()
    );

    // Step 2: Discover new accounts via topic search
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
        return Ok((0, 0, false));
    }

    // Step 3: Resolve DIDs to handles via getProfiles (batch, 25 per call)
    let did_handle_map =
        crate::bluesky::profiles::resolve_dids_to_handles(client, &new_dids).await?;

    let did_handle_pairs: Vec<(String, String)> = did_handle_map.into_iter().collect();

    println!(
        "  Resolved {}/{} DIDs to handles",
        did_handle_pairs.len(),
        new_dids.len()
    );

    if did_handle_pairs.is_empty() {
        return Ok((new_dids.len(), 0, false));
    }

    let discovered = did_handle_pairs.len();

    // Build the candidate set. As with `run`, discovery sweep accounts have no
    // NLI pairs and no graph distance — match the old `build_profile` `None`s.
    let candidates: Vec<CandidateInput> = did_handle_pairs
        .into_iter()
        .map(|(did, handle)| CandidateInput {
            is_pile_on: pile_on_dids.contains(&did),
            account_did: did,
            account_handle: handle,
            direct_pairs: None,
            graph_distance: None,
        })
        .collect();

    let summary = run_sweep_phased(
        client,
        scorer,
        db,
        user_did,
        protected_fingerprint,
        weights,
        embedder,
        protected_embedding,
        median_engagement,
        data_dir,
        concurrency,
        &candidates,
    )
    .await?;

    let (scored, degraded) = summary;
    Ok((discovered, scored, degraded))
}

/// Shared driver: build `PhasedScanDeps` from the sweep's shared refs and run
/// the three-phase scan over `candidates`. Returns `(accounts_scored, degraded)`
/// — the number of accounts that reached a finalized `AccountScore`, and whether
/// the scan was cost-capped and left resumable.
///
/// Incremental persistence is preserved by construction: terminal accounts
/// (insufficient data / clean early-exit) are written inside Phase A gather,
/// and survivors are written inside Phase C finalize — the same incremental DB
/// writes as the old per-item stream, just staged through the work queue.
///
/// Resilience note: the old sweep wrapped each `build_profile` in
/// `catch_unwind` to isolate a per-account panic. The phased pipeline's model
/// is different (and stronger): a crash or cost-cap mid-run is recoverable by
/// re-running, which resumes from the DB-staged `scan_phase` marker rather than
/// re-scoring everything. A gather panic now aborts the process, but a re-run
/// picks up where it left off — the intended #208 architecture.
#[allow(clippy::too_many_arguments)]
async fn run_sweep_phased(
    client: &PublicAtpClient,
    scorer: &TwoStageToxicityScorer,
    db: &Arc<dyn Database>,
    user_did: &str,
    protected_fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
    embedder: Option<&SentenceEmbedder>,
    protected_embedding: Option<&[f64]>,
    median_engagement: f64,
    data_dir: Option<&std::path::Path>,
    concurrency: usize,
    candidates: &[CandidateInput],
) -> Result<(usize, bool)> {
    let fetcher = AtpPostFetcher { client };
    let classifier = scorer.classifier();

    let deps = PhasedScanDeps {
        fetcher: &fetcher,
        scorer: scorer as &dyn ToxicityScorer,
        clean_pass: scorer as &dyn CleanPassScorer,
        classifier: &classifier,
        protected_fingerprint,
        weights,
        embedder,
        protected_embedding,
        nli_scorer: None, // sweep does not run NLI context scoring
        protected_posts_with_embeddings: None,
        data_dir,
        median_engagement,
        gather_concurrency: concurrency,
        burst_concurrency: burst::burst_concurrency(),
        burst_batch: burst::burst_batch(),
    };

    let summary = run_phased_scan(db, user_did, candidates, &deps).await?;
    Ok((summary.accounts_scored, summary.degraded))
}

/// Fold the concurrently-fetched second-degree follower batches into a
/// deduplicated pool, mutating `seen` in place.
///
/// Separated from the network fetch so the dedup is pure and unit-testable:
/// `buffer_unordered` delivers batches in arbitrary order, so this must produce
/// the same *set* of new followers regardless of arrival order. A failed fetch
/// (the `Err` arm) is warn-logged and skipped — identical behaviour to the old
/// serial loop. The first batch to contain a given DID "wins" it, but a
/// follower's profile fields are its own (not the referrer's), so which batch
/// wins is immaterial to the result.
fn dedup_second_degree(
    seen: &mut HashSet<String>,
    fetch_results: Vec<(String, Result<Vec<followers::Follower>>)>,
) -> Vec<followers::Follower> {
    let mut pool = Vec::new();
    for (handle, result) in fetch_results {
        match result {
            Ok(their_followers) => {
                for f in their_followers {
                    if seen.insert(f.did.clone()) {
                        pool.push(f);
                    }
                }
            }
            Err(e) => {
                warn!(handle, error = %e, "Failed to fetch followers, skipping");
            }
        }
    }
    pool
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bluesky::followers::Follower;
    use std::collections::BTreeSet;

    fn follower(did: &str) -> Follower {
        Follower {
            did: did.to_string(),
            handle: format!("{did}.test"),
            display_name: None,
        }
    }

    fn dids(pool: &[Follower]) -> BTreeSet<String> {
        pool.iter().map(|f| f.did.clone()).collect()
    }

    // Two first-degree followers whose follower lists overlap on `did:c` and
    // each include an already-seen DID. `Result` is `anyhow::Result`, which
    // isn't `Clone`, so rebuild fresh each time rather than cloning.
    fn batches() -> Vec<(String, Result<Vec<Follower>>)> {
        vec![
            (
                "fd1.test".to_string(),
                Ok(vec![
                    follower("did:a"),
                    follower("did:c"),
                    follower("did:fd1"),
                ]),
            ),
            (
                "fd2.test".to_string(),
                Ok(vec![
                    follower("did:b"),
                    follower("did:c"),
                    follower("did:self"),
                ]),
            ),
        ]
    }

    fn preseed() -> HashSet<String> {
        // Mirrors `run`: protected user + first-degree followers are pre-inserted.
        let mut seen = HashSet::new();
        seen.insert("did:self".to_string());
        seen.insert("did:fd1".to_string());
        seen
    }

    #[test]
    fn dedup_is_order_independent_and_excludes_seen() {
        // The whole risk of going concurrent: `buffer_unordered` delivers batches
        // in arbitrary order. The deduped pool must be the same SET regardless.
        let mut seen_fwd = preseed();
        let pool_fwd = dedup_second_degree(&mut seen_fwd, batches());

        let mut reversed = batches();
        reversed.reverse();
        let mut seen_rev = preseed();
        let pool_rev = dedup_second_degree(&mut seen_rev, reversed);

        // did:self and did:fd1 excluded (pre-seeded); did:c collapsed to one.
        let expected: BTreeSet<String> = ["did:a", "did:b", "did:c"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(dids(&pool_fwd), expected);
        assert_eq!(dids(&pool_rev), expected);
        // Exactly one entry per unique new DID — no duplicate did:c.
        assert_eq!(pool_fwd.len(), 3);
        assert_eq!(pool_rev.len(), 3);
    }

    #[test]
    fn dedup_skips_failed_fetches() {
        let mut seen = HashSet::new();
        let results = vec![
            (
                "ok.test".to_string(),
                Ok(vec![follower("did:a"), follower("did:b")]),
            ),
            (
                "bad.test".to_string(),
                Err(anyhow::anyhow!("transient fetch error")),
            ),
        ];
        let pool = dedup_second_degree(&mut seen, results);
        // The failed batch is skipped (warn-logged); the successful one still counts.
        let expected: BTreeSet<String> = ["did:a", "did:b"].iter().map(|s| s.to_string()).collect();
        assert_eq!(dids(&pool), expected);
    }

    #[test]
    fn dedup_preserves_input_order() {
        // The fold is a stable, input-order-preserving pass: new DIDs appear in
        // the order their batches are supplied. `run` sorts the concurrent fetch
        // results back into first-degree order before calling this, so the final
        // pool is deterministic. Forward batches -> [a, c] from batch 1, then [b]
        // from batch 2 (c is a dup, fd1/self are pre-seeded out).
        let mut seen = preseed();
        let pool = dedup_second_degree(&mut seen, batches());
        let order: Vec<&str> = pool.iter().map(|f| f.did.as_str()).collect();
        assert_eq!(order, vec!["did:a", "did:c", "did:b"]);
    }
}
