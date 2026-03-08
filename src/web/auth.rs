// Auth middleware — stateless HMAC-SHA256 session cookie validation.
//
// Session token format: {timestamp_secs}.{nonce_hex}.{hmac_hex}
//
// The HMAC covers "{timestamp_secs}.{nonce_hex}" signed with CHARCOAL_SESSION_SECRET.
// Tokens are valid for SESSION_TTL_SECS (24 hours).
//
// Login flow:
//   POST /api/login { password } → check CHARCOAL_WEB_PASSWORD
//     success: set charcoal_session cookie with new HMAC token
//     failure: 401
//
// Auth check (this middleware):
//   extract charcoal_session cookie → parse → verify HMAC → verify age → allow

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;

use super::{AppState, AuthUser};

type HmacSha256 = Hmac<Sha256>;

/// Session cookie name.
pub const COOKIE_NAME: &str = "charcoal_session";

/// Session lifetime: 24 hours.
pub const SESSION_TTL_SECS: u64 = 86_400;

/// Build a new session token with the authenticated DID embedded.
///
/// Token format: `{timestamp}.{did_b64}.{nonce_hex}.{hmac_hex}`
/// The HMAC covers `"{timestamp}.{did_b64}.{nonce_hex}"`.
/// `did_b64` is URL-safe base64 (no padding) of the DID UTF-8 bytes.
pub fn create_token(secret: &str, did: &str) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let did_b64 = URL_SAFE_NO_PAD.encode(did.as_bytes());

    let mut nonce_bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = hex::encode(nonce_bytes);

    let payload = format!("{timestamp}.{did_b64}.{nonce}");
    let sig = hmac_sign(secret, &payload);

    format!("{payload}.{sig}")
}

/// Verify a session token and extract the embedded DID.
///
/// Returns `Some(did)` if:
/// - The token has exactly 4 `.`-separated segments
/// - The HMAC is valid (constant-time check)
/// - The token age is within SESSION_TTL_SECS (using checked_sub to reject future-dated tokens)
/// - The did_b64 segment decodes to valid UTF-8
///
/// Returns `None` otherwise.
pub fn verify_token_did(secret: &str, token: &str) -> Option<String> {
    // Format: {timestamp}.{did_b64}.{nonce}.{hmac}
    let parts: Vec<&str> = token.splitn(5, '.').collect();
    if parts.len() != 4 {
        return None;
    }
    let timestamp_str = parts[0];
    let did_b64 = parts[1];
    let nonce = parts[2];
    let provided_sig = parts[3];

    // Verify HMAC first (covers timestamp + did_b64 + nonce)
    let payload = format!("{timestamp_str}.{did_b64}.{nonce}");
    let expected_sig = hmac_sign(secret, &payload);
    if !constant_time_eq(provided_sig, &expected_sig) {
        return None;
    }

    // Verify age — checked_sub rejects future-dated tokens
    let timestamp = timestamp_str.parse::<u64>().ok()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let age = now.checked_sub(timestamp)?;
    if age >= SESSION_TTL_SECS {
        return None;
    }

    // Decode DID
    let did_bytes = URL_SAFE_NO_PAD.decode(did_b64).ok()?;
    String::from_utf8(did_bytes).ok()
}

/// Verify a session token (ignores the embedded DID).
/// Kept for backward compatibility with existing tests.
/// New code should use verify_token_did.
pub fn verify_token(secret: &str, token: &str) -> bool {
    verify_token_did(secret, token).is_some()
}

/// Check whether a DID is allowed to authenticate.
///
/// Returns `false` if `allowed_did` is empty (CHARCOAL_ALLOWED_DID not configured).
/// Uses constant-time comparison to avoid timing oracle on the DID.
pub fn did_is_allowed(did: &str, allowed_did: &str) -> bool {
    !allowed_did.is_empty() && constant_time_eq(did, allowed_did)
}

/// Axum middleware: reject requests without a valid session cookie.
///
/// Returns 401 if no valid session, 403 if the DID is not allowed.
/// On success, inserts `AuthUser { did }` into request extensions.
pub async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let session_secret = &state.config.session_secret;
    let allowed_did = &state.config.allowed_did;

    match extract_session_did(&request, session_secret) {
        None => super::api_error(
            axum::http::StatusCode::UNAUTHORIZED,
            "Authentication required",
        ),
        Some(did) if !did_is_allowed(&did, allowed_did) => {
            super::api_error(axum::http::StatusCode::FORBIDDEN, "Access denied")
        }
        Some(did) => {
            request.extensions_mut().insert(AuthUser { did });
            next.run(request).await
        }
    }
}

/// Build the `Set-Cookie` header value for a new session.
pub fn set_cookie_header(token: &str, secure: bool) -> String {
    let secure_flag = if secure { "; Secure" } else { "" };
    format!(
        "{COOKIE_NAME}={token}; HttpOnly{secure_flag}; SameSite=Strict; Path=/; Max-Age={SESSION_TTL_SECS}"
    )
}

/// Build the `Set-Cookie` header value that clears the session cookie.
pub fn clear_cookie_header() -> String {
    format!("{COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0")
}

// --- Private helpers ---

fn hmac_sign(secret: &str, payload: &str) -> String {
    // new_from_slice only fails for zero-length keys; the session secret is
    // validated to be non-empty at server startup, so this is always safe.
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC-SHA256 accepts any non-empty key");
    mac.update(payload.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Constant-time string comparison to prevent timing attacks.
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Extract the authenticated DID from the session cookie, if valid.
fn extract_session_did(request: &Request, session_secret: &str) -> Option<String> {
    let cookie_header = request.headers().get(header::COOKIE)?.to_str().ok()?;
    for pair in cookie_header.split(';') {
        let pair = pair.trim();
        if let Some((name, value)) = pair.split_once('=') {
            if name.trim() == COOKIE_NAME {
                return verify_token_did(session_secret, value.trim());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_DID: &str = "did:plc:test000000000000000000";

    #[test]
    fn test_token_roundtrip() {
        let secret = "test_secret_32_bytes_long_enough!";
        let token = create_token(secret, TEST_DID);
        assert!(verify_token(secret, &token));
    }

    #[test]
    fn test_wrong_secret_rejected() {
        let token = create_token("correct_secret", TEST_DID);
        assert!(!verify_token("wrong_secret", &token));
    }

    #[test]
    fn test_tampered_token_rejected() {
        let secret = "my_secret";
        let token = create_token(secret, TEST_DID);
        // Flip the last byte deterministically — tokens are hex-encoded so always ASCII.
        let mut tampered_bytes = token.clone().into_bytes();
        let last = tampered_bytes.len() - 1;
        tampered_bytes[last] = if tampered_bytes[last] == b'0' {
            b'1'
        } else {
            b'0'
        };
        let tampered = String::from_utf8(tampered_bytes).expect("token is ASCII");
        assert!(!verify_token(secret, &tampered));
    }

    #[test]
    fn test_malformed_token_rejected() {
        assert!(!verify_token("secret", "not.a.valid.token.format"));
        assert!(!verify_token("secret", ""));
        assert!(!verify_token("secret", "onlytwoparts.here"));
    }
}
