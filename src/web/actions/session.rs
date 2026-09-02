//! OAuth write sessions (#315): store on consent, load-with-refresh before
//! every PDS call, disconnect. See spec §3.4–§3.7.
//!
//! Refresh is hand-rolled rather than `atproto_oauth::workflow::oauth_refresh`
//! because that helper parses every response as a success body and so cannot
//! tell `invalid_grant` (the user revoked us — forget the session) from a
//! transient 5xx (keep the session, try later). The request shape is the
//! same one the crate sends: `private_key_jwt` client assertion + DPoP proof.

use std::collections::HashMap;
use std::sync::Arc;

use atproto_identity::key::{identify_key, KeyData};
use atproto_oauth::jwt::{mint, Claims, Header, JoseClaims};
use atproto_oauth::resources::{pds_resources, AuthorizationServer};
use atproto_oauth::workflow::{OAuthClient, TokenResponse};
use rand::distr::{Alphanumeric, SampleString};
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::crypto::TokenCrypto;
use super::dpop_http::send_dpop;
use super::pds::PdsClient;
use super::scope::scope_grants_write;
use crate::config::Config;
use crate::db::traits::OauthSessionRow;
use crate::db::Database;

/// Refresh when fewer than this many seconds remain on the access token, so
/// a batch that starts now does not die mid-way with a 401.
const REFRESH_THRESHOLD_SECS: i64 = 60;

#[derive(Debug)]
pub enum SessionError {
    /// No usable session: never connected, disconnected, or the refresh
    /// token was rejected (in which case the row has been deleted).
    NotConnected,
    Db(String),
    Crypto(String),
    /// Transient refresh failure; the stored session is kept.
    Refresh(String),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::NotConnected => write!(f, "not connected"),
            SessionError::Db(m) => write!(f, "database: {m}"),
            SessionError::Crypto(m) => write!(f, "crypto: {m}"),
            SessionError::Refresh(m) => write!(f, "refresh: {m}"),
        }
    }
}

impl std::error::Error for SessionError {}

/// Everything a PDS call needs, decrypted, for the duration of one batch.
/// Never logged, never serialized.
pub struct WriteSession {
    pub pds_url: String,
    pub did: String,
    pub dpop_key: KeyData,
    pub access_token: String,
    pub scope: String,
}

// Manual, redacting `Debug` — not `#[derive(Debug)]`. `Result::unwrap_err`
// requires the `Ok` type to be `Debug` even though only the `Err` value is
// used, and tests call it; the token/key fields must never reach a log or
// test-failure message verbatim.
impl std::fmt::Debug for WriteSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteSession")
            .field("pds_url", &self.pds_url)
            .field("did", &self.did)
            .field("scope", &self.scope)
            .field("dpop_key", &"<redacted>")
            .field("access_token", &"<redacted>")
            .finish()
    }
}

impl WriteSession {
    pub fn pds_client(&self, http: reqwest::Client) -> PdsClient {
        PdsClient::new(
            http,
            self.pds_url.clone(),
            self.did.clone(),
            self.dpop_key.clone(),
            self.access_token.clone(),
        )
    }
}

/// Non-secret view for `GET /api/actions/status`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionStatus {
    pub scope: String,
    pub pds_url: String,
    pub connected_at: String,
}

pub struct SessionStore {
    pub(crate) crypto: TokenCrypto,
    /// Per-DID refresh serialization. AT Protocol refresh tokens are
    /// single-use; two concurrent refreshes for one user must never both run.
    pub(crate) locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

enum RefreshError {
    /// The authorization server rejected the refresh token outright.
    InvalidGrant,
    Other(String),
}

impl SessionStore {
    /// `None` (feature disabled) when the key is missing or malformed. Logs a
    /// warning either way — an operator must be able to see why the buttons
    /// are gone. Never panics.
    pub fn from_config(config: &Config) -> Option<Self> {
        let Some(hex) = config.token_key.as_deref() else {
            warn!("CHARCOAL_TOKEN_KEY not set — mute/block actions are disabled");
            return None;
        };
        match TokenCrypto::from_hex(hex) {
            Ok(crypto) => Some(Self {
                crypto,
                locks: Mutex::new(HashMap::new()),
            }),
            Err(e) => {
                warn!("CHARCOAL_TOKEN_KEY rejected ({e}) — mute/block actions are disabled");
                None
            }
        }
    }

