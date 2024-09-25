mod rate_limiter;
mod token_bucket;

use std::num::{NonZeroU64, TryFromIntError};

use rate_limiter::RateLimiterImpl;
use tokio::io::AsyncRead;

pub use crate::rate_limiter::{
    DefaultSharedRateLimiter, FuturesRateLimiter, PerConnectionRateLimiter, SleepingRateLimiter,
    SleepingRateLimiterImpl,
};
pub use crate::token_bucket::TokenBucket;

const LOG_TARGET: &str = "rate-limiter";

#[derive(PartialEq, Eq, Clone, Copy)]
pub struct NonZeroRatePerSecond(NonZeroU64);

impl From<NonZeroRatePerSecond> for u64 {
    fn from(value: NonZeroRatePerSecond) -> Self {
        value.0.into()
    }
}

impl From<NonZeroRatePerSecond> for NonZeroU64 {
    fn from(value: NonZeroRatePerSecond) -> Self {
        value.0
    }
}

impl From<NonZeroU64> for NonZeroRatePerSecond {
    fn from(value: NonZeroU64) -> Self {
        NonZeroRatePerSecond(value)
    }
}

impl TryFrom<u64> for NonZeroRatePerSecond {
    type Error = TryFromIntError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Ok(NonZeroRatePerSecond(NonZeroU64::try_from(value)?))
    }
}

#[derive(PartialEq, Eq)]
pub enum RatePerSecond {
    Block,
    Rate(NonZeroRatePerSecond),
}

impl From<u64> for RatePerSecond {
    fn from(value: u64) -> Self {
        match value {
            0 => Self::Block,
            _ => Self::Rate(NonZeroRatePerSecond(
                NonZeroU64::new(value).expect("`value` != 0 and `value`:u64 qed"),
            )),
        }
    }
}

impl From<RatePerSecond> for u64 {
    fn from(value: RatePerSecond) -> Self {
        match value {
            RatePerSecond::Block => 0,
            RatePerSecond::Rate(NonZeroRatePerSecond(value)) => value.into(),
        }
    }
}

impl From<NonZeroRatePerSecond> for RatePerSecond {
    fn from(value: NonZeroRatePerSecond) -> Self {
        RatePerSecond::Rate(value)
    }
}

pub struct RateLimitedAsyncRead<Read, RL> {
    rate_limiter: RateLimiterImpl<RL>,
    inner: Read,
}

impl<Read, RL> RateLimitedAsyncRead<Read, RL> {
    pub fn new(read: Read, rate_limiter: RateLimiterImpl<RL>) -> Self {
        Self {
            rate_limiter,
            inner: read,
        }
    }

    pub fn inner(&self) -> &Read {
        &self.inner
    }
}

impl<Read, RL> AsyncRead for RateLimitedAsyncRead<Read, RL>
where
    Read: AsyncRead + Unpin,
    RL: SleepingRateLimiter + Send + 'static,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let read = std::pin::Pin::new(&mut this.inner);
        this.rate_limiter.rate_limit(read, cx, buf)
    }
}

pub struct FuturesRateLimitedAsyncReadWrite<ReadWrite, ARL> {
    rate_limiter: FuturesRateLimiter<ARL>,
    inner: ReadWrite,
}

impl<ReadWrite, ARL> FuturesRateLimitedAsyncReadWrite<ReadWrite, ARL> {
    pub fn new(wrapped: ReadWrite, rate_limiter: FuturesRateLimiter<ARL>) -> Self {
        Self {
            rate_limiter,
            inner: wrapped,
        }
    }

    fn get_inner(self: std::pin::Pin<&mut Self>) -> std::pin::Pin<&mut ReadWrite>
    where
        ReadWrite: Unpin,
    {
        let this = self.get_mut();
        std::pin::Pin::new(&mut this.inner)
    }
}

impl<Read, ARL> futures::AsyncRead for FuturesRateLimitedAsyncReadWrite<Read, ARL>
where
    Read: futures::AsyncRead + Unpin,
    ARL: SleepingRateLimiter + 'static,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let read = std::pin::Pin::new(&mut this.inner);
        this.rate_limiter.rate_limit(read, cx, buf)
    }
}

impl<Write: futures::AsyncWrite + Unpin, ARL> futures::AsyncWrite
    for FuturesRateLimitedAsyncReadWrite<Write, ARL>
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.get_inner().poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.get_inner().poll_flush(cx)
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.get_inner().poll_close(cx)
    }
}
