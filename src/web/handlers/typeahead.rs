// GET /api/typeahead?q=... — handle suggestions for the login screen (#227).
//
// PUBLIC by necessity: the login screen is pre-auth, so this cannot sit behind
// require_auth. That makes it an unauthenticated endpoint which performs an
// outbound request on demand, so it is guarded on three axes:
//
//   1. Query validation  — src/web/typeahead.rs::normalize_query
//   2. Per-IP rate limit — src/web/typeahead.rs::TypeaheadLimiter (rejects, not waits)
//   3. Upstream timeout  — a slow upstream must not pin our workers
//
// PRIVACY: this is why the browser does not call upstream directly. Proxying
// means the typeahead host sees Charcoal's server, not the user's IP, and a
// partially-typed handle on a pre-auth screen is exactly the kind of signal
// that should not leak for a tool whose users are worried about being targeted.
//
// We deliberately do NOT log the query. Logging what someone typed before they
// logged in would recreate the leak we are proxying to avoid.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use tracing::{debug, warn};

use crate::web::typeahead::{normalize_query, RateDecision};
use crate::web::{api_error, AppState};

/// How long we will wait on upstream before giving up. Typeahead is a
/// latency-sensitive nicety — a slow answer is worthless to the user and a
/// held worker is a cost to us.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(3);

/// Maximum suggestions returned. The dropdown shows a handful; asking for more
/// just makes upstream work harder for results nobody sees.
const RESULT_LIMIT: usize = 8;

#[derive(Debug, Deserialize)]
pub struct TypeaheadParams {
    pub q: String,
}

/// One suggestion, trimmed to what the dropdown actually renders.
#[derive(Debug, Serialize)]
pub struct Suggestion {
    pub did: String,
    pub handle: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

/// Upstream's response shape. Identical for typeahead.waow.tech and
/// public.api.bsky.app — both speak `app.bsky.actor.searchActorsTypeahead`.
#[derive(Debug, Deserialize)]
struct UpstreamResponse {
    #[serde(default)]
    actors: Vec<UpstreamActor>,
}

#[derive(Debug, Deserialize)]
struct UpstreamActor {
    did: String,
    handle: String,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    avatar: Option<String>,
}

/// Best-effort client identity for rate limiting.
///
/// Charcoal always runs behind Railway's proxy, so the socket address would be
/// the proxy for every caller — useless as a key. The first hop in
/// `x-forwarded-for` is the meaningful one.
///
/// This is spoofable by design. It is a fairness mechanism against ordinary
/// abuse, not an authentication control; the query validation and upstream
/// timeout stand on their own regardless of what this returns. Callers we
/// cannot identify share the "unknown" bucket, which is deliberately
/// conservative — they throttle each other rather than going unlimited.
fn client_key(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

pub async fn suggest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<TypeaheadParams>,
) -> Response {
    let key = client_key(&headers);

    if state.typeahead_limiter.check(&key, Instant::now()) == RateDecision::TooMany {
        // No query in this log line — see the privacy note at the top.
        debug!("typeahead rate limited");
        return api_error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests — slow down a moment.",
        );
    }

    // An unusable query is not an error worth surfacing; the field is simply
    // too short yet. Return an empty list so the UI renders nothing.
    let Some(query) = normalize_query(&params.q) else {
        return Json(Vec::<Suggestion>::new()).into_response();
    };

    let url = format!(
        "{}/xrpc/app.bsky.actor.searchActorsTypeahead",
        state.config.typeahead_url.trim_end_matches('/')
    );

    let request = state
        .http
        .get(&url)
        .query(&[("q", query.as_str()), ("limit", &RESULT_LIMIT.to_string())])
        .timeout(UPSTREAM_TIMEOUT)
        .send();

    let response = match request.await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %format!("{e:#}"), "typeahead upstream request failed");
            // Degrade to "no suggestions" rather than an error: typeahead is an
            // enhancement, and a broken one must never block someone logging in.
            return Json(Vec::<Suggestion>::new()).into_response();
        }
    };

    if !response.status().is_success() {
        warn!(status = %response.status(), "typeahead upstream returned an error status");
        return Json(Vec::<Suggestion>::new()).into_response();
    }

    let parsed: UpstreamResponse = match response.json().await {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %format!("{e:#}"), "typeahead upstream returned unparseable JSON");
            return Json(Vec::<Suggestion>::new()).into_response();
        }
    };

    let suggestions: Vec<Suggestion> = parsed
        .actors
        .into_iter()
        .take(RESULT_LIMIT)
        .map(|a| Suggestion {
            did: a.did,
            handle: a.handle,
            display_name: a.display_name,
            avatar: a.avatar,
        })
        .collect();

    Json(suggestions).into_response()
}

/// Build the shared limiter. Separate so `mod.rs` stays declarative.
pub fn build_limiter() -> Arc<crate::web::typeahead::TypeaheadLimiter> {
    // 30 requests per 10s per caller: generous for debounced typing (the UI
    // debounces at 200ms, so a fast typist lands well under), tight enough that
    // a scripted caller is throttled quickly. 10k keys caps the map's memory.
    Arc::new(crate::web::typeahead::TypeaheadLimiter::new(
        30,
        Duration::from_secs(10),
        10_000,
    ))
}
