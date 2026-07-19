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
