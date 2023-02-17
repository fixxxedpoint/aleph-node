use std::{
    pin::Pin,
    time::{Duration, Instant},
};

use futures::Future;
use tokio::io::{AsyncRead, AsyncWrite};

use super::{ConnectionInfo, Listener, Splittable};
use crate::network::{clique::Dialer, Data};

#[async_trait::async_trait]
pub trait RateLimiter {
    async fn rate_limit(&mut self, requested: usize, now: impl FnMut() -> Instant + Send);
}

#[derive(Clone)]
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

    fn update_units(&mut self, now: Instant, time_since_last_update: Duration) -> usize {
        let new_units = time_since_last_update.as_secs_f64() * self.rate;
        self.available += new_units as usize;
        self.last_update = now;
        self.available
    }
}

#[async_trait::async_trait]
impl RateLimiter for LeakyBucket {
    async fn rate_limit(&mut self, requested: usize, mut now: impl FnMut() -> Instant + Send) {
        if self.available < requested {
            let mut now = now();
            assert!(
                now >= self.last_update,
                "Provided value for `now` should be at least equal to `self.last_update`."
            );
            let mut last_duration = now.duration_since(self.last_update);
            while self.update_units(now, last_duration) < requested {
                last_duration = self.calculate_delay(requested - self.available);
                now = now + last_duration;
                tokio::time::sleep_until(now.into()).await;
            }
        }
        self.available -= requested;
    }
}

pub struct RateLimitedAsyncRead<RL, A> {
    last_read_size: Option<usize>,
    rate_limiter: RL,
    read: A,
}

impl<RL: RateLimiter, A: AsyncRead> RateLimitedAsyncRead<RL, A> {
    pub fn new(read: A, rate_limiter: RL) -> Self {
        Self {
            rate_limiter,
            read,
            last_read_size: None,
        }
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
        self.last_read_size = Some(diff);
        result
    }
}

impl<RL: RateLimiter + Unpin, A: AsyncWrite + Unpin> AsyncWrite for RateLimitedAsyncRead<RL, A> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        AsyncWrite::poll_write(Pin::new(&mut self.read), cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.read), cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.read), cx)
    }
}

impl<R: Splittable, RL: RateLimiter + Unpin + Send> Splittable for RateLimitedAsyncRead<RL, R> {
    type Sender = R::Sender;
    type Receiver = RateLimitedAsyncRead<RL, R::Receiver>;

    fn split(self) -> (Self::Sender, Self::Receiver) {
        let (sender, receiver) = self.read.split();
        let receiver = RateLimitedAsyncRead::new(receiver, self.rate_limiter);
        (sender, receiver)
    }
}

impl<A: ConnectionInfo, RL: Unpin + Send> ConnectionInfo for RateLimitedAsyncRead<RL, A> {
    fn peer_address_info(&self) -> super::PeerAddressInfo {
        self.read.peer_address_info()
    }
}

#[derive(Clone)]
pub struct RateLimitingDialer<D, RL: RateLimiter> {
    dialer: D,
    rate_limiter: RL,
}

impl<D, RL: RateLimiter> RateLimitingDialer<D, RL> {
    pub fn new(dialer: D, rate_limiter: RL) -> Self {
        Self {
            dialer,
            rate_limiter,
        }
    }
}

#[async_trait::async_trait]
impl<RL: RateLimiter + Clone + Unpin + Send + 'static, A: Data, D: Dialer<A>> Dialer<A>
    for RateLimitingDialer<D, RL>
{
    type Connection = RateLimitedAsyncRead<RL, D::Connection>;
    type Error = D::Error;

    async fn connect(&mut self, address: A) -> Result<Self::Connection, Self::Error> {
        let connection = self.dialer.connect(address).await?;
        Ok(RateLimitedAsyncRead::new(
            connection,
            self.rate_limiter.clone(),
        ))
    }
}

pub struct RateLimitingListener<L, RL> {
    listener: L,
    rate_limiter: RL,
}

impl<L, RL> RateLimitingListener<L, RL> {
    pub fn new(listener: L, rate_limiter: RL) -> Self {
        Self {
            listener,
            rate_limiter,
        }
    }
}

#[async_trait::async_trait]
impl<L: Listener + Send, RL: RateLimiter + Clone + Unpin + Send + 'static> Listener
    for RateLimitingListener<L, RL>
{
    type Connection = RateLimitedAsyncRead<RL, L::Connection>;
    type Error = L::Error;

    async fn accept(&mut self) -> Result<Self::Connection, Self::Error> {
        let connection = self.listener.accept().await?;
        Ok(RateLimitedAsyncRead::new(
            connection,
            self.rate_limiter.clone(),
        ))
    }
}
