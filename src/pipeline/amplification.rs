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
use rusqlite::Connection;
use std::panic::AssertUnwindSafe;
use tracing::{info, warn};

use crate::bluesky::amplification::AmplificationNotification;
use crate::bluesky::client::PublicAtpClient;
use crate::bluesky::followers;
use crate::bluesky::posts;
use crate::db::queries;
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
    conn: &Connection,
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
) -> Result<(usize, usize)> {
    info!(
        total_events = events.len(),
        "Processing amplification events"
    );

    // Record the scan timestamp
    queries::set_scan_state(
        conn,
        "last_scan_at",
        &chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    )?;

    // Store each event in the database, fetching quote text when available
    for event in &events {
        let mut quote_text: Option<String> = None;
        let mut quote_toxicity: Option<f64> = None;

        // For quote events, fetch the quote post text and score it
        if event.event_type == "quote" && analyze_followers {
            match posts::fetch_post_text(client, &event.amplifier_post_uri).await {
                Ok(Some(text)) => {
                    // Score the quote text for toxicity
                    match scorer.score_text(&text).await {
                        Ok(result) => {
                            quote_toxicity = Some(result.toxicity);
                        }
                        Err(e) => {
                            warn!(error = %e, "Failed to score quote text");
                        }
                    }
                    quote_text = Some(text);
                }
                Ok(None) => {
                    info!(uri = event.amplifier_post_uri, "Quote post text not found");
                }
                Err(e) => {
                    warn!(error = %e, "Failed to fetch quote post text");
                }
            }
        }

        queries::insert_amplification_event(
            conn,
            &event.event_type,
            &event.amplifier_did,
            &event.amplifier_handle,
            event.original_post_uri.as_deref().unwrap_or("unknown"),
            Some(&event.amplifier_post_uri),
            quote_text.as_deref(),
        )?;

        // Display the event with quote context if available
        let event_label = if event.event_type == "quote" {
            "Quote"
        } else {
            "Repost"
        };
        println!(
            "  {} by @{} ({})",
            event_label, event.amplifier_handle, event.indexed_at,
        );
        if let Some(ref text) = quote_text {
            let preview = crate::output::truncate_chars(text, 120);
            let tox_str = quote_toxicity
                .map(|t| format!(" [tox: {:.2}]", t))
                .unwrap_or_default();
            println!("    \"{}\"{}", preview, tox_str);
        }
    }

    let mut accounts_scored = 0;

    // If --analyze flag is set, score the followers of each quote amplifier.
    // Reposts are recorded as events but don't trigger follower analysis —
    // quotes are the primary harassment vector (hostile commentary framing
    // the original post), while reposts are usually supportive sharing.
    if analyze_followers && !events.is_empty() {
        let quote_events: Vec<_> = events.iter().filter(|e| e.event_type == "quote").collect();
        let repost_count = events.len() - quote_events.len();

        if repost_count > 0 {
            info!(
                reposts_skipped = repost_count,
                "Skipping follower analysis for reposts"
            );
            println!(
                "  Skipping {} reposts (follower analysis is quote-only)",
                repost_count
            );
        }

        if quote_events.is_empty() {
            info!("No quote events to analyze");
        }

        for event in &quote_events {
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
                    let stale_followers: Vec<_> = follower_list
                        .iter()
                        .filter(|f| f.handle != protected_handle)
                        .filter(|f| queries::is_score_stale(conn, &f.did, 7).unwrap_or(true))
                        .collect();

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

                    // Phase 2: Score in parallel — each future does network I/O + ONNX inference
                    // Results are written incrementally so a crash doesn't lose everything
                    let mut stream = stream::iter(stale_followers.into_iter().map(|follower| {
                        let handle_for_panic = follower.handle.clone();
                        async move {
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
                            ))
                            .catch_unwind()
                            .await
                            .unwrap_or_else(|_| {
                                Err(anyhow::anyhow!("Panic while scoring @{}", handle_for_panic))
                            })
                        }
                    }))
                    .buffer_unordered(concurrency);

                    // Phase 3: Write results to DB incrementally as they arrive
                    while let Some(result) = stream.next().await {
                        match result {
                            Ok(score) => {
                                queries::upsert_account_score(conn, &score)?;
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
