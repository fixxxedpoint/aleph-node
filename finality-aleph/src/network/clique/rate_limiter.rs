use std::{
    pin::Pin,
    task::Ready,
    time::{Duration, Instant},
};

use futures::{
    future::{Pending, Ready},
    Future,
};
use tokio::io::AsyncRead;

#[async_trait::async_trait]
pub trait RateLimiter {
    async fn rate_limit(&mut self, requested: usize, now: impl FnMut() -> Instant);
}

// impl<F: FnMut(usize, impl Instant) -> usize> RateLimiter for F {
//     async fn rate_limit(requested: usize, now: impl FnMut() -> Instant) -> usize {
//         self(requested, now)
//     }
// }

// pub struct RateLimitedAsyncRead<R, RL> {
//     reader: R,
//     rate_limiter: R,
// }

// impl<R: AsyncRead, RL: RateLimiter> AsyncRead for RateLimitedAsyncRead<R, RL> {
//     fn poll_read(
//         self: std::pin::Pin<&mut Self>,
//         cx: &mut std::task::Context<'_>,
//         buf: &mut tokio::io::ReadBuf<'_>,
//     ) -> std::task::Poll<std::io::Result<()>> {
//         cx.waker().
//     }
// }

pub struct LeakyBucket {
    rate: f64,
    available: usize,
    last_update: Instant,
}

impl LeakyBucket {
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
        self.last_update = now;
        new_units as usize
    }
}

impl RateLimiter for LeakyBucket {
    async fn rate_limit(&mut self, requested: usize, now: impl FnMut() -> Instant) {
        let to_return = std::cmp::min(self.available, requested);
        if to_return != requested {
            let now = now();
            let new_units = self.update_units(now);
            if new_units + to_return < requested {
                let delay = self.calculate_delay(now, requested - to_return);
                let till_when = now + delay;
                tokio::time::sleep_until(till_when).await;
            }
            return rate_limit(requested, till_when).await;
        }
        self.available -= requested;
    }
}

pub struct RateLimitedAsyncRead<RL, A> {
    rate_limiter: RL,
    read: A,
    last_read_size: Option<usize>,
}

impl<RL: RateLimiter, A: AsyncRead> RateLimitedAsyncRead<RL, A> {
    async fn read(buf: &mut tokio::io::ReadBuf<'_>) {}
}

impl<RL: RateLimiter + Unpin, A: AsyncRead> AsyncRead for RateLimitedAsyncRead<RL, A> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if let Some(last_read_size) = self.last_read_size {
            let now = || Instant::now();

            let rate_task = self.rate_limiter.rate_limit(last_read_size, now);
            if let Pending = Pin::new(&mut rate_task).poll(cx) {
                return Pending;
            }
            self.last_read_size = None;
        }
        let filled_before = buf.filled().len();
        let result = Pin::new(&mut self.read).poll_read(cx, buf);
        let filled_after = buf.filled().len();
        let diff = filled_after - filled_before;
        if diff > 0 {
            self.last_read_size = Some(diff);
        }
        result
    }
}
