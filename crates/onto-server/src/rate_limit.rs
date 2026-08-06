//! Rate limiting for OntoDB HTTP API.
//!
//! Implements token bucket algorithm for per-key rate limiting.

use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::IntoResponse,
    Json,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Rate limiter configuration.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Default requests per minute per key.
    pub default_rpm: u32,
    /// Whether rate limiting is enabled.
    pub enabled: bool,
    /// Burst size (max requests in a short period).
    pub burst_size: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            default_rpm: 60,
            enabled: true,
            burst_size: 10,
        }
    }
}

/// Token bucket for a single client.
#[derive(Debug)]
struct TokenBucket {
    /// Current number of tokens.
    tokens: f64,
    /// Maximum tokens (burst size).
    max_tokens: f64,
    /// Tokens added per second.
    refill_rate: f64,
    /// Last refill time.
    last_refill: Instant,
}

impl TokenBucket {
    fn new(max_tokens: f64, refill_rate: f64) -> Self {
        Self {
            tokens: max_tokens,
            max_tokens,
            refill_rate,
            last_refill: Instant::now(),
        }
    }

    /// Try to consume a token. Returns true if allowed.
    fn try_consume(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Refill tokens based on elapsed time.
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
        self.last_refill = now;
    }

    /// Get the time until the next token is available.
    fn time_until_next_token(&self) -> Duration {
        if self.tokens >= 1.0 {
            Duration::ZERO
        } else {
            let deficit = 1.0 - self.tokens;
            Duration::from_secs_f64(deficit / self.refill_rate)
        }
    }
}

/// Rate limiter state.
#[derive(Clone)]
pub struct RateLimiter {
    /// Per-key token buckets.
    buckets: Arc<Mutex<HashMap<String, TokenBucket>>>,
    /// Configuration.
    config: RateLimitConfig,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            buckets: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
    }

    /// Check if a request is allowed for the given key.
    /// Returns Ok(remaining) if allowed, Err(retry_after) if rate limited.
    pub async fn check(&self, key: &str, custom_rpm: Option<u32>) -> Result<u32, Duration> {
        if !self.config.enabled {
            return Ok(u32::MAX);
        }

        let rpm = custom_rpm.unwrap_or(self.config.default_rpm);
        let refill_rate = rpm as f64 / 60.0; // Convert RPM to tokens per second

        let mut buckets = self.buckets.lock().await;
        let bucket = buckets
            .entry(key.to_string())
            .or_insert_with(|| TokenBucket::new(self.config.burst_size as f64, refill_rate));

        if bucket.try_consume() {
            Ok(bucket.tokens as u32)
        } else {
            Err(bucket.time_until_next_token())
        }
    }

    /// Get the current rate limit info for a key.
    pub async fn get_info(&self, key: &str) -> RateLimitInfo {
        let buckets = self.buckets.lock().await;
        if let Some(bucket) = buckets.get(key) {
            RateLimitInfo {
                limit: self.config.default_rpm,
                remaining: bucket.tokens as u32,
                reset_after: bucket.time_until_next_token(),
            }
        } else {
            RateLimitInfo {
                limit: self.config.default_rpm,
                remaining: self.config.burst_size,
                reset_after: Duration::ZERO,
            }
        }
    }
}

/// Rate limit information returned in response headers.
#[derive(Debug)]
#[allow(dead_code)]
pub struct RateLimitInfo {
    pub limit: u32,
    pub remaining: u32,
    pub reset_after: Duration,
}

/// Rate limiting middleware.
pub async fn rate_limit_middleware(
    axum::extract::State(limiter): axum::extract::State<RateLimiter>,
    request: Request,
    next: Next,
) -> impl IntoResponse {
    // Skip rate limiting for health check
    if request.uri().path() == "/api/health" {
        return next.run(request).await;
    }

    // Get the API key from extensions (set by auth middleware) or use IP
    let key = request
        .extensions()
        .get::<crate::auth::KeyInfo>()
        .map(|info| info.key.clone())
        .unwrap_or_else(|| {
            // Fall back to IP address
            request
                .headers()
                .get("X-Forwarded-For")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "anonymous".to_string())
        });

    // Get custom rate limit for this key
    let custom_rpm = request
        .extensions()
        .get::<crate::auth::KeyInfo>()
        .and_then(|_| None); // Will be set by auth state

    match limiter.check(&key, custom_rpm).await {
        Ok(remaining) => {
            let info = limiter.get_info(&key).await;
            let mut response = next.run(request).await;
            let headers = response.headers_mut();
            headers.insert("X-RateLimit-Limit", info.limit.into());
            headers.insert("X-RateLimit-Remaining", remaining.into());
            headers.insert(
                "X-RateLimit-Reset",
                info.reset_after.as_secs().into(),
            );
            response
        }
        Err(retry_after) => (
            StatusCode::TOO_MANY_REQUESTS,
            [
                ("Retry-After", retry_after.as_secs().to_string()),
                ("X-RateLimit-Limit", "0".to_string()),
                ("X-RateLimit-Remaining", "0".to_string()),
            ],
            Json(json!({
                "success": false,
                "error": format!("Rate limit exceeded. Retry after {} seconds", retry_after.as_secs())
            })),
        )
            .into_response(),
    }
}
