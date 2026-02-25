// Account list and detail handlers.
//
// GET /api/accounts         — paginated, optional ?tier= filter and ?q= search
// GET /api/accounts/:handle — single account detail
//
// AT-URIs (at://did/collection/rkey) in top_toxic_posts are converted to
// clickable Bluesky web URLs (https://bsky.app/profile/did/post/rkey).
//
// The ?q= search is a case-insensitive substring match done in Rust after
// loading all accounts — the DB layer doesn't have a LIKE query for this.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::db::models::AccountScore;
use crate::web::{api_error, AppState};

#[derive(Deserialize, Default)]
pub struct AccountsQuery {
    /// Filter by tier: High | Elevated | Watch | Low
    pub tier: Option<String>,
    /// Case-insensitive handle search
    pub q: Option<String>,
    /// Page number (1-based)
    pub page: Option<usize>,
    /// Results per page (default 50, max 200)
    pub per_page: Option<usize>,
}

/// GET /api/accounts — list accounts with optional tier filter and search.
pub async fn list_accounts(
    State(state): State<AppState>,
    Query(params): Query<AccountsQuery>,
) -> impl IntoResponse {
    let mut accounts = state.db.get_ranked_threats(0.0).await.unwrap_or_default();

    // Tier filter
    if let Some(ref tier) = params.tier {
        let tier_upper = tier.to_uppercase();
        let tier_str = match tier_upper.as_str() {
            "HIGH" => "High",
            "ELEVATED" => "Elevated",
            "WATCH" => "Watch",
            "LOW" => "Low",
            _ => "",
        };
        if !tier_str.is_empty() {
            accounts.retain(|a| a.threat_tier.as_deref() == Some(tier_str));
        }
    }

    // Handle search — case-insensitive substring match
    if let Some(ref q) = params.q {
        let q_lower = q.to_lowercase();
        accounts.retain(|a| a.handle.to_lowercase().contains(&q_lower));
    }

    let total = accounts.len();

    // Pagination
    let per_page = params.per_page.unwrap_or(50).min(200);
    let page = params.page.unwrap_or(1).max(1);
    let offset = (page - 1) * per_page;
    let accounts: Vec<serde_json::Value> = accounts
        .into_iter()
        .skip(offset)
        .take(per_page)
        .enumerate()
        .map(|(i, a)| account_to_json(a, offset + i + 1))
        .collect();

    Json(serde_json::json!({
        "accounts": accounts,
        "total": total,
        "page": page,
        "per_page": per_page,
    }))
}

/// GET /api/accounts/:handle — single account by handle.
pub async fn get_account(State(state): State<AppState>, Path(handle): Path<String>) -> Response {
    match state.db.get_account_by_handle(&handle).await {
        Ok(Some(account)) => Json(account_to_json(account, 0)).into_response(),
        Ok(None) => api_error(StatusCode::NOT_FOUND, "Account not found"),
        Err(e) => {
            tracing::error!(error = %e, handle = %handle, "DB error fetching account");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        }
    }
}

// --- Helpers ---

/// Convert an AccountScore to a JSON value, transforming AT-URIs to bsky.app links.
fn account_to_json(mut account: AccountScore, rank: usize) -> serde_json::Value {
    // Convert AT-URIs in top_toxic_posts to bsky.app URLs.
    for post in &mut account.top_toxic_posts {
        post.uri = at_uri_to_bsky_url(&post.uri);
    }

    // Parse behavioral_signals from JSON string to structured object.
    let behavioral = account
        .behavioral_signals
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());

    serde_json::json!({
        "rank": rank,
        "did": account.did,
        "handle": account.handle,
        "toxicity_score": account.toxicity_score,
        "topic_overlap": account.topic_overlap,
        "threat_score": account.threat_score,
        "threat_tier": account.threat_tier,
        "posts_analyzed": account.posts_analyzed,
        "top_toxic_posts": account.top_toxic_posts,
        "scored_at": account.scored_at,
        "behavioral_signals": behavioral,
    })
}

/// Convert an AT-URI to a bsky.app web URL.
///
/// AT-URI format: `at://did:plc:xxxx/app.bsky.feed.post/rkey`
/// Web URL format: `https://bsky.app/profile/did:plc:xxxx/post/rkey`
fn at_uri_to_bsky_url(uri: &str) -> String {
    // If it's already a web URL, return as-is.
    if uri.starts_with("https://") {
        return uri.to_string();
    }

    // Strip the "at://" prefix.
    let rest = match uri.strip_prefix("at://") {
        Some(r) => r,
        None => return uri.to_string(),
    };

    // Split into parts: did / collection / rkey
    let parts: Vec<&str> = rest.splitn(3, '/').collect();
    if parts.len() != 3 {
        return uri.to_string();
    }
    let did = parts[0];
    let rkey = parts[2];

    format!("https://bsky.app/profile/{did}/post/{rkey}")
}
