// GET /api/events — list recent amplification events.
//
// Optional ?limit= parameter (default 50, max 500).
// AT-URIs in amplifier_post_uri are converted to bsky.app URLs.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;

use crate::web::{api_error, AppState, AuthUser};

#[derive(Deserialize, Default)]
pub struct EventsQuery {
    pub limit: Option<usize>,
}

/// GET /api/events — recent amplification events, newest first.
pub async fn list_events(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
    Query(params): Query<EventsQuery>,
) -> Response {
    let limit = params.limit.unwrap_or(50).min(500);
    let events = match state.db.get_recent_events(&auth.did, limit as u32).await {
        Ok(events) => events,
        Err(e) => {
            tracing::error!(error = %e, "DB error listing events");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error");
        }
    };

    let events: Vec<serde_json::Value> = events
        .into_iter()
        .map(|mut e| {
            // Convert AT-URIs to bsky.app URLs.
            // For reposts, amplifier_post_uri is an app.bsky.feed.repost record which
            // has no viewable page on bsky.app. Use the original_post_uri instead so
            // the "View post" link shows the reposted content.
            let view_uri = if e.event_type == "repost" {
                Some(at_uri_to_bsky_url(&e.original_post_uri))
            } else if let Some(ref uri) = e.amplifier_post_uri {
                Some(at_uri_to_bsky_url(uri))
            } else {
                None
            };
            e.amplifier_post_uri = view_uri;
            serde_json::json!({
                "id": e.id,
                "event_type": e.event_type,
                "amplifier_did": e.amplifier_did,
                "amplifier_handle": e.amplifier_handle,
                "original_post_uri": at_uri_to_bsky_url(&e.original_post_uri),
                "amplifier_post_uri": e.amplifier_post_uri,
                "amplifier_text": e.amplifier_text,
                "detected_at": e.detected_at,
            })
        })
        .collect();

    Json(serde_json::json!({ "events": events })).into_response()
}

/// Convert an AT-URI to a bsky.app web URL.
///
/// Only converts `app.bsky.feed.post` URIs — other collections (like
/// `app.bsky.feed.repost`) don't have viewable pages on bsky.app.
fn at_uri_to_bsky_url(uri: &str) -> String {
    if uri.starts_with("https://") {
        return uri.to_string();
    }
    let rest = match uri.strip_prefix("at://") {
        Some(r) => r,
        None => return uri.to_string(),
    };
    let parts: Vec<&str> = rest.splitn(3, '/').collect();
    if parts.len() != 3 {
        return uri.to_string();
    }
    let did = parts[0];
    let collection = parts[1];
    let rkey = parts[2];
    // Only app.bsky.feed.post records have viewable pages on bsky.app
    if collection == "app.bsky.feed.post" {
        format!("https://bsky.app/profile/{did}/post/{rkey}")
    } else {
        uri.to_string()
    }
}
