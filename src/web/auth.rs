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
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;

use super::{AppState, AuthUser};

type HmacSha256 = Hmac<Sha256>;

/// Session cookie name.
pub const COOKIE_NAME: &str = "charcoal_session";

/// Session lifetime: 24 hours.
pub const SESSION_TTL_SECS: u64 = 86_400;

/// Build a new session token signed with `secret`.
///
/// Returns the raw cookie value (the token string, not the full Set-Cookie header).
pub fn create_token(secret: &str) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut nonce_bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = hex::encode(nonce_bytes);

    let payload = format!("{timestamp}.{nonce}");
    let sig = hmac_sign(secret, &payload);

    format!("{payload}.{sig}")
}

/// Verify a session token. Returns `true` if the HMAC is valid and the token
/// is not older than `SESSION_TTL_SECS`.
pub fn verify_token(secret: &str, token: &str) -> bool {
    // Format: {timestamp}.{nonce}.{hmac}
    let parts: Vec<&str> = token.splitn(3, '.').collect();
    if parts.len() != 3 {
        return false;
    }
    let timestamp_str = parts[0];
    let nonce = parts[1];
    let provided_sig = parts[2];

    // Verify HMAC
    let payload = format!("{timestamp_str}.{nonce}");
    let expected_sig = hmac_sign(secret, &payload);
    if !constant_time_eq(provided_sig, &expected_sig) {
        return false;
    }

    // Verify age
    let Ok(timestamp) = timestamp_str.parse::<u64>() else {
        return false;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now.saturating_sub(timestamp) < SESSION_TTL_SECS
}

/// Axum middleware: reject requests without a valid session cookie with 401.
pub async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let secret = &state.config.web_password; // password doubles as signing context
    let session_secret = &state.config.session_secret;

    if !has_valid_session(&request, session_secret, secret) {
        return super::api_error(
            axum::http::StatusCode::UNAUTHORIZED,
            "Authentication required",
        );
    }

    // Insert AuthUser marker so handlers can extract it if needed
    request.extensions_mut().insert(AuthUser);
    next.run(request).await
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
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .unwrap_or_else(|_| HmacSha256::new_from_slice(b"fallback").unwrap());
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

/// Extract and validate the session cookie from the request.
fn has_valid_session(request: &Request, session_secret: &str, _password: &str) -> bool {
    let cookie_header = match request.headers().get(header::COOKIE) {
        Some(v) => match v.to_str() {
            Ok(s) => s,
            Err(_) => return false,
        },
        None => return false,
    };

    // Parse individual cookie pairs
    for pair in cookie_header.split(';') {
        let pair = pair.trim();
        if let Some((name, value)) = pair.split_once('=') {
            if name.trim() == COOKIE_NAME {
                return verify_token(session_secret, value.trim());
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_roundtrip() {
        let secret = "test_secret_32_bytes_long_enough!";
        let token = create_token(secret);
        assert!(verify_token(secret, &token));
    }

    #[test]
    fn test_wrong_secret_rejected() {
        let token = create_token("correct_secret");
        assert!(!verify_token("wrong_secret", &token));
    }

    #[test]
    fn test_tampered_token_rejected() {
        let secret = "my_secret";
        let token = create_token(secret);
        let tampered = token.replace('a', "b");
        // Only check if token actually changed (if no 'a', test is trivially true)
        if tampered != token {
            assert!(!verify_token(secret, &tampered));
        }
    }

    #[test]
    fn test_malformed_token_rejected() {
        assert!(!verify_token("secret", "not.a.valid.token.format"));
        assert!(!verify_token("secret", ""));
        assert!(!verify_token("secret", "onlytwoparts.here"));
    }
}
