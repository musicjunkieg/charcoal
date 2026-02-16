// Amplification response pipeline: detect event -> score followers.
//
// This is the main threat detection workflow (Mode 1). When someone quotes
// or reposts the protected user's content, this pipeline:
// 1. Detects the event via notification polling
// 2. Records it in the database
// 3. Fetches the amplifier's follower list
// 4. Scores each follower for toxicity and topic overlap
// 5. Stores the results for the threat report

use anyhow::Result;
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use rusqlite::Connection;
use tracing::{info, warn};

use crate::bluesky::followers;
use crate::bluesky::notifications;
use crate::bluesky::posts;
use crate::db::queries;
use crate::scoring::profile;
use crate::scoring::threat::ThreatWeights;
use crate::topics::fingerprint::TopicFingerprint;
use crate::toxicity::traits::ToxicityScorer;

use bsky_sdk::BskyAgent;

/// Run the amplification detection pipeline.
///
/// Polls for new quote/repost events, fetches amplifier followers,
/// and scores them. Returns the number of new events detected and
/// accounts scored.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    agent: &BskyAgent,
    scorer: &dyn ToxicityScorer,
    conn: &Connection,
    protected_fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
    analyze_followers: bool,
    max_followers_per_amplifier: usize,
    concurrency: usize,
) -> Result<(usize, usize)> {
    // Get the stored cursor from the last scan
    let last_cursor = queries::get_scan_state(conn, "notifications_cursor")?;

    // Fetch new amplification events
    let (events, new_cursor) = notifications::fetch_amplification_events(
        agent,
        last_cursor.as_deref(),
    )
    .await?;

    info!(
        new_events = events.len(),
        "Amplification scan complete"
    );

    // Store the new cursor for next time
    if let Some(ref cursor) = new_cursor {
        queries::set_scan_state(conn, "notifications_cursor", cursor)?;
    }

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
            match posts::fetch_post_text(agent, &event.amplifier_post_uri).await {
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
        let event_label = if event.event_type == "quote" { "Quote" } else { "Repost" };
        println!(
            "  {} by @{} ({})",
            event_label,
            event.amplifier_handle,
            event.indexed_at,
        );
        if let Some(ref text) = quote_text {
            let preview = if text.len() > 120 { &text[..120] } else { text };
            let tox_str = quote_toxicity
                .map(|t| format!(" [tox: {:.2}]", t))
                .unwrap_or_default();
            println!("    \"{}\"{}", preview, tox_str);
        }
    }

    let mut accounts_scored = 0;

    // If --analyze flag is set, score the followers of each amplifier
    if analyze_followers && !events.is_empty() {
        info!("Analyzing followers of amplifiers...");

        for event in &events {
            println!(
                "\nFetching followers of @{}...",
                event.amplifier_handle
            );

            match followers::fetch_followers(
                agent,
                &event.amplifier_handle,
                max_followers_per_amplifier,
            )
            .await
            {
                Ok(follower_list) => {
                    // Phase 1: Filter — find followers with stale scores (DB reads on main task)
                    let stale_followers: Vec<_> = follower_list
                        .iter()
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
                    let results: Vec<Result<_>> = stream::iter(
                        stale_followers.into_iter().map(|follower| async move {
                            profile::build_profile(
                                agent,
                                scorer,
                                &follower.handle,
                                &follower.did,
                                protected_fingerprint,
                                weights,
                            )
                            .await
                        }),
                    )
                    .buffer_unordered(concurrency)
                    .collect()
                    .await;

                    // Phase 3: Write results to DB sequentially (Connection is not Send)
                    for result in results {
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
