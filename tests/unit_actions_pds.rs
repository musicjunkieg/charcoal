//! PdsClient against a wiremock stand-in PDS (#315, spec §4.2/§4.3/§6).
#![cfg(feature = "web")]

use std::sync::atomic::{AtomicUsize, Ordering};

use atproto_identity::key::{generate_key, KeyType};
use charcoal::web::actions::pds::{PdsClient, PdsError, Write, MAX_LIST_PAGES};
use charcoal::web::actions::scope::APPVIEW_DID;
use wiremock::matchers::{body_partial_json, header, header_exists, method, path, query_param};
use wiremock::{Match, Mock, MockServer, Request, Respond, ResponseTemplate};

/// Matches only requests that do NOT carry the named header.
struct NoHeader(&'static str);

impl Match for NoHeader {
    fn matches(&self, request: &Request) -> bool {
        !request.headers.contains_key(self.0)
    }
}

const ME: &str = "did:plc:me00000000000000000000";

fn client(mock: &MockServer) -> PdsClient {
    let key = generate_key(KeyType::P256Private).unwrap();
    PdsClient::new(
        reqwest::Client::new(),
        mock.uri(),
        ME.to_string(),
        key,
        "access-token".to_string(),
    )
}

#[tokio::test]
async fn apply_writes_sends_dpop_and_returns_uris_in_order() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.repo.applyWrites"))
        .and(header_exists("DPoP"))
        .and(header_exists("Authorization"))
        .and(body_partial_json(serde_json::json!({ "repo": ME })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "results": [
                { "$type": "com.atproto.repo.applyWrites#createResult",
                  "uri": format!("at://{ME}/app.bsky.graph.block/aaa"), "cid": "x" },
                { "$type": "com.atproto.repo.applyWrites#deleteResult" }
            ]
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let c = client(&mock);
    let out = c
        .apply_writes(&[
            PdsClient::block_create("did:plc:target1"),
            PdsClient::block_delete("bbb"),
        ])
        .await
        .unwrap();
    assert_eq!(
        out,
        vec![Some(format!("at://{ME}/app.bsky.graph.block/aaa")), None]
    );
}

#[tokio::test]
async fn block_create_record_carries_only_subject_and_created_at() {
    // The invariant (#261): nothing Charcoal-specific is ever written.
    let w = PdsClient::block_create("did:plc:target1");
    let Write::Create { collection, value } = w else {
        panic!("expected create")
    };
    assert_eq!(collection, "app.bsky.graph.block");
    let obj = value.as_object().unwrap();
    assert_eq!(obj.len(), 3, "exactly $type, subject, createdAt");
    assert_eq!(obj["$type"], "app.bsky.graph.block");
    assert_eq!(obj["subject"], "did:plc:target1");
    assert!(
        obj["createdAt"].as_str().unwrap().ends_with('Z')
            || obj["createdAt"].as_str().unwrap().contains('+')
    );
}

#[tokio::test]
async fn dpop_nonce_challenge_is_retried_once() {
    let mock = MockServer::start().await;
    // First call: nonce challenge. Second (with nonce): success.
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("WWW-Authenticate", "DPoP error=\"use_dpop_nonce\"")
                .insert_header("DPoP-Nonce", "server-nonce-1")
                .set_body_json(serde_json::json!({ "error": "use_dpop_nonce" })),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock)
        .await;

    client(&mock).mute_actor("did:plc:target1").await.unwrap();
}

#[tokio::test]
async fn error_mapping() {
    let mock = MockServer::start().await;
    let c = client(&mock);

    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .respond_with(ResponseTemplate::new(429).insert_header("ratelimit-reset", "1700000000"))
        .up_to_n_times(1)
        .mount(&mock)
        .await;
    match c.mute_actor("did:plc:t").await.unwrap_err() {
        PdsError::RateLimited {
            reset_at: Some(1_700_000_000),
        } => {}
        other => panic!("expected RateLimited, got {other:?}"),
    }

    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(serde_json::json!({"error":"ExpiredToken"})),
        )
        .up_to_n_times(1)
        .mount(&mock)
        .await;
    assert!(matches!(
        c.mute_actor("did:plc:t").await.unwrap_err(),
        PdsError::Auth
    ));

    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            serde_json::json!({"error":"InvalidRequest","message":"actor not found"}),
        ))
        .up_to_n_times(1)
        .mount(&mock)
        .await;
    match c.mute_actor("did:plc:t").await.unwrap_err() {
        PdsError::Client {
            status: 400,
            message,
        } => assert_eq!(message, "InvalidRequest: actor not found"),
        other => panic!("expected Client, got {other:?}"),
    }

    Mock::given(method("POST"))
        .and(path("/xrpc/app.bsky.graph.muteActor"))
        .respond_with(ResponseTemplate::new(502))
        .up_to_n_times(1)
        .mount(&mock)
        .await;
    assert!(matches!(
        c.mute_actor("did:plc:t").await.unwrap_err(),
        PdsError::Server { status: 502 }
    ));
}

