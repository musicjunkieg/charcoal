// Rate limiting for Bluesky API calls with exponential backoff.
//
// Bluesky's rate limit is approximately 3000 requests per 5 minutes.
// This module provides a sliding-window rate limiter that throttles
// requests to stay under the limit, plus a retry wrapper that handles
// 429 (Too Many Requests) responses with exponential backoff and jitter.
//
// The rate limiter is designed to be shared across all concurrent tasks
// via Arc<RateLimiter>, using interior mutability (Mutex) so callers
// only need a &self reference.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use tracing::{info, warn};

/// A sliding-window rate limiter for API calls.
///
/// Tracks request timestamps in a sliding window and pauses when
/// approaching the configured limit. Thread-safe via interior mutability
/// so it can be shared across concurrent tasks with `Arc<RateLimiter>`.
pub struct RateLimiter {
    /// Timestamps of recent requests within the current window.
    requests: Mutex<VecDeque<Instant>>,
    /// Maximum number of requests allowed per window.
    max_requests: u32,
    /// Duration of the sliding window.
    window: Duration,
    /// Minimum delay between consecutive requests to avoid bursts.
    min_delay: Duration,
    /// Timestamp of the last request (for enforcing min_delay).
    last_request: Mutex<Option<Instant>>,
}

impl RateLimiter {
    /// Create a new rate limiter.
    ///
    /// - `max_requests_per_window`: how many requests are allowed in the window
    /// - `window_seconds`: the sliding window duration in seconds
    /// - `min_delay_ms`: minimum milliseconds between consecutive requests
    pub fn new(max_requests_per_window: u32, window_seconds: u64, min_delay_ms: u64) -> Self {
        Self {
            requests: Mutex::new(VecDeque::new()),
            max_requests: max_requests_per_window,
            window: Duration::from_secs(window_seconds),
            min_delay: Duration::from_millis(min_delay_ms),
            last_request: Mutex::new(None),
        }
    }

