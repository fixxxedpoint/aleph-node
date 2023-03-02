use std::{
    cell::RefCell,
    cmp::min,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{future::ready, Future};
use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncWrite};

use super::{ConnectionInfo, Listener, Splittable};
use crate::network::{clique::Dialer, Data};

#[async_trait::async_trait]
pub trait RateLimiter {
    async fn rate_limit(&mut self, requested: usize, now: impl FnMut() -> Instant + Send);
}

pub struct TokenBucket {
    rate: f64,
    tokens_limit: usize,
    available: usize,
    last_update: Instant,
}

impl Clone for TokenBucket {
    fn clone(&self) -> Self {
        Self {
            rate: self.rate.clone(),
            tokens_limit: self.tokens_limit.clone(),
            available: 0,
            last_update: Instant::now(),
        }
    }
}

// TODO fix me! it should have a max value so there is no possibility of bursts
impl TokenBucket {
    pub fn new(rate: f64, initial: usize, now: Instant) -> Self {
        Self {
            rate,
            tokens_limit: (rate * 2.0) as usize,
            available: initial,
            last_update: now,
        }
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

#[async_trait::async_trait]
impl RateLimiter for TokenBucket {
    async fn rate_limit(&mut self, requested: usize, mut now: impl FnMut() -> Instant + Send) {
        if self.available < requested {
            while self.available < requested {
                let now_value = now();
                assert!(
                    now_value >= self.last_update,
                    "Provided value for `now` should be at least equal to `self.last_update`: now = {:#?} self.last_update = {:#?}.",
                    now_value,
                    self.last_update
                );
                let last_duration = now_value.duration_since(self.last_update);

                if self.update_units(now_value, last_duration) >= requested {
                    break;
                }

                let required_delay = self.calculate_delay(requested - self.available);
                let sleep_until = now_value + required_delay;
                tokio::time::sleep_until(sleep_until.into()).await;
            }
        }
        self.available -= requested;
        self.available = min(self.available, self.tokens_limit);
    }
}

struct TaskHolder<RL> {
    stored: RL,
}

pub struct RateLimitedAsyncRead<RL, A> {
    rate_limiter: Arc<Mutex<RL>>,
    rate_limiter_task: Pin<Box<dyn Future<Output = ()> + Send>>,
    read: A,
}

impl<RL: RateLimiter + Send + Clone, A: AsyncRead> RateLimitedAsyncRead<RL, A> {
    pub fn new(read: A, rate_limiter: RL) -> Self {
        Self {
            rate_limiter: Arc::new(Mutex::new(rate_limiter)),
            rate_limiter_task: Box::pin(ready(())),
            // rate_limiter: rate_limiter.clone(),
            // rate_limiter_holder: TaskHolder {
            //     stored: (rate_limiter, Box::pin(futures::future::ready(()))),
            // },
            read,
        }
    }
}

impl<RL: RateLimiter + Send + Unpin, A: AsyncRead + Unpin + Send> AsyncRead
    for RateLimitedAsyncRead<RL, A>
{
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.rate_limiter_task.as_mut().poll(cx).is_pending() {
            return std::task::Poll::Pending;
        }

        let filled_before = buf.filled().len();
        let result = Pin::new(&mut self.read).poll_read(cx, buf);
        if result.is_pending() {
            return std::task::Poll::Pending;
        }
        let filled_after = buf.filled().len();
        let last_read_size = filled_after - filled_before;

        let task = Box::pin({
            let rate_limiter = Arc::clone(&self.rate_limiter);
            let mut rate_limiter = rate_limiter.lock();
            rate_limiter.rate_limit(last_read_size, || Instant::now())
        });

        self.rate_limiter_task = task;

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

impl<R: Splittable, RL: RateLimiter + Unpin + Send + Clone> Splittable
    for RateLimitedAsyncRead<RL, R>
{
    type Sender = R::Sender;
    type Receiver = RateLimitedAsyncRead<RL, R::Receiver>;

    fn split(self) -> (Self::Sender, Self::Receiver) {
        let (sender, receiver) = self.read.split();
        let receiver = RateLimitedAsyncRead::new(receiver, self.rate_limiter.into_inner());
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
