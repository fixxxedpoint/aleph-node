mod rate_limiter;
mod token_bucket;

use tokio::io::{AsyncRead, AsyncWrite};

pub use crate::rate_limiter::{RateLimiter, SleepingRateLimiter};

const LOG_TARGET: &str = "rate-limiter";

pub struct RateLimitedAsyncRead<Read> {
    rate_limiter: RateLimiter,
    read: Read,
}

impl<Read> RateLimitedAsyncRead<Read> {
    pub fn new(read: Read, rate_limiter: RateLimiter) -> Self {
        Self { rate_limiter, read }
    }

    fn get_inner(self: std::pin::Pin<&mut Self>) -> std::pin::Pin<&mut Read>
    where
        Read: Unpin,
    {
        let this = self.get_mut();
        std::pin::Pin::new(&mut this.read)
    }

    pub fn inner(&self) -> &Read {
        &self.read
    }
}

impl<Read: AsyncRead + Unpin> AsyncRead for RateLimitedAsyncRead<Read> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let read = std::pin::Pin::new(&mut this.read);
        this.rate_limiter.rate_limit(read, cx, buf)
    }
}

impl<Write: AsyncWrite + Unpin> AsyncWrite for RateLimitedAsyncRead<Write> {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        self.get_inner().poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        self.get_inner().poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        self.get_inner().poll_shutdown(cx)
    }
}

impl<Read: futures::AsyncRead + Unpin> futures::AsyncRead for RateLimitedAsyncRead<Read> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let read = std::pin::Pin::new(&mut this.read);
        this.rate_limiter.rate_limit_futures(read, cx, buf)
    }
}

impl<Write: futures::AsyncWrite + Unpin> futures::AsyncWrite for RateLimitedAsyncRead<Write> {
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
