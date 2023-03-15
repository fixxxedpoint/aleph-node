use std::{
    cmp::min,
    pin::Pin,
    time::{Duration, Instant},
};

use futures::Future;
use log::info;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::Sleep,
};

use super::{ConnectionInfo, Listener, Splittable};
use crate::network::{clique::Dialer, Data};

pub trait RateLimiter {
    fn rate_limit(&mut self, requested: usize, now: impl FnMut() -> Instant) -> Option<Duration>;
}

#[derive(Clone)]
pub struct TokenBucket {
    rate: f64,
    tokens_limit: usize,
    available: usize,
    requested: usize,
    last_update: Instant,
}

impl TokenBucket {
    pub fn new(rate: f64, initial: usize, now: Instant) -> Self {
        Self {
            rate,
            tokens_limit: rate as usize,
            available: initial,
            requested: 0,
            last_update: now,
        }
    }

    pub fn new_default(rate: f64) -> Self {
        Self::new(rate, 0, Instant::now())
    }

    fn calculate_delay(&self) -> Duration {
        Duration::from_secs_f64((self.requested - self.available) as f64 / self.rate)
    }

    fn update_units(&mut self, now: Instant) -> usize {
        let time_since_last_update = now.duration_since(self.last_update);
        let new_units = (time_since_last_update.as_secs_f64() * self.rate).floor() as usize;
        self.available = self.available.saturating_add(new_units);
        self.last_update = now;

        let used = min(self.available, self.requested);
        self.available -= used;
        self.requested -= used;
        self.available = min(self.available, self.tokens_limit);
        self.available
    }
}

impl RateLimiter for TokenBucket {
    fn rate_limit(
        &mut self,
        requested: usize,
        mut now: impl FnMut() -> Instant,
    ) -> Option<Duration> {
        if self.available < requested {
            let now_value = now();
            assert!(
                now_value >= self.last_update,
                "Provided value for `now` should be at least equal to `self.last_update`: now = {:#?} self.last_update = {:#?}.",
                now_value,
                self.last_update
            );
            if self.update_units(now_value) < requested {
                self.requested = self.requested.saturating_add(requested);
                let required_delay = self.calculate_delay();
                return Some(required_delay);
            }
        }
        self.available -= requested;
        self.available = min(self.available, self.tokens_limit);
        None
    }
}

pub struct SleepingRateLimiter<RL> {
    rate_limiter: RL,
    sleep: Pin<Box<Sleep>>,
    finished: bool,
}

impl<RL> SleepingRateLimiter<RL> {
    pub fn new(rate_limiter: RL) -> Self {
        Self {
            rate_limiter,
            sleep: Box::pin(tokio::time::sleep(Duration::ZERO)),
            finished: false,
        }
    }

    fn into_inner(self) -> RL {
        self.rate_limiter
    }
}

impl<RL: RateLimiter> SleepingRateLimiter<RL> {
    fn set_sleep(&mut self, read_size: usize) -> Option<RateLimiterTask> {
        let mut now = None;
        let mut now_closure = || now.get_or_insert_with(|| Instant::now()).clone();
        let next_wait = self.rate_limiter.rate_limit(read_size, &mut now_closure);
        let next_wait = if let Some(next_wait) = next_wait {
            now_closure() + next_wait
        } else {
            return None;
        };

        info!(target: "aleph-network", "rate_limit of {} - waiting until {:?}", read_size, next_wait);
        self.sleep.set(tokio::time::sleep_until(next_wait.into()));
        Some(RateLimiterTask::new(&mut self.sleep, &mut self.finished))
    }

    pub fn rate_limit(&mut self, read_size: usize) -> Option<RateLimiterTask> {
        self.set_sleep(read_size)
    }

    pub fn current_sleep(&mut self) -> RateLimiterTask {
        RateLimiterTask::new(&mut self.sleep, &mut self.finished)
    }

    pub fn rate_limit_into(mut self, read_size: usize) -> OwningRateLimiterTask<RL> {
        self.set_sleep(read_size);
        OwningRateLimiterTask {
            rate_limiter: Some(self),
        }
    }
}

pub struct RateLimiterTask<'a> {
    rate_limiter_sleep: &'a mut Pin<Box<Sleep>>,
    finished: &'a mut bool,
}

impl<'a> RateLimiterTask<'a> {
    fn new(rate_limiter_sleep: &'a mut Pin<Box<Sleep>>, finished: &'a mut bool) -> Self {
        Self {
            rate_limiter_sleep,
            finished,
        }
    }
}

impl<'a> Future for RateLimiterTask<'a> {
    type Output = ();

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if *self.finished {
            return std::task::Poll::Ready(());
        }
        if self.rate_limiter_sleep.as_mut().poll(cx).is_ready() {
            *self.finished = true;
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    }
}

pub struct OwningRateLimiterTask<RL> {
    rate_limiter: Option<SleepingRateLimiter<RL>>,
}

impl<RL> OwningRateLimiterTask<RL> {
    pub fn new(rate_limiter: SleepingRateLimiter<RL>) -> Self {
        Self {
            rate_limiter: Some(rate_limiter),
        }
    }

