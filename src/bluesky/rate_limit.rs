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
    use std::sync::Arc;

    // ── RateLimiter::new ────────────────────────────────────────────

    #[test]
    fn test_new_creates_empty_limiter() {
        let limiter = RateLimiter::new(100, 60, 50);
        assert_eq!(limiter.max_requests, 100);
        assert_eq!(limiter.window, Duration::from_secs(60));
        assert_eq!(limiter.min_delay, Duration::from_millis(50));
        assert!(limiter.requests.lock().unwrap().is_empty());
        assert!(limiter.last_request.lock().unwrap().is_none());
    }

    #[test]
    fn test_new_zero_min_delay() {
        let limiter = RateLimiter::new(10, 60, 0);
        assert_eq!(limiter.min_delay, Duration::ZERO);
    }

    // ── RateLimiter::acquire — under limit ──────────────────────────

    #[tokio::test]
    async fn test_acquire_allows_requests_under_limit() {
        let limiter = RateLimiter::new(10, 60, 0);

        for _ in 0..10 {
            limiter.acquire().await;
        }

        // All 10 should be recorded
        assert_eq!(limiter.requests.lock().unwrap().len(), 10);
    }

    #[tokio::test]
    async fn test_acquire_first_request_is_immediate() {
        let limiter = RateLimiter::new(100, 60, 100);

        // First request should complete quickly even with min_delay set
        // (min_delay only applies between consecutive requests)
        let start = Instant::now();
        limiter.acquire().await;
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(50),
            "First request should be near-instant, got {:?}",
            elapsed
        );
        assert!(limiter.last_request.lock().unwrap().is_some());
    }

    // ── RateLimiter::acquire — min_delay ────────────────────────────

    #[tokio::test]
    async fn test_acquire_min_delay_enforced() {
        let limiter = RateLimiter::new(1000, 60, 50);

        let start = Instant::now();
        limiter.acquire().await;
        limiter.acquire().await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(45),
            "Expected at least ~50ms delay, got {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_acquire_min_delay_accumulates_over_multiple_requests() {
        let limiter = RateLimiter::new(1000, 60, 20);

        let start = Instant::now();
        for _ in 0..5 {
            limiter.acquire().await;
        }
        let elapsed = start.elapsed();

        // 4 inter-request gaps of at least ~20ms each = ~80ms minimum
        assert!(
            elapsed >= Duration::from_millis(70),
            "Expected at least ~80ms for 5 requests with 20ms delay, got {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_acquire_zero_min_delay_allows_rapid_fire() {
        let limiter = RateLimiter::new(100, 60, 0);

        let start = Instant::now();
        for _ in 0..50 {
            limiter.acquire().await;
        }
        let elapsed = start.elapsed();

        // With zero delay, 50 requests should complete very quickly
        assert!(
            elapsed < Duration::from_millis(50),
            "Zero-delay requests should be near-instant, got {:?}",
            elapsed
        );
    }

    // ── RateLimiter::acquire — window saturation & eviction ─────────

    #[tokio::test]
    async fn test_acquire_blocks_when_window_full() {
        // Window: max 3 requests per 100ms
        let limiter = RateLimiter {
            requests: Mutex::new(VecDeque::new()),
            max_requests: 3,
            window: Duration::from_millis(100),
            min_delay: Duration::ZERO,
            last_request: Mutex::new(None),
        };

        let start = Instant::now();

        // Fill the window
        limiter.acquire().await;
        limiter.acquire().await;
        limiter.acquire().await;

        // 4th request should block until the 100ms window expires
        limiter.acquire().await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(90),
            "Expected at least ~100ms wait for window expiry, got {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_acquire_single_slot_window() {
        // Only 1 request per 100ms window
        let limiter = RateLimiter {
            requests: Mutex::new(VecDeque::new()),
            max_requests: 1,
            window: Duration::from_millis(100),
            min_delay: Duration::ZERO,
            last_request: Mutex::new(None),
        };

        let start = Instant::now();
        limiter.acquire().await; // instant
        limiter.acquire().await; // waits ~100ms
        limiter.acquire().await; // waits another ~100ms
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(180),
            "Expected at least ~200ms for 3 requests with 1-slot window, got {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_acquire_window_evicts_old_requests() {
        // 2 requests per 100ms window
        let limiter = RateLimiter {
            requests: Mutex::new(VecDeque::new()),
            max_requests: 2,
            window: Duration::from_millis(100),
            min_delay: Duration::ZERO,
            last_request: Mutex::new(None),
        };

        // Fill window
        limiter.acquire().await;
        limiter.acquire().await;

        // Wait for window to expire
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Should be able to acquire again quickly (old requests evicted)
        let start = Instant::now();
        limiter.acquire().await;
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(50),
            "Should not block after window expires, got {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_acquire_after_long_idle_evicts_all() {
        let limiter = RateLimiter {
            requests: Mutex::new(VecDeque::new()),
            max_requests: 3,
            window: Duration::from_millis(50),
            min_delay: Duration::ZERO,
            last_request: Mutex::new(None),
        };

        // Fill the window completely
        for _ in 0..3 {
            limiter.acquire().await;
        }

        // Wait much longer than the window
        tokio::time::sleep(Duration::from_millis(150)).await;

        // All old requests should be evicted, allowing a full batch again
        let start = Instant::now();
        for _ in 0..3 {
            limiter.acquire().await;
        }
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(50),
            "Should not block after all requests expired, got {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_acquire_updates_last_request() {
        let limiter = RateLimiter::new(100, 60, 0);

        assert!(limiter.last_request.lock().unwrap().is_none());

        limiter.acquire().await;
        let first = limiter.last_request.lock().unwrap().unwrap();

        // Small real sleep to ensure Instant advances
        tokio::time::sleep(Duration::from_millis(5)).await;
        limiter.acquire().await;
        let second = limiter.last_request.lock().unwrap().unwrap();

        assert!(
            second > first,
            "last_request should advance with each acquire"
        );
    }

    // ── RateLimiter::record_request ─────────────────────────────────

    #[tokio::test]
    async fn test_record_request_adds_to_window() {
        let limiter = RateLimiter::new(3, 60, 0);

        limiter.record_request();
        limiter.record_request();
        assert_eq!(limiter.requests.lock().unwrap().len(), 2);

        // One more via acquire fills the window to 3
        limiter.acquire().await;
        assert_eq!(limiter.requests.lock().unwrap().len(), 3);
    }

    #[test]
    fn test_record_request_updates_last_request() {
        let limiter = RateLimiter::new(100, 60, 0);

        assert!(limiter.last_request.lock().unwrap().is_none());
        limiter.record_request();
        assert!(limiter.last_request.lock().unwrap().is_some());
    }

    #[tokio::test]
    async fn test_record_request_affects_min_delay() {
        let limiter = RateLimiter::new(100, 60, 50);

        // Record a request manually
        limiter.record_request();

        // Next acquire should respect min_delay from the recorded request
        let start = Instant::now();
        limiter.acquire().await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(40),
            "Acquire should respect min_delay after record_request, got {:?}",
            elapsed
        );
    }

    #[test]
    fn test_record_request_multiple_calls() {
        let limiter = RateLimiter::new(100, 60, 0);

        for _ in 0..10 {
            limiter.record_request();
        }
        assert_eq!(limiter.requests.lock().unwrap().len(), 10);
    }

    // ── is_rate_limit_error ─────────────────────────────────────────

    #[test]
    fn test_is_rate_limit_error_with_429() {
        assert!(is_rate_limit_error(&anyhow::anyhow!(
            "HTTP 429 Too Many Requests"
        )));
    }

    #[test]
    fn test_is_rate_limit_error_with_rate_limit_text() {
        assert!(is_rate_limit_error(&anyhow::anyhow!("rate limit exceeded")));
    }

    #[test]
    fn test_is_rate_limit_error_with_ratelimit_no_space() {
        assert!(is_rate_limit_error(&anyhow::anyhow!(
            "RateLimit: too many requests"
        )));
    }

    #[test]
    fn test_is_rate_limit_error_mixed_case() {
        assert!(is_rate_limit_error(&anyhow::anyhow!("Rate Limit Exceeded")));
        assert!(is_rate_limit_error(&anyhow::anyhow!("RATE LIMIT")));
        assert!(is_rate_limit_error(&anyhow::anyhow!("RateLimit")));
    }

    #[test]
    fn test_is_rate_limit_error_rejects_unrelated_errors() {
        assert!(!is_rate_limit_error(&anyhow::anyhow!("connection refused")));
        assert!(!is_rate_limit_error(&anyhow::anyhow!("timeout")));
        assert!(!is_rate_limit_error(&anyhow::anyhow!(
            "HTTP 500 Internal Server Error"
        )));
        assert!(!is_rate_limit_error(&anyhow::anyhow!("HTTP 403 Forbidden")));
    }

    #[test]
    fn test_is_rate_limit_error_empty_message() {
        assert!(!is_rate_limit_error(&anyhow::anyhow!("")));
    }

    #[test]
    fn test_is_rate_limit_error_429_embedded_in_context() {
        // Nested error with 429 in the chain — should still detect it
        let inner = anyhow::anyhow!("HTTP 429");
        let outer = inner.context("Failed to fetch followers");
        assert!(is_rate_limit_error(&outer));
    }

    #[test]
    fn test_is_rate_limit_error_429_bare_number() {
        assert!(is_rate_limit_error(&anyhow::anyhow!("status: 429")));
    }

    #[test]
    fn test_is_rate_limit_error_not_fooled_by_similar_codes() {
        // 428 and 430 should not match
        assert!(!is_rate_limit_error(&anyhow::anyhow!("HTTP 428")));
        assert!(!is_rate_limit_error(&anyhow::anyhow!("HTTP 430")));
    }

    // ── with_retry — success cases ──────────────────────────────────
    // Note: with_retry tests use start_paused to skip the exponential
    // backoff sleeps (which use tokio::time::sleep). These tests only
    // check call counts and return values, not elapsed time.

    #[tokio::test(start_paused = true)]
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

    #[tokio::test(start_paused = true)]
    async fn test_with_retry_retries_on_429_then_succeeds() {
        let limiter = RateLimiter::new(100, 60, 0);
        let call_count = AtomicU32::new(0);

        let result = with_retry(&limiter, || {
            let attempt = call_count.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt < 2 {
                    Err(anyhow::anyhow!("HTTP 429 Too Many Requests"))
                } else {
                    Ok(99)
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), 99);
        assert_eq!(call_count.load(Ordering::SeqCst), 3); // 2 failures + 1 success
    }

    #[tokio::test(start_paused = true)]
    async fn test_with_retry_retries_on_rate_limit_text_then_succeeds() {
        let limiter = RateLimiter::new(100, 60, 0);
        let call_count = AtomicU32::new(0);

        let result = with_retry(&limiter, || {
            let attempt = call_count.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt == 0 {
                    Err(anyhow::anyhow!("rate limit exceeded"))
                } else {
                    Ok("ok")
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), "ok");
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn test_with_retry_single_retry_on_ratelimit_no_space() {
        let limiter = RateLimiter::new(100, 60, 0);
        let call_count = AtomicU32::new(0);

        let result = with_retry(&limiter, || {
            let attempt = call_count.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt == 0 {
                    Err(anyhow::anyhow!("RateLimit exceeded"))
                } else {
                    Ok(7)
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), 7);
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }

    // ── with_retry — error cases ────────────────────────────────────

    #[tokio::test(start_paused = true)]
    async fn test_with_retry_passes_through_non_rate_limit_errors() {
        let limiter = RateLimiter::new(100, 60, 0);
        let call_count = AtomicU32::new(0);

        let result: Result<i32> = with_retry(&limiter, || {
            call_count.fetch_add(1, Ordering::SeqCst);
            async { Err(anyhow::anyhow!("connection refused")) }
        })
        .await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("connection refused"));
        // Non-rate-limit errors should NOT be retried
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn test_with_retry_exhausts_retries_on_persistent_429() {
        let limiter = RateLimiter::new(100, 60, 0);
        let call_count = AtomicU32::new(0);

        let result: Result<i32> = with_retry(&limiter, || {
            call_count.fetch_add(1, Ordering::SeqCst);
            async { Err(anyhow::anyhow!("HTTP 429 Too Many Requests")) }
        })
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("429"));
        // 1 initial + MAX_RETRIES (5) = 6 total calls
        assert_eq!(call_count.load(Ordering::SeqCst), 6);
    }

    #[tokio::test(start_paused = true)]
    async fn test_with_retry_succeeds_on_last_attempt() {
        let limiter = RateLimiter::new(100, 60, 0);
        let call_count = AtomicU32::new(0);

        let result = with_retry(&limiter, || {
            let attempt = call_count.fetch_add(1, Ordering::SeqCst);
            async move {
                // Fail with 429 for attempts 0..4, succeed on attempt 5 (the last retry)
                if attempt < 5 {
                    Err(anyhow::anyhow!("HTTP 429"))
                } else {
                    Ok("recovered")
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), "recovered");
        assert_eq!(call_count.load(Ordering::SeqCst), 6);
    }

    #[tokio::test(start_paused = true)]
    async fn test_with_retry_fails_on_6th_429() {
        let limiter = RateLimiter::new(100, 60, 0);
        let call_count = AtomicU32::new(0);

        // Fails on all 6 attempts (initial + 5 retries). The 6th attempt (attempt=5)
        // hits `attempt >= MAX_RETRIES` and returns the error.
        let result: Result<i32> = with_retry(&limiter, || {
            call_count.fetch_add(1, Ordering::SeqCst);
            async { Err(anyhow::anyhow!("429")) }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(call_count.load(Ordering::SeqCst), 6);
    }

    #[tokio::test(start_paused = true)]
    async fn test_with_retry_preserves_original_error_message() {
        let limiter = RateLimiter::new(100, 60, 0);

        let result: Result<i32> = with_retry(&limiter, || async {
            Err(anyhow::anyhow!(
                "HTTP 429 Too Many Requests: slow down buddy"
            ))
        })
        .await;

        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("slow down buddy"),
            "Original error message should be preserved, got: {}",
            err
        );
    }

    // ── with_retry — acquire integration ────────────────────────────

    #[tokio::test(start_paused = true)]
    async fn test_with_retry_calls_acquire_each_attempt() {
        let limiter = RateLimiter::new(100, 60, 0);

        let call_count = AtomicU32::new(0);
        let _ = with_retry(&limiter, || {
            let attempt = call_count.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt < 2 {
                    Err(anyhow::anyhow!("HTTP 429"))
                } else {
                    Ok(())
                }
            }
        })
        .await;

        // 3 attempts = 3 acquire calls = 3 recorded requests in the window
        assert_eq!(limiter.requests.lock().unwrap().len(), 3);
    }

    // ── Concurrency ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_acquire_concurrent_tasks_share_limiter() {
        let limiter = Arc::new(RateLimiter::new(10, 60, 0));
        let mut handles = Vec::new();

        // Spawn 10 tasks that each acquire once
        for _ in 0..10 {
            let lim = Arc::clone(&limiter);
            handles.push(tokio::spawn(async move {
                lim.acquire().await;
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        // All 10 should be recorded in the shared window
        assert_eq!(limiter.requests.lock().unwrap().len(), 10);
    }

    #[tokio::test]
    async fn test_acquire_concurrent_tasks_blocked_by_window() {
        // 3 slots in a 100ms window
        let limiter = Arc::new(RateLimiter {
            requests: Mutex::new(VecDeque::new()),
            max_requests: 3,
            window: Duration::from_millis(100),
            min_delay: Duration::ZERO,
            last_request: Mutex::new(None),
        });
        let completed = Arc::new(AtomicU32::new(0));

        let mut handles = Vec::new();

        // Spawn 6 tasks — 3 should complete immediately, 3 should wait for window
        for _ in 0..6 {
            let lim = Arc::clone(&limiter);
            let done = Arc::clone(&completed);
            handles.push(tokio::spawn(async move {
                lim.acquire().await;
                done.fetch_add(1, Ordering::SeqCst);
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        // All 6 should eventually complete
        assert_eq!(completed.load(Ordering::SeqCst), 6);
    }

    #[tokio::test(start_paused = true)]
    async fn test_with_retry_concurrent_calls() {
        let limiter = Arc::new(RateLimiter::new(100, 60, 0));
        let mut handles = Vec::new();

        for i in 0..5 {
            let lim = Arc::clone(&limiter);
            handles.push(tokio::spawn(async move {
                with_retry(&lim, || {
                    let val = i;
                    async move { Ok::<_, anyhow::Error>(val) }
                })
                .await
                .unwrap()
            }));
        }

        let mut results = Vec::new();
        for h in handles {
            results.push(h.await.unwrap());
        }
        results.sort();
        assert_eq!(results, vec![0, 1, 2, 3, 4]);
    }

    // ── Edge cases ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_acquire_min_delay_and_window_interact() {
        // Both constraints active: 2 requests per 100ms window, 30ms min delay
        let limiter = RateLimiter {
            requests: Mutex::new(VecDeque::new()),
            max_requests: 2,
            window: Duration::from_millis(100),
            min_delay: Duration::from_millis(30),
            last_request: Mutex::new(None),
        };

        let start = Instant::now();
        limiter.acquire().await; // instant
        limiter.acquire().await; // waits ~30ms (min_delay)
                                 // 3rd request: window full (2/2), must wait for window to expire
        limiter.acquire().await;
        let elapsed = start.elapsed();

        // Should have waited at least 100ms for the window to expire
        assert!(
            elapsed >= Duration::from_millis(90),
            "Expected at least ~100ms total, got {:?}",
            elapsed
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_with_retry_returns_correct_value_type() {
        let limiter = RateLimiter::new(100, 60, 0);

        // Test with String return type
        let result: Result<String> =
            with_retry(&limiter, || async { Ok("hello".to_string()) }).await;
        assert_eq!(result.unwrap(), "hello");

        // Test with Vec return type
        let result: Result<Vec<i32>> = with_retry(&limiter, || async { Ok(vec![1, 2, 3]) }).await;
        assert_eq!(result.unwrap(), vec![1, 2, 3]);
    }

    #[tokio::test(start_paused = true)]
    async fn test_with_retry_error_on_first_attempt_non_429_no_retry() {
        let limiter = RateLimiter::new(100, 60, 0);
        let call_count = AtomicU32::new(0);

        // Various non-429 errors should all fail immediately without retry
        for msg in &["timeout", "DNS resolution failed", "HTTP 500", "EOF"] {
            call_count.store(0, Ordering::SeqCst);
            let err_msg = msg.to_string();
            let result: Result<i32> = with_retry(&limiter, || {
                call_count.fetch_add(1, Ordering::SeqCst);
                let m = err_msg.clone();
                async move { Err(anyhow::anyhow!("{}", m)) }
            })
            .await;

            assert!(result.is_err());
            assert_eq!(
                call_count.load(Ordering::SeqCst),
                1,
                "Non-429 error '{}' should not trigger retry",
                msg
            );
        }
    }
}
