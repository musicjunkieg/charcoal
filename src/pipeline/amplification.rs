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
use rusqlite::Connection;
use tracing::{info, warn};

use crate::bluesky::followers;
use crate::bluesky::notifications;
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
pub async fn run(
    agent: &BskyAgent,
    scorer: &dyn ToxicityScorer,
    conn: &Connection,
    protected_fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
    analyze_followers: bool,
    max_followers_per_amplifier: usize,
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

    // Store each event in the database
    for event in &events {
        queries::insert_amplification_event(
            conn,
            &event.event_type,
            &event.amplifier_did,
            &event.amplifier_handle,
            event.original_post_uri.as_deref().unwrap_or("unknown"),
            Some(&event.amplifier_post_uri),
            None, // We'd need a separate API call to get quote text
        )?;

        println!(
            "  {} by @{} ({})",
            if event.event_type == "quote" { "Quote" } else { "Repost" },
            event.amplifier_handle,
            event.indexed_at,
        );
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
                    println!(
                        "  Found {} followers, scoring...",
                        follower_list.len()
                    );

                    for follower in &follower_list {
                        // Skip if we already have a fresh score
                        if !queries::is_score_stale(conn, &follower.did, 7)? {
                            continue;
                        }

                        match profile::build_profile(
                            agent,
                            scorer,
                            &follower.handle,
                            &follower.did,
                            protected_fingerprint,
                            weights,
                        )
                        .await
                        {
                            Ok(score) => {
                                queries::upsert_account_score(conn, &score)?;
                                accounts_scored += 1;
                            }
                            Err(e) => {
                                warn!(
                                    handle = follower.handle,
                                    error = %e,
                                    "Failed to score follower, skipping"
                                );
                            }
                        }
                    }
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
