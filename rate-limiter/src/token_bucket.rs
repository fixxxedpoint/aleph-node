use crate::{NonZeroRatePerSecond, LOG_TARGET, MIN};
use futures::{future::pending, Future, FutureExt};
use log::trace;
use std::{
    cmp::min,
    num::NonZeroU64,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::time::sleep;

pub trait TimeProvider {
    fn now(&self) -> Instant;
}

impl<F> TimeProvider for F
where
    F: Fn() -> Instant,
{
    fn now(&self) -> Instant {
        self()
    }
}

#[derive(Clone, Default)]
pub struct DefaultTimeProvider;

impl TimeProvider for DefaultTimeProvider {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

pub trait SleepUntil {
    fn sleep_until(&mut self, instant: Instant) -> impl Future<Output = ()> + Send;
}

#[derive(Clone, Default)]
pub struct TokioSleepUntil;

impl SleepUntil for TokioSleepUntil {
    async fn sleep_until(&mut self, instant: Instant) {
        tokio::time::sleep_until(instant.into()).await;
    }
}

/// Implementation of the `Token Bucket` algorithm for the purpose of rate-limiting access to some abstract resource, e.g. an incoming network traffic.
#[derive(Clone)]
struct TokenBucket<T = DefaultTimeProvider> {
    last_update: Instant,
    rate_per_second: NonZeroU64,
    requested: u64,
    time_provider: T,
}

impl<T> std::fmt::Debug for TokenBucket<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenBucket")
            .field("last_update", &self.last_update)
            .field("rate_per_second", &self.rate_per_second)
            .field("requested", &self.requested)
            .finish()
    }
}

impl TokenBucket {
    /// Constructs a instance of [`TokenBucket`] with given target rate-per-second.
    pub fn new(rate_per_second: NonZeroRatePerSecond) -> Self {
        let time_provider = DefaultTimeProvider;
        let now = time_provider.now();
        Self {
            time_provider,
            last_update: now,
            rate_per_second: rate_per_second.into(),
            requested: NonZeroU64::from(rate_per_second).into(),
        }
    }
}

impl<TP> TokenBucket<TP>
where
    TP: TimeProvider,
{
    fn max_possible_available_tokens(&self) -> u64 {
        self.rate_per_second.into()
    }

    fn available(&self) -> Option<u64> {
        (self.requested <= self.max_possible_available_tokens())
            .then(|| u64::from(self.rate_per_second).saturating_sub(self.requested))
    }

    fn account_requested_tokens(&mut self, requested: u64) {
        self.requested = self.requested.saturating_add(requested);
    }

    fn calculate_delay(&self) -> Option<Instant> {
        if self.available().is_some() {
            return None;
        }

        let scheduled_for_later = self.requested - u64::from(self.rate_per_second);
        let delay_micros = scheduled_for_later
            .saturating_mul(1_000_000)
            .saturating_div(self.rate_per_second.into());

        Some(self.last_update + Duration::from_micros(delay_micros))
    }

    fn update_tokens(&mut self) {
        let now = self.time_provider.now();
        assert!(
            now >= self.last_update,
            "Provided value for `now` should be at least equal to `self.last_update`: now = {:#?} self.last_update = {:#?}.",
            now,
            self.last_update
        );

        let time_since_last_update = now.duration_since(self.last_update);
        self.last_update = now;
        let new_units = time_since_last_update
            .as_micros()
            .saturating_mul(u64::from(self.rate_per_second).into())
            .saturating_div(1_000_000)
            .try_into()
            .unwrap_or(u64::MAX);
        self.requested = self.requested.saturating_sub(new_units);
    }

    /// Get current rate in bits per second.
    pub fn rate(&self) -> NonZeroRatePerSecond {
        self.rate_per_second.into()
    }

    /// Set a rate in bits per second.
    pub fn set_rate(&mut self, rate_per_second: NonZeroRatePerSecond) {
        self.update_tokens();
        let available = self.available();
        let previous_rate_per_second = self.rate_per_second.get();
        self.rate_per_second = rate_per_second.into();
        if available.is_some() {
            let max_for_available = self.max_possible_available_tokens();
            let available_after_rate_update = min(available.unwrap_or(0), max_for_available);
            self.requested = self.rate_per_second.get() - available_after_rate_update;
        } else {
            self.requested = self.requested - previous_rate_per_second + self.rate_per_second.get();
        }
    }

    /// Calculates amount of time by which we should delay next call to some governed resource in order to satisfy
    /// specified rate limit.
    pub fn rate_limit(&mut self, requested: u64) -> Option<Instant> {
        trace!(
            target: LOG_TARGET,
            "TokenBucket called for {requested} of requested bytes. Internal state: {self:?}.",
        );
        let now_available = self.available().unwrap_or(0);
        if now_available < requested {
            self.update_tokens()
        }
        self.account_requested_tokens(requested);
        let delay = self.calculate_delay();
        trace!(
            target: LOG_TARGET,
            "TokenBucket calculated delay after receiving a request of {requested}: {delay:?}.",
        );
        delay
    }
}

