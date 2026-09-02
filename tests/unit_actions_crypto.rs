//! AES-256-GCM token encryption at rest (#315, spec §3.5).
#![cfg(feature = "web")]

use charcoal::web::actions::crypto::TokenCrypto;

const KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

#[test]
fn round_trip() {
    let c = TokenCrypto::from_hex(KEY).unwrap();
    let blob = c.encrypt("access_token_enc", b"secret-token");
    assert_eq!(blob[0], 1, "first byte is the format version");
    assert_eq!(blob.len(), 1 + 12 + 12 + 16, "version + nonce + ct + tag");
    let back = c.decrypt("access_token_enc", &blob).unwrap();
    assert_eq!(back, b"secret-token");
}

#[test]
fn nonces_differ_per_call() {
    let c = TokenCrypto::from_hex(KEY).unwrap();
    let a = c.encrypt("x", b"same");
    let b = c.encrypt("x", b"same");
    assert_ne!(a, b);
}

#[test]
fn tampered_ciphertext_is_rejected() {
    let c = TokenCrypto::from_hex(KEY).unwrap();
    let mut blob = c.encrypt("x", b"payload");
    let last = blob.len() - 1;
    blob[last] ^= 0x01;
    assert!(c.decrypt("x", &blob).is_err());
}

#[test]
fn wrong_column_is_rejected() {
    let c = TokenCrypto::from_hex(KEY).unwrap();
    let blob = c.encrypt("access_token_enc", b"payload");
    assert!(
        c.decrypt("refresh_token_enc", &blob).is_err(),
        "column name is bound as AAD; a blob must not be movable between columns"
    );
}

#[test]
fn unknown_version_and_short_blobs_are_rejected() {
    let c = TokenCrypto::from_hex(KEY).unwrap();
    let mut blob = c.encrypt("x", b"payload");
    blob[0] = 2;
    assert!(c.decrypt("x", &blob).is_err());
    assert!(c.decrypt("x", &[1, 2, 3]).is_err());
    assert!(c.decrypt("x", &[]).is_err());
}

#[test]
fn bad_keys_are_rejected() {
    assert!(TokenCrypto::from_hex("").is_err());
    assert!(TokenCrypto::from_hex("abcd").is_err());
    assert!(TokenCrypto::from_hex(&"zz".repeat(32)).is_err());
    assert!(TokenCrypto::from_hex(&"00".repeat(31)).is_err());
}