#[tokio::test]
async fn get_blocks_and_mutes_paginate() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xrpc/app.bsky.graph.getBlocks"))
        .and(query_param("limit", "100"))
        .and(query_param("cursor", "c1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "blocks": [ { "did": "did:plc:b2", "handle": "b2.test",
                          "viewer": { "blocking": format!("at://{ME}/app.bsky.graph.block/r2") } } ]
        })))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/xrpc/app.bsky.graph.getBlocks"))
        .and(query_param("limit", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "cursor": "c1",
            "blocks": [ { "did": "did:plc:b1", "handle": "b1.test",
                          "viewer": { "blocking": format!("at://{ME}/app.bsky.graph.block/r1") } } ]
        })))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/xrpc/app.bsky.graph.getMutes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "mutes": [ { "did": "did:plc:m1", "handle": "m1.test" } ]
        })))
        .mount(&mock)
        .await;

    let c = client(&mock);
    let blocks = c.get_blocks().await.unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(
        blocks["did:plc:b1"],
        format!("at://{ME}/app.bsky.graph.block/r1")
    );
    let mutes = c.get_mutes().await.unwrap();
    assert!(mutes.contains("did:plc:m1") && mutes.len() == 1);
}

/// #322: a PDS with no default AppView configured (any self-hosted reference
/// PDS, e.g. airlock.ltd) answers 501 `MethodNotImplemented` to `app.bsky.*`
/// calls unless the request names the AppView in an `atproto-proxy` header.
/// bsky.social fills that in implicitly, which is how the gap shipped. Every
/// proxied call must carry the header; the value must be the exact service
/// DID the `rpc:` scopes were granted for, or the PDS's scope check fails.
#[tokio::test]
async fn app_bsky_calls_carry_the_atproto_proxy_header() {
    let mock = MockServer::start().await;
    for (m, p, body) in [
        (
            "GET",
            "/xrpc/app.bsky.graph.getBlocks",
            serde_json::json!({ "blocks": [] }),
        ),
        (
            "GET",
            "/xrpc/app.bsky.graph.getMutes",
            serde_json::json!({ "mutes": [] }),
        ),
        (
            "POST",
            "/xrpc/app.bsky.graph.muteActor",
            serde_json::json!({}),
        ),
        (
            "POST",
            "/xrpc/app.bsky.graph.unmuteActor",
            serde_json::json!({}),
        ),
    ] {
        // Without the header nothing matches, wiremock answers 404, and the
        // call below errors — so this is a real assertion, not a formality.
        Mock::given(method(m))
            .and(path(p))
            .and(header("atproto-proxy", APPVIEW_DID))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&mock)
            .await;
    }

    let c = client(&mock);
    c.get_blocks().await.expect("getBlocks with proxy header");
    c.get_mutes().await.expect("getMutes with proxy header");
    c.mute_actor("did:plc:t")
        .await
        .expect("muteActor with proxy header");
    c.unmute_actor("did:plc:t")
        .await
        .expect("unmuteActor with proxy header");
}

/// The mirror: `com.atproto.repo.applyWrites` is served by the PDS itself.
/// Sending it with a proxy header would ask the PDS to forward a repo write
/// to the AppView, which is wrong in principle and rejected in practice.
#[tokio::test]
async fn apply_writes_does_not_carry_the_atproto_proxy_header() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.repo.applyWrites"))
        .and(NoHeader("atproto-proxy"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "results": [] })),
        )
        .expect(1)
        .mount(&mock)
        .await;

    client(&mock)
        .apply_writes(&[PdsClient::block_delete("bbb")])
        .await
        .expect("applyWrites without proxy header");
}

/// Always returns a fresh, non-empty cursor so `paginate` never sees a
/// repeat and never exits early — the only way to actually reach the cap.
struct EverAdvancingCursor(AtomicUsize);

impl Respond for EverAdvancingCursor {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "cursor": format!("c{n}"),
            "blocks": []
        }))
    }
}

#[tokio::test]
async fn get_blocks_page_cap_is_an_error_not_a_partial_success() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xrpc/app.bsky.graph.getBlocks"))
        .respond_with(EverAdvancingCursor(AtomicUsize::new(0)))
        .expect(MAX_LIST_PAGES as u64)
        .mount(&mock)
        .await;

    match client(&mock).get_blocks().await.unwrap_err() {
        PdsError::Transport(msg) => assert!(
            msg.contains("pages"),
            "error should mention the page cap: {msg}"
        ),
        other => panic!("expected Transport, got {other:?}"),
    }
}

#[test]
fn rkey_from_uri_parses_only_own_block_uris() {
    assert_eq!(
        PdsClient::rkey_from_uri(ME, &format!("at://{ME}/app.bsky.graph.block/3kabc")),
        Some("3kabc".to_string())
    );
    assert_eq!(
        PdsClient::rkey_from_uri(ME, "at://did:plc:other/app.bsky.graph.block/3kabc"),
        None
    );
    assert_eq!(
        PdsClient::rkey_from_uri(ME, &format!("at://{ME}/app.bsky.feed.post/3kabc")),
        None
    );
    assert_eq!(PdsClient::rkey_from_uri(ME, "garbage"), None);
}