/// Implementation of the bandwidth sharing strategy that attempts to assign equal portion of the total bandwidth to all active
/// consumers of the bandwidth.
pub struct SharedBandwidthManager {
    max_rate: NonZeroRatePerSecond,
    peers_count: Arc<AtomicU64>,
    already_requested: Option<NonZeroRatePerSecond>,
}

impl Clone for SharedBandwidthManager {
    fn clone(&self) -> Self {
        Self {
            max_rate: self.max_rate,
            peers_count: self.peers_count.clone(),
            already_requested: None,
        }
    }
}

impl SharedBandwidthManager {
    fn calculate_bandwidth_without_children_increament(
        &mut self,
        active_children: Option<u64>,
    ) -> NonZeroRatePerSecond {
        let active_children =
            active_children.unwrap_or_else(|| self.peers_count.load(Ordering::Relaxed));
        let rate = u64::from(self.max_rate) / active_children;
        NonZeroU64::try_from(rate)
            .map(NonZeroRatePerSecond::from)
            .unwrap_or(MIN)
    }
}

impl SharedBandwidthManager {
    pub fn new(max_rate: NonZeroRatePerSecond) -> Self {
        Self {
            max_rate,
            peers_count: Arc::new(AtomicU64::new(0)),
            already_requested: None,
        }
    }
}

impl SharedBandwidthManager {
    pub fn request_bandwidth(&mut self) -> NonZeroRatePerSecond {
        let active_children = (self.already_requested.is_none())
            .then(|| 1 + self.peers_count.fetch_add(1, Ordering::Relaxed));
        let rate = self.calculate_bandwidth_without_children_increament(active_children);
        self.already_requested = Some(rate);
        rate
    }

    pub fn notify_idle(&mut self) {
        if self.already_requested.take().is_some() {
            self.peers_count.fetch_sub(1, Ordering::Relaxed);
        }
    }

    pub async fn await_bandwidth_change(&mut self) -> NonZeroRatePerSecond {
        let Some(previous_rate) = self.already_requested else {
            return pending().await;
        };
        let sleep_amount = Duration::from_millis(250);
        let mut rate = self.calculate_bandwidth_without_children_increament(None);
        while rate == previous_rate {
            sleep(sleep_amount).await;
            rate = self.calculate_bandwidth_without_children_increament(None);
        }
        self.already_requested = Some(rate);
        rate
    }
}

/// Wrapper around the [TokenBucket] that allows conveniently manage its internal token-rate and allows to idle/sleep in order
/// to fulfill its rate-limit.
#[derive(Clone)]
struct AsyncTokenBucket<TP = DefaultTimeProvider, SU = TokioSleepUntil> {
    token_bucket: TokenBucket<TP>,
    next_deadline: Option<Instant>,
    sleep_until: SU,
}

impl<TP, SU> AsyncTokenBucket<TP, SU> {
    pub fn new(token_bucket: TokenBucket<TP>, sleep_until: SU) -> Self {
        Self {
            token_bucket,
            next_deadline: None,
            sleep_until,
        }
    }
}

impl<TP, SU> AsyncTokenBucket<TP, SU>
where
    TP: TimeProvider,
{
    pub fn rate_limit(&mut self, requested: u64) {
        self.next_deadline = TokenBucket::rate_limit(&mut self.token_bucket, requested);
    }

    pub fn set_rate(&mut self, rate: NonZeroRatePerSecond) {
        if self.token_bucket.rate() != rate {
            self.token_bucket.set_rate(rate);
            self.next_deadline = self.token_bucket.rate_limit(0);
        }
    }

    pub async fn wait(&mut self)
    where
        TP: TimeProvider + Send,
        SU: SleepUntil + Send,
    {
        if let Some(deadline) = self.next_deadline {
            self.sleep_until.sleep_until(deadline).await;
            self.next_deadline = None;
        }
    }
}

#[derive(Clone)]
pub struct HierarchicalTokenBucket<TP = DefaultTimeProvider, SU = TokioSleepUntil> {
    shared_bandwidth: SharedBandwidthManager,
    rate_limiter: AsyncTokenBucket<TP, SU>,
    need_to_notify_parent: bool,
}

impl HierarchicalTokenBucket {
    pub fn new(rate: NonZeroRatePerSecond) -> Self {
        let token_bucket = TokenBucket::new(rate);
        let sleep_until = TokioSleepUntil;
        let rate_limiter = AsyncTokenBucket::new(token_bucket, sleep_until);
        Self {
            shared_bandwidth: SharedBandwidthManager::new(rate),
            rate_limiter,
            need_to_notify_parent: false,
        }
    }
}

impl<TP, SU> HierarchicalTokenBucket<TP, SU> {
    fn request_bandwidth(&mut self) -> NonZeroRatePerSecond {
        self.need_to_notify_parent = true;
        self.shared_bandwidth.request_bandwidth()
    }

    fn notify_idle(&mut self) {
        if self.need_to_notify_parent {
            self.shared_bandwidth.notify_idle();
            self.need_to_notify_parent = false;
        }
    }

