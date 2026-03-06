// GET /api/events — list recent amplification events.
//
// Optional ?limit= parameter (default 50, max 500).
// AT-URIs in amplifier_post_uri are converted to bsky.app URLs.

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

use crate::web::AppState;

#[derive(Deserialize, Default)]
pub struct EventsQuery {
    pub limit: Option<usize>,
}

/// GET /api/events — recent amplification events, newest first.
pub async fn list_events(
    State(state): State<AppState>,
    Query(params): Query<EventsQuery>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(50).min(500);
    let events = state
        .db
        .get_recent_events(limit as u32)
        .await
        .unwrap_or_default();

    let events: Vec<serde_json::Value> = events
        .into_iter()
        .map(|mut e| {
            // Convert AT-URI amplifier_post_uri to bsky.app URL.
            if let Some(ref uri) = e.amplifier_post_uri {
                e.amplifier_post_uri = Some(at_uri_to_bsky_url(uri));
            }
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

    Json(serde_json::json!({ "events": events }))
}

/// Convert an AT-URI to a bsky.app web URL.
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
    let rkey = parts[2];
    format!("https://bsky.app/profile/{did}/post/{rkey}")
}
