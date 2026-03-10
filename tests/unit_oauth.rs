// tests/unit_oauth.rs
// Unit tests for DID-aware session tokens and the DID gate check.
//
// These tests drive the changes to src/web/auth.rs.
// They MUST FAIL (compile error) until Task 3 is complete.
//
// Run: cargo test --features web --test unit_oauth

#[cfg(feature = "web")]
mod token_tests {
    use charcoal::web::auth::{create_token, verify_token_did};

    const SECRET: &str = "test_session_secret_at_least_32_bytes!";
    const TEST_DID: &str = "did:plc:h3wpawnrlptr4534chevddo6";

    #[test]
    fn token_with_did_roundtrip() {
        let token = create_token(SECRET, TEST_DID);
        let result = verify_token_did(SECRET, &token);
        assert!(
            result.is_some(),
            "verify_token_did should return Some(did) for a fresh token"
        );
        assert_eq!(result.unwrap(), TEST_DID);
    }

    #[test]
    fn wrong_secret_rejected() {
        let token = create_token(SECRET, TEST_DID);
        let result = verify_token_did("wrong_secret_also_32_bytes_long!!", &token);
        assert!(result.is_none(), "Wrong secret should return None");
    }

    #[test]
    fn tampered_hmac_rejected() {
        let token = create_token(SECRET, TEST_DID);
        // Flip the last byte of the token (the HMAC suffix is hex so it's ASCII)
        let mut bytes = token.into_bytes();
        let last = bytes.len() - 1;
        bytes[last] = if bytes[last] == b'0' { b'1' } else { b'0' };
        let tampered = String::from_utf8(bytes).unwrap();
        assert!(
            verify_token_did(SECRET, &tampered).is_none(),
            "Tampered HMAC should be rejected"
        );
    }

    #[test]
    fn future_dated_token_rejected() {
        // Build a token manually with a future timestamp so checked_sub returns None.
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let future_ts = u64::MAX - 1;
        let did_b64 = URL_SAFE_NO_PAD.encode(TEST_DID.as_bytes());
        let nonce = "deadbeefdeadbeef";
        let payload = format!("{future_ts}.{did_b64}.{nonce}");
        let mut mac = HmacSha256::new_from_slice(SECRET.as_bytes()).unwrap();
        mac.update(payload.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());
        let token = format!("{payload}.{sig}");

        assert!(
            verify_token_did(SECRET, &token).is_none(),
            "Future-dated token should be rejected by checked_sub"
        );
    }

    #[test]
    fn malformed_token_rejected() {
        assert!(verify_token_did(SECRET, "").is_none());
        assert!(verify_token_did(SECRET, "only.three.parts").is_none());
        assert!(verify_token_did(SECRET, "a.b.c.d.e").is_none()); // too many segments
    }
}

#[cfg(feature = "web")]
mod gate_tests {
    use charcoal::web::auth::did_is_allowed;

    const ALLOWED: &str = "did:plc:h3wpawnrlptr4534chevddo6";

    #[test]
    fn allowed_did_passes() {
        assert!(did_is_allowed(ALLOWED, ALLOWED));
    }

    #[test]
    fn disallowed_did_rejected() {
        assert!(!did_is_allowed(
            "did:plc:attacker00000000000000000",
            ALLOWED
        ));
    }

    #[test]
    fn empty_allowed_did_rejects_everything() {
        // If CHARCOAL_ALLOWED_DID is not set, no DID should pass.
        assert!(!did_is_allowed(ALLOWED, ""));
    }

    #[test]
    fn did_comparison_is_exact() {
        // No prefix matching or substring matching.
        let prefix = &ALLOWED[..ALLOWED.len() - 1];
        assert!(!did_is_allowed(prefix, ALLOWED));
    }
}