    pub async fn rate_limit(mut self, requested: u64) -> Self
    where
        TP: TimeProvider + Send,
        SU: SleepUntil + Send,
    {
        let rate = self.request_bandwidth();
        self.rate_limiter.set_rate(rate);

        self.rate_limiter.rate_limit(requested);

        loop {
            futures::select! {
                _ = self.rate_limiter.wait().fuse() => {
                    self.notify_idle();
                    return self;
                },
                rate = self.shared_bandwidth.await_bandwidth_change().fuse() => {
                    self.rate_limiter.set_rate(rate);
                },
            }
        }
    }
}

impl<TP, SU> Drop for HierarchicalTokenBucket<TP, SU> {
    fn drop(&mut self) {
        self.notify_idle();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cmp::max,
        iter::repeat,
        ops::DerefMut,
        sync::Arc,
        task::Poll,
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use futures::{
        future::{poll_fn, BoxFuture, Future},
        pin_mut,
        stream::FuturesOrdered,
        StreamExt,
    };
    use parking_lot::Mutex;
    use tokio::task::yield_now;

    use super::{SharedBandwidthManager, SleepUntil, TimeProvider, TokenBucket};
    use crate::token_bucket::{AsyncTokenBucket, HierarchicalTokenBucket, NonZeroRatePerSecond};

    #[tokio::test]
    async fn basic_checks_of_shared_bandwidth_manager() {
        let rate = 10.try_into().expect("10 > 0 qed");
        let mut bandwidth_share = SharedBandwidthManager::new(rate);
        let mut cloned_bandwidth_share = bandwidth_share.clone();
        let mut another_cloned_bandwidth_share = cloned_bandwidth_share.clone();

        // only one consumer, so it should get whole bandwidth
        assert_eq!(bandwidth_share.request_bandwidth(), rate);

        // since other instances did not request any bandwidth, they should not receive a notification about its change
        let poll_result = poll_fn(|cx| {
            let future = cloned_bandwidth_share.await_bandwidth_change();
            pin_mut!(future);
            Poll::Ready(Future::poll(future, cx))
        })
        .await;
        assert_eq!(poll_result, Poll::Pending);

        let poll_result = poll_fn(|cx| {
            let future = another_cloned_bandwidth_share.await_bandwidth_change();
            pin_mut!(future);
            Poll::Ready(Future::poll(future, cx))
        })
        .await;
        assert_eq!(poll_result, Poll::Pending);

        // two consumers should equally divide the bandwidth
        let rate = 5.try_into().expect("5 > 0 qed");
        assert_eq!(cloned_bandwidth_share.request_bandwidth(), rate);
        assert_eq!(bandwidth_share.await_bandwidth_change().await, rate);

        // similarly when there are three consumers
        let bandwidth: u64 = another_cloned_bandwidth_share.request_bandwidth().into();
        let another_bandwidth: u64 = bandwidth_share.await_bandwidth_change().await.into();
        let yet_another_bandwidth: u64 =
            cloned_bandwidth_share.await_bandwidth_change().await.into();

        assert!((3..4).contains(&bandwidth));
        assert!((3..4).contains(&another_bandwidth));
        assert!((3..4).contains(&yet_another_bandwidth));

        assert!((9..10).contains(&(bandwidth + another_bandwidth + yet_another_bandwidth)));

        // all consumers should be notified after one of them become idle
        let rate = 5.try_into().expect("5 > 0 qed");
        another_cloned_bandwidth_share.notify_idle();
        assert_eq!(cloned_bandwidth_share.await_bandwidth_change().await, rate);
        assert_eq!(bandwidth_share.await_bandwidth_change().await, rate);
    }

    /// Allows to treat [TokenBucket] and [HierarchicalTokenBucket] similarly in tests.
    trait RateLimiter: Sized {
        async fn rate_limit(self, requested: u64) -> (Self, Option<Instant>);
    }

    impl<TP> RateLimiter for TokenBucket<TP>
    where
        TP: TimeProvider,
    {
        async fn rate_limit(mut self, requested: u64) -> (Self, Option<Instant>) {
            let delay = TokenBucket::rate_limit(&mut self, requested);
            (self, delay)
        }
    }

    type TracingRateLimiter<TP> = HierarchicalTokenBucket<TP, TestSleepUntilShared>;

    impl<TP> RateLimiter for TracingRateLimiter<TP>
    where
        TP: TimeProvider + Send,
    {
        async fn rate_limit(mut self, requested: u64) -> (Self, Option<Instant>) {
            let last_sleep = self.rate_limiter.sleep_until.latest_sleep_until();
            let time_before = *last_sleep.lock();
            self = self.rate_limit(requested).await;
            let time_after = *last_sleep.lock();
            (
                self,
                (time_before != time_after).then_some(time_after).flatten(),
            )
        }
    }

    impl<TP> From<(NonZeroRatePerSecond, TP)> for TokenBucket<TP>
    where
        TP: TimeProvider,
    {
        fn from((rate_per_second, time_provider): (NonZeroRatePerSecond, TP)) -> Self {
            Self {
                last_update: time_provider.now(),
                rate_per_second: rate_per_second.into(),
                requested: rate_per_second.into(),
                time_provider,
            }
        }
    }

    impl<TP, SU> From<(NonZeroRatePerSecond, TP, SU)> for HierarchicalTokenBucket<TP, SU>
    where
        TP: TimeProvider,
    {
        fn from((rate, time_provider, sleep_until): (NonZeroRatePerSecond, TP, SU)) -> Self {
            let shared_bandwidth = SharedBandwidthManager::new(rate);
            let token_bucket = TokenBucket::from((rate, time_provider));
            let rate_limiter = AsyncTokenBucket::new(token_bucket, sleep_until);
            Self {
                shared_bandwidth,
                rate_limiter,
                need_to_notify_parent: false,
            }
        }
    }

    #[derive(Clone)]
    struct TracingSleepUntil<SU> {
        wrapped: SU,
        pub last_deadline: Option<Instant>,
    }

    impl<SU> TracingSleepUntil<SU> {
        pub fn new(sleep_until: SU) -> Self {
            Self {
                wrapped: sleep_until,
                last_deadline: None,
            }
        }
    }

    impl<SU> SleepUntil for TracingSleepUntil<SU>
    where
        SU: SleepUntil + Send,
    {
        async fn sleep_until(&mut self, instant: Instant) {
            self.last_deadline = Some(instant);
            self.wrapped.sleep_until(instant).await
        }
    }

    #[derive(Clone)]
    struct TestSleepUntilShared {
        last_instant: Arc<Mutex<Option<Instant>>>,
    }

    impl TestSleepUntilShared {
        pub fn new() -> Self {
            Self {
                last_instant: Arc::new(Mutex::new(None)),
            }
        }

        pub fn latest_sleep_until(&self) -> Arc<Mutex<Option<Instant>>> {
            self.last_instant.clone()
        }
    }

    impl SleepUntil for TestSleepUntilShared {
        async fn sleep_until(&mut self, instant: Instant) {
            let mut last_instant = self.last_instant.lock();
            *last_instant = max(*last_instant, Some(instant));
        }
    }

    impl TimeProvider for Arc<Box<dyn TimeProvider + Send + Sync + 'static>> {
        fn now(&self) -> Instant {
            self.as_ref().now()
        }
    }

