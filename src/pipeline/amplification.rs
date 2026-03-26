// Amplification response pipeline: detect event -> score followers.
//
// This is the main threat detection workflow (Mode 1). When someone quotes
// or reposts the protected user's content, this pipeline:
// 1. Receives events from Constellation backlink queries
// 2. Records them in the database
// 3. Fetches the amplifier's follower list
// 4. Scores each follower for toxicity and topic overlap
// 5. Stores the results for the threat report

use anyhow::Result;
use futures::stream::{self, StreamExt};
use futures::FutureExt;
use indicatif::{ProgressBar, ProgressStyle};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use tracing::{info, warn};

use crate::bluesky::amplification::AmplificationNotification;
use crate::bluesky::client::PublicAtpClient;
use crate::bluesky::followers;
use crate::bluesky::posts;
use crate::db::Database;
use crate::scoring::nli::NliScorer;
use crate::scoring::profile;
use crate::scoring::threat::ThreatWeights;
use crate::topics::embeddings::SentenceEmbedder;
use crate::topics::fingerprint::TopicFingerprint;
use crate::toxicity::traits::ToxicityScorer;

/// Run the amplification detection pipeline.
///
/// Processes pre-fetched amplification events (from Constellation backlinks),
/// fetches amplifier followers, and scores them. Returns the number of events
/// processed and accounts scored.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    client: &PublicAtpClient,
    scorer: &dyn ToxicityScorer,
    db: &Arc<dyn Database>,
    user_did: &str,
    protected_fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
    protected_handle: &str,
    analyze_followers: bool,
    max_followers_per_amplifier: usize,
    concurrency: usize,
    embedder: Option<&SentenceEmbedder>,
    protected_embedding: Option<&[f64]>,
    events: Vec<AmplificationNotification>,
    median_engagement: f64,
    pile_on_dids: &std::collections::HashSet<String>,
    original_text_cache: &std::collections::HashMap<String, String>,
    nli_scorer: Option<&NliScorer>,
    protected_posts_with_embeddings: Option<&[(String, Vec<f64>)]>,
    data_dir: Option<&std::path::Path>,
) -> Result<(usize, usize)> {
    info!(
        total_events = events.len(),
        "Processing amplification events"
    );

    // Record the scan timestamp
    db.set_scan_state(
        user_did,
        "last_scan_at",
        &chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    )
    .await?;

    // Store each event in the database, fetching quote text when available.
    // Look up original post text from the cache for all event types.
    for event in &events {
        let mut amplifier_text: Option<String> = None;
        let mut quote_toxicity: Option<f64> = None;

        // For quote and reply events, fetch the amplifier's text and score it
        if (event.event_type == "quote" || event.event_type == "reply") && analyze_followers {
            match posts::fetch_post_text(client, &event.amplifier_post_uri).await {
                Ok(Some(text)) => {
                    match scorer.score_text(&text).await {
                        Ok(result) => {
                            quote_toxicity = Some(result.toxicity);
                        }
                        Err(e) => {
                            warn!(error = %e, "Failed to score amplifier text");
                        }
                    }
                    amplifier_text = Some(text);
                }
                Ok(None) => {
                    info!(
                        uri = event.amplifier_post_uri,
                        "Amplifier post text not found"
                    );
                }
                Err(e) => {
                    warn!(error = %e, "Failed to fetch amplifier post text");
                }
            }
        }

        // Look up the original (protected user's) post text from the cache
        let original_post_text = event
            .original_post_uri
            .as_deref()
            .and_then(|uri| original_text_cache.get(uri))
            .map(|s| s.as_str());

        // Score the interaction pair via NLI when both texts are available
        let context_score = match (nli_scorer, amplifier_text.as_deref(), original_post_text) {
            (Some(nli), Some(amp_text), Some(orig_text)) => {
                match nli.score_pair(orig_text, amp_text).await {
                    Ok((score, hypothesis_scores)) => {
                        info!(
                            handle = event.amplifier_handle,
                            context_score = format!("{:.3}", score),
                            "NLI scored event pair"
                        );
                        if let Some(dir) = data_dir {
                            crate::scoring::nli_audit::log_nli_audit(
                                &crate::scoring::nli_audit::NliAuditEntry {
                                    timestamp: chrono::Utc::now().to_rfc3339(),
                                    target_did: event.amplifier_did.clone(),
                                    target_handle: event.amplifier_handle.clone(),
                                    pair_type: "direct".to_string(),
                                    original_text: orig_text.to_string(),
                                    response_text: amp_text.to_string(),
                                    hypothesis_scores,
                                    hostility_score: score,
                                    similarity: None,
                                },
                                Some(dir),
                            );
                        }
                        Some(score)
                    }
                    Err(e) => {
                        warn!(error = %e, "NLI scoring failed for event pair");
                        None
                    }
                }
            }
            _ => None,
        };

        db.insert_amplification_event(
            user_did,
            &event.event_type,
            &event.amplifier_did,
            &event.amplifier_handle,
            event.original_post_uri.as_deref().unwrap_or("unknown"),
            Some(&event.amplifier_post_uri),
            amplifier_text.as_deref(),
            original_post_text,
            context_score,
        )
        .await?;

        let event_label = match event.event_type.as_str() {
            "quote" => "Quote",
            "repost" => "Repost",
            "like" => "Like",
            "reply" => "Reply",
            other => other,
        };
        println!(
            "  {} by @{} ({})",
            event_label, event.amplifier_handle, event.indexed_at,
        );
        if let Some(ref text) = amplifier_text {
            let preview = crate::output::truncate_chars(text, 120);
            let tox_str = quote_toxicity
                .map(|t| format!(" [tox: {:.2}]", t))
                .unwrap_or_default();
            println!("    \"{}\"{}", preview, tox_str);
        }
    }

    // Phase B: Score amplifiers via build_profile() with direct NLI pairs.
    //
    // Collect unique amplifier DIDs and their text pairs from stored events,
    // then run full profile builds. This gives each amplifier a threat tier
    // informed by their actual interactions with the protected user.
    let mut accounts_scored = 0;
    {
        let mut amplifier_handles: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        for event in &events {
            amplifier_handles
                .entry(event.amplifier_did.clone())
                .or_insert_with(|| event.amplifier_handle.clone());
        }

        let amplifier_count = amplifier_handles.len();
        if amplifier_count > 0 {
            println!("\nScoring {} amplifiers…", amplifier_count);

            for (did, handle) in &amplifier_handles {
                if handle == protected_handle {
                    continue;
                }
                if !db.is_score_stale(user_did, did, 7).await.unwrap_or(true) {
                    continue;
                }

                // Gather direct text pairs from stored events, deduplicating
                // across scans (the same event can be recorded multiple times)
                let mut seen_pairs: std::collections::HashSet<(String, String)> =
                    std::collections::HashSet::new();
                let mut pairs: Vec<(String, String)> = Vec::new();
                if let Ok(db_events) = db.get_events_by_amplifier(user_did, did).await {
                    for ev in db_events {
                        if let (Some(orig), Some(amp)) = (ev.original_post_text, ev.amplifier_text)
                        {
                            if !orig.is_empty()
                                && !amp.is_empty()
                                && seen_pairs.insert((orig.clone(), amp.clone()))
                            {
                                pairs.push((orig, amp));
                            }
                        }
                    }
                }

                match profile::build_profile(
                    client,
                    scorer,
                    handle,
                    did,
                    protected_fingerprint,
                    weights,
                    embedder,
                    protected_embedding,
                    median_engagement,
                    pile_on_dids,
                    nli_scorer,
                    None, // No inferred pairs — using direct pairs
                    Some(&pairs),
                    data_dir,
                    None, // Graph distance wired in Task 4
                )
                .await
                {
                    Ok(score) => {
                        db.upsert_account_score(user_did, &score).await?;
                        accounts_scored += 1;
                        println!(
                            "  @{}: {} (context: {})",
                            handle,
                            score.threat_tier.as_deref().unwrap_or("?"),
                            score
                                .context_score
                                .map(|s| format!("{:.2}", s))
                                .unwrap_or_else(|| "n/a".to_string())
                        );
                    }
                    Err(e) => {
                        warn!(handle = handle.as_str(), error = %e, "Failed to score amplifier");
                    }
                }
            }
        }
    }

    // If --analyze flag is set, score the followers of each quote/reply amplifier.
    // Quotes and replies are direct hostile engagement vectors that warrant
    // follower analysis. Reposts and likes are recorded but don't trigger
    // follower analysis — reposts are usually supportive sharing, and likes
    // are low-signal engagement.
    if analyze_followers && !events.is_empty() {
        let scorable_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "quote" || e.event_type == "reply")
            .collect();
        let skipped_count = events.len() - scorable_events.len();

        if skipped_count > 0 {
            info!(
                skipped = skipped_count,
                "Skipping follower analysis for reposts/likes"
            );
            println!(
                "  Skipping {} reposts/likes (follower analysis is quote/reply-only)",
                skipped_count
            );
        }

        if scorable_events.is_empty() {
            info!("No quote/reply events to analyze");
        }

        for event in &scorable_events {
            println!("\nFetching followers of @{}...", event.amplifier_handle);

            match followers::fetch_followers(
                client,
                &event.amplifier_handle,
                max_followers_per_amplifier,
            )
            .await
            {
                Ok(follower_list) => {
                    // Phase 1: Filter — find followers with stale scores (DB reads on main task)
                    // Also exclude the protected user from their own threat report
                    let mut stale_followers = Vec::new();
                    for f in follower_list
                        .iter()
                        .filter(|f| f.handle != protected_handle)
                    {
                        if db.is_score_stale(user_did, &f.did, 7).await.unwrap_or(true) {
                            // Clone to produce an owned Vec<Follower> — required for
                            // the async move closure in the scoring stream to be
                            // 'static-compatible when called from tokio::spawn.
                            stale_followers.push(f.clone());
                        }
                    }

                    println!(
                        "  Found {} followers, {} need scoring ({} concurrent)...",
                        follower_list.len(),
                        stale_followers.len(),
                        concurrency,
                    );

                    if stale_followers.is_empty() {
                        continue;
                    }

                    let pb = ProgressBar::new(stale_followers.len() as u64);
                    pb.set_style(
                        ProgressStyle::default_bar()
                            .template("  Scoring [{bar:30}] {pos}/{len} ({eta})")
                            .unwrap(),
                    );

                    // Phase 2: Two-pass scoring in parallel
                    // Pass 1: score without NLI (fast). If raw_score >= 8.0 (Watch threshold),
                    // pass 2 re-scores with NLI inferred pairs. Falls back to pass 1 on panic.
                    let nli_ref = nli_scorer;
                    let ppwe_ref = protected_posts_with_embeddings;

                    let mut stream = stream::iter(stale_followers.into_iter().map(|follower| {
                        let handle_for_panic = follower.handle.clone();
                        async move {
                            // Pass 1: score without NLI (fast)
                            let result = AssertUnwindSafe(profile::build_profile(
                                client,
                                scorer,
                                &follower.handle,
                                &follower.did,
                                protected_fingerprint,
                                weights,
                                embedder,
                                protected_embedding,
                                median_engagement,
                                pile_on_dids,
                                None, // No NLI in pass 1
                                None, // No protected post embeddings
                                None, // No direct pairs
                                None, // No audit logging in pass 1
                                None, // No graph distance for followers
                            ))
                            .catch_unwind()
                            .await
                            .unwrap_or_else(|_| {
                                Err(anyhow::anyhow!("Panic while scoring @{}", handle_for_panic))
                            });

                            match result {
                                Ok(ref score)
                                    if score.threat_score.unwrap_or(0.0) >= 8.0
                                        && nli_ref.is_some()
                                        && ppwe_ref.is_some() =>
                                {
                                    // Pass 2: above Watch threshold — re-score with NLI
                                    info!(
                                        handle = follower.handle.as_str(),
                                        raw_score =
                                            format!("{:.1}", score.threat_score.unwrap_or(0.0)),
                                        "Follower above Watch threshold, running NLI"
                                    );
                                    AssertUnwindSafe(profile::build_profile(
                                        client,
                                        scorer,
                                        &follower.handle,
                                        &follower.did,
                                        protected_fingerprint,
                                        weights,
                                        embedder,
                                        protected_embedding,
                                        median_engagement,
                                        pile_on_dids,
                                        nli_ref,  // NLI enabled
                                        ppwe_ref, // Inferred pairs
                                        None,     // No direct pairs
                                        data_dir, // Audit logging
                                        None,     // No graph distance for followers
                                    ))
                                    .catch_unwind()
                                    .await
                                    .unwrap_or(result) // Fall back to pass 1 on panic
                                }
                                other => other,
                            }
                        }
                    }))
                    .buffer_unordered(concurrency);

                    // Phase 3: Write results to DB incrementally as they arrive
                    while let Some(result) = stream.next().await {
                        match result {
                            Ok(score) => {
                                db.upsert_account_score(user_did, &score).await?;
                                accounts_scored += 1;
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to score follower, skipping");
                            }
                        }
                        pb.inc(1);
                    }

                    pb.finish_and_clear();
                }
                Err(e) => {
                    warn!(
                        handle = event.amplifier_handle,
                        error = %e,
                        "Failed to fetch followers, skipping"
                    );
                }
            }
        }
    }

    Ok((events.len(), accounts_scored))
}
