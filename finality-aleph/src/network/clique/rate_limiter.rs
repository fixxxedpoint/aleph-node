use std::{
    cmp::min,
    pin::Pin,
    time::{Duration, Instant},
};

use futures::Future;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::Sleep,
};

use super::{ConnectionInfo, Listener, Splittable};
use crate::network::{clique::Dialer, Data};

pub trait RateLimiter {
    fn rate_limit(
        &mut self,
        requested: usize,
        now: impl FnMut() -> Instant + Send,
    ) -> Option<Duration>;
}

#[derive(Clone)]
pub struct TokenBucket {
    rate: f64,
    tokens_limit: usize,
    available: usize,
    last_update: Instant,
}

impl TokenBucket {
    pub fn new(rate: f64, initial: usize, now: Instant) -> Self {
        Self {
            rate,
            tokens_limit: rate as usize,
            available: initial,
            last_update: now,
        }
    }

    pub fn new_default(rate: f64) -> Self {
        Self::new(rate, 0, Instant::now())
    }

    fn calculate_delay(&self, amount: usize) -> Duration {
        Duration::from_secs_f64(amount as f64 / self.rate)
    }

    fn update_units(&mut self, now: Instant, time_since_last_update: Duration) -> usize {
        let new_units = (time_since_last_update.as_secs_f64() * self.rate).floor() as usize;
        self.available = self.available.saturating_add(new_units);
        self.last_update = now;
        self.available
    }
}

impl RateLimiter for TokenBucket {
    fn rate_limit(
        &mut self,
        requested: usize,
        mut now: impl FnMut() -> Instant + Send,
    ) -> Option<Duration> {
        if self.available < requested {
            let now_value = now();
            assert!(
                now_value >= self.last_update,
                "Provided value for `now` should be at least equal to `self.last_update`: now = {:#?} self.last_update = {:#?}.",
                now_value,
                self.last_update
            );
            let last_duration = now_value.duration_since(self.last_update);

            if self.update_units(now_value, last_duration) < requested {
                let required_delay = self.calculate_delay(requested - self.available);
                return Some(required_delay);
            }
        }
        self.available -= requested;
        self.available = min(self.available, self.tokens_limit);
        None
    }
}

pub struct RateLimitedAsyncRead<RL, A> {
    rate_limiter: RL,
    rate_limiter_sleep: Pin<Box<Sleep>>,
    read: Pin<Box<A>>,
}

impl<RL: RateLimiter, A: AsyncRead> RateLimitedAsyncRead<RL, A> {
    pub fn new(read: A, rate_limiter: RL) -> Self {
        Self {
            rate_limiter,
            rate_limiter_sleep: Box::pin(tokio::time::sleep(Duration::ZERO)),
            read: Box::pin(read),
        }
    }
}

impl<RL: RateLimiter + Unpin, A: AsyncRead> AsyncRead for RateLimitedAsyncRead<RL, A> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let deref_self = self.get_mut();
        if deref_self.rate_limiter_sleep.as_mut().poll(cx).is_pending() {
            return std::task::Poll::Pending;
        }

        let filled_before = buf.filled().len();
        let result = deref_self.read.as_mut().poll_read(cx, buf);
        let filled_after = buf.filled().len();
        let last_read_size = filled_after - filled_before;

        let rate_limit_sleep_time = deref_self
            .rate_limiter
            .rate_limit(last_read_size, || Instant::now())
            .unwrap_or(Duration::ZERO);

        deref_self
            .rate_limiter_sleep
            .set(tokio::time::sleep(rate_limit_sleep_time));

        result
    }
}

impl<RL: RateLimiter + Unpin, A: AsyncWrite + Unpin> AsyncWrite for RateLimitedAsyncRead<RL, A> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        self.read.as_mut().poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        self.read.as_mut().poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        self.read.as_mut().poll_shutdown(cx)
    }
}

impl<R: Splittable, RL: RateLimiter + Send + Unpin> Splittable for RateLimitedAsyncRead<RL, R> {
    type Sender = R::Sender;
    type Receiver = RateLimitedAsyncRead<RL, R::Receiver>;

    fn split(self) -> (Self::Sender, Self::Receiver) {
        let (sender, receiver) = Pin::<Box<R>>::into_inner(self.read).split();
        let receiver = RateLimitedAsyncRead::new(receiver, self.rate_limiter);
        (sender, receiver)
    }
}

impl<A: ConnectionInfo, RL: Send> ConnectionInfo for RateLimitedAsyncRead<RL, A> {
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
impl<RL: RateLimiter + Clone + Send + Unpin + 'static, A: Data, D: Dialer<A>> Dialer<A>
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
impl<L: Listener + Send, RL: RateLimiter + Clone + Send + Unpin + 'static> Listener
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