    impl
        From<(
            NonZeroRatePerSecond,
            Arc<Box<dyn TimeProvider + Send + Sync>>,
            TestSleepUntilShared,
        )> for TokenBucket<Arc<Box<dyn TimeProvider + Send + Sync>>>
    {
        fn from(
            (rate, time_provider, _): (
                NonZeroRatePerSecond,
                Arc<Box<dyn TimeProvider + Send + Sync>>,
                TestSleepUntilShared,
            ),
        ) -> Self {
            TokenBucket::from((rate, time_provider))
        }
    }

    struct SleepUntilWithBarrier<SU> {
        wrapped: SU,
        barrier: Arc<tokio::sync::RwLock<tokio::sync::Barrier>>,
        initial_counter: u64,
        counter: u64,
        to_wait: Option<BoxFuture<'static, ()>>,
        id: u64,
    }

    impl<SU> Clone for SleepUntilWithBarrier<SU>
    where
        SU: Clone,
    {
        fn clone(&self) -> Self {
            Self {
                wrapped: self.wrapped.clone(),
                barrier: self.barrier.clone(),
                initial_counter: self.initial_counter,
                counter: self.counter,
                to_wait: None,
                id: self.id + 1,
            }
        }
    }

    impl<SU> SleepUntilWithBarrier<SU> {
        pub fn new(
            sleep_until: SU,
            barrier: Arc<tokio::sync::RwLock<tokio::sync::Barrier>>,
            how_many_times_to_use_barrier: u64,
        ) -> Self {
            Self {
                wrapped: sleep_until,
                barrier,
                initial_counter: how_many_times_to_use_barrier,
                counter: how_many_times_to_use_barrier,
                to_wait: None,
                id: 0,
            }
        }

        pub fn reset(&mut self) {
            self.counter = self.initial_counter;
        }

        pub async fn wait(&mut self) {
            while self.counter > 0 {
                self.to_wait
                    .get_or_insert_with(|| {
                        let barrier = self.barrier.clone();
                        Box::pin(async move {
                            barrier.read().await.wait().await;
                        })
                    })
                    .await;
                self.to_wait = None;
                self.counter -= 1;
                yield_now().await;
            }
        }
    }

    impl<SU> SleepUntil for SleepUntilWithBarrier<SU>
    where
        SU: SleepUntil + Send,
    {
        async fn sleep_until(&mut self, instant: Instant) {
            self.wait().await;
            self.wrapped.sleep_until(instant).await;
        }
    }

