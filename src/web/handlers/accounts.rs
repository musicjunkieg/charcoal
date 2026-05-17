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
use axum::{Extension, Json};
use serde::Deserialize;

use crate::db::models::AccountScore;
use crate::web::{api_error, AppState, AuthUser};

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
    Extension(auth): Extension<AuthUser>,
    Query(params): Query<AccountsQuery>,
) -> Response {
    let mut accounts = match state.db.get_ranked_threats(&auth.effective_did, 0.0).await {
        Ok(accounts) => accounts,
        Err(e) => {
            tracing::error!(error = %e, "DB error fetching accounts");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error");
        }
    };

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
    .into_response()
}

/// GET /api/accounts/:handle — single account by handle.
///
/// If the account has been scored, returns full score data.
/// If the account appears in amplification events but hasn't been scored yet,
/// returns a stub with handle and "not yet scored" status so the page renders.
pub async fn get_account(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
    Path(handle): Path<String>,
) -> Response {
    match state
        .db
        .get_account_by_handle(&auth.effective_did, &handle)
        .await
    {
        Ok(Some(account)) => {
            let mut json = account_to_json(account.clone(), 0);
            // Include user label if one exists for this account
            let label_json = match state
                .db
                .get_user_label(&auth.effective_did, &account.did)
                .await
            {
                Ok(Some(label)) => serde_json::json!({
                    "label": label.label,
                    "labeled_at": label.labeled_at,
                    "notes": label.notes,
                }),
                _ => serde_json::Value::Null,
            };
            json.as_object_mut()
                .unwrap()
                .insert("user_label".to_string(), label_json);
            Json(json).into_response()
        }
        Ok(None) => {
            // No score yet — return a stub so the detail page can still render.
            // The frontend should show "not yet scored" instead of a 404.
            Json(serde_json::json!({
                "rank": 0,
                "did": null,
                "handle": handle,
                "toxicity_score": null,
                "topic_overlap": null,
                "threat_score": null,
                "threat_tier": null,
                "posts_analyzed": 0,
                "top_toxic_posts": [],
                "scored_at": null,
                "behavioral_signals": null,
            }))
            .into_response()
        }
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
        "context_score": account.context_score,
    })
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
    if collection == "app.bsky.feed.post" {
        format!("https://bsky.app/profile/{did}/post/{rkey}")
    } else {
        uri.to_string()
    }
}
