//! #350: the getAuthorFeed walk must stop after a fixed number of pages.
//!
//! Reposts and posts under 15 characters are skipped, but they still cost a
//! page. Before this fix the walk had no page bound, so an account whose feed
//! is almost all reposts (or image-only posts) was read back to its very
//! first post: on staging, rgesteve took 1,070 pages / 243 s and davedawn
//! 856 pages / 187 s. One such account held the whole gather open for minutes.
//!
//! Every endless-feed test here wraps the call in a tokio timeout, so the
//! unfixed code FAILS (times out) instead of hanging the test run.

use std::time::Duration;

use charcoal::bluesky::client::PublicAtpClient;
use charcoal::bluesky::posts::{
    collect_feed_posts, fetch_recent_posts, FeedKind, MAX_FEED_PAGES, MAX_PROTECTED_FEED_PAGES,
};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const FEED: &str = "/xrpc/app.bsky.feed.getAuthorFeed";

/// The cap this fix introduces, in pages. Spelled out here (not imported) so
/// the test states the number the maintainer approved.
const CAP: u64 = 10;

/// Generous for CAP pages against a local mock; far too short for the unfixed
/// code, which never stops on an endless feed.
const TIMEOUT: Duration = Duration::from_secs(20);

fn author() -> Value {
    json!({"did": "did:plc:target", "handle": "target.test"})
}

/// One authored post. `n` keeps URIs unique across pages.
fn post_view(n: usize, text: &str) -> Value {
    json!({
        "uri": format!("at://did:plc:target/app.bsky.feed.post/{n}"),
        "cid": "bafyreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy",
        "author": author(),
        "record": {
            "$type": "app.bsky.feed.post",
            "text": text,
            "createdAt": "2026-10-01T12:00:00.000Z"
        },
        "indexedAt": "2026-10-01T12:00:00.000Z"
    })
}

/// A repost by the target of someone else's post.
fn repost_item(n: usize) -> Value {
    json!({
        "post": post_view(n, "someone else's long enough post text"),
        "reason": {
            "$type": "app.bsky.feed.defs#reasonRepost",
            "by": author(),
            "indexedAt": "2026-10-01T12:00:00.000Z"
        }
    })
}

fn original_item(n: usize, text: &str) -> Value {
    json!({"post": post_view(n, text)})
}

/// Serves an endless feed: every page is `make_page(page_index)` and always
/// carries a cursor, so only a page cap can end the walk.
struct EndlessFeed<F: Fn(usize) -> Vec<Value> + Send + Sync + 'static> {
    make_page: F,
    served: std::sync::atomic::AtomicUsize,
}

impl<F: Fn(usize) -> Vec<Value> + Send + Sync + 'static> Respond for EndlessFeed<F> {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        let i = self
            .served
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ResponseTemplate::new(200).set_body_json(json!({
            "feed": (self.make_page)(i),
            "cursor": format!("c{}", i + 1),
        }))
    }
}

async fn endless_server<F>(make_page: F) -> MockServer
where
    F: Fn(usize) -> Vec<Value> + Send + Sync + 'static,
{
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(FEED))
        .respond_with(EndlessFeed {
            make_page,
            served: Default::default(),
        })
        .mount(&server)
        .await;
    server
}

async fn feed_requests(server: &MockServer) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.url.path() == FEED)
        .collect()
}

fn limit_of(req: &Request) -> Option<String> {
    req.url
        .query_pairs()
        .find(|(k, _)| k == "limit")
        .map(|(_, v)| v.into_owned())
}

/// rgesteve / davedawn: every item is a repost, forever.
#[tokio::test]
async fn repost_only_feed_stops_at_the_page_cap_with_ok() {
    let server = endless_server(|p| (0..100).map(|i| repost_item(p * 100 + i)).collect()).await;
    let client = PublicAtpClient::new(&server.uri()).unwrap();

    let result = tokio::time::timeout(TIMEOUT, collect_feed_posts(&client, "target.test", 50))
        .await
        .expect("walk did not stop: no page cap on a repost-only feed");

    let feed = result.expect("a capped walk is a normal Ok result, not an error");
    assert!(feed.is_empty(), "reposts are never kept");
    assert_eq!(feed_requests(&server).await.len() as u64, CAP);
    assert_eq!(
        MAX_FEED_PAGES as u64, CAP,
        "the library constant is the approved cap"
    );
}

/// superposition: image posts whose text is under 15 characters.
#[tokio::test]
async fn short_post_only_feed_stops_at_the_page_cap_with_ok() {
    let server = endless_server(|p| {
        (0..100)
            .map(|i| original_item(p * 100 + i, "nice pic"))
            .collect()
    })
    .await;
    let client = PublicAtpClient::new(&server.uri()).unwrap();

    let result = tokio::time::timeout(TIMEOUT, collect_feed_posts(&client, "target.test", 50))
        .await
        .expect("walk did not stop: no page cap on a short-post feed");

    let feed = result.expect("a capped walk is a normal Ok result, not an error");
    assert!(feed.is_empty(), "posts under 15 chars are never kept");
    assert_eq!(feed_requests(&server).await.len() as u64, CAP);
}

