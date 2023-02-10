use std::{
    pin::Pin,
    time::{Duration, Instant},
};

use futures::Future;
use tokio::io::AsyncRead;

#[async_trait::async_trait]
pub trait RateLimiter {
    async fn rate_limit(&mut self, requested: usize, now: Instant) -> usize;
}

impl<F: FnMut(usize, Instant) -> usize> RateLimiter for F {
    fn rate_limit(requested: usize, now: Instant) -> usize {
        self(requested, now)
    }
}

pub struct RateLimitedAsyncRead<R, RL> {
    reader: R,
    rate_limiter: R,
}

// impl<R: AsyncRead, RL: RateLimiter> AsyncRead for RateLimitedAsyncRead<R, RL> {
//     fn poll_read(
//         self: std::pin::Pin<&mut Self>,
//         cx: &mut std::task::Context<'_>,
//         buf: &mut tokio::io::ReadBuf<'_>,
//     ) -> std::task::Poll<std::io::Result<()>> {
//         cx.waker().
//     }
// }

pub struct LeackyBucket {
    rate: f64,
    available: usize,
    last_update: Instant,
}

impl LeackyBucket {
    pub fn new(rate: f64, initial: usize, now: Instant) -> Self {
        Self {
            rate,
            available: initial,
            last_update: now,
        }
    }

    fn calculate_delay(&self, now: Instant, amount: usize) -> Duration {
        Duration::from_secs_f64(amount as f64 / self.rate)
    }

    fn update_units(&mut self, now: Instant) -> usize {
        let passed_time = now.duration_since(self.last_update);
        let new_units = passed_time.as_secs_f64() * self.rate;
        self.available += new_units;
        new_units as usize
    }
}

impl RateLimiter for LeackyBucket {
    async fn rate_limit(&mut self, requested: usize, now: Instant) -> usize {
        let to_return = std::cmp::min(self.available, requested);
        if to_return != requested {
            let new_units = self.update_units(now);
            if new_units + to_return >= requested {
                self.available -= requested;
                return requested;
            }
            let delay = self.calculate_delay(now, requested - to_return);
            let till_when = now + delay;
            tokio::time::sleep_until(till_when).await;
            return rate_limit(requested, till_when).await;
        }
        self.available -= to_return;
        to_return
    }
}

pub struct RateLimitedAsyncRead<RL, A> {
    rate_limiter: RL,
    read: A,
    last_timestamp: Option<Instant>,
}

impl<RL: RateLimiter, A: AsyncRead> RateLimitedAsyncRead<RL, A> {
    async fn read(buf: &mut tokio::io::ReadBuf<'_>) {
        
    }
}

impl<RL: RateLimiter + Unpin, A: AsyncRead> AsyncRead for RateLimitedAsyncRead<RL, A> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let result = self.read(buf);
        Future::poll(self, cx)
        rate_limiter.poll();
        self.rate_limiter.rate_limit(requested, now)
    }
}
