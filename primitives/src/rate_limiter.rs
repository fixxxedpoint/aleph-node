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

use crate::{ConnectionInfo, Data, Dialer, Listener, Splittable};

#[derive(Clone)]
pub struct TokenBucket {
    rate: f64,
    tokens_limit: usize,
    available: usize,
    requested: usize,
    last_update: Instant,
}

impl TokenBucket {
    pub fn new(rate: f64) -> Self {
        Self {
            rate,
            tokens_limit: rate as usize,
            available: 0,
            requested: 0,
            last_update: Instant::now(),
        }
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

    pub fn rate_limit(
        &mut self,
        requested: usize,
        mut now: impl FnMut() -> Instant,
    ) -> Option<Duration> {
        if self.requested > 0 || self.available < requested {
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

pub struct SleepingRateLimiter {
    rate_limiter: TokenBucket,
    sleep: Pin<Box<Sleep>>,
    finished: bool,
}

impl SleepingRateLimiter {
    pub fn new(rate_limiter: TokenBucket) -> Self {
        Self {
            rate_limiter,
            sleep: Box::pin(tokio::time::sleep(Duration::ZERO)),
            finished: true,
        }
    }

    fn into_inner(self) -> TokenBucket {
        self.rate_limiter
    }
}

impl SleepingRateLimiter {
    fn set_sleep(&mut self, read_size: usize) -> Option<RateLimiterTask> {
        let mut now = None;
        let mut now_closure = || now.get_or_insert_with(|| Instant::now()).clone();
        let next_wait = self.rate_limiter.rate_limit(read_size, &mut now_closure)?;
        let next_wait = now_closure() + next_wait;
        self.finished = false;

        self.sleep.set(tokio::time::sleep_until(next_wait.into()));
        Some(self.current_sleep())
    }

    pub fn rate_limit(&mut self, read_size: usize) -> Option<RateLimiterTask> {
        self.set_sleep(read_size)
    }

    pub fn current_sleep(&mut self) -> RateLimiterTask {
        RateLimiterTask::new(&mut self.sleep, &mut self.finished)
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

pub struct RateLimitedAsyncRead<A> {
    rate_limiter: SleepingRateLimiter,
    read: Pin<Box<A>>,
}

impl<A: AsyncRead> RateLimitedAsyncRead<A> {
    pub fn new(read: A, rate_limiter: TokenBucket) -> Self {
        Self {
            rate_limiter: SleepingRateLimiter::new(rate_limiter),
            read: Box::pin(read),
        }
    }
}

impl<A: AsyncRead> AsyncRead for RateLimitedAsyncRead<A> {
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

impl<A: AsyncWrite + Unpin> AsyncWrite for RateLimitedAsyncRead<A> {
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

impl<R: Splittable> Splittable for RateLimitedAsyncRead<R> {
    type Sender = R::Sender;
    type Receiver = RateLimitedAsyncRead<R::Receiver>;

    fn split(self) -> (Self::Sender, Self::Receiver) {
        let (sender, receiver) = Pin::<Box<R>>::into_inner(self.read).split();
        let rate_limiter = self.rate_limiter;
        let receiver = RateLimitedAsyncRead::new(receiver, rate_limiter.into_inner());
        (sender, receiver)
    }
}

impl<A: ConnectionInfo> ConnectionInfo for RateLimitedAsyncRead<A> {
    fn peer_address_info(&self) -> super::PeerAddressInfo {
        self.read.peer_address_info()
    }
}

#[derive(Clone)]
pub struct RateLimitingDialer<D> {
    dialer: D,
    rate_limiter: TokenBucket,
}

impl<D> RateLimitingDialer<D> {
    pub fn new(dialer: D, rate_limiter: TokenBucket) -> Self {
        Self {
            dialer,
            rate_limiter,
        }
    }
}

#[async_trait::async_trait]
impl<A: Data, D: Dialer<A>> Dialer<A> for RateLimitingDialer<D> {
    type Connection = RateLimitedAsyncRead<D::Connection>;
    type Error = D::Error;

    async fn connect(&mut self, address: A) -> Result<Self::Connection, Self::Error> {
        let connection = self.dialer.connect(address).await?;
        Ok(RateLimitedAsyncRead::new(
            connection,
            self.rate_limiter.clone(),
        ))
    }
}

pub struct RateLimitingListener<L> {
    listener: L,
    rate_limiter: TokenBucket,
}

impl<L> RateLimitingListener<L> {
    pub fn new(listener: L, rate_limiter: TokenBucket) -> Self {
        Self {
            listener,
            rate_limiter,
        }
    }
}

#[async_trait::async_trait]
impl<L: Listener + Send> Listener for RateLimitingListener<L> {
    type Connection = RateLimitedAsyncRead<L::Connection>;
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

    use super::TokenBucket;

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

        // we should wait some time after reaching the limit
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
