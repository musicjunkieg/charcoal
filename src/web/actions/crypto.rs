//! AES-256-GCM encryption for OAuth write-session secrets at rest (#315).
//!
//! Blob layout: `version(1) || nonce(12) || ciphertext || tag(16)`. The
//! column name is bound as associated data, so a ciphertext copied from
//! `access_token_enc` into `refresh_token_enc` fails to decrypt rather than
//! silently becoming a different secret. The version byte is `1`; a future
//! key rotation scheme can introduce `2` without touching stored rows.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, bail, Context, Result};
use rand::RngCore;

const VERSION: u8 = 1;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

pub struct TokenCrypto {
    cipher: Aes256Gcm,
}

impl std::fmt::Debug for TokenCrypto {
    // Never print key material, even by accident through a derived Debug.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenCrypto(..)")
    }
}

impl TokenCrypto {
    /// Build from `CHARCOAL_TOKEN_KEY`: exactly 32 bytes as 64 hex chars.
    pub fn from_hex(key_hex: &str) -> Result<Self> {
        let bytes = hex::decode(key_hex.trim()).context("CHARCOAL_TOKEN_KEY is not valid hex")?;
        if bytes.len() != 32 {
            bail!(
                "CHARCOAL_TOKEN_KEY must be 32 bytes (64 hex chars), got {} bytes",
                bytes.len()
            );
        }
        let cipher = Aes256Gcm::new_from_slice(&bytes).map_err(|e| anyhow!("bad key: {e}"))?;
        Ok(Self { cipher })
    }

    pub fn encrypt(&self, column: &str, plaintext: &[u8]) -> Vec<u8> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = self
            .cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plaintext,
                    aad: column.as_bytes(),
                },
            )
            // AES-GCM encryption only fails on absurd input lengths (>2^36 B).
            .expect("AES-GCM encrypt cannot fail for realistic token sizes");
        let mut out = Vec::with_capacity(1 + NONCE_LEN + ct.len());
        out.push(VERSION);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);
        out
    }

    pub fn decrypt(&self, column: &str, blob: &[u8]) -> Result<Vec<u8>> {
        if blob.len() < 1 + NONCE_LEN + TAG_LEN {
            bail!("encrypted blob too short");
        }
        if blob[0] != VERSION {
            bail!("unsupported encrypted blob version {}", blob[0]);
        }
        let nonce = Nonce::from_slice(&blob[1..1 + NONCE_LEN]);
        self.cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &blob[1 + NONCE_LEN..],
                    aad: column.as_bytes(),
                },
            )
            // The aead error type is deliberately opaque; do not add detail.
            .map_err(|_| anyhow!("decryption failed for column {column}"))
    }
}