    /// Persist a freshly granted token pair + the DPoP key it is bound to.
    /// Replaces any existing session for the DID (re-consent).
    pub async fn store(
        &self,
        db: &dyn Database,
        did: &str,
        pds_url: &str,
        dpop_key: &KeyData,
        tokens: &TokenResponse,
    ) -> Result<(), SessionError> {
        let refresh = tokens
            .refresh_token
            .as_deref()
            .ok_or_else(|| SessionError::Refresh("token response had no refresh_token".into()))?;
        let now = chrono::Utc::now();
        let row = OauthSessionRow {
            user_did: did.to_string(),
            pds_url: pds_url.to_string(),
            scope: tokens.scope.clone(),
            access_token_enc: self
                .crypto
                .encrypt("access_token", tokens.access_token.as_bytes()),
            refresh_token_enc: self.crypto.encrypt("refresh_token", refresh.as_bytes()),
            dpop_key_enc: self
                .crypto
                .encrypt("dpop_key", dpop_key.to_string().as_bytes()),
            access_expires_at: now.timestamp() + i64::from(tokens.expires_in),
            created_at: now.to_rfc3339(),
            updated_at: now.to_rfc3339(),
        };
        db.upsert_oauth_session(&row)
            .await
            .map_err(|e| SessionError::Db(e.to_string()))?;
        info!(did, "stored OAuth write session");
        Ok(())
    }

    pub async fn status(
        &self,
        db: &dyn Database,
        did: &str,
    ) -> Result<Option<SessionStatus>, SessionError> {
        let row = db
            .get_oauth_session(did)
            .await
            .map_err(|e| SessionError::Db(e.to_string()))?;
        Ok(row.filter(usable).map(|r| SessionStatus {
            scope: r.scope,
            pds_url: r.pds_url,
            connected_at: r.created_at,
        }))
    }

    /// Decrypt the session, refreshing first if the access token is within
    /// `REFRESH_THRESHOLD_SECS` of expiry. Serialized per DID.
    pub async fn load_for_write(
        &self,
        db: &dyn Database,
        http: &reqwest::Client,
        oauth_client: &OAuthClient,
        did: &str,
    ) -> Result<WriteSession, SessionError> {
        let row = db
            .get_oauth_session(did)
            .await
            .map_err(|e| SessionError::Db(e.to_string()))?
            .filter(usable)
            .ok_or(SessionError::NotConnected)?;
        if !needs_refresh(&row) {
            return self.decrypt(&row);
        }

        // Slow path. Take the per-DID lock, then RE-READ: a sibling may have
        // refreshed while we waited, in which case its row is fresh and we
        // must not spend the (now-stale) refresh token we read above.
        let lock = self.lock_for(did).await;
        let _guard = lock.lock().await;
        let row = db
            .get_oauth_session(did)
            .await
            .map_err(|e| SessionError::Db(e.to_string()))?
            .filter(usable)
            .ok_or(SessionError::NotConnected)?;
        if !needs_refresh(&row) {
            return self.decrypt(&row);
        }

        let current = self.decrypt(&row)?;
        let refresh_token = self.decrypt_field(&row.refresh_token_enc, "refresh_token")?;
        match refresh(http, oauth_client, &current, &refresh_token).await {
            Ok(fresh) => {
                // The token endpoint reports the grant the rotated pair
                // carries (RFC 6749 §5.1: omitted only when unchanged). A
                // grant that no longer covers the writes is treated like a
                // revoked refresh token — forget the session, so the person
                // is re-consented rather than 403'd on the next proxied call.
                let scope = if fresh.scope.is_empty() {
                    row.scope.clone()
                } else {
                    fresh.scope.clone()
                };
                if !scope_grants_write(&scope) {
                    warn!(
                        did,
                        "refresh narrowed the grant — deleting OAuth write session"
                    );
                    db.delete_oauth_session(did)
                        .await
                        .map_err(|e| SessionError::Db(e.to_string()))?;
                    return Err(SessionError::NotConnected);
                }
                let new_refresh = fresh.refresh_token.as_deref().unwrap_or(&refresh_token);
                let new_updated_at = chrono::Utc::now().to_rfc3339();
                let expires_at = chrono::Utc::now().timestamp() + i64::from(fresh.expires_in);
                let swapped = db
                    .update_oauth_tokens(
                        did,
                        &self
                            .crypto
                            .encrypt("access_token", fresh.access_token.as_bytes()),
                        &self.crypto.encrypt("refresh_token", new_refresh.as_bytes()),
                        expires_at,
                        &scope,
                        &row.updated_at,
                        &new_updated_at,
                    )
                    .await
                    .map_err(|e| SessionError::Db(e.to_string()))?;
                if swapped {
                    return Ok(WriteSession {
                        access_token: fresh.access_token,
                        scope,
                        ..current
                    });
                }
                // CAS miss: another process (a second replica) won. Its row
                // is newer and valid; use it.
                warn!(did, "refresh CAS miss — another writer refreshed first");
                self.reload_or_disconnect(db, did, &row.updated_at).await
            }
            Err(RefreshError::InvalidGrant) => {
                // Either the user revoked us, or the token was already spent
                // by another process. Re-read once to tell those apart.
                self.reload_or_disconnect(db, did, &row.updated_at).await
            }
            Err(RefreshError::Other(m)) => Err(SessionError::Refresh(m)),
        }
    }

