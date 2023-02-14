use std::{
    pin::Pin,
    time::{Duration, Instant},
};

use futures::Future;
use tokio::io::{AsyncRead, AsyncWrite};

use super::{ConnectionInfo, Splittable};
use crate::network::{clique::Dialer, Data};

#[async_trait::async_trait]
pub trait RateLimiter {
    async fn rate_limit(&mut self, requested: usize, now: impl FnMut() -> Instant + Send);
}

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

    fn calculate_delay(&self, amount: usize) -> Duration {
        Duration::from_secs_f64(amount as f64 / self.rate)
    }

    fn update_units(&mut self, now: Instant) -> usize {
        let passed_time = now.duration_since(self.last_update);
        let new_units = passed_time.as_secs_f64() * self.rate;
        self.available += new_units as usize;
        self.last_update = now;
        new_units as usize
    }
}

#[async_trait::async_trait]
impl RateLimiter for LeakyBucket {
    async fn rate_limit(&mut self, requested: usize, mut now: impl FnMut() -> Instant + Send) {
        let to_return = std::cmp::min(self.available, requested);
        if to_return != requested {
            let now = now();
            let new_units = self.update_units(now);
            if new_units + to_return < requested {
                let delay = self.calculate_delay(requested - to_return);
                let till_when = now + delay;
                tokio::time::sleep_until(till_when.into()).await;
                self.update_units(till_when);
            }
            let last_update = self.last_update;
            return self.rate_limit(requested, || last_update).await;
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
    pub fn new(read: A, rate_limiter: RL) -> Self {
        Self {
            rate_limiter,
            read,
            last_read_size: None,
        }
    }

    pub fn wrap(self, read: A) -> Self {
        self.read = read;
        self
    }

    pub fn unwrap<A, B, C>(
        mut self,
        update: impl FnOnce(A) -> (B, C),
    ) -> (RateLimitedAsyncRead<RL, B>, C) {
        self.read = update(self.read);
        self
    }
}

impl<RL: RateLimiter + Unpin, A: AsyncRead + Unpin> AsyncRead for RateLimitedAsyncRead<RL, A> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if let Some(last_read_size) = self.last_read_size {
            let now = || Instant::now();

            {
                let mut rate_task = self.rate_limiter.rate_limit(last_read_size, now);
                if Pin::new(&mut rate_task).poll(cx).is_pending() {
                    return std::task::Poll::Pending;
                }
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

impl<RL: RateLimiter + Unpin, A: AsyncWrite + Unpin> AsyncWrite for RateLimitedAsyncRead<RL, A> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        AsyncWrite::poll_write(Pin::new(&mut self.splittable), cx, buf)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.splittable), cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.splittable), cx)
    }
}

impl<R: Splittable, RL: Unpin + Send> Splittable for RateLimitedAsyncRead<Rl, RL> {
    type Sender = R::Sender;
    type Receiver = RateLimitedAsyncRead<RL, R::Receiver>;

    fn split(self) -> (Self::Sender, Self::Receiver) {
        let (sender, receiver) = self.read.split();
        self.read = receiver;
        (sender, self)
    }
}

pub struct RateLimitedSplittable<R: Unpin + Send, RL: Unpin + Send> {
    splittable: RateLimitedAsyncRead<RL, R>,
}

impl<R: Splittable, RL: Unpin + Send> RateLimitedSplittable<R, RL> {
    fn split(self) -> (R::Sender, R::Receiver) {
        self.splittable.split()
    }
}

impl<R: AsyncRead + Unpin + Send, RL: RateLimiter + Unpin + Send> AsyncRead
    for RateLimitedSplittable<R, W, RL>
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        AsyncRead::poll_read(Pin::new(&mut self.splittable), cx, buf)
    }
}

impl<R: AsyncWrite + Unpin + Send, RL: Unpin + Send> AsyncWrite for RateLimitedSplittable<R, RL> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        AsyncWrite::poll_write(Pin::new(&mut self.splittable), cx, buf)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.splittable), cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.splittable), cx)
    }
}

impl<R: ConectionInfo + Unpin + Send, RL: Unpin + Send> ConnectionInfo
    for RateLimitedSplittable<R, RL>
{
    fn peer_address_info(&self) -> super::PeerAddressInfo {
        self.splittable.peek_address_info()
    }
}

impl<A: ConnectionInfo, RL: Unpin + Send> ConnectionInfo for RateLimitedAsyncRead<RL, A> {
    fn peer_address_info(&self) -> super::PeerAddressInfo {
        self.read.peer_address_info()
    }
}

impl<R: Splittable, RL: Unpin + Send> Splittable for RateLimitedSplittable<R, RL> {
    type Sender = R::Sender;
    type Receiver = RateLimitedAsyncRead<RL, R::Receiver>;

    fn split(self) -> (Self::Sender, Self::Receiver) {
        let (read, write) = self.splittable.unwrap(|to_split| {
            let (sender, receiver) = to_split.split();
            (receiver, sender)
        });
    }
}

pub struct RateLimitingDialer<D, RL: RateLimiter + Clone> {
    dialer: D,
    rate_limiter: RL,
}

#[async_trait::async_trait]
impl<A: Data, D: Dialer<A>> Dialer<A> for RateLimitingDialer<D> {
    type Connection = RateLimitedSplittable<D::Connection::Receiver, D::Connection::Sender, RL>;
    type Error = D::Error;

    async fn connect(&mut self, address: A) -> Result<Self::Connection, Self::Error> {
        let connection = self.dialer.connect(address).await?;
        RateLimitedSplittable {}
    }
}