    /// Wait if necessary before making a request.
    ///
    /// This does two things:
    /// 1. Enforces the minimum delay between consecutive requests
    /// 2. If the sliding window is nearly full, sleeps until enough
    ///    old requests expire to make room
    pub async fn acquire(&self) {
        // First, enforce the minimum inter-request delay.
        // Compute the wait duration while holding the lock, then drop
        // the lock before sleeping (to avoid holding a MutexGuard across await).
        let min_delay_wait = {
            let last = self.last_request.lock().unwrap();
            if let Some(last_time) = *last {
                let elapsed = last_time.elapsed();
                if elapsed < self.min_delay {
                    Some(self.min_delay - elapsed)
                } else {
                    None
                }
            } else {
                None
            }
        };

        if let Some(wait) = min_delay_wait {
            tokio::time::sleep(wait).await;
        }

        // Then, check the sliding window
        loop {
            // Compute what to do while holding the lock, then drop it
            // before any await points.
            let action = {
                let now = Instant::now();
                let mut requests = self.requests.lock().unwrap();

                // Evict requests that have fallen outside the window
                while let Some(&oldest) = requests.front() {
                    if now.duration_since(oldest) > self.window {
                        requests.pop_front();
                    } else {
                        break;
                    }
                }

                if (requests.len() as u32) < self.max_requests {
                    // We have room — record this request and proceed
                    requests.push_back(now);
                    // Also update last_request timestamp
                    let mut last = self.last_request.lock().unwrap();
                    *last = Some(now);
                    None // No wait needed
                } else {
                    // Window is full — calculate how long until the oldest request expires
                    let oldest = *requests.front().unwrap();
                    let wait_until = oldest + self.window;
                    let wait = wait_until.duration_since(now);
                    Some(wait)
                }
            }; // Lock is dropped here

            match action {
                None => return, // Acquired successfully
                Some(wait) => {
                    info!(
                        delay_ms = wait.as_millis() as u64,
                        "Rate limit: waiting {}ms before next request",
                        wait.as_millis()
                    );
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }

    /// Record that a request was made (for cases where acquire() wasn't called,
    /// e.g. when a retry succeeds after backoff).
    pub fn record_request(&self) {
        let now = Instant::now();
        let mut requests = self.requests.lock().unwrap();
        requests.push_back(now);

        let mut last = self.last_request.lock().unwrap();
        *last = Some(now);
    }
}

/// Maximum number of retry attempts on rate-limit (429) errors.
const MAX_RETRIES: u32 = 5;

/// Base delay for exponential backoff (doubles each retry).
const BASE_BACKOFF: Duration = Duration::from_secs(2);

/// Maximum backoff delay to cap exponential growth.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Check whether an error is a rate-limit (HTTP 429) error.
///
/// The bsky-sdk wraps HTTP errors in its own error types, so we check
/// the error chain's Debug representation for "429" or "rate limit".
fn is_rate_limit_error(err: &anyhow::Error) -> bool {
    let debug_str = format!("{:?}", err);
    debug_str.contains("429")
        || debug_str.to_lowercase().contains("rate limit")
        || debug_str.to_lowercase().contains("ratelimit")
}

/// Retry an async operation with exponential backoff on rate-limit errors.
///
/// If the operation fails with a 429-like error, it will be retried up to
/// `MAX_RETRIES` times with exponentially increasing delays (plus jitter
/// to avoid thundering herd). Non-rate-limit errors are returned immediately.
///
/// The rate limiter's `acquire()` is called before each attempt to respect
/// the sliding window even during retries.
pub async fn with_retry<F, Fut, T>(rate_limiter: &RateLimiter, operation: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut attempt = 0u32;

    loop {
        rate_limiter.acquire().await;

        match operation().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if !is_rate_limit_error(&err) || attempt >= MAX_RETRIES {
                    return Err(err);
                }

                attempt += 1;

                // Exponential backoff: base * 2^attempt, capped at MAX_BACKOFF
                let backoff = BASE_BACKOFF
                    .saturating_mul(1u32 << attempt)
                    .min(MAX_BACKOFF);

                // Add jitter: +/- 25% of the backoff to avoid thundering herd.
                // Using a simple deterministic-ish jitter based on the attempt
                // number and current time, since we don't want to add `rand`
                // just for this. The nanosecond component of the current time
                // provides enough variation.
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos();
                let jitter_factor = 0.75 + (nanos % 500) as f64 / 1000.0; // 0.75 to 1.25
                let jittered = Duration::from_secs_f64(backoff.as_secs_f64() * jitter_factor);

                warn!(
                    attempt = attempt,
                    max_retries = MAX_RETRIES,
                    backoff_secs = jittered.as_secs_f64(),
                    "Rate limited (429), retrying in {:.1}s (attempt {}/{})",
                    jittered.as_secs_f64(),
                    attempt,
                    MAX_RETRIES,
                );

                tokio::time::sleep(jittered).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn test_rate_limiter_allows_requests_under_limit() {
        let limiter = RateLimiter::new(10, 60, 0);

        // Should allow 10 requests without blocking
        for _ in 0..10 {
            limiter.acquire().await;
        }
    }

    #[tokio::test]
    async fn test_min_delay_enforced() {
        let limiter = RateLimiter::new(1000, 60, 50);

        let start = Instant::now();
        limiter.acquire().await;
        limiter.acquire().await;
        let elapsed = start.elapsed();

        // Second acquire should have waited at least 50ms
        assert!(
            elapsed >= Duration::from_millis(50),
            "Expected at least 50ms delay, got {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_with_retry_succeeds_immediately() {
        let limiter = RateLimiter::new(100, 60, 0);
        let call_count = AtomicU32::new(0);

        let result = with_retry(&limiter, || {
            call_count.fetch_add(1, Ordering::SeqCst);
            async { Ok::<_, anyhow::Error>(42) }
        })
        .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_with_retry_passes_through_non_rate_limit_errors() {
        let limiter = RateLimiter::new(100, 60, 0);

        let result: Result<i32> = with_retry(&limiter, || async {
            Err(anyhow::anyhow!("some other error"))
        })
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("some other error"));
    }

    #[tokio::test]
    async fn test_is_rate_limit_error_detection() {
        assert!(is_rate_limit_error(&anyhow::anyhow!(
            "HTTP 429 Too Many Requests"
        )));
        assert!(is_rate_limit_error(&anyhow::anyhow!("rate limit exceeded")));
        assert!(!is_rate_limit_error(&anyhow::anyhow!("connection refused")));
    }
}
