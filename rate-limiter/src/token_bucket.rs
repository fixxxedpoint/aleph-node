use crate::{
    rate_limiter::{Deadline, RateLimiter},
    NonZeroRatePerSecond, SleepingRateLimiter, LOG_TARGET, MIN,
};
use futures::{future::pending, Future, FutureExt};
use log::trace;
use parking_lot::Mutex;
use std::{
    cmp::min,
    num::NonZeroU64,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

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

/// Implementation of the `Token Bucket` algorithm for the purpose of rate-limiting access to some abstract resource.
#[derive(Clone)]
pub struct TokenBucket<T = DefaultTimeProvider> {
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

impl<TP> From<(NonZeroRatePerSecond, TP)> for TokenBucket<TP>
where
    TP: TimeProvider,
{
    fn from((rate_per_second, time_provider): (NonZeroRatePerSecond, TP)) -> Self {
        Self::new_with_time_provider(rate_per_second, time_provider)
    }
}

impl<TP> From<NonZeroRatePerSecond> for TokenBucket<TP>
where
    TP: TimeProvider + Default,
{
    fn from(rate_per_second: NonZeroRatePerSecond) -> Self {
        Self::new_with_time_provider(rate_per_second, TP::default())
    }
}

impl TokenBucket {
    /// Constructs a instance of [`TokenBucket`] with given target rate-per-second.
    pub fn new(rate_per_second: NonZeroRatePerSecond) -> Self {
        Self::new_with_time_provider(rate_per_second, DefaultTimeProvider)
    }
}

impl<T> TokenBucket<T>
where
    T: TimeProvider,
{
    fn new_with_time_provider(rate_per_second: NonZeroRatePerSecond, time_provider: T) -> Self {
        Self {
            last_update: time_provider.now(),
            rate_per_second: rate_per_second.into(),
            requested: rate_per_second.into(),
            time_provider,
        }
    }

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

    fn calculate_delay(&self) -> Option<Deadline> {
        if self.available().is_some() {
            return None;
        }

        let scheduled_for_later = self.requested - u64::from(self.rate_per_second);
        let delay_micros = scheduled_for_later
            .saturating_mul(1_000_000)
            .saturating_div(self.rate_per_second.into());

        Some(Deadline::Instant(
            self.last_update + Duration::from_micros(delay_micros),
        ))
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

    /// Get current rate in bits per second for an instance of [`TokenBucket`].
    pub fn rate(&self) -> NonZeroRatePerSecond {
        self.rate_per_second.into()
    }

    /// Set a rate in bits per second for and instance of [`TokenBucket`].
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
            self.requested =
                self.requested - previous_rate_per_second + self.max_possible_available_tokens();
        }
    }

    /// Calculates [Duration](time::Duration) by which we should delay next call to some governed resource in order to satisfy
    /// configured rate limit.
    pub fn rate_limit(&mut self, requested: u64) -> Option<Deadline> {
        trace!(
            target: LOG_TARGET,
            "TokenBucket called for {} of requested bytes. Internal state: {:?}.",
            requested,
            self
        );
        let now_available = self.available().unwrap_or(0);
        if now_available < requested {
            self.update_tokens()
        }
        self.account_requested_tokens(requested);
        self.calculate_delay()
    }
}

impl<TP> RateLimiter for TokenBucket<TP>
where
    TP: TimeProvider,
{
    fn rate_limit(&mut self, requested: u64) -> Option<Deadline> {
        TokenBucket::rate_limit(self, requested)
    }
}

/// Implementations should allow to share some given bandwidth within multiple connections. It
/// allows to track number of active connections and is responsible for notifying all existing
/// connections of any changes of their currently allocated bandwidth.
pub trait SharedBandwidth {
    /// Based on the given number of requested bits to process, allocate and return a non-zero rate
    /// of bits per second.
    fn request_bandwidth(
        &mut self,
        requested: u64,
    ) -> impl Future<Output = NonZeroRatePerSecond> + Send;
    /// Notify this manager that we already processed requested data and no longer use our allocated
    /// bandwidth.
    fn notify_idle(&mut self);
    /// Allows to await any changes in bandwidth configuration that was allocated for this instance.
    fn await_bandwidth_change(&mut self) -> impl Future<Output = NonZeroRatePerSecond> + Send;
}

#[derive(Clone)]
pub struct SharedBandwidthManager {
    max_rate: NonZeroRatePerSecond,
    update_mutex: Arc<parking_lot::Mutex<()>>,
    notify: Arc<tokio::sync::Notify>,
    peer_counter: Arc<AtomicU64>,
    already_requested: Option<NonZeroRatePerSecond>,
}

impl SharedBandwidthManager {
    fn request_bandwidth_without_children_increament(&mut self) -> NonZeroRatePerSecond {
        let active_children = self.peer_counter.load(Ordering::Relaxed);
        let rate = u64::from(self.max_rate) / active_children;
        rate.try_into().unwrap_or(MIN)
    }
}

impl From<NonZeroRatePerSecond> for SharedBandwidthManager {
    fn from(rate: NonZeroRatePerSecond) -> Self {
        Self::new(rate)
    }
}

impl SharedBandwidthManager {
    pub fn new(max_rate: NonZeroRatePerSecond) -> Self {
        Self {
            max_rate,
            update_mutex: Arc::new(Mutex::new(())),
            notify: Arc::new(tokio::sync::Notify::new()),
            peer_counter: Arc::new(0.into()),
            already_requested: None,
        }
    }
}

impl SharedBandwidth for SharedBandwidthManager {
    async fn request_bandwidth(&mut self, _requested: u64) -> NonZeroRatePerSecond {
        if let Some(requested_rate) = self.already_requested {
            return requested_rate;
        }
        if let Some(_guard) = self.update_mutex.try_lock() {
            self.notify.notify_waiters();
        }
        let active_children = self.peer_counter.fetch_add(1, Ordering::Relaxed) + 1;
        let rate = u64::from(self.max_rate) / active_children;
        *self
            .already_requested
            .insert(rate.try_into().unwrap_or(MIN))
    }

    fn notify_idle(&mut self) {
        if self.already_requested.is_none() {
            return;
        }
        self.already_requested = None;
        self.peer_counter.fetch_sub(1, Ordering::Relaxed);
        if let Some(_guard) = self.update_mutex.try_lock() {
            self.notify.notify_waiters();
        }
    }

    async fn await_bandwidth_change(&mut self) -> NonZeroRatePerSecond {
        self.notify.notified().await;
        self.request_bandwidth_without_children_increament()
    }
}

pub trait AsyncRateLimiter {
    fn rate_limit(&mut self, requested: u64) -> Option<Deadline>;
    fn set_rate(&mut self, rate: NonZeroRatePerSecond);
    fn wait(&mut self) -> impl std::future::Future<Output = ()> + std::marker::Send;
}

#[derive(Clone)]
pub struct AsyncTokenBucket<TP = DefaultTimeProvider, SU = TokioSleepUntil> {
    token_bucket: TokenBucket<TP>,
    next_deadline: Option<Deadline>,
    sleep_until: SU,
}

impl<TP, SU> From<NonZeroRatePerSecond> for AsyncTokenBucket<TP, SU>
where
    TP: TimeProvider + Default,
    SU: Default,
{
    fn from(value: NonZeroRatePerSecond) -> Self {
        Self::new(TokenBucket::from(value), SU::default())
    }
}

impl<TP, SU> AsyncTokenBucket<TP, SU>
where
    TP: TimeProvider,
{
    pub fn new(token_bucket: TokenBucket<TP>, sleep_until: SU) -> Self {
        Self {
            token_bucket,
            next_deadline: None,
            sleep_until,
        }
    }
}

impl<TP, SU> AsyncRateLimiter for AsyncTokenBucket<TP, SU>
where
    TP: TimeProvider,
    SU: SleepUntil + Send,
{
    fn rate_limit(&mut self, requested: u64) -> Option<Deadline> {
        self.next_deadline = TokenBucket::rate_limit(&mut self.token_bucket, requested);
        self.next_deadline
    }

    fn set_rate(&mut self, rate: NonZeroRatePerSecond) {
        if self.token_bucket.rate() != rate {
            self.token_bucket.set_rate(rate);
            self.next_deadline = self.token_bucket.rate_limit(0);
        }
    }

    fn wait(&mut self) -> impl std::future::Future<Output = ()> + std::marker::Send {
        async {
            match self.next_deadline {
                Some(Deadline::Instant(deadline)) => {
                    self.sleep_until.sleep_until(deadline).await;
                }
                Some(Deadline::Never) => pending().await,
                _ => {}
            }
        }
    }
}

pub trait SleepUntil {
    fn sleep_until(&mut self, instant: Instant) -> impl Future<Output = ()> + Send;
}

#[derive(Clone)]
pub struct TokioSleepUntil;

impl Default for TokioSleepUntil {
    fn default() -> Self {
        Self {}
    }
}

impl SleepUntil for TokioSleepUntil {
    fn sleep_until(&mut self, instant: Instant) -> impl Future<Output = ()> + Send {
        async move {
            tokio::time::sleep_until(instant.into()).await;
        }
    }
}

#[derive(Clone)]
pub struct HierarchicalTokenBucket<
    BD = SharedBandwidthManager,
    ARL = AsyncTokenBucket<DefaultTimeProvider, TokioSleepUntil>,
> where
    BD: SharedBandwidth,
{
    shared_bandwidth: BD,
    rate_limiter: ARL,
    need_to_notify_parent: bool,
}

impl<BD, ARL> From<NonZeroRatePerSecond> for HierarchicalTokenBucket<BD, ARL>
where
    BD: SharedBandwidth + From<NonZeroRatePerSecond>,
    // ARL: From<NonZeroRatePerSecond>,
    ARL: From<NonZeroRatePerSecond>,
{
    fn from(rate: NonZeroRatePerSecond) -> Self {
        HierarchicalTokenBucket::new(rate)
    }
}

impl<BD, ARL> From<(BD, ARL)> for HierarchicalTokenBucket<BD, ARL>
where
    BD: SharedBandwidth,
{
    fn from((shared_bandwidth, async_token_bucket): (BD, ARL)) -> Self {
        Self {
            shared_bandwidth,
            rate_limiter: async_token_bucket,
            need_to_notify_parent: false,
        }
    }
}

impl<BD, ARL> HierarchicalTokenBucket<BD, ARL>
where
    BD: SharedBandwidth,
{
    pub fn new(rate: NonZeroRatePerSecond) -> Self
    where
        BD: From<NonZeroRatePerSecond>,
        ARL: From<NonZeroRatePerSecond>,
    {
        Self {
            shared_bandwidth: BD::from(rate),
            rate_limiter: ARL::from(rate),
            need_to_notify_parent: false,
        }
    }

    async fn request_bandwidth(&mut self, requested: u64) -> NonZeroRatePerSecond
    where
        BD: SharedBandwidth,
    {
        self.need_to_notify_parent = true;
        self.shared_bandwidth.request_bandwidth(requested).await
    }

    fn notify_idle(&mut self) {
        if self.need_to_notify_parent {
            self.shared_bandwidth.notify_idle();
            self.need_to_notify_parent = false;
        }
    }

    pub async fn rate_limit(mut self, requested: u64) -> Self
    where
        ARL: AsyncRateLimiter,
    {
        let rate = self.request_bandwidth(requested).await;
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

impl<BD, ARL> Drop for HierarchicalTokenBucket<BD, ARL>
where
    BD: SharedBandwidth,
{
    fn drop(&mut self) {
        self.notify_idle();
    }
}

impl<BD, ARL> SleepingRateLimiter for HierarchicalTokenBucket<BD, ARL>
where
    BD: SharedBandwidth + Send,
    ARL: AsyncRateLimiter + Send,
{
    async fn rate_limit(self, read_size: usize) -> Self {
        HierarchicalTokenBucket::rate_limit(self, read_size.try_into().unwrap_or(u64::MAX)).await
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        cmp::max,
        iter::repeat,
        ops::DerefMut,
        rc::Rc,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use futures::{
        future::{BoxFuture, Future},
        stream::FuturesOrdered,
        StreamExt,
    };
    use parking_lot::Mutex;
    use tokio::{sync::Barrier, task::yield_now};

    use super::{AsyncRateLimiter, SharedBandwidth, SleepUntil, TimeProvider, TokenBucket};
    use crate::{
        rate_limiter::RateLimiter,
        token_bucket::{
            AsyncTokenBucket, Deadline, HierarchicalTokenBucket, NonZeroRatePerSecond,
            SharedBandwidthManager,
        },
    };

    impl<BD, ARL, TP, SU> From<(NonZeroRatePerSecond, TP, SU)> for HierarchicalTokenBucket<BD, ARL>
    where
        BD: From<NonZeroRatePerSecond> + SharedBandwidth,
        ARL: From<(NonZeroRatePerSecond, TP, SU)>,
        TP: TimeProvider,
    {
        fn from((rate, time_provider, sleep_until): (NonZeroRatePerSecond, TP, SU)) -> Self {
            let shared_bandwidth = BD::from(rate);
            let async_token_bucket = ARL::from((rate, time_provider, sleep_until));
            Self {
                shared_bandwidth,
                rate_limiter: async_token_bucket,
                need_to_notify_parent: false,
            }
        }
    }

    impl<TP, SU> From<(TokenBucket<TP>, SU)> for AsyncTokenBucket<TP, SU>
    where
        TP: TimeProvider,
        SU: SleepUntil,
    {
        fn from((token_bucket, sleep_until): (TokenBucket<TP>, SU)) -> Self {
            AsyncTokenBucket::new(token_bucket, sleep_until)
        }
    }

    impl<TP, SU> From<(NonZeroRatePerSecond, TP, SU)> for AsyncTokenBucket<TP, SU>
    where
        SU: SleepUntil,
        TP: TimeProvider,
    {
        fn from((rate, time_provider, sleep_until): (NonZeroRatePerSecond, TP, SU)) -> Self {
            let token_bucket = TokenBucket::from((rate, time_provider)).into();
            AsyncTokenBucket::new(token_bucket, sleep_until)
        }
    }

    impl<BD, ARL> RateLimiter for HierarchicalTokenBucket<BD, ARL>
    where
        BD: SharedBandwidth,
        ARL: AsyncRateLimiter,
    {
        fn rate_limit(&mut self, requested: u64) -> Option<Deadline> {
            self.rate_limiter.rate_limit(requested)
        }
    }

    impl<TP> TimeProvider for Arc<TP>
    where
        TP: TimeProvider,
    {
        fn now(&self) -> Instant {
            self.as_ref().now()
        }
    }

    impl<TP> TimeProvider for std::rc::Rc<TP>
    where
        TP: TimeProvider,
    {
        fn now(&self) -> Instant {
            self.as_ref().now()
        }
    }

    impl TimeProvider for Box<dyn TimeProvider> {
        fn now(&self) -> Instant {
            self.as_ref().now()
        }
    }

    impl TimeProvider for Box<dyn TimeProvider + Send + Sync> {
        fn now(&self) -> Instant {
            self.as_ref().now()
        }
    }

    #[derive(Clone)]
    struct TestSleepUntilShared {
        last_instant: Arc<Mutex<Instant>>,
    }

    impl TestSleepUntilShared {
        pub fn new(initial_instant: Instant) -> Self {
            Self {
                last_instant: Arc::new(Mutex::new(initial_instant)),
            }
        }

        pub fn latest_sleep_until(&self) -> Arc<Mutex<Instant>> {
            self.last_instant.clone()
        }
    }

    impl SleepUntil for TestSleepUntilShared {
        fn sleep_until(&mut self, instant: Instant) -> impl Future<Output = ()> + Send {
            async move {
                let mut last_instant = self.last_instant.lock();
                *last_instant = max(*last_instant, instant);
            }
        }
    }

    impl
        From<(
            NonZeroRatePerSecond,
            Rc<Box<dyn TimeProvider>>,
            TestSleepUntilShared,
        )> for TokenBucket<Rc<Box<dyn TimeProvider>>>
    {
        fn from(
            (rate, time_provider, _): (
                NonZeroRatePerSecond,
                Rc<Box<dyn TimeProvider>>,
                TestSleepUntilShared,
            ),
        ) -> Self {
            TokenBucket::new_with_time_provider(rate, time_provider)
        }
    }

    #[test]
    fn rate_limiter_sanity_check() {
        token_bucket_sanity_check_test::<TokenBucket<_>>();
        token_bucket_sanity_check_test::<
            HierarchicalTokenBucket<SharedBandwidthManager, AsyncTokenBucket<_, _>>,
        >()
    }

    fn token_bucket_sanity_check_test<RL>()
    where
        RL: RateLimiter
            + From<(
                NonZeroRatePerSecond,
                Rc<Box<dyn TimeProvider>>,
                TestSleepUntilShared,
            )>,
    {
        let limit_per_second = 10.try_into().expect("10 > 0 qed");
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> =
            Box::new(move || *time_provider.as_ref().borrow());
        let mut rate_limiter = RL::from((
            limit_per_second,
            time_provider.into(),
            TestSleepUntilShared::new(now),
        ));

        *time_to_return.borrow_mut() = now + Duration::from_secs(1);
        assert!(rate_limiter.rate_limit(9).is_none());

        *time_to_return.borrow_mut() = now + Duration::from_secs(1);
        assert!(rate_limiter.rate_limit(12).is_some());

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert!(rate_limiter.rate_limit(8).is_none());
    }

    #[test]
    fn no_slowdown_while_within_rate_limit() {
        no_slowdown_while_within_rate_limit_test::<TokenBucket<_>>();
        no_slowdown_while_within_rate_limit_test::<
            HierarchicalTokenBucket<SharedBandwidthManager, AsyncTokenBucket<_, _>>,
        >()
    }

    fn no_slowdown_while_within_rate_limit_test<RL>()
    where
        RL: RateLimiter
            + From<(
                NonZeroRatePerSecond,
                Rc<Box<dyn TimeProvider>>,
                TestSleepUntilShared,
            )>,
    {
        let limit_per_second = 10.try_into().expect("10 > 0 qed");
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> =
            Box::new(move || *time_provider.as_ref().borrow());
        let mut rate_limiter = RL::from((
            limit_per_second,
            time_provider.into(),
            TestSleepUntilShared::new(now),
        ));

        *time_to_return.borrow_mut() = now + Duration::from_secs(1);
        assert_eq!(rate_limiter.rate_limit(9), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(2);
        assert_eq!(rate_limiter.rate_limit(5), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(rate_limiter.rate_limit(1), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(rate_limiter.rate_limit(9), None);
    }

    #[test]
    fn slowdown_when_limit_reached_token_bucket() {
        slowdown_when_limit_reached_test::<TokenBucket<_>>();
        slowdown_when_limit_reached_test::<
            HierarchicalTokenBucket<SharedBandwidthManager, AsyncTokenBucket<_, _>>,
        >()
    }

    fn slowdown_when_limit_reached_test<RL>()
    where
        RL: RateLimiter
            + From<(
                NonZeroRatePerSecond,
                Rc<Box<dyn TimeProvider>>,
                TestSleepUntilShared,
            )>,
    {
        let limit_per_second = 10.try_into().expect("10 > 0 qed");
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> =
            Box::new(move || *time_provider.as_ref().borrow());
        let mut rate_limiter = RL::from((
            limit_per_second,
            time_provider.into(),
            TestSleepUntilShared::new(now),
        ));

        *time_to_return.borrow_mut() = now;
        assert_eq!(
            rate_limiter.rate_limit(10),
            Some(Deadline::Instant(now + Duration::from_secs(1)))
        );

        // we should wait some time after reaching the limit
        *time_to_return.borrow_mut() = now + Duration::from_secs(1);
        assert!(rate_limiter.rate_limit(1).is_some());

        *time_to_return.borrow_mut() = now + Duration::from_secs(1);
        assert_eq!(
            rate_limiter.rate_limit(19),
            Some(Deadline::Instant(now + Duration::from_secs(3))),
            "we should wait exactly 2 seconds"
        );
    }

    #[test]
    fn buildup_tokens_but_no_more_than_limit_token_bucket() {
        buildup_tokens_but_no_more_than_limit_test::<TokenBucket<_>>();
        buildup_tokens_but_no_more_than_limit_test::<
            HierarchicalTokenBucket<SharedBandwidthManager, AsyncTokenBucket<_, _>>,
        >()
    }

    fn buildup_tokens_but_no_more_than_limit_test<RL>()
    where
        RL: RateLimiter
            + From<(
                NonZeroRatePerSecond,
                Rc<Box<dyn TimeProvider>>,
                TestSleepUntilShared,
            )>,
    {
        let limit_per_second = 10.try_into().expect("10 > 0 qed");
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> =
            Box::new(move || *time_provider.as_ref().borrow());
        let mut rate_limiter = RL::from((
            limit_per_second,
            time_provider.into(),
            TestSleepUntilShared::new(now),
        ));

        *time_to_return.borrow_mut() = now + Duration::from_secs(2);
        assert_eq!(rate_limiter.rate_limit(10), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(10);
        assert_eq!(
            rate_limiter.rate_limit(40),
            Some(Deadline::Instant(
                now + Duration::from_secs(10) + Duration::from_secs(3)
            )),
        );

        *time_to_return.borrow_mut() = now + Duration::from_secs(11);
        assert_eq!(
            rate_limiter.rate_limit(40),
            Some(Deadline::Instant(
                now + Duration::from_secs(11) + Duration::from_secs(6)
            ))
        );
    }

    #[test]
    fn multiple_calls_buildup_wait_time() {
        multiple_calls_buildup_wait_time_test::<TokenBucket<_>>();
        multiple_calls_buildup_wait_time_test::<
            HierarchicalTokenBucket<SharedBandwidthManager, AsyncTokenBucket<_, _>>,
        >()
    }

    fn multiple_calls_buildup_wait_time_test<RL>()
    where
        RL: RateLimiter
            + From<(
                NonZeroRatePerSecond,
                Rc<Box<dyn TimeProvider>>,
                TestSleepUntilShared,
            )>,
    {
        let limit_per_second = 10.try_into().expect("10 > 0 qed");
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> =
            Box::new(move || *time_provider.as_ref().borrow());
        let mut rate_limiter = RL::from((
            limit_per_second,
            time_provider.into(),
            TestSleepUntilShared::new(now),
        ));

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(rate_limiter.rate_limit(10), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(
            rate_limiter.rate_limit(10),
            Some(Deadline::Instant(now + Duration::from_secs(4)))
        );

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(
            rate_limiter.rate_limit(10),
            Some(Deadline::Instant(
                now + Duration::from_secs(4) + Duration::from_secs(1)
            ))
        );

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(
            rate_limiter.rate_limit(50),
            Some(Deadline::Instant(
                now + Duration::from_secs(4) + Duration::from_secs(6)
            ))
        );
    }

    struct TestBandwidthManager<BM> {
        wrapped: BM,
        waiters_counter: Arc<AtomicUsize>,
        wait: Arc<parking_lot::RwLock<Barrier>>,
    }

    impl<BM> TestBandwidthManager<BM> {
        pub fn new(bandwidth_manager: BM) -> Self {
            let wait = Arc::new(parking_lot::RwLock::new(Barrier::new(1)));
            Self {
                wrapped: bandwidth_manager,
                wait,
                waiters_counter: Arc::new(AtomicUsize::new(1)),
            }
        }
    }

    impl<BM> Clone for TestBandwidthManager<BM>
    where
        BM: Clone,
    {
        fn clone(&self) -> Self {
            let mut lock = self.wait.write();
            let waiters_counter = self.waiters_counter.fetch_add(1, Ordering::Relaxed) + 1;
            *lock = Barrier::new(waiters_counter);
            Self {
                wrapped: self.wrapped.clone(),
                wait: self.wait.clone(),
                waiters_counter: self.waiters_counter.clone(),
            }
        }
    }

    impl<BM> SharedBandwidth for TestBandwidthManager<BM>
    where
        BM: SharedBandwidth + Send,
    {
        async fn request_bandwidth(&mut self, requested: u64) -> NonZeroRatePerSecond {
            self.wrapped.request_bandwidth(requested).await
        }

        fn notify_idle(&mut self) {
            self.wrapped.notify_idle();
        }

        async fn await_bandwidth_change(&mut self) -> NonZeroRatePerSecond {
            self.wrapped.await_bandwidth_change().await
        }
    }

    impl<BD> From<NonZeroRatePerSecond> for TestBandwidthManager<BD>
    where
        BD: From<NonZeroRatePerSecond> + SharedBandwidth,
    {
        fn from(value: NonZeroRatePerSecond) -> Self {
            let shared_bandwidth = BD::from(value);
            Self::new(shared_bandwidth)
        }
    }

    #[tokio::test]
    async fn two_peers_can_share_bandwidth() {
        let limit_per_second = 10.try_into().expect("10 > 0 qed");
        let now = Instant::now();
        let time_to_return = Arc::new(Mutex::new(now));
        let time_provider_value = time_to_return.clone();
        let time_provider: Arc<Box<dyn TimeProvider + Send + Sync>> =
            Arc::new(Box::new(move || *time_provider_value.lock()));

        let last_deadline = time_to_return.clone();

        let mut rate_limiter =
            HierarchicalTokenBucket::<SharedBandwidthManager, AsyncTokenBucket<_, _>>::from((
                limit_per_second,
                time_provider,
                TestSleepUntil::new(last_deadline.clone()),
            ));
        let mut rate_limiter_cloned = rate_limiter.clone();

        let total_data_sent = thread::scope(|s| {
            let first_handle = s.spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                runtime.block_on(async move {
                    rate_limiter = rate_limiter.rate_limit(10).await;
                    rate_limiter.rate_limit(30).await;
                });
                10 + 30
            });

            let second_handle = s.spawn(|| {
                // return 0u128;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                runtime.block_on(async move {
                    rate_limiter_cloned = rate_limiter_cloned.rate_limit(10).await;
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
        let duration = *last_deadline.lock() - now;
        let rate = total_data_sent * 1000 / duration.as_millis();
        assert!(
            rate.abs_diff(10) <= 3,
            "calculated bandwidth should be within some error bounds: rate = {rate}; duration = {duration:?}"
        );
    }

    #[test]
    fn single_peer_can_use_whole_bandwidth_when_needed() {
        let limit_per_second = 10.try_into().expect("10 > 0 qed");
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Rc<Box<dyn TimeProvider>> =
            Rc::new(Box::new(move || *time_provider.as_ref().borrow()));

        let mut rate_limiter =
            HierarchicalTokenBucket::<SharedBandwidthManager, AsyncTokenBucket<_, _>>::from((
                limit_per_second,
                time_provider,
                TestSleepUntilShared::new(now),
            ));

        let mut rate_limiter_cloned = rate_limiter.clone();

        assert_eq!(
            RateLimiter::rate_limit(&mut rate_limiter, 5),
            Some(Deadline::Instant(now + Duration::from_millis(500)))
        );
        assert_eq!(
            RateLimiter::rate_limit(&mut rate_limiter_cloned, 5),
            Some(Deadline::Instant(now + Duration::from_millis(500)))
        );

        *time_to_return.borrow_mut() = now + Duration::from_millis(1500);

        assert_eq!(RateLimiter::rate_limit(&mut rate_limiter, 10), None);
    }

    #[test]
    fn peers_receive_at_least_one_token_per_second() {
        let limit_per_second = 1.try_into().expect("1 > 0 qed");
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Rc<Box<dyn TimeProvider>> =
            Rc::new(Box::new(move || *time_provider.as_ref().borrow()));

        let mut rate_limiter =
            HierarchicalTokenBucket::<SharedBandwidthManager, AsyncTokenBucket<_, _>>::from((
                limit_per_second,
                time_provider,
                TestSleepUntilShared::new(now),
            ));

        *time_to_return.borrow_mut() = now + Duration::from_secs(1);

        let mut rate_limiter_cloned = rate_limiter.clone();

        assert_eq!(RateLimiter::rate_limit(&mut rate_limiter, 1), None);
        assert_eq!(RateLimiter::rate_limit(&mut rate_limiter_cloned, 1), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(2);

        assert_eq!(RateLimiter::rate_limit(&mut rate_limiter, 1), None);
        assert_eq!(
            RateLimiter::rate_limit(&mut rate_limiter_cloned, 2),
            Some(Deadline::Instant(now + Duration::from_secs(3)))
        );
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
                initial_counter: self.initial_counter.clone(),
                counter: self.counter.clone(),
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
        fn sleep_until(&mut self, instant: Instant) -> impl Future<Output = ()> + Send {
            async move {
                self.wait().await;
                self.wrapped.sleep_until(instant).await;
            }
        }
    }
    #[derive(Clone)]
    struct TracingSleepUntil<SU> {
        wrapped: SU,
        last_deadline: Instant,
    }

    impl<SU> TracingSleepUntil<SU> {
        pub fn new(sleep_until: SU, initial_deadline: Instant) -> Self {
            Self {
                wrapped: sleep_until,
                last_deadline: initial_deadline,
            }
        }
    }

    impl<SU> SleepUntil for TracingSleepUntil<SU>
    where
        SU: SleepUntil + Send,
    {
        fn sleep_until(&mut self, instant: Instant) -> impl Future<Output = ()> + Send {
            async move {
                self.last_deadline = instant;
                self.wrapped.sleep_until(instant).await
            }
        }
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
        let time_rand_gen = RefCell::new(rand::rngs::StdRng::seed_from_u64(seed));

        let data_gen = Uniform::from(0..100 * 1024 * 1024);
        let time_gen = Uniform::from(1..1000 * 10);
        let limiters_count = Uniform::from(10..=128).sample(&mut rand_gen);
        let batch_gen = Uniform::from(1..limiters_count);

        let rate_limit = 4 * 1024 * 1024;
        let limit_per_second = rate_limit.try_into().expect("(4 * 1024 * 1024) > 0 qed");
        let rate_limit = rate_limit.into();
        let initial_time = Instant::now();

        let test_sleep_until_shared = TestSleepUntilShared::new(initial_time);
        let last_deadline = test_sleep_until_shared.latest_sleep_until();
        let last_deadline_from_time_provider = last_deadline.clone();

        let time_to_return = Rc::new(RefCell::new(initial_time));
        let time_provider = time_to_return.clone();
        let time_provider: Rc<Box<dyn TimeProvider>> = Rc::new(Box::new(move || {
            // TODO this broke Barrier in sleep_until
            // return *time_provider.borrow();
            let mut current_time = time_provider.borrow_mut();
            let millis_from_current_time: u64 = last_deadline_from_time_provider
                .lock()
                .duration_since(*current_time)
                .as_millis()
                .try_into()
                .expect("something wrong with our `time` calculations");
            let time_gen = Uniform::from(1..1000 * 10 + millis_from_current_time);
            let time_passed =
                Duration::from_millis(time_gen.sample(time_rand_gen.borrow_mut().deref_mut()));
            *current_time += time_passed;
            *current_time

            // let mut current_time = time_provider.borrow_mut();
            // *current_time += Duration::from_micros(1);
            // *current_time

            // *last_deadline_from_time_provider.lock()
        }));

        let barrier = Arc::new(tokio::sync::RwLock::new(tokio::sync::Barrier::new(0)));
        let how_many_times_stop_on_barrier = 3;
        let test_sleep_until_with_barrier = SleepUntilWithBarrier::new(
            test_sleep_until_shared,
            barrier.clone(),
            how_many_times_stop_on_barrier,
        );
        let tracing_sleep_until =
            TracingSleepUntil::new(test_sleep_until_with_barrier, initial_time);
        let rate_limiter =
            HierarchicalTokenBucket::<SharedBandwidthManager, AsyncTokenBucket<_, _>>::from((
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
        while total_number_of_calls < 10000 {
            let batch_size = batch_gen.sample(&mut rand_gen);
            println!("batch size = {batch_size}");

            total_number_of_calls += batch_size;
            *barrier.write().await = tokio::sync::Barrier::new(batch_size);

            rate_limiters.shuffle(&mut rand_gen);
            let batch = &mut rate_limiters[0..batch_size];

            let current_time = *time_to_return.as_ref().borrow();
            let mut batch_test: FuturesOrdered<_> = batch
                .into_iter()
                .map(|(selected_limiter_id, selected_rate_limiter)| {
                    let data_read = data_gen.sample(&mut rand_gen);

                    let mut rate_limiter = selected_rate_limiter
                        .take()
                        .expect("we should be able to retrieve a rate-limiter");
                    rate_limiter.rate_limiter.sleep_until.wrapped.reset();

                    let last_deadline = rate_limiter.rate_limiter.sleep_until.last_deadline;
                    let task_start_time = max(last_deadline, current_time);

                    let rate_task = HierarchicalTokenBucket::rate_limit(rate_limiter, data_read);

                    // TODO use TracingSleepUntil here
                    test_state.push((*selected_limiter_id, data_read));

                    total_data_scheduled += u128::from(data_read);

                    async move {
                        let mut rate_limiter = rate_task.await;
                        rate_limiter.rate_limiter.sleep_until.wrapped.wait().await;

                        let next_deadline = rate_limiter.rate_limiter.sleep_until.last_deadline;
                        // let time_passed = next_deadline - last_deadline;
                        let time_passed = next_deadline - task_start_time;
                        let mut result_rate = 0;
                        if time_passed.as_millis() != 0 {
                            result_rate = u128::from(data_read) * 1000 / time_passed.as_millis();
                        }
                        println!("calculated rate: {result_rate}");

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
            let current_time = max(*time_to_return.as_ref().borrow(), *last_deadline.lock());
            *time_to_return.borrow_mut() = current_time;
        }
        // TODO try to compute real rate instead of this
        // let abs_rate_diff = total_rate.abs_diff(rate_limit);
        let time_passed = *last_deadline.lock() - initial_time;
        let total_rate = total_data_scheduled * 1000 / time_passed.as_millis();
        let abs_rate_diff = total_rate.abs_diff(rate_limit);
        assert!(
            abs_rate_diff <= rate_limit * 5/10,
            "Used bandwidth should be oscillating close to {rate_limit} b/s (+/- 50%), but got {total_rate} b/s instead. Total data sent: {total_data_scheduled}; Time: {time_passed:?}"
        );
        // panic!("expected rate-limit = {rate_limit} calculated rate limit = {total_rate}");
    }

    #[derive(Clone)]
    struct TestSleepUntil {
        last_instant: Arc<Mutex<Instant>>,
    }

    impl TestSleepUntil {
        pub fn new(initial_instant: Arc<Mutex<Instant>>) -> Self {
            Self {
                last_instant: initial_instant,
            }
        }
    }

    impl SleepUntil for TestSleepUntil {
        fn sleep_until(&mut self, instant: Instant) -> impl Future<Output = ()> + Send {
            let mut last_instant = self.last_instant.lock();
            println!(
                "last instant was {:?}; diff = {:?}",
                *last_instant,
                instant - *last_instant
            );
            *last_instant = max(*last_instant, instant);
            async {}
        }
    }
}
