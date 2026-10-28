//! `charcoal eval-classifiers` — compare classifiers on labelled posts (#404).
//! See docs/superpowers/specs/2026-10-08-404-classifier-eval-design.md.

pub mod cache;
pub mod cost;
pub mod data;
pub mod labelling;
pub mod metrics;
pub mod models;
pub mod report;
pub mod runner;

/// FNV-1a 64-bit. A fixed, dependency-free hash: used for content identity
/// in the verdict cache and for the stable holdout/sample selections, so the
/// same post lands in the same bucket on every run and every machine.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Identity of a post's exact text. An edited post gets a new hash, so its
/// cached verdicts stop being current.
pub fn content_hash(content: &str) -> String {
    format!("{:016x}", fnv1a(content.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_matches_the_reference_vectors() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn content_hash_changes_when_the_post_changes() {
        assert_eq!(content_hash("x").len(), 16);
        assert_eq!(content_hash("x"), content_hash("x"));
        assert_ne!(content_hash("x"), content_hash("x "));
    }
}