    pub fn into_inner(self) -> RL {
        self.rate_limiter
            .expect("`rate_limiter` should not be empty")
            .rate_limiter
    }
}

impl<RL: Unpin> Future for OwningRateLimiterTask<RL> {
    type Output = SleepingRateLimiter<RL>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let self_deref = self.get_mut();
        let rate_limiter = if let Some(rate_limiter) = &mut self_deref.rate_limiter {
            rate_limiter
        } else {
            return std::task::Poll::Pending;
        };

        if rate_limiter.sleep.as_mut().poll(cx).is_pending() {
            return std::task::Poll::Pending;
        }

        match self_deref.rate_limiter.take() {
            Some(rate_limiter) => std::task::Poll::Ready(rate_limiter),
            None => std::task::Poll::Pending,
        }
    }
}

pub struct RateLimitedAsyncRead<RL, A> {
    rate_limiter: SleepingRateLimiter<RL>,
    read: Pin<Box<A>>,
}

impl<RL: RateLimiter, A: AsyncRead> RateLimitedAsyncRead<RL, A> {
    pub fn new(read: A, rate_limiter: RL) -> Self {
        Self {
            rate_limiter: SleepingRateLimiter::new(rate_limiter),
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
        match Pin::new(&mut deref_self.rate_limiter.current_sleep()).poll(cx) {
            std::task::Poll::Ready(rate_limiter) => rate_limiter,
            std::task::Poll::Pending => return std::task::Poll::Pending,
        };

        let filled_before = buf.filled().len();
        let result = deref_self.read.as_mut().poll_read(cx, buf);
        let filled_after = buf.filled().len();
        let last_read_size = filled_after - filled_before;

        deref_self.rate_limiter.rate_limit(last_read_size);

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
        let rate_limiter = self.rate_limiter;
        let receiver = RateLimitedAsyncRead::new(receiver, rate_limiter.into_inner());
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{RateLimiter, TokenBucket};

    #[test]
    fn no_slowdown_while_within_rate_limit() {
        let now = Instant::now();
        let limit_per_second = 10_f64;
        let mut rate_limiter = TokenBucket::new(limit_per_second, 0, now.clone());

        assert_eq!(
            rate_limiter.rate_limit(9, || now + Duration::from_secs(1)),
            None
        );
        assert_eq!(
            rate_limiter.rate_limit(5, || now + Duration::from_secs(2)),
            None
        );
        assert_eq!(
            rate_limiter.rate_limit(1, || now + Duration::from_secs(3)),
            None
        );
        assert_eq!(
            rate_limiter.rate_limit(0, || panic!("`now()` shouldn't be called")),
            None
        );
        assert_eq!(
            rate_limiter.rate_limit(0, || panic!("`now()` shouldn't be called")),
            None
        );
        assert_eq!(
            rate_limiter.rate_limit(9, || now + Duration::from_secs(3)),
            None
        );
    }

    #[test]
    fn slowdown_when_limit_reached() {
        let now = Instant::now();
        let limit_per_second = 10_f64;
        let mut rate_limiter = TokenBucket::new(limit_per_second, 0, now.clone());

        assert_eq!(
            rate_limiter.rate_limit(10, || now + Duration::from_secs(1)),
            None
        );

        // we should wait some after reaching the limit
        assert!(rate_limiter
            .rate_limit(1, || now + Duration::from_secs(1))
            .is_some());

        let sleep = rate_limiter
            .rate_limit(19, || now + Duration::from_secs(1))
            .expect("we should already reach the limit");

        assert_eq!(
            sleep,
            Duration::from_secs(2),
            "we should wait exactly 2 seconds"
        );
    }

    #[test]
    fn buildup_tokens_but_no_more_than_limit() {
        let now = Instant::now();
        let limit_per_second = 10_f64;
        let mut rate_limiter = TokenBucket::new(limit_per_second, 0, now.clone());

        assert_eq!(
            rate_limiter.rate_limit(10, || now + Duration::from_secs(2)),
            None
        );

        assert_eq!(
            rate_limiter.rate_limit(40, || now + Duration::from_secs(10)),
            Some(Duration::from_secs(3)),
        );
        assert_eq!(
            rate_limiter.rate_limit(40, || now + Duration::from_secs(11)),
            Some(Duration::from_secs(6))
        );
    }

    #[test]
    fn multiple_calls_buildup_wait_time() {
        let now = Instant::now();
        let limit_per_second = 10_f64;
        let mut rate_limiter = TokenBucket::new(limit_per_second, 0, now.clone());

        assert_eq!(
            rate_limiter.rate_limit(10, || now + Duration::from_secs(3)),
            None
        );

        assert_eq!(
            rate_limiter.rate_limit(10, || now + Duration::from_secs(3)),
            Some(Duration::from_secs(1))
        );

        assert_eq!(
            rate_limiter.rate_limit(10, || now + Duration::from_secs(3)),
            Some(Duration::from_secs(2))
        );

        assert_eq!(
            rate_limiter.rate_limit(50, || now + Duration::from_secs(3)),
            Some(Duration::from_secs(7))
        );
    }
}