/// A capped walk keeps whatever usable posts it found before the cap.
#[tokio::test]
async fn capped_walk_returns_the_few_usable_posts_it_found() {
    // One usable post on every page; 99 reposts around it.
    let server = endless_server(|p| {
        let mut page: Vec<Value> = (0..99).map(|i| repost_item(p * 100 + i)).collect();
        page.push(original_item(
            p * 100 + 99,
            "a genuinely long enough original post",
        ));
        page
    })
    .await;
    let client = PublicAtpClient::new(&server.uri()).unwrap();

    let feed = tokio::time::timeout(TIMEOUT, collect_feed_posts(&client, "target.test", 50))
        .await
        .expect("walk did not stop")
        .unwrap();

    assert_eq!(feed.len() as u64, CAP, "one kept post per page, CAP pages");
    assert_eq!(feed_requests(&server).await.len() as u64, CAP);
}

/// A normal account: half its items are reposts, so 50 usable posts arrive on
/// page 1 at limit=100. The walk stops there and keeps feed order.
#[tokio::test]
async fn normal_account_stops_early_with_an_unchanged_result() {
    let server = endless_server(|p| {
        (0..100)
            .map(|i| {
                let n = p * 100 + i;
                if i % 2 == 0 {
                    repost_item(n)
                } else {
                    original_item(n, &format!("original post number {n} with text"))
                }
            })
            .collect()
    })
    .await;
    let client = PublicAtpClient::new(&server.uri()).unwrap();

    let feed = tokio::time::timeout(TIMEOUT, collect_feed_posts(&client, "target.test", 50))
        .await
        .expect("walk did not stop")
        .unwrap();

    assert_eq!(feed.len(), 50);
    assert!(feed.iter().all(|fp| matches!(fp.kind, FeedKind::Original)));
    // Feed order preserved: the odd-numbered items of page 0, ascending.
    let expected: Vec<String> = (0..50)
        .map(|k| format!("at://did:plc:target/app.bsky.feed.post/{}", 2 * k + 1))
        .collect();
    let got: Vec<String> = feed.iter().map(|fp| fp.post.uri.clone()).collect();
    assert_eq!(got, expected);
    assert!(
        (feed_requests(&server).await.len() as u64) < CAP,
        "a normal account must stop well before the cap"
    );
}

/// A feed that ends (no cursor) before the cap still ends normally.
#[tokio::test]
async fn short_feed_ends_at_its_last_page() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(FEED))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "feed": [original_item(1, "the only post this account ever wrote")],
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = PublicAtpClient::new(&server.uri()).unwrap();

    let feed = collect_feed_posts(&client, "target.test", 50)
        .await
        .unwrap();
    assert_eq!(feed.len(), 1);
}

/// Every page asks for the API maximum, 100 items — even when fewer posts are
/// wanted — so a deep walk costs half the round trips it did at 50.
#[tokio::test]
async fn every_request_asks_for_limit_100() {
    let server = endless_server(|p| (0..100).map(|i| repost_item(p * 100 + i)).collect()).await;
    let client = PublicAtpClient::new(&server.uri()).unwrap();

    for wanted in [25, 50] {
        let _ = tokio::time::timeout(TIMEOUT, collect_feed_posts(&client, "target.test", wanted))
            .await
            .expect("walk did not stop");
    }

    let reqs = feed_requests(&server).await;
    assert!(!reqs.is_empty());
    for r in &reqs {
        assert_eq!(limit_of(r).as_deref(), Some("100"), "{}", r.url);
    }
}

/// The protected user's own feed read (`fetch_recent_posts`) had the same
/// unbounded loop. It is bounded too — with a larger budget, since a
/// 500-post topic fingerprint needs at least 5 pages even when every item is
/// usable — and it must still end with Ok rather than walk forever.
#[tokio::test]
async fn protected_user_feed_read_is_bounded_too() {
    let server = endless_server(|p| (0..100).map(|i| repost_item(p * 100 + i)).collect()).await;
    let client = PublicAtpClient::new(&server.uri()).unwrap();

    let posts = tokio::time::timeout(TIMEOUT, fetch_recent_posts(&client, "target.test", 500))
        .await
        .expect("protected-user walk did not stop")
        .expect("a capped walk is Ok");

    assert!(posts.is_empty());
    let reqs = feed_requests(&server).await;
    assert_eq!(reqs.len(), 50, "protected-user cap is 50 pages");
    assert_eq!(MAX_PROTECTED_FEED_PAGES, 50);
    for r in &reqs {
        assert_eq!(limit_of(r).as_deref(), Some("100"), "{}", r.url);
    }
}
