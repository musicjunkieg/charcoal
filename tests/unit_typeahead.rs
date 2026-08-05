//! Tests for the login-screen handle typeahead (#227).
//!
//! Two things need guarding, and both are consequences of WHERE this lives:
//! the login screen is pre-auth, so the endpoint backing it must be public.
//! A public endpoint that makes outbound requests on demand is an open proxy
//! unless the query is validated and the caller is rate-limited.
//!
//! Time is injected rather than slept so the limiter tests are deterministic.
//!
//! Run: cargo test --features web --test unit_typeahead

#[cfg(feature = "web")]
mod typeahead_tests {
    use charcoal::web::typeahead::{normalize_query, RateDecision, TypeaheadLimiter};
    use std::time::Duration;
    use tokio::time::Instant;

    // ── query validation ─────────────────────────────────────────────────────────

    #[test]
    fn accepts_a_normal_partial_handle() {
        assert_eq!(
            normalize_query("chaosgrem").as_deref(),
            Some("chaosgrem"),
            "an ordinary partial handle must pass through"
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(normalize_query("  bryan  ").as_deref(), Some("bryan"));
    }

    #[test]
    fn rejects_empty_and_whitespace_only() {
        // No point spending an upstream request on nothing.
        assert!(normalize_query("").is_none());
        assert!(normalize_query("   ").is_none());
    }

    #[test]
    fn rejects_single_character_queries() {
        // One character matches most of the network — expensive upstream, useless
        // to the user, and the obvious way to make us do pointless work.
        assert!(normalize_query("a").is_none());
    }

    #[test]
    fn caps_absurdly_long_queries() {
        // A handle cannot be 5000 chars. Refuse rather than forward it upstream.
        let long = "a".repeat(5000);
        assert!(
            normalize_query(&long).is_none(),
            "over-long input must be rejected, not proxied"
        );
    }

    #[test]
    fn accepts_a_full_length_realistic_handle() {
        // Must not reject legitimate long-but-valid handles.
        let handle = "some.quite.long.custom.domain.handle.example.com";
        assert_eq!(normalize_query(handle).as_deref(), Some(handle));
    }

    // ── rate limiting ────────────────────────────────────────────────────────────

    #[test]
    fn allows_requests_under_the_limit() {
        let limiter = TypeaheadLimiter::new(3, Duration::from_secs(10), 100);
        let now = Instant::now();

        for i in 0..3 {
            assert_eq!(
                limiter.check("1.2.3.4", now),
                RateDecision::Allow,
                "request {i} should be allowed"
            );
        }
    }

    #[test]
    fn rejects_once_over_the_limit() {
        let limiter = TypeaheadLimiter::new(3, Duration::from_secs(10), 100);
        let now = Instant::now();

        for _ in 0..3 {
            limiter.check("1.2.3.4", now);
        }
        assert_eq!(
            limiter.check("1.2.3.4", now),
            RateDecision::TooMany,
            "the 4th request in the window must be rejected"
        );
    }

    #[test]
    fn the_window_resets() {
        let limiter = TypeaheadLimiter::new(2, Duration::from_secs(10), 100);
        let start = Instant::now();

        limiter.check("1.2.3.4", start);
        limiter.check("1.2.3.4", start);
        assert_eq!(limiter.check("1.2.3.4", start), RateDecision::TooMany);

        // Same caller, a fresh window later — allowed again.
        let later = start + Duration::from_secs(11);
        assert_eq!(limiter.check("1.2.3.4", later), RateDecision::Allow);
    }

    #[test]
    fn callers_are_limited_independently() {
        let limiter = TypeaheadLimiter::new(1, Duration::from_secs(10), 100);
        let now = Instant::now();

        assert_eq!(limiter.check("1.1.1.1", now), RateDecision::Allow);
        assert_eq!(limiter.check("1.1.1.1", now), RateDecision::TooMany);
        // One noisy caller must not lock everyone else out.
        assert_eq!(limiter.check("2.2.2.2", now), RateDecision::Allow);
    }

    #[test]
    fn memory_is_bounded_against_key_rotation() {
        // An attacker rotating source addresses must not be able to grow the
        // limiter's map without bound — that turns a rate limiter into a memory
        // exhaustion vector.
        let max_keys = 50;
        let limiter = TypeaheadLimiter::new(5, Duration::from_secs(60), max_keys);
        let now = Instant::now();

        for i in 0..500 {
            limiter.check(&format!("10.0.0.{i}"), now);
        }

        assert!(
            limiter.tracked_keys() <= max_keys,
            "tracked keys {} exceeded the {max_keys} cap",
            limiter.tracked_keys()
        );
    }

    // NOTE: the URL-redaction test lives beside `redact_upstream_error` in
    // src/web/handlers/typeahead.rs, where it can exercise the private helper
    // that is the module's only sanctioned error-to-string path.
}
