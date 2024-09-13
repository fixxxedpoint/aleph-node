use std::task::ready;

use futures::{
    future::{pending, BoxFuture},
    FutureExt,
};
use log::trace;
use tokio::{io::AsyncRead, time::sleep_until};

pub use crate::token_bucket::RateLimiter as RateLimiterT;
use crate::{
    token_bucket::{HierarchicalTokenBucket, RateLimiterFacade},
    NonZeroRatePerSecond, RatePerSecond, TokenBucket, LOG_TARGET,
};

pub type SingleConnectionRateLimiter = SleepingRateLimiter<TokenBucket>;

pub type MultipleConnectionsRateLimiter = SleepingRateLimiter<HierarchicalTokenBucket>;

/// Allows to limit access to some resource. Given a preferred rate (units of something) and last used amount of units of some
/// resource, it calculates how long we should delay our next access to that resource in order to satisfy that rate.
#[derive(Clone)]
pub struct SleepingRateLimiter<RL = HierarchicalTokenBucket> {
    rate_limiter: RateLimiterFacade<RL>,
}

impl<RL> SleepingRateLimiter<RL>
where
    RL: RateLimiterT + From<NonZeroRatePerSecond>,
{
    /// Constructs a instance of [SleepingRateLimiter] with given target rate-per-second.
    pub fn new(rate_per_second: RatePerSecond) -> Self {
        Self {
            rate_limiter: RateLimiterFacade::<RL>::new(rate_per_second),
        }
    }

    /// Given `read_size`, that is an amount of units of some governed resource, delays return of `Self` to satisfy configure
    /// rate.
    pub async fn rate_limit(self, read_size: usize) -> Self {
        trace!(
            target: LOG_TARGET,
            "Rate-Limiter attempting to read {}.",
            read_size
        );

        let delay = self
            .rate_limiter
            .rate_limit(read_size.try_into().unwrap_or(u64::MAX));

        match delay {
            None => {}
            Some(None) => pending().await,
            Some(Some(delay)) => {
                trace!(
                    target: LOG_TARGET,
                    "Rate-Limiter will sleep {:?} after reading {} byte(s).",
                    delay,
                    read_size
                );
                sleep_until(delay.into()).await;
            }
        }

        self
    }
}

/// Wrapper around [SleepingRateLimiter] to simplify implementation of the [AsyncRead](tokio::io::AsyncRead) trait.
pub struct RateLimiter<RL> {
    rate_limiter: BoxFuture<'static, SleepingRateLimiter<RL>>,
}

impl<RL> RateLimiter<RL>
where
    RL: RateLimiterT + From<NonZeroRatePerSecond> + Send + 'static,
{
    /// Constructs an instance of [RateLimiter] that uses already configured rate-limiting access governor
    /// ([SleepingRateLimiter]).
    pub fn new(rate_limiter: SleepingRateLimiter<RL>) -> Self {
        Self {
            rate_limiter: Box::pin(rate_limiter.rate_limit(0)),
        }
    }

    /// Helper method for the use of the [AsyncRead](tokio::io::AsyncRead) implementation.
    pub fn rate_limit<Read: AsyncRead + Unpin>(
        &mut self,
        read: std::pin::Pin<&mut Read>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let sleeping_rate_limiter = ready!(self.rate_limiter.poll_unpin(cx));

        let filled_before = buf.filled().len();
        let result = read.poll_read(cx, buf);
        let filled_after = buf.filled().len();
        let last_read_size = filled_after.saturating_sub(filled_before);

        self.rate_limiter = sleeping_rate_limiter.rate_limit(last_read_size).boxed();

        result
    }
}

/// Wrapper around [SleepingRateLimiter] to simplify implementation of the [AsyncRead](futures::AsyncRead) trait.
pub struct FuturesRateLimiter {
    rate_limiter: BoxFuture<'static, SleepingRateLimiter>,
}

impl FuturesRateLimiter {
    /// Constructs an instance of [RateLimiter] that uses already configured rate-limiting access governor
    /// ([SleepingRateLimiter]).
    pub fn new(rate_limiter: SleepingRateLimiter) -> Self {
        Self {
            rate_limiter: Box::pin(rate_limiter.rate_limit(0)),
        }
    }

    /// Helper method for the use of the [AsyncRead](futures::AsyncRead) implementation.
    pub fn rate_limit<Read: futures::AsyncRead + Unpin>(
        &mut self,
        read: std::pin::Pin<&mut Read>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let sleeping_rate_limiter = ready!(self.rate_limiter.poll_unpin(cx));

        let result = read.poll_read(cx, buf);
        let last_read_size = match &result {
            std::task::Poll::Ready(Ok(read_size)) => *read_size,
            _ => 0,
        };

        self.rate_limiter = sleeping_rate_limiter.rate_limit(last_read_size).boxed();

        result
    }
}
