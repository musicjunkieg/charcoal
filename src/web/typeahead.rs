// Handle typeahead for the login screen (#227).
//
// The login screen is pre-auth, so the endpoint backing this must be public.
// A public endpoint that makes an outbound request per keystroke is an open
// proxy unless the query is validated and the caller is rate-limited — both
// live here, split from the handler so the policy is unit-testable without
// standing up a server.
//
// Upstream speaks the AT Protocol lexicon (`app.bsky.actor.searchActorsTypeahead`)
// and returns `{actors: [{did, handle, displayName, avatar}]}`. The default host
// is configurable precisely because that lexicon is not proprietary: pointing
// CHARCOAL_TYPEAHEAD_URL at https://public.api.bsky.app is a working failover
// with no code change, which matters for a single-instance dependency.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

/// Shortest query worth sending upstream. A single character matches most of
/// the network: expensive for upstream, useless to the user, and the obvious
/// way to make us do pointless work.
const MIN_QUERY_CHARS: usize = 2;

/// Longest query we will forward. A handle is a DNS name, so 253 characters is
/// the real ceiling; anything longer is not a handle and is not our problem.
const MAX_QUERY_CHARS: usize = 253;

/// Validate and normalise a typeahead query.
///
/// Returns `None` when the query should not be sent upstream at all. Counting
/// `chars()` rather than bytes keeps this correct for non-ASCII handles.
pub fn normalize_query(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let len = trimmed.chars().count();
    if !(MIN_QUERY_CHARS..=MAX_QUERY_CHARS).contains(&len) {
        return None;
    }
    Some(trimmed.to_string())
}

/// Outcome of a rate-limit check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateDecision {
    Allow,
    TooMany,
}

#[derive(Debug, Clone, Copy)]
struct Window {
    started: Instant,
    count: u32,
}

/// Fixed-window rate limiter that REJECTS rather than waits.
///
/// Deliberately not `toxicity::rate_limiter::RateLimiter`: that one sleeps
/// until a token frees up, which is right for throttling our own outbound
/// calls and wrong here — making an unauthenticated caller wait means holding
/// a connection open for them, which is the thing an attacker wants.
///
/// `max_keys` bounds the map: without it, a caller rotating source addresses
/// turns the rate limiter itself into a memory-exhaustion vector.
pub struct TypeaheadLimiter {
    windows: Mutex<HashMap<String, Window>>,
    max_per_window: u32,
    window: Duration,
    max_keys: usize,
}