    #[tokio::test]
    async fn rate_limiter_sanity_check() {
        token_bucket_sanity_check_test::<TokenBucket<_>>().await;
        token_bucket_sanity_check_test::<TracingRateLimiter<_>>().await
    }

    async fn token_bucket_sanity_check_test<RL>()
    where
        RL: RateLimiter
            + From<(
                NonZeroRatePerSecond,
                Arc<Box<dyn TimeProvider + Send + Sync>>,
                TestSleepUntilShared,
            )>,
    {
        let limit_per_second = 10.try_into().expect("10 > 0 qed");
        let now = Instant::now();
        let time_to_return = Arc::new(parking_lot::RwLock::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider + Send + Sync> =
            Box::new(move || *time_provider.read());
        let rate_limiter = RL::from((
            limit_per_second,
            Arc::new(time_provider),
            TestSleepUntilShared::new(),
        ));

        *time_to_return.write() = now + Duration::from_secs(1);
        let (rate_limiter, deadline) = rate_limiter.rate_limit(9).await;
        assert!(deadline.is_none());

        *time_to_return.write() = now + Duration::from_secs(1);
        let (rate_limiter, deadline) = rate_limiter.rate_limit(12).await;
        assert!(deadline.is_some());

        *time_to_return.write() = now + Duration::from_secs(3);
        let (_, deadline) = rate_limiter.rate_limit(8).await;
        assert!(deadline.is_none());
    }

    #[tokio::test]
    async fn no_slowdown_while_within_rate_limit() {
        no_slowdown_while_within_rate_limit_test::<TokenBucket<_>>().await;
        no_slowdown_while_within_rate_limit_test::<TracingRateLimiter<_>>().await;
    }

    async fn no_slowdown_while_within_rate_limit_test<RL>()
    where
        RL: RateLimiter
            + From<(
                NonZeroRatePerSecond,
                Arc<Box<dyn TimeProvider + Send + Sync>>,
                TestSleepUntilShared,
            )>,
    {
        let limit_per_second = 10.try_into().expect("10 > 0 qed");
        let now = Instant::now();
        let time_to_return = Arc::new(parking_lot::RwLock::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider + Send + Sync> =
            Box::new(move || *time_provider.read());
        let sleep_until = TestSleepUntilShared::new();
        let rate_limiter = RL::from((limit_per_second, Arc::new(time_provider), sleep_until));

        *time_to_return.write() = now + Duration::from_secs(1);
        let (rate_limiter, deadline) = rate_limiter.rate_limit(9).await;
        assert_eq!(deadline, None);

        *time_to_return.write() = now + Duration::from_secs(2);
        let (rate_limiter, deadline) = rate_limiter.rate_limit(5).await;
        assert_eq!(deadline, None);

        *time_to_return.write() = now + Duration::from_secs(3);
        let (rate_limiter, deadline) = rate_limiter.rate_limit(1).await;
        assert_eq!(deadline, None);

        *time_to_return.write() = now + Duration::from_secs(3);
        let (_, deadline) = rate_limiter.rate_limit(9).await;
        assert_eq!(deadline, None);
    }

    #[tokio::test]
    async fn slowdown_when_limit_reached_token_bucket() {
        slowdown_when_limit_reached_test::<TokenBucket<_>>().await;
        slowdown_when_limit_reached_test::<TracingRateLimiter<_>>().await
    }

    async fn slowdown_when_limit_reached_test<RL>()
    where
        RL: RateLimiter
            + From<(
                NonZeroRatePerSecond,
                Arc<Box<dyn TimeProvider + Send + Sync>>,
                TestSleepUntilShared,
            )>,
    {
        let limit_per_second = 10.try_into().expect("10 > 0 qed");
        let now = Instant::now();
        let time_to_return = Arc::new(parking_lot::RwLock::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider + Send + Sync> =
            Box::new(move || *time_provider.read());
        let rate_limiter = RL::from((
            limit_per_second,
            Arc::new(time_provider),
            TestSleepUntilShared::new(),
        ));

        *time_to_return.write() = now;
        let (rate_limiter, deadline) = rate_limiter.rate_limit(10).await;
        assert_eq!(deadline, Some(now + Duration::from_secs(1)));

        // we should wait some time after reaching the limit
        *time_to_return.write() = now + Duration::from_secs(1);
        let (rate_limiter, deadline) = rate_limiter.rate_limit(1).await;
        assert!(deadline.is_some());

        *time_to_return.write() = now + Duration::from_secs(1);
        let (_, deadline) = rate_limiter.rate_limit(19).await;
        assert_eq!(
            deadline,
            Some(now + Duration::from_secs(3)),
            "we should wait exactly 2 seconds"
        );
    }

    #[tokio::test]
    async fn buildup_tokens_but_no_more_than_limit_token_bucket() {
        buildup_tokens_but_no_more_than_limit_test::<TokenBucket<_>>().await;
        buildup_tokens_but_no_more_than_limit_test::<TracingRateLimiter<_>>().await
    }

    async fn buildup_tokens_but_no_more_than_limit_test<RL>()
    where
        RL: RateLimiter
            + From<(
                NonZeroRatePerSecond,
                Arc<Box<dyn TimeProvider + Send + Sync>>,
                TestSleepUntilShared,
            )>,
    {
        let limit_per_second = 10.try_into().expect("10 > 0 qed");
        let now = Instant::now();
        let time_to_return = Arc::new(parking_lot::RwLock::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider + Send + Sync> =
            Box::new(move || *time_provider.read());
        let rate_limiter = RL::from((
            limit_per_second,
            time_provider.into(),
            TestSleepUntilShared::new(),
        ));

        *time_to_return.write() = now + Duration::from_secs(2);
        let (rate_limiter, deadline) = rate_limiter.rate_limit(10).await;
        assert_eq!(deadline, None);

        *time_to_return.write() = now + Duration::from_secs(10);
        let (rate_limiter, deadline) = rate_limiter.rate_limit(40).await;
        assert_eq!(
            deadline,
            Some(now + Duration::from_secs(10) + Duration::from_secs(3)),
        );

        *time_to_return.write() = now + Duration::from_secs(11);
        let (_, deadline) = rate_limiter.rate_limit(40).await;
        assert_eq!(
            deadline,
            Some(now + Duration::from_secs(11) + Duration::from_secs(6))
        );
    }

    #[tokio::test]
    async fn multiple_calls_buildup_wait_time() {
        multiple_calls_buildup_wait_time_test::<TokenBucket<_>>().await;
        multiple_calls_buildup_wait_time_test::<TracingRateLimiter<_>>().await
    }

    async fn multiple_calls_buildup_wait_time_test<RL>()
    where
        RL: RateLimiter
            + From<(
                NonZeroRatePerSecond,
                Arc<Box<dyn TimeProvider + Send + Sync>>,
                TestSleepUntilShared,
            )>,
    {
        let limit_per_second = 10.try_into().expect("10 > 0 qed");
        let now = Instant::now();
        let time_to_return = Arc::new(parking_lot::RwLock::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider + Send + Sync> =
            Box::new(move || *time_provider.read());
        let rate_limiter = RL::from((
            limit_per_second,
            time_provider.into(),
            TestSleepUntilShared::new(),
        ));

        *time_to_return.write() = now + Duration::from_secs(3);
        let (rate_limiter, deadline) = rate_limiter.rate_limit(10).await;
        assert_eq!(deadline, None);

        *time_to_return.write() = now + Duration::from_secs(3);
        let (rate_limiter, deadline) = rate_limiter.rate_limit(10).await;
        assert_eq!(deadline, Some(now + Duration::from_secs(4)));

        *time_to_return.write() = now + Duration::from_secs(3);
        let (rate_limiter, deadline) = rate_limiter.rate_limit(10).await;
        assert_eq!(
            deadline,
            Some(now + Duration::from_secs(4) + Duration::from_secs(1))
        );

        *time_to_return.write() = now + Duration::from_secs(3);
        let (_, deadline) = rate_limiter.rate_limit(50).await;
        assert_eq!(
            deadline,
            Some(now + Duration::from_secs(4) + Duration::from_secs(6))
        );
    }

    #[tokio::test]
    async fn two_peers_can_share_bandwidth() {
        let limit_per_second = 10.try_into().expect("10 > 0 qed");
        let initial_time = Instant::now();
        let time_to_return = Arc::new(Mutex::new(initial_time));

        let current_time = time_to_return.clone();
        let current_time_clone = time_to_return.clone();

        let time_mutex = Arc::new(parking_lot::Mutex::new(()));
        let time_mutex_clone = time_mutex.clone();

        let time_provider: Arc<Box<dyn TimeProvider + Send + Sync>> =
            Arc::new(Box::new(move || *time_to_return.lock()));

        let sleep_until = TestSleepUntilShared::new();

        let last_deadline = sleep_until.latest_sleep_until();
        let last_deadline_clone = last_deadline.clone();

        let barrier = Arc::new(tokio::sync::RwLock::new(tokio::sync::Barrier::new(2)));
        let sleep_until = SleepUntilWithBarrier::new(sleep_until, barrier, 3);

        let mut rate_limiter =
            HierarchicalTokenBucket::<_, _>::from((limit_per_second, time_provider, sleep_until));
        let mut rate_limiter_cloned = rate_limiter.clone();

        let total_data_sent = thread::scope(|s| {
            let first_handle = s.spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                runtime.block_on(async move {
                    rate_limiter = rate_limiter.rate_limit(10).await;

                    {
                        let _guard = time_mutex.lock();
                        let last_deadline = last_deadline.lock();
                        let time = *current_time.lock();
                        *current_time.lock() = last_deadline.unwrap_or(time);
                    }

                    rate_limiter.rate_limit(30).await;
                });
                10 + 30
            });

            let second_handle = s.spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                runtime.block_on(async {
                    rate_limiter_cloned = rate_limiter_cloned.rate_limit(10).await;

                    {
                        let _guard = time_mutex_clone.lock();
                        let last_deadline = last_deadline_clone.lock();
                        let time = *current_time_clone.lock();
                        *current_time_clone.lock() = last_deadline.unwrap_or(time);
                    }

                    rate_limiter_cloned.rate_limit(25).await;
                });
                10 + 25
            });
            let total_data_sent: u128 = first_handle
                .join()
                .expect("first thread should finish without errors")
                + second_handle
                    .join()
                    .expect("second thread should finish without errors");

            total_data_sent
        });
        let duration = last_deadline_clone.lock().expect("we should sleep a bit") - initial_time;
        let rate = total_data_sent * 1000 / duration.as_millis();
        assert!(
            rate.abs_diff(10) <= 5,
            "calculated bandwidth should be within some error bounds: rate = {rate}; duration = {duration:?}"
        );
    }

    #[tokio::test]
    async fn single_peer_can_use_whole_bandwidth_when_needed() {
        let limit_per_second = 10.try_into().expect("10 > 0 qed");
        let now = Instant::now();
        let time_to_return = Arc::new(parking_lot::RwLock::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Arc<Box<dyn TimeProvider + Send + Sync>> =
            Arc::new(Box::new(move || *time_provider.read()));

        let rate_limiter = TracingRateLimiter::<_>::from((
            limit_per_second,
            time_provider,
            TestSleepUntilShared::new(),
        ));

        let rate_limiter_cloned = rate_limiter.clone();

        let (rate_limiter, deadline) = RateLimiter::rate_limit(rate_limiter, 5).await;
        assert_eq!(deadline, Some(now + Duration::from_millis(500)));
        let (_, deadline) = RateLimiter::rate_limit(rate_limiter_cloned, 5).await;
        assert_eq!(deadline, None,);

        *time_to_return.write() = now + Duration::from_millis(1500);

        let (_, deadline) = RateLimiter::rate_limit(rate_limiter, 10).await;
        assert_eq!(deadline, None);
    }

    #[tokio::test]
    async fn peers_receive_at_least_one_token_per_second() {
        let limit_per_second = 1.try_into().expect("1 > 0 qed");
        let now = Instant::now();
        let time_to_return = Arc::new(parking_lot::RwLock::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Arc<Box<dyn TimeProvider + Send + Sync>> =
            Arc::new(Box::new(move || *time_provider.read()));

        let rate_limiter = TracingRateLimiter::<_>::from((
            limit_per_second,
            time_provider,
            TestSleepUntilShared::new(),
        ));

        *time_to_return.write() = now + Duration::from_secs(1);

        let rate_limiter_cloned = rate_limiter.clone();

        let (rate_limiter, deadline) = RateLimiter::rate_limit(rate_limiter, 1).await;
        assert_eq!(deadline, None);

        let (rate_limiter_cloned, deadline) = RateLimiter::rate_limit(rate_limiter_cloned, 1).await;
        assert_eq!(deadline, None);

        *time_to_return.write() = now + Duration::from_secs(2);

        let (_, deadline) = RateLimiter::rate_limit(rate_limiter, 1).await;
        assert_eq!(deadline, None);
        let (_, deadline) = RateLimiter::rate_limit(rate_limiter_cloned, 2).await;
        assert_eq!(deadline, Some(now + Duration::from_secs(3)));
    }

    #[tokio::test]
    async fn avarage_bandwidth_should_be_within_some_bounds() {
        use rand::{
            distributions::{Distribution, Uniform},
            seq::SliceRandom,
            SeedableRng,
        };

        let mut test_state = vec![];

        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("back to the future")
            .as_secs();
        let mut rand_gen = rand::rngs::StdRng::seed_from_u64(seed);
        let time_rand_gen = parking_lot::RwLock::new(rand::rngs::StdRng::seed_from_u64(seed));

        let data_gen = Uniform::from(0..100 * 1024 * 1024);
        let limiters_count = Uniform::from(10..=128).sample(&mut rand_gen);
        let batch_gen = Uniform::from(1..limiters_count);

        let rate_limit = 4 * 1024 * 1024;
        let limit_per_second = rate_limit.try_into().expect("(4 * 1024 * 1024) > 0 qed");
        let rate_limit = rate_limit.into();
        let initial_time = Instant::now();

        let test_sleep_until_shared = TestSleepUntilShared::new();
        let last_deadline = test_sleep_until_shared.latest_sleep_until();
        let last_deadline_from_time_provider = last_deadline.clone();

        let time_to_return = Arc::new(parking_lot::RwLock::new(initial_time));
        let time_provider = time_to_return.clone();
        let time_provider: Arc<Box<dyn TimeProvider + Send + Sync>> =
            Arc::new(Box::new(move || {
                let mut current_time = time_provider.write();
                let millis_from_current_time: u64 = last_deadline_from_time_provider
                    .lock()
                    .unwrap_or(initial_time)
                    .duration_since(*current_time)
                    .as_millis()
                    .try_into()
                    .expect("something wrong with our `time` calculations");
                let time_gen = Uniform::from(1..1000 * 10 + millis_from_current_time);
                let time_passed =
                    Duration::from_millis(time_gen.sample(time_rand_gen.write().deref_mut()));
                *current_time += time_passed;
                *current_time

                // let mut current_time = time_provider.borrow_mut();
                // *current_time += Duration::from_micros(1);
                // *current_time

                // *last_deadline_from_time_provider.lock()
            }));

        let barrier = Arc::new(tokio::sync::RwLock::new(tokio::sync::Barrier::new(0)));
        let how_many_times_stop_on_barrier = 0;
        let test_sleep_until_with_barrier = SleepUntilWithBarrier::new(
            test_sleep_until_shared,
            barrier.clone(),
            how_many_times_stop_on_barrier,
        );
        let tracing_sleep_until = TracingSleepUntil::new(test_sleep_until_with_barrier);
        let rate_limiter = HierarchicalTokenBucket::<_, _>::from((
            limit_per_second,
            time_provider,
            tracing_sleep_until,
        ));

        let mut rate_limiters = repeat(())
            .scan((0usize, rate_limiter), |(id, rate_limiter), _| {
                let new_rate_limiter = rate_limiter.clone();
                let new_state = rate_limiter.clone();
                let limiter_id = *id;
                *rate_limiter = new_state;
                *id += 1;
                Some((limiter_id, Some(new_rate_limiter)))
            })
            .take(limiters_count)
            .collect::<Vec<_>>();

        let mut total_data_scheduled = 0;
        let mut total_rate = 0;

        let mut total_number_of_calls = 0;
        while total_number_of_calls < 1000 {
            let batch_size = batch_gen.sample(&mut rand_gen);
            println!("batch size = {batch_size}");

            total_number_of_calls += batch_size;
            *barrier.write().await = tokio::sync::Barrier::new(batch_size);

            rate_limiters.shuffle(&mut rand_gen);

            let current_time = *time_to_return.read();
            let mut batch_test: FuturesOrdered<_> = rate_limiters[0..batch_size]
                .iter_mut()
                .map(|(selected_limiter_id, selected_rate_limiter)| {
                    let data_read = data_gen.sample(&mut rand_gen);

                    let mut rate_limiter = selected_rate_limiter
                        .take()
                        .expect("we should be able to retrieve a rate-limiter");
                    rate_limiter.rate_limiter.sleep_until.wrapped.reset();

                    let last_deadline = rate_limiter.rate_limiter.sleep_until.last_deadline;
                    let task_start_time = max(last_deadline, Some(current_time));

                    let rate_task = HierarchicalTokenBucket::rate_limit(rate_limiter, data_read);

                    test_state.push((*selected_limiter_id, data_read));

                    total_data_scheduled += u128::from(data_read);

                    async move {
                        let mut rate_limiter = rate_task.await;
                        rate_limiter.rate_limiter.sleep_until.wrapped.wait().await;

                        let next_deadline = rate_limiter.rate_limiter.sleep_until.last_deadline;
                        let time_passed = if let (Some(next_deadline), Some(task_start_time)) =
                            (next_deadline, task_start_time)
                        {
                            (next_deadline - task_start_time).as_millis()
                        } else {
                            0
                        };
                        let result_rate = (time_passed != 0)
                            .then(|| u128::from(data_read) * 1000 / time_passed)
                            .unwrap_or(0);

                        (rate_limiter, selected_rate_limiter, result_rate)
                    }
                })
                .collect();
            // perform the actual rate-limiting
            let mut rate_for_batch = 0;
            while let Some((rate_limiter, store, calculated_rate)) = batch_test.next().await {
                let _ = store.insert(rate_limiter);
                rate_for_batch += calculated_rate;
            }
            // panic!("jaisodaiosdj");
            total_rate = (total_rate + rate_for_batch) / 2;
            println!("batch rate: {rate_for_batch}");

            // let time_passed = time_gen.sample(&mut rand_gen);
            // let current_time = *time_to_return.borrow() + Duration::from_millis(time_passed);
            // let current_time = max(*time_to_return.borrow(), *last_deadline.lock())
            //     + Duration::from_millis(time_passed);
            let current_time = max(
                *time_to_return.read(),
                (*last_deadline.lock()).unwrap_or(*time_to_return.read()),
            );
            *time_to_return.write() = current_time;
        }
        // TODO try to compute real rate instead of this
        // let abs_rate_diff = total_rate.abs_diff(rate_limit);
        let time_passed = last_deadline.lock().expect("empty `last_deadline`") - initial_time;
        let total_rate = total_data_scheduled * 1000 / time_passed.as_millis();
        let abs_rate_diff = total_rate.abs_diff(rate_limit);
        assert!(
            abs_rate_diff <= rate_limit * 3/10,
            "Used bandwidth should be oscillating close to {rate_limit} b/s (+/- 50%), but got {total_rate} b/s instead. Total data sent: {total_data_scheduled}; Time: {time_passed:?}"
        );
        // panic!("expected rate-limit = {rate_limit} calculated rate limit = {total_rate}");
    }
}
