//! Typed PDS client for the five XRPC calls the actions feature makes (#315).
//!
//! Every write goes to the user's OWN PDS with their DPoP-bound access token.
//! The only record this module ever creates is `app.bsky.graph.block` with
//! `subject` + `createdAt` (the invariant, docs/self-protective-invariant.md);
//! the only records it deletes are ones whose URI the caller stored.
//! `app.bsky.graph.*` calls are sent to the PDS base URL too, for it to proxy
//! to the AppView. Which AppView is NOT implicit: bsky.social fills in a
//! default, but a PDS with none configured (every self-hosted reference PDS)
//! answers 501 `MethodNotImplemented` unless the request names the service in
//! an `atproto-proxy` header (#322). So every `app.bsky.*` call carries
//! `atproto-proxy: <APPVIEW_DID>`, and the native `com.atproto.*` calls never
//! do — they are served by the PDS itself.

use std::collections::{HashMap, HashSet};

use atproto_identity::key::KeyData;
use serde_json::{json, Value};

use super::dpop_http::{send_dpop, DpopResponse, NonceCache};
use super::scope::APPVIEW_DID;

/// The header that tells a PDS which service to forward an XRPC call to.
const PROXY_HEADER: &str = "atproto-proxy";

/// Does the PDS proxy this method to the AppView (and so need the header)?
/// The check is on the lexicon namespace, not a list of methods, so a new
/// `app.bsky.*` call added later cannot silently ship without it.
fn is_appview_method(nsid: &str) -> bool {
    nsid.starts_with("app.bsky.")
}

/// Add `atproto-proxy` for AppView methods; leave PDS-native ones untouched.
fn with_proxy(nsid: &str, r: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    if is_appview_method(nsid) {
        r.header(PROXY_HEADER, APPVIEW_DID)
    } else {
        r
    }
}

pub const BLOCK_COLLECTION: &str = "app.bsky.graph.block";
/// applyWrites hard limit on the reference PDS.
pub const APPLY_WRITES_MAX: usize = 200;
/// Hard stop on `getBlocks`/`getMutes` pagination. A misbehaving server that
/// repeats a cursor must not spin forever, but hitting this cap means the
/// list is truncated — the caller must never treat that as success.
pub const MAX_LIST_PAGES: usize = 100;

#[derive(Debug, thiserror::Error)]
pub enum PdsError {
    /// HTTP 429. `reset_at` is the unix-seconds value of `ratelimit-reset` if
    /// the header was present and parseable.
    #[error("rate limited")]
    RateLimited { reset_at: Option<i64> },
    /// 401 that is not a nonce challenge: the access token is dead.
    #[error("not authorized")]
    Auth,
    /// Any other 4xx. `message` is `"<error>: <message>"` from the JSON body
    /// when present, else the status reason.
    #[error("{status}: {message}")]
    Client { status: u16, message: String },
    /// 5xx — retryable.
    #[error("server error {status}")]
    Server { status: u16 },
    /// Could not reach the server, or minting failed — retryable.
    #[error("transport: {0}")]
    Transport(String),
}

impl PdsError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, PdsError::Server { .. } | PdsError::Transport(_))
    }
}

/// One entry for `com.atproto.repo.applyWrites`.
#[derive(Debug, Clone, PartialEq)]
pub enum Write {
    Create { collection: String, value: Value },
    Delete { collection: String, rkey: String },
}

pub struct PdsClient {
    http: reqwest::Client,
    pds_url: String,
    did: String,
    dpop_key: KeyData,
    access_token: String,
    /// One per client, i.e. one per batch — the same granularity as the
    /// session load (#333).
    nonce: NonceCache,
}

impl PdsClient {
    pub fn new(
        http: reqwest::Client,
        pds_url: String,
        did: String,
        dpop_key: KeyData,
        access_token: String,
    ) -> Self {
        Self {
            http,
            pds_url: pds_url.trim_end_matches('/').to_string(),
            did,
            dpop_key,
            access_token,
            nonce: NonceCache::default(),
        }
    }