impl TypeaheadLimiter {
    pub fn new(max_per_window: u32, window: Duration, max_keys: usize) -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            max_per_window,
            window,
            max_keys,
        }
    }

    /// Record a request from `key` and decide whether to serve it.
    ///
    /// `now` is injected so the caller controls the clock and tests need no
    /// sleeping.
    pub fn check(&self, key: &str, now: Instant) -> RateDecision {
        let mut windows = match self.windows.lock() {
            Ok(guard) => guard,
            // A poisoned lock means another thread panicked mid-update. Failing
            // open on a rate limiter is worse than failing closed, but taking
            // the whole login screen down over it is worse still — recover the
            // guard and carry on with possibly-stale counts.
            Err(poisoned) => poisoned.into_inner(),
        };

        match windows.get_mut(key) {
            Some(w) if now.duration_since(w.started) < self.window => {
                if w.count >= self.max_per_window {
                    return RateDecision::TooMany;
                }
                w.count += 1;
                RateDecision::Allow
            }
            // Absent, or present but its window has expired — start a fresh one.
            _ => {
                Self::evict_if_needed(&mut windows, self.max_keys, self.window, now);
                windows.insert(
                    key.to_string(),
                    Window {
                        started: now,
                        count: 1,
                    },
                );
                RateDecision::Allow
            }
        }
    }

    /// Number of callers currently tracked. Exposed for tests asserting the
    /// memory bound holds.
    pub fn tracked_keys(&self) -> usize {
        match self.windows.lock() {
            Ok(guard) => guard.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    /// Keep the map under `max_keys`: drop expired windows first, then the
    /// oldest surviving ones if that was not enough.
    fn evict_if_needed(
        windows: &mut HashMap<String, Window>,
        max_keys: usize,
        window: Duration,
        now: Instant,
    ) {
        if windows.len() < max_keys {
            return;
        }
        windows.retain(|_, w| now.duration_since(w.started) < window);

        while windows.len() >= max_keys {
            // All entries can share a timestamp under synthetic load, so this
            // is "an oldest", not "the oldest" — sufficient to hold the bound.
            let Some(oldest) = windows
                .iter()
                .min_by_key(|(_, w)| w.started)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            windows.remove(&oldest);
        }
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    /// Whitespace that `str::trim` strips, including non-ASCII spaces a
    /// pasted handle can carry (no-break space, ideographic space, em space).
    fn ws() -> impl Strategy<Value = char> {
        prop_oneof![
            Just(' '),
            Just('\t'),
            Just('\n'),
            Just('\u{a0}'),
            Just('\u{2003}'),
            Just('\u{3000}'),
        ]
    }

    /// Queries concentrated where bugs live: a body whose length sits on or
    /// next to MIN_QUERY_CHARS and MAX_QUERY_CHARS (or anywhere in between),
    /// built from multi-byte characters, wrapped in whitespace padding.
    fn boundary_query() -> impl Strategy<Value = String> {
        let body_len = prop_oneof![
            0..=MIN_QUERY_CHARS + 1,
            MAX_QUERY_CHARS - 1..=MAX_QUERY_CHARS + 2,
            0..=MAX_QUERY_CHARS + 20,
        ];
        let body_char = prop_oneof![Just('a'), Just('.'), Just('é'), Just('漢'), Just('🦀')];
        (
            prop::collection::vec(ws(), 0..4),
            body_len.prop_flat_map(move |n| prop::collection::vec(body_char.clone(), n)),
            prop::collection::vec(ws(), 0..4),
        )
            .prop_map(|(l, body, r)| l.into_iter().chain(body).chain(r).collect())
    }

    /// Any printable text, plus the boundary-focused queries above.
    fn query() -> impl Strategy<Value = String> {
        prop_oneof!["\\PC{0,300}", boundary_query()]
    }

    proptest! {
        // Invariant 1: an accepted query is exactly the input with surrounding
        // whitespace removed, and its length in chars is within
        // MIN_QUERY_CHARS..=MAX_QUERY_CHARS. Nothing else is altered.
        #[test]
        fn accepted_query_is_the_trimmed_input_within_bounds(raw in query()) {
            if let Some(q) = normalize_query(&raw) {
                prop_assert_eq!(q.as_str(), raw.trim());
                let n = q.chars().count();
                prop_assert!((MIN_QUERY_CHARS..=MAX_QUERY_CHARS).contains(&n));
            }
        }

        // Invariant 2: a query is rejected only when its trimmed length is out
        // of bounds, so every in-bounds query reaches upstream.
        #[test]
        fn rejected_only_when_trimmed_length_out_of_bounds(raw in query()) {
            let n = raw.trim().chars().count();
            let in_bounds = (MIN_QUERY_CHARS..=MAX_QUERY_CHARS).contains(&n);
            prop_assert_eq!(normalize_query(&raw).is_some(), in_bounds);
        }

        // Invariant 3: surrounding whitespace (ASCII or not) never changes the
        // outcome, and normalizing an accepted query again is a no-op.
        #[test]
        fn padding_is_irrelevant_and_normalizing_is_idempotent(
            core in query(),
            pad_l in prop::collection::vec(ws(), 0..5),
            pad_r in prop::collection::vec(ws(), 0..5),
        ) {
            let padded: String = pad_l.into_iter().chain(core.chars()).chain(pad_r).collect();
            prop_assert_eq!(normalize_query(&padded), normalize_query(&core));
            if let Some(q) = normalize_query(&core) {
                prop_assert_eq!(normalize_query(&q), Some(q.clone()));
            }
        }

        // Invariant 4: the char count, not the byte count, is what is bounded,
        // and both edges are exact: MIN and MAX are accepted, one below MIN and
        // one above MAX are rejected, whatever the character's byte width.
        #[test]
        fn bounds_are_exact_and_count_chars_not_bytes(c in "[a.é漢🦀]") {
            let ch = c.chars().next().unwrap();
            let of_len = |n: usize| -> String { std::iter::repeat_n(ch, n).collect() };
            prop_assert!(normalize_query(&of_len(MIN_QUERY_CHARS - 1)).is_none());
            prop_assert!(normalize_query(&of_len(MIN_QUERY_CHARS)).is_some());
            prop_assert!(normalize_query(&of_len(MAX_QUERY_CHARS)).is_some());
            prop_assert!(normalize_query(&of_len(MAX_QUERY_CHARS + 1)).is_none());
        }
    }
}
