//! #394 A: telling a deleted account apart from every other feed failure.
//!
//! A deleted or deactivated account answers `getAuthorFeed` with HTTP 400 and
//! a body naming why. That is permanent: no retry will ever succeed, so the
//! scan must stop asking. Every other failure — a 5xx, a timeout, a 400 that
//! is OUR bug (a malformed parameter) — must NOT be read as "gone", or one bad
//! deploy would retire every account a user is watching.

use anyhow::Context;
use charcoal::bluesky::client::{account_gone_reason, PublicAtpClient, XrpcStatusError};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FEED: &str = "/xrpc/app.bsky.feed.getAuthorFeed";

/// Serve one canned `getAuthorFeed` failure and return the error the client
/// produced, wrapped in context exactly as `posts.rs` wraps it.
async fn feed_error(status: u16, body: serde_json::Value) -> anyhow::Error {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(FEED))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&server)
        .await;
    let client = PublicAtpClient::new(&server.uri()).unwrap();
    client
        .xrpc_get::<serde_json::Value>("app.bsky.feed.getAuthorFeed", &[("actor", "gone.test")])
        .await
        .context("Failed to fetch feed for @gone.test")
        .unwrap_err()
}

#[tokio::test]
async fn profile_not_found_is_an_account_gone() {
    let err = feed_error(
        400,
        serde_json::json!({"error": "InvalidRequest", "message": "Profile not found"}),
    )
    .await;
    assert_eq!(account_gone_reason(&err), Some("Profile not found"));
}

#[tokio::test]
async fn a_deactivated_or_taken_down_account_is_gone() {
    for code in ["AccountDeactivated", "AccountTakedown"] {
        let err = feed_error(
            400,
            serde_json::json!({"error": code, "message": "whatever the AppView says"}),
        )
        .await;
        assert_eq!(account_gone_reason(&err), Some(code), "{code}");
    }
}

/// A 400 that is not about the account — e.g. our own malformed parameter —
/// is permanent too, but it is NOT a gone account. Retiring the account for it
/// would be wrong, and would hit every account at once.
#[tokio::test]
async fn an_unrelated_400_is_not_an_account_gone() {
    let err = feed_error(
        400,
        serde_json::json!({"error": "InvalidRequest", "message": "Error: limit must be <= 100"}),
    )
    .await;
    assert_eq!(account_gone_reason(&err), None);
}

/// The status and body survive as a typed error, not just a string — and its
/// text is unchanged, so `scan_skips` rows and logs read exactly as before.
#[tokio::test]
async fn the_failure_is_typed_and_its_text_is_unchanged() {
    let err = feed_error(
        400,
        serde_json::json!({"error": "InvalidRequest", "message": "x"}),
    )
    .await;
    let typed = err
        .downcast_ref::<XrpcStatusError>()
        .expect("a non-success status is a typed XrpcStatusError");
    assert_eq!(typed.status, 400);
    assert_eq!(typed.error.as_deref(), Some("InvalidRequest"));
    assert!(
        format!("{err:#}").contains("XRPC app.bsky.feed.getAuthorFeed returned 400 Bad Request:"),
        "got {err:#}"
    );
}

/// A server error is transient by definition, whatever its body says.
#[test]
fn a_server_error_is_never_an_account_gone() {
    let err = anyhow::Error::new(XrpcStatusError::new(
        "app.bsky.feed.getAuthorFeed",
        reqwest::StatusCode::BAD_GATEWAY,
        r#"{"error":"InvalidRequest","message":"Profile not found"}"#.to_string(),
    ));
    assert_eq!(account_gone_reason(&err), None);
}

/// Anything that is not an XRPC status at all — a timeout, a parse error.
#[test]
fn an_untyped_error_is_never_an_account_gone() {
    let err = anyhow::anyhow!("Profile not found");
    assert_eq!(
        account_gone_reason(&err),
        None,
        "matching on the text alone would be guessing"
    );
}
