// Label API endpoints — user-provided ground-truth labels for scored accounts.
//
// POST /api/accounts/{did}/label — upsert a label (high/elevated/watch/safe)
// GET  /api/review               — unlabeled accounts sorted by threat_score
// GET  /api/accuracy             — accuracy metrics comparing predicted vs labeled

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;

use crate::web::{api_error, AppState, AuthUser};

const VALID_LABELS: &[&str] = &["high", "elevated", "watch", "safe"];

#[derive(Deserialize)]
pub struct LabelRequest {
    pub label: String,
    pub notes: Option<String>,
}

#[derive(Deserialize)]
pub struct ReviewQuery {
    pub limit: Option<i64>,
}

/// POST /api/accounts/{did}/label — create or update a label for a target account.
pub async fn upsert_label(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
    Path(target_did): Path<String>,
    Json(body): Json<LabelRequest>,
) -> Response {
    let label = body.label.to_lowercase();
    if !VALID_LABELS.contains(&label.as_str()) {
        return api_error(
            StatusCode::BAD_REQUEST,
            &format!(
                "Invalid label '{}'. Must be one of: high, elevated, watch, safe",
                body.label
            ),
        );
    }

    if let Err(e) = state
        .db
        .upsert_user_label(&auth.did, &target_did, &label, body.notes.as_deref())
        .await
    {
        tracing::error!(error = %e, "Failed to upsert label");
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error");
    }

    // Fetch the label back to get the labeled_at timestamp
    let user_label = match state.db.get_user_label(&auth.did, &target_did).await {
        Ok(Some(l)) => l,
        Ok(None) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Label not found after upsert",
            )
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to fetch label after upsert");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error");
        }
    };

    // Look up the predicted tier from account_scores
    let predicted_tier = match state.db.get_account_by_did(&auth.did, &target_did).await {
        Ok(Some(account)) => account.threat_tier,
        _ => None,
    };

    Json(serde_json::json!({
        "user_did": auth.did,
        "target_did": target_did,
        "label": user_label.label,
        "labeled_at": user_label.labeled_at,
        "notes": user_label.notes,
        "predicted_tier": predicted_tier,
    }))
    .into_response()
}

/// GET /api/review — unlabeled accounts sorted by threat_score descending.
pub async fn get_review_queue(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
    Query(params): Query<ReviewQuery>,
) -> Response {
    let limit = params.limit.unwrap_or(20).min(100);

    match state.db.get_unlabeled_accounts(&auth.did, limit).await {
        Ok(accounts) => {
            let json_accounts: Vec<serde_json::Value> = accounts
                .into_iter()
                .map(|a| {
                    serde_json::json!({
                        "did": a.did,
                        "handle": a.handle,
                        "toxicity_score": a.toxicity_score,
                        "topic_overlap": a.topic_overlap,
                        "threat_score": a.threat_score,
                        "threat_tier": a.threat_tier,
                        "posts_analyzed": a.posts_analyzed,
                        "scored_at": a.scored_at,
                        "context_score": a.context_score,
                    })
                })
                .collect();

            Json(serde_json::json!({
                "accounts": json_accounts,
                "total": json_accounts.len(),
            }))
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to fetch review queue");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        }
    }
}

/// GET /api/accuracy — scoring accuracy metrics comparing predicted tiers to user labels.
pub async fn get_accuracy(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
) -> Response {
    match state.db.get_accuracy_metrics(&auth.did).await {
        Ok(metrics) => Json(serde_json::json!({
            "total_labeled": metrics.total_labeled,
            "exact_matches": metrics.exact_matches,
            "overscored": metrics.overscored,
            "underscored": metrics.underscored,
            "accuracy": metrics.accuracy,
        }))
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to fetch accuracy metrics");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        }
    }
}