    /// After a lost refresh race: if the row changed under us, the other
    /// writer's tokens are good — return them, provided the grant they
    /// carry still covers the writes (a stale replica could have written an
    /// older one). If it did not change, the refresh token is genuinely
    /// dead: forget the session (spec §3.7).
    async fn reload_or_disconnect(
        &self,
        db: &dyn Database,
        did: &str,
        seen_updated_at: &str,
    ) -> Result<WriteSession, SessionError> {
        let row = db
            .get_oauth_session(did)
            .await
            .map_err(|e| SessionError::Db(e.to_string()))?;
        match row {
            Some(r) if r.updated_at != seen_updated_at && usable(&r) => self.decrypt(&r),
            Some(r) if r.updated_at != seen_updated_at => {
                warn!(did, "replacement OAuth row has an insufficient grant");
                Err(SessionError::NotConnected)
            }
            Some(_) => {
                warn!(did, "refresh token rejected — deleting OAuth write session");
                db.delete_oauth_session(did)
                    .await
                    .map_err(|e| SessionError::Db(e.to_string()))?;
                Err(SessionError::NotConnected)
            }
            None => Err(SessionError::NotConnected),
        }
    }

    /// Revoke the refresh token at the authorization server (best effort —
    /// the server may not expose a revocation endpoint) and delete the row.
    /// `Ok(true)` when a row existed.
    pub async fn disconnect(
        &self,
        db: &dyn Database,
        http: &reqwest::Client,
        oauth_client: &OAuthClient,
        did: &str,
    ) -> Result<bool, SessionError> {
        let Some(row) = db
            .get_oauth_session(did)
            .await
            .map_err(|e| SessionError::Db(e.to_string()))?
        else {
            return Ok(false);
        };
        if let Ok(refresh_token) = self.decrypt_field(&row.refresh_token_enc, "refresh_token") {
            if let Err(e) = revoke(http, oauth_client, &row.pds_url, &refresh_token).await {
                warn!(
                    did,
                    "token revocation failed (continuing with local delete): {e}"
                );
            }
        }
        let deleted = db
            .delete_oauth_session(did)
            .await
            .map_err(|e| SessionError::Db(e.to_string()))?;
        info!(did, "disconnected OAuth write session");
        Ok(deleted)
    }