    pub fn block_create(target_did: &str) -> Write {
        Write::Create {
            collection: BLOCK_COLLECTION.to_string(),
            value: json!({
                "$type": BLOCK_COLLECTION,
                "subject": target_did,
                "createdAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            }),
        }
    }

    pub fn block_delete(rkey: &str) -> Write {
        Write::Delete {
            collection: BLOCK_COLLECTION.to_string(),
            rkey: rkey.to_string(),
        }
    }

    /// `at://<did>/app.bsky.graph.block/<rkey>` → `rkey`, ONLY when the DID is
    /// the user's own and the collection is the block collection. Anything
    /// else is not something Charcoal is allowed to delete.
    pub fn rkey_from_uri(own_did: &str, uri: &str) -> Option<String> {
        let rest = uri.strip_prefix("at://")?;
        let mut parts = rest.splitn(3, '/');
        let (did, coll, rkey) = (parts.next()?, parts.next()?, parts.next()?);
        (did == own_did && coll == BLOCK_COLLECTION && !rkey.is_empty() && !rkey.contains('/'))
            .then(|| rkey.to_string())
    }

    /// One `applyWrites` call (≤ `APPLY_WRITES_MAX` entries — the caller
    /// chunks). Returns, per input, the created record URI (`Some` for
    /// creates, `None` for deletes). The whole call fails or succeeds together.
    pub async fn apply_writes(&self, writes: &[Write]) -> Result<Vec<Option<String>>, PdsError> {
        let entries: Vec<Value> = writes
            .iter()
            .map(|w| match w {
                Write::Create { collection, value } => json!({
                    "$type": "com.atproto.repo.applyWrites#create",
                    "collection": collection,
                    "value": value,
                }),
                Write::Delete { collection, rkey } => json!({
                    "$type": "com.atproto.repo.applyWrites#delete",
                    "collection": collection,
                    "rkey": rkey,
                }),
            })
            .collect();
        let body = json!({ "repo": self.did, "validate": true, "writes": entries });
        let resp = self.post("com.atproto.repo.applyWrites", &body).await?;
        let v: Value = serde_json::from_str(&resp.body)
            .map_err(|e| PdsError::Transport(format!("applyWrites response: {e}")))?;
        let results = v["results"].as_array().cloned().unwrap_or_default();
        Ok(writes
            .iter()
            .enumerate()
            .map(|(i, w)| match w {
                Write::Create { .. } => results
                    .get(i)
                    .and_then(|r| r["uri"].as_str())
                    .map(str::to_owned),
                Write::Delete { .. } => None,
            })
            .collect())
    }

    pub async fn mute_actor(&self, target_did: &str) -> Result<(), PdsError> {
        self.post("app.bsky.graph.muteActor", &json!({ "actor": target_did }))
            .await
            .map(|_| ())
    }

    pub async fn unmute_actor(&self, target_did: &str) -> Result<(), PdsError> {
        self.post(
            "app.bsky.graph.unmuteActor",
            &json!({ "actor": target_did }),
        )
        .await
        .map(|_| ())
    }

    /// All current blocks: target DID → the block record URI.
    pub async fn get_blocks(&self) -> Result<HashMap<String, String>, PdsError> {
        let mut out = HashMap::new();
        self.paginate("app.bsky.graph.getBlocks", "blocks", |item| {
            if let (Some(did), Some(uri)) =
                (item["did"].as_str(), item["viewer"]["blocking"].as_str())
            {
                out.insert(did.to_string(), uri.to_string());
            }
        })
        .await?;
        Ok(out)
    }

    pub async fn get_mutes(&self) -> Result<HashSet<String>, PdsError> {
        let mut out = HashSet::new();
        self.paginate("app.bsky.graph.getMutes", "mutes", |item| {
            if let Some(did) = item["did"].as_str() {
                out.insert(did.to_string());
            }
        })
        .await?;
        Ok(out)
    }

    async fn paginate(
        &self,
        nsid: &str,
        key: &str,
        mut each: impl FnMut(&Value),
    ) -> Result<(), PdsError> {
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let url = format!("{}/xrpc/{nsid}", self.pds_url);
            let c = cursor.clone();
            let resp = send_dpop(
                &self.http,
                &self.dpop_key,
                "GET",
                &url,
                Some(&self.access_token),
                &self.nonce,
                |r| {
                    let mut q = vec![("limit", "100".to_string())];
                    if let Some(c) = &c {
                        q.push(("cursor", c.clone()));
                    }
                    with_proxy(nsid, r).query(&q)
                },
            )
            .await
            .map_err(|e| PdsError::Transport(e.to_string()))?;
            let resp = classify(resp)?;
            let v: Value = serde_json::from_str(&resp.body)
                .map_err(|e| PdsError::Transport(format!("{nsid} response: {e}")))?;
            for item in v[key].as_array().into_iter().flatten() {
                each(item);
            }
            match v["cursor"].as_str() {
                Some(next) if !next.is_empty() && Some(next) != cursor.as_deref() => {
                    cursor = Some(next.to_string())
                }
                _ => return Ok(()),
            }
        }
        Err(PdsError::Transport(format!(
            "{nsid}: list exceeded {MAX_LIST_PAGES} pages; refusing a partial view"
        )))
    }

    async fn post(&self, nsid: &str, body: &Value) -> Result<DpopResponse, PdsError> {
        let url = format!("{}/xrpc/{nsid}", self.pds_url);
        let resp = send_dpop(
            &self.http,
            &self.dpop_key,
            "POST",
            &url,
            Some(&self.access_token),
            &self.nonce,
            |r| with_proxy(nsid, r).json(body),
        )
        .await
        .map_err(|e| PdsError::Transport(e.to_string()))?;
        classify(resp)
    }
}

/// Status → `PdsError`. Success passes the response through.
fn classify(resp: DpopResponse) -> Result<DpopResponse, PdsError> {
    let s = resp.status.as_u16();
    match s {
        200..=299 => Ok(resp),
        429 => Err(PdsError::RateLimited {
            reset_at: resp
                .headers
                .get("ratelimit-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<i64>().ok()),
        }),
        401 => Err(PdsError::Auth),
        400..=499 => {
            let message = serde_json::from_str::<Value>(&resp.body)
                .ok()
                .and_then(|v| {
                    let err = v["error"].as_str()?.to_string();
                    Some(match v["message"].as_str() {
                        Some(m) => format!("{err}: {m}"),
                        None => err,
                    })
                })
                .unwrap_or_else(|| {
                    resp.status
                        .canonical_reason()
                        .unwrap_or("client error")
                        .to_string()
                });
            Err(PdsError::Client { status: s, message })
        }
        _ => Err(PdsError::Server { status: s }),
    }
}
