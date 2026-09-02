//! One DPoP-proofed HTTP request with the single nonce retry every AT
//! Protocol server may demand (#315).
//!
//! Hand-rolled rather than the `atproto-oauth` `DpopRetry` middleware: that
//! middleware consumes the body of any 400/401 it inspects, which makes token
//! endpoint errors (`invalid_grant`) indistinguishable from parse failures.
//! Here the body is always read to a string and handed back with the status,
//! so callers classify errors themselves.

use anyhow::{Context, Result};
use atproto_identity::key::KeyData;
use atproto_oauth::dpop::{auth_dpop, request_dpop};
use atproto_oauth::jwt::mint;
use reqwest::header::HeaderMap;
use reqwest::StatusCode;

pub struct DpopResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

/// Send `method url` with a fresh DPoP proof (bound to `access_token` when
/// given). `build` adds body/query/headers to the request. If the server
/// answers 400/401 with a `DPoP-Nonce` header and a `use_dpop_nonce` /
/// `invalid_dpop_proof` signal (WWW-Authenticate or JSON body), the request
/// is re-signed with the nonce and sent exactly once more.
pub async fn send_dpop(
    http: &reqwest::Client,
    key: &KeyData,
    method: &str,
    url: &str,
    access_token: Option<&str>,
    build: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
) -> Result<DpopResponse> {
    let (proof, header, claims) = match access_token {
        Some(t) => request_dpop(key, method, url, t),
        None => auth_dpop(key, method, url),
    }
    .context("mint DPoP proof")?;

    let first = send_once(http, method, url, access_token, &proof, &build).await?;
    let nonce = first
        .headers
        .get("DPoP-Nonce")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let (Some(nonce), true) = (nonce, is_nonce_challenge(&first)) else {
        return Ok(first);
    };

    let mut claims = claims;
    claims
        .private
        .insert("nonce".to_string(), serde_json::Value::String(nonce));
    let proof = mint(key, &header, &claims).context("mint DPoP proof with nonce")?;
    send_once(http, method, url, access_token, &proof, &build).await
}

fn is_nonce_challenge(r: &DpopResponse) -> bool {
    if r.status != StatusCode::BAD_REQUEST && r.status != StatusCode::UNAUTHORIZED {
        return false;
    }
    let in_header = r
        .headers
        .get("WWW-Authenticate")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("use_dpop_nonce") || v.contains("invalid_dpop_proof"));
    let in_body = serde_json::from_str::<serde_json::Value>(&r.body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
        .is_some_and(|e| e == "use_dpop_nonce" || e == "invalid_dpop_proof");
    in_header || in_body
}

async fn send_once(
    http: &reqwest::Client,
    method: &str,
    url: &str,
    access_token: Option<&str>,
    proof: &str,
    build: &impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
) -> Result<DpopResponse> {
    let m = reqwest::Method::from_bytes(method.as_bytes()).context("http method")?;
    let mut req = http.request(m, url).header("DPoP", proof);
    if let Some(t) = access_token {
        req = req.header("Authorization", format!("DPoP {t}"));
    }
    let resp = build(req).send().await.context("send")?;
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp.text().await.unwrap_or_default();
    Ok(DpopResponse {
        status,
        headers,
        body,
    })
}
