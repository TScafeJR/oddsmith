//! Client-side rate limiting for the Polymarket and Alchemy APIs.
//!
//! Rate limits from the Polymarket docs:
//!   CLOB general: 9000 req/10s (900/s)
//!   CLOB /trades:  200 req/10s (20/s)
//!   Gamma general: 4000 req/10s (400/s)
//!   Gamma /markets: 300 req/10s (30/s)

use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

/// A thread-safe token bucket.
///
/// The bucket starts full (`burst` tokens) and refills at `rate_per_sec`. [`Limiter::wait`]
/// reserves one token immediately (the balance may go negative, which queues later callers
/// behind earlier ones) and then sleeps until that token is due.
#[derive(Debug)]
pub struct Limiter {
    rate_per_sec: f64,
    burst: u32,
    state: Mutex<(f64, Instant)>,
}

impl Limiter {
    pub fn new(rate_per_sec: f64, burst: u32) -> Limiter {
        Limiter { rate_per_sec, burst, state: Mutex::new((burst as f64, Instant::now())) }
    }

    /// Take one token and return how long the caller must wait before acting.
    pub fn reserve(&self) -> Result<Duration> {
        self.reserve_at(Instant::now())
    }

    fn reserve_at(&self, now: Instant) -> Result<Duration> {
        if self.rate_per_sec == f64::INFINITY {
            return Ok(Duration::ZERO);
        }
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (tokens, last) = &mut *st;
        let elapsed = now.saturating_duration_since(*last).as_secs_f64();
        if elapsed > 0.0 {
            *tokens = (*tokens + elapsed * self.rate_per_sec).min(self.burst as f64);
        }
        *last = (*last).max(now);
        *tokens -= 1.0;
        if *tokens >= 0.0 {
            return Ok(Duration::ZERO);
        }
        if self.rate_per_sec <= 0.0 {
            bail!("rate limiter has a zero rate, wait would never return");
        }
        Ok(Duration::from_secs_f64(-*tokens / self.rate_per_sec))
    }

    /// Block until one token is available.
    pub fn wait(&self) -> Result<()> {
        let delay = self.reserve()?;
        if !delay.is_zero() {
            thread::sleep(delay);
        }
        Ok(())
    }
}

/// One limiter per API, shared by all clients (wrap in an `Arc` to share across threads).
#[derive(Debug)]
pub struct RateLimiter {
    pub general: Limiter,
    pub trades: Limiter,
    pub gamma: Limiter,
    pub alchemy: Limiter,
}

impl Default for RateLimiter {
    fn default() -> Self {
        RateLimiter::new()
    }
}

impl RateLimiter {
    pub fn new() -> RateLimiter {
        RateLimiter {
            general: Limiter::new(800.0, 50), // slightly under 900/s, burst 50
            trades: Limiter::new(18.0, 10),   // slightly under 20/s, burst 10
            gamma: Limiter::new(280.0, 30),   // slightly under 300/s, burst 30
            alchemy: Limiter::new(8.0, 5),    // conservative for the Alchemy free tier
        }
    }

    pub fn wait_general(&self) -> Result<()> {
        self.general.wait()
    }

    pub fn wait_trades(&self) -> Result<()> {
        self.trades.wait()
    }

    pub fn wait_gamma(&self) -> Result<()> {
        self.gamma.wait()
    }

    pub fn wait_alchemy(&self) -> Result<()> {
        self.alchemy.wait()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn burst_is_free_then_requests_queue() {
        let l = Limiter::new(10.0, 3);
        let now = Instant::now();
        for _ in 0..3 {
            assert_eq!(l.reserve_at(now).unwrap(), Duration::ZERO);
        }
        // Each further token is due 100ms after the previous one.
        let d1 = l.reserve_at(now).unwrap().as_secs_f64();
        let d2 = l.reserve_at(now).unwrap().as_secs_f64();
        assert!((d1 - 0.1).abs() < 1e-9, "{d1}");
        assert!((d2 - 0.2).abs() < 1e-9, "{d2}");
    }

    #[test]
    fn refills_up_to_burst() {
        let l = Limiter::new(10.0, 2);
        let t0 = Instant::now();
        l.reserve_at(t0).unwrap();
        l.reserve_at(t0).unwrap();
        // 10 seconds later the bucket is full again, but only holds 2 tokens.
        let t1 = t0 + Duration::from_secs(10);
        assert_eq!(l.reserve_at(t1).unwrap(), Duration::ZERO);
        assert_eq!(l.reserve_at(t1).unwrap(), Duration::ZERO);
        assert!(l.reserve_at(t1).unwrap() > Duration::ZERO);
    }

    #[test]
    fn zero_rate_errors_and_infinite_rate_never_waits() {
        let l = Limiter::new(0.0, 0);
        assert!(l.reserve().is_err());
        let l = Limiter::new(f64::INFINITY, 0);
        assert_eq!(l.reserve().unwrap(), Duration::ZERO);
    }

    #[test]
    fn wait_paces_threads() {
        let l = Arc::new(Limiter::new(200.0, 1));
        let start = Instant::now();
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let l = l.clone();
                thread::spawn(move || l.wait().unwrap())
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        // 1 free token, then 3 more at 5ms intervals.
        assert!(start.elapsed() >= Duration::from_millis(14), "{:?}", start.elapsed());
    }
}