    async fn lock_for(&self, did: &str) -> Arc<Mutex<()>> {
        let mut map = self.locks.lock().await;
        map.entry(did.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn decrypt(&self, row: &OauthSessionRow) -> Result<WriteSession, SessionError> {
        let access_token = self.decrypt_field(&row.access_token_enc, "access_token")?;
        let key_text = self.decrypt_field(&row.dpop_key_enc, "dpop_key")?;
        let dpop_key =
            identify_key(&key_text).map_err(|e| SessionError::Crypto(format!("dpop key: {e}")))?;
        Ok(WriteSession {
            pds_url: row.pds_url.clone(),
            did: row.user_did.clone(),
            dpop_key,
            access_token,
            scope: row.scope.clone(),
        })
    }

    fn decrypt_field(&self, blob: &[u8], column: &str) -> Result<String, SessionError> {
        let bytes = self
            .crypto
            .decrypt(column, blob)
            .map_err(|e| SessionError::Crypto(e.to_string()))?;
        String::from_utf8(bytes).map_err(|_| SessionError::Crypto(format!("{column}: not utf-8")))
    }
}

/// A stored row counts as a connection only while its grant still covers
/// everything the runner does. Consent checked `scope_grants_write` when the
/// row was written, so this only bites when `write_scope()` has since grown —
/// #322 added the two reconcile reads — and the row predates it. Reading such
/// a row as "not connected" sends the person through consent once more; the
/// alternative was a 403 on the first proxied read of every batch.
fn usable(row: &OauthSessionRow) -> bool {
    scope_grants_write(&row.scope)
}

fn needs_refresh(row: &OauthSessionRow) -> bool {
    row.access_expires_at - chrono::Utc::now().timestamp() < REFRESH_THRESHOLD_SECS
}

/// The `private_key_jwt` client assertion every token-endpoint call carries.
fn client_assertion(
    oauth_client: &OAuthClient,
    authorization_server: &AuthorizationServer,
) -> Result<String, RefreshError> {
    let header: Header = oauth_client
        .private_signing_key_data
        .clone()
        .try_into()
        .map_err(|e| RefreshError::Other(format!("assertion header: {e}")))?;
    let now = chrono::Utc::now().timestamp() as u64;
    let claims = Claims::new(JoseClaims {
        issuer: Some(oauth_client.client_id.clone()),
        subject: Some(oauth_client.client_id.clone()),
        audience: Some(authorization_server.issuer.clone()),
        json_web_token_id: Some(Alphanumeric.sample_string(&mut rand::rng(), 30)),
        issued_at: Some(now),
        expiration: Some(now + 60),
        ..Default::default()
    });
    mint(&oauth_client.private_signing_key_data, &header, &claims)
        .map_err(|e| RefreshError::Other(format!("mint assertion: {e}")))
}

async fn discover(
    http: &reqwest::Client,
    pds_url: &str,
) -> Result<AuthorizationServer, RefreshError> {
    let (_, authorization_server) = pds_resources(http, pds_url)
        .await
        .map_err(|e| RefreshError::Other(format!("discover authorization server: {e}")))?;
    Ok(authorization_server)
}

async fn refresh(
    http: &reqwest::Client,
    oauth_client: &OAuthClient,
    session: &WriteSession,
    refresh_token: &str,
) -> Result<TokenResponse, RefreshError> {
    let authz = discover(http, &session.pds_url).await?;
    let assertion = client_assertion(oauth_client, &authz)?;
    let form = [
        ("client_id", oauth_client.client_id.as_str()),
        ("redirect_uri", oauth_client.redirect_uri.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        (
            "client_assertion_type",
            "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
        ),
        ("client_assertion", assertion.as_str()),
    ];
    let resp = send_dpop(
        http,
        &session.dpop_key,
        "POST",
        &authz.token_endpoint,
        None,
        |r| r.form(&form),
    )
    .await
    .map_err(|e| RefreshError::Other(e.to_string()))?;

    if resp.status.is_success() {
        return serde_json::from_str::<TokenResponse>(&resp.body)
            .map_err(|e| RefreshError::Other(format!("token response: {e}")));
    }
    let error = serde_json::from_str::<serde_json::Value>(&resp.body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_owned))
        .unwrap_or_default();
    if resp.status.as_u16() == 400 && error == "invalid_grant" {
        return Err(RefreshError::InvalidGrant);
    }
    // Body may carry error_description; never the token. Status + error code
    // is all we log.
    Err(RefreshError::Other(format!(
        "token endpoint {} {}",
        resp.status.as_u16(),
        error
    )))
}

/// RFC 7009 revocation. `pds_resources` does not surface
/// `revocation_endpoint`, so read the raw metadata document once.
async fn revoke(
    http: &reqwest::Client,
    oauth_client: &OAuthClient,
    pds_url: &str,
    refresh_token: &str,
) -> anyhow::Result<()> {
    let authz = discover(http, pds_url).await.map_err(|e| {
        anyhow::anyhow!(match e {
            RefreshError::Other(m) => m,
            RefreshError::InvalidGrant => "invalid_grant".to_string(),
        })
    })?;
    let meta_url = format!(
        "{}/.well-known/oauth-authorization-server",
        authz.issuer.trim_end_matches('/')
    );
    let meta: serde_json::Value = http
        .get(&meta_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let Some(endpoint) = meta["revocation_endpoint"].as_str() else {
        anyhow::bail!("authorization server publishes no revocation_endpoint");
    };
    let assertion = client_assertion(oauth_client, &authz).map_err(|e| match e {
        RefreshError::Other(m) => anyhow::anyhow!(m),
        RefreshError::InvalidGrant => anyhow::anyhow!("invalid_grant"),
    })?;
    let form = [
        ("token", refresh_token),
        ("token_type_hint", "refresh_token"),
        ("client_id", oauth_client.client_id.as_str()),
        (
            "client_assertion_type",
            "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
        ),
        ("client_assertion", assertion.as_str()),
    ];
    http.post(endpoint)
        .form(&form)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}
