// Token-bucket rate limiter for API calls.
//
// Perspective API's free tier allows 1 QPS (query per second). This rate
// limiter enforces that limit to avoid getting throttled. It uses a simple
// token-bucket approach: one token is added per second, and each request
// consumes one token. If no tokens are available, we sleep until one is.

use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};

/// A simple rate limiter that enforces a maximum request rate.
#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<RateLimiterInner>>,
}

struct RateLimiterInner {
    /// Minimum time between requests
    interval: Duration,
    /// When the last request was allowed through
    last_request: Option<Instant>,
}

impl RateLimiter {
    /// Create a new rate limiter that allows `requests_per_second` requests per second.
    pub fn new(requests_per_second: f64) -> Self {
        let interval = Duration::from_secs_f64(1.0 / requests_per_second);
        Self {
            inner: Arc::new(Mutex::new(RateLimiterInner {
                interval,
                last_request: None,
            })),
        }
    }

    /// Wait until a request is allowed, then return.
    ///
    /// If we're within the rate limit, this returns immediately.
    /// If we need to wait, it sleeps for the appropriate duration.
    pub async fn acquire(&self) {
        let mut inner = self.inner.lock().await;
        let now = Instant::now();

        if let Some(last) = inner.last_request {
            let elapsed = now.duration_since(last);
            if elapsed < inner.interval {
                let sleep_time = inner.interval - elapsed;
                // Drop the lock before sleeping so other tasks aren't blocked
                drop(inner);
                tokio::time::sleep(sleep_time).await;
                // Re-acquire after sleeping
                inner = self.inner.lock().await;
            }
        }

        inner.last_request = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_rate_limiter_allows_first_request_immediately() {
        let limiter = RateLimiter::new(1.0); // 1 QPS
        let start = Instant::now();
        limiter.acquire().await;
        let elapsed = start.elapsed();
        // First request should be near-instant
        assert!(elapsed < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn test_rate_limiter_delays_second_request() {
        let limiter = RateLimiter::new(2.0); // 2 QPS = 500ms between requests
        limiter.acquire().await;
        let start = Instant::now();
        limiter.acquire().await;
        let elapsed = start.elapsed();
        // Second request should wait ~500ms
        assert!(
            elapsed >= Duration::from_millis(400),
            "Expected ~500ms delay, got {:?}",
            elapsed
        );
    }
}
