// Notification polling — detect quote/repost events.
//
// Polls the authenticated user's notifications for amplification events
// (quotes and reposts). These are the primary harassment escalation vectors
// on Bluesky — someone quotes your post to broadcast it to their audience.

use anyhow::{Context, Result};
use atrium_api::app::bsky::notification::list_notifications;
use bsky_sdk::BskyAgent;
use tracing::{debug, info};

use super::rate_limit::RateLimiter;

/// An amplification event detected from notifications.
#[derive(Debug, Clone)]
pub struct AmplificationNotification {
    pub event_type: String, // "quote" or "repost"
    pub amplifier_did: String,
    pub amplifier_handle: String,
    /// The protected user's post that was amplified
    pub original_post_uri: Option<String>,
    /// The amplifier's post URI (for quotes — the quote-post itself)
    pub amplifier_post_uri: String,
    pub indexed_at: String,
}

/// Fetch amplification notifications (quotes and reposts) since the given cursor.
///
/// Returns the events and the new cursor to use for the next poll.
/// Pass `None` as cursor to fetch all recent notifications.
pub async fn fetch_amplification_events(
    agent: &BskyAgent,
    since_cursor: Option<&str>,
    rate_limiter: &RateLimiter,
) -> Result<(Vec<AmplificationNotification>, Option<String>)> {
    let mut events = Vec::new();
    let mut cursor: Option<String> = since_cursor.map(String::from);
    let mut latest_cursor: Option<String> = None;

    loop {
        let params = list_notifications::ParametersData {
            cursor: cursor.clone(),
            limit: Some(
                100u8
                    .try_into()
                    .map_err(|e: String| anyhow::anyhow!("{}", e))?,
            ),
            priority: None,
            // Filter server-side to only quotes and reposts
            reasons: Some(vec!["quote".to_string(), "repost".to_string()]),
            seen_at: None,
        };

        rate_limiter.acquire().await;

        let output = agent
            .api
            .app
            .bsky
            .notification
            .list_notifications(params.into())
            .await
            .context("Failed to fetch notifications")?;

        // Save the first page's cursor as our "latest" for next poll
        if latest_cursor.is_none() {
            latest_cursor = output.data.cursor.clone();
        }

        for notification in &output.notifications {
            let event_type = notification.reason.clone();

            // Only process quotes and reposts (should be filtered by the API,
            // but double-check in case the server doesn't support reason filtering)
            if event_type != "quote" && event_type != "repost" {
                continue;
            }

            events.push(AmplificationNotification {
                event_type,
                amplifier_did: notification.author.did.as_str().to_string(),
                amplifier_handle: notification.author.handle.as_str().to_string(),
                original_post_uri: notification.reason_subject.clone(),
                amplifier_post_uri: notification.uri.clone(),
                indexed_at: notification.indexed_at.as_ref().to_string(),
            });
        }

        debug!(
            page_size = output.notifications.len(),
            events_found = events.len(),
            "Fetched page of notifications"
        );

        cursor = output.data.cursor.clone();
        if cursor.is_none() || output.notifications.is_empty() {
            break;
        }
    }

    info!(
        quotes = events.iter().filter(|e| e.event_type == "quote").count(),
        reposts = events.iter().filter(|e| e.event_type == "repost").count(),
        "Detected amplification events"
    );

    Ok((events, latest_cursor))
}
