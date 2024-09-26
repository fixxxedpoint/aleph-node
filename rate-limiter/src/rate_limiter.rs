use std::{task::ready, time::Instant};

use futures::{
    future::{pending, BoxFuture},
    Future, FutureExt,
};
use log::trace;
use tokio::{io::AsyncRead, time::sleep_until};

use crate::{
    token_bucket::HierarchicalTokenBucket, NonZeroRatePerSecond, RatePerSecond, TokenBucket,
    LOG_TARGET,
};

pub type PerConnectionRateLimiter = SleepingRateLimiterImpl<RateLimiterFacade<TokenBucket>>;

pub type DefaultSharedRateLimiter = RateLimiterFacade<HierarchicalTokenBucket>;

pub trait SleepingRateLimiter {
    fn rate_limit(self, read_size: usize) -> impl Future<Output = Self> + Send;
}

#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub enum Deadline {
    Never,
    Instant(Instant),
}

impl From<Deadline> for Option<Instant> {
    fn from(value: Deadline) -> Self {
        match value {
            Deadline::Never => None,
            Deadline::Instant(value) => Some(value),
        }
    }
}

// impl From<Instant> for Deadline {
//     fn from(value: Instant) -> Self {
//         Deadline::Instant(value)
//     }
// }

// impl PartialOrd for Deadline {
//     fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
//         Some(self.cmp(other))
//     }
// }

// impl Ord for Deadline {
//     fn cmp(&self, other: &Self) -> std::cmp::Ordering {
//         use std::cmp::Ordering;
//         match (self, other) {
//             (Deadline::Never, Deadline::Never) => Ordering::Equal,
//             (Deadline::Never, Deadline::Instant(_)) => Ordering::Greater,
//             (Deadline::Instant(_), Deadline::Never) => Ordering::Less,
//             (Deadline::Instant(self_instant), Deadline::Instant(other_instant)) => {
//                 self_instant.cmp(other_instant)
//             }
//         }
//     }
// }

pub trait RateLimiter {
    fn rate_limit(&mut self, requested: u64) -> Option<Deadline>;
}

/// Allows to limit access to some resource. Given a preferred rate (units of something) and last used amount of units of some
/// resource, it calculates how long we should delay our next access to that resource in order to satisfy that rate.
#[derive(Clone)]
pub struct SleepingRateLimiterImpl<RL = PerConnectionRateLimiter> {
    rate_limiter: RL,
}

impl<RL> SleepingRateLimiterImpl<RateLimiterFacade<RL>>
where
    RL: RateLimiter,
{
    /// Constructs a instance of [SleepingRateLimiter] with given target rate-per-second.
    pub fn new(rate_per_second: RatePerSecond) -> Self
    where
        RL: From<NonZeroRatePerSecond>,
    {
        Self {
            rate_limiter: RateLimiterFacade::<RL>::new(rate_per_second),
        }
    }

    /// Given `read_size`, that is an amount of units of some governed resource, delays return of `Self` to satisfy configure
    /// rate.
    pub async fn rate_limit(mut self, read_size: usize) -> Self {
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
            Some(Deadline::Never) => pending().await,
            Some(Deadline::Instant(delay)) => {
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

impl<RL> SleepingRateLimiter for SleepingRateLimiterImpl<RateLimiterFacade<RL>>
where
    RL: RateLimiter + Send,
{
    fn rate_limit(self, read_size: usize) -> impl Future<Output = Self> + Send {
        SleepingRateLimiterImpl::rate_limit(self, read_size)
    }
}

// impl<RL> RateLimiterSleeper for SleepingRateLimiter<RL>
// where
//     RL: RateLimiterT + From<NonZeroRatePerSecond> + Send,
// {
//     fn rate_limit(self, read_size: u64) -> impl Future<Output = Self> + Send {
//         async move {
//             self.rate_limit(read_size.try_into().unwrap_or(usize::MAX))
//                 .await
//         }
//     }
// }

// impl<RL> RateLimiterSleeper for SleepingRateLimiter<RL>
// where
//     RL: RateLimiterSleeper + Send,
// {
//     fn rate_limit(self, read_size: u64) -> impl Future<Output = Self> + Send {
//     }
// }

/// Wrapper around [SleepingRateLimiter] to simplify implementation of the [AsyncRead](tokio::io::AsyncRead) trait.
pub struct RateLimiterImpl<RL> {
    rate_limiter: BoxFuture<'static, RL>,
}

impl<RL> RateLimiterImpl<RL>
where
    RL: SleepingRateLimiter + Send + 'static,
{
    /// Constructs an instance of [RateLimiter] that uses already configured rate-limiting access governor
    /// ([SleepingRateLimiter]).
    pub fn new(rate_limiter: RL) -> Self {
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

pub struct FuturesRateLimiter<ARL> {
    rate_limiter: BoxFuture<'static, ARL>,
}

impl<ARL> FuturesRateLimiter<ARL>
where
    ARL: SleepingRateLimiter + 'static,
{
    /// Constructs an instance of [RateLimiter] that uses already configured rate-limiting access governor
    /// ([SleepingRateLimiter]).
    pub fn new(rate_limiter: ARL) -> Self {
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

#[derive(Clone)]
pub enum RateLimiterFacade<RL> {
    NoTraffic,
    RateLimiter(RL),
}

impl<RL> RateLimiterFacade<RL> {
    pub fn new(rate: RatePerSecond) -> Self
    where
        RL: From<NonZeroRatePerSecond>,
    {
        match rate {
            RatePerSecond::Block => Self::NoTraffic,
            RatePerSecond::Rate(rate) => Self::RateLimiter(rate.into()),
        }
    }

    pub fn rate_limit(&mut self, requested: u64) -> Option<Deadline>
    where
        RL: RateLimiter,
    {
        match self {
            RateLimiterFacade::NoTraffic => Some(Deadline::Never),
            RateLimiterFacade::RateLimiter(limiter) => limiter.rate_limit(requested),
        }
    }
}

impl SleepingRateLimiter for RateLimiterFacade<HierarchicalTokenBucket> {
    fn rate_limit(self, read_size: usize) -> impl Future<Output = Self> + Send {
        async move {
            match self {
                RateLimiterFacade::NoTraffic => pending().await,
                RateLimiterFacade::RateLimiter(rate_limiter) => RateLimiterFacade::RateLimiter(
                    rate_limiter
                        .rate_limit(read_size.try_into().unwrap_or(u64::MAX))
                        .await,
                ),
            }
        }
    }
}
