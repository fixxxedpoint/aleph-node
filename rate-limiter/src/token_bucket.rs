use crate::{NonZeroRatePerSecond, RateLimiterSleeper, RatePerSecond, LOG_TARGET};
use futures::{future::pending, Future, FutureExt};
use log::trace;
use std::{
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

pub struct RateLimitResult {
    pub delay: Instant,
    pub scheduled_for_later: u64,
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum Deadline {
    Never,
    Instant(Instant),
}

impl PartialOrd for Deadline {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Deadline {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (Deadline::Never, Deadline::Never) => Ordering::Equal,
            (Deadline::Never, Deadline::Instant(_)) => Ordering::Greater,
            (Deadline::Instant(_), Deadline::Never) => Ordering::Less,
            (Deadline::Instant(self_instant), Deadline::Instant(other_instant)) => {
                self_instant.cmp(other_instant)
            }
        }
    }
}

impl From<Deadline> for Option<Instant> {
    fn from(value: Deadline) -> Self {
        match value {
            Deadline::Never => None,
            Deadline::Instant(value) => Some(value),
        }
    }
}

impl From<Instant> for Deadline {
    fn from(value: Instant) -> Self {
        Deadline::Instant(value)
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
    fn from(
        (NonZeroRatePerSecond(rate_per_second), time_provider): (NonZeroRatePerSecond, TP),
    ) -> Self {
        Self::new_with_time_provider(rate_per_second, time_provider)
    }
}

impl<TP> From<NonZeroRatePerSecond> for TokenBucket<TP>
where
    TP: TimeProvider + Default,
{
    fn from(NonZeroRatePerSecond(rate_per_second): NonZeroRatePerSecond) -> Self {
        Self::new_with_time_provider(rate_per_second, TP::default())
    }
}

impl TokenBucket {
    /// Constructs a instance of [`TokenBucket`] with given target rate-per-second.
    pub fn new(rate_per_second: NonZeroU64) -> Self {
        Self::new_with_time_provider(rate_per_second, DefaultTimeProvider)
    }
}

impl<T> TokenBucket<T>
where
    T: TimeProvider,
{
    fn new_with_time_provider(rate_per_second: NonZeroU64, time_provider: T) -> Self {
        Self {
            last_update: time_provider.now(),
            rate_per_second,
            requested: 0,
            time_provider,
        }
    }

    fn available(&self) -> u64 {
        u64::from(self.rate_per_second).saturating_sub(self.requested)
    }

    fn account_requested(&mut self, requested: u64) {
        self.requested = self.requested.saturating_add(requested);
    }

    fn calculate_delay(&self) -> Option<RateLimitResult> {
        if self.available() > 0 {
            return None;
        }

        let scheduled_for_later = self.requested - u64::from(self.rate_per_second);
        let delay_micros = scheduled_for_later
            .saturating_mul(1_000_000)
            .saturating_div(self.rate_per_second.into());

        Some(RateLimitResult {
            delay: self.last_update + Duration::from_micros(delay_micros),
            scheduled_for_later,
        })
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
            .as_millis()
            .saturating_mul(u64::from(self.rate_per_second).into())
            .saturating_div(1_000_000)
            .try_into()
            .unwrap_or(u64::MAX);
        self.requested = self.requested.saturating_sub(new_units);
    }

    pub fn rate(&self) -> NonZeroRatePerSecond {
        self.rate_per_second.into()
    }

    pub fn set_rate(&mut self, rate_per_second: NonZeroRatePerSecond) {
        self.update_tokens();
        self.rate_per_second = rate_per_second.into();
    }

    /// Calculates [Duration](time::Duration) by which we should delay next call to some governed resource in order to satisfy
    /// configured rate limit.
    pub fn rate_limit(&mut self, requested: u64) -> Option<RateLimitResult> {
        trace!(
            target: LOG_TARGET,
            "TokenBucket called for {} of requested bytes. Internal state: {:?}.",
            requested,
            self
        );
        let now_available = self.available();
        if now_available < requested {
            self.update_tokens()
        }
        self.account_requested(requested);
        self.calculate_delay()
    }
}

pub trait RateLimiter {
    fn rate_limit(&mut self, requested: u64) -> Option<Deadline>;
}

impl<TP> RateLimiter for TokenBucket<TP>
where
    TP: TimeProvider,
{
    fn rate_limit(&mut self, requested: u64) -> Option<Deadline> {
        Some(Deadline::Instant(
            TokenBucket::rate_limit(self, requested).map(|result| result.delay)?,
        ))
    }
}

#[derive(Clone)]
pub enum RateLimiterFacade<RL> {
    NoTraffic,
    RateLimiter(RL),
}

impl<RL> RateLimiterFacade<RL> {
    pub fn new(rate: RatePerSecond) -> Self
    where
        RL: From<NonZeroRatePerSecond>,
    {
        match rate {
            RatePerSecond::Block => Self::NoTraffic,
            RatePerSecond::Rate(rate) => Self::RateLimiter(rate.into()),
        }
    }

    pub fn rate_limit(&mut self, requested: u64) -> Option<Deadline>
    where
        RL: RateLimiter,
    {
        match self {
            RateLimiterFacade::NoTraffic => Some(Deadline::Never),
            RateLimiterFacade::RateLimiter(limiter) => limiter.rate_limit(requested),
        }
    }
}

impl RateLimiterSleeper for RateLimiterFacade<HierarchicalTokenBucket> {
    fn rate_limit(self, read_size: usize) -> impl Future<Output = Self> + Send {
        async move {
            match self {
                RateLimiterFacade::NoTraffic => pending().await,
                RateLimiterFacade::RateLimiter(rate_limiter) => RateLimiterFacade::RateLimiter(
                    rate_limiter
                        .rate_limit(read_size.try_into().unwrap_or(u64::MAX))
                        .await,
                ),
            }
        }
    }
}

struct AtomicSharedBandwidthDivider {
    max_rate: NonZeroRatePerSecond,
    active_children: AtomicU64,
    bandwidth_change: tokio::sync::Notify,
}

impl AtomicSharedBandwidthDivider {
    pub fn new(rate: NonZeroRatePerSecond) -> Self {
        Self {
            max_rate: rate,
            active_children: AtomicU64::new(0),
            bandwidth_change: tokio::sync::Notify::new(),
        }
    }
}

#[derive(Clone)]
pub struct SharedBandwidthImpl(Arc<AtomicSharedBandwidthDivider>);

impl SharedBandwidthImpl {
    fn request_bandwidth_without_children_increament(&self) -> NonZeroRatePerSecond {
        let active_children = self.0.active_children.load(Ordering::Relaxed);
        let rate = u64::from(self.0.max_rate) / active_children;
        NonZeroRatePerSecond(NonZeroU64::new(rate).unwrap_or(NonZeroU64::MIN))
    }
}

impl From<NonZeroRatePerSecond> for SharedBandwidthImpl {
    fn from(rate: NonZeroRatePerSecond) -> Self {
        Self(Arc::new(AtomicSharedBandwidthDivider::new(rate)))
    }
}

impl SharedBandwidth for SharedBandwidthImpl {
    // TODO make it grow exponentially on per-connection basis. It won't depend on number of children, but only how often they are calling it.
    fn request_bandwidth(&mut self, _requested: u64) -> NonZeroRatePerSecond {
        // let active_children = self.active_children.load(Ordering::Relaxed);
        let active_children = self.0.active_children.fetch_add(1, Ordering::Relaxed) + 1;
        let rate = u64::from(self.0.max_rate) / active_children;
        NonZeroRatePerSecond(NonZeroU64::new(rate).unwrap_or(NonZeroU64::MIN))
    }

    fn notify_idle(&mut self) {
        self.0.active_children.fetch_sub(1, Ordering::Relaxed);
        self.0.bandwidth_change.notify_waiters();
    }

    async fn await_bandwidth_change(&mut self) -> NonZeroRatePerSecond {
        self.0.bandwidth_change.notified().await;
        self.request_bandwidth_without_children_increament()
    }
}

pub trait SharedBandwidth {
    // TODO perhaps return something that holds the counter and sub when dropped - meh, nope
    fn request_bandwidth(&mut self, requestd: u64) -> NonZeroRatePerSecond;
    fn notify_idle(&mut self);
    fn await_bandwidth_change(&mut self) -> impl Future<Output = NonZeroRatePerSecond> + Send;
}

#[derive(Clone)]
pub struct AsyncTokenBucket<TP, SU>
where
    TP: TimeProvider,
{
    token_bucket: TokenBucket<TP>,
    next_deadline: Option<Deadline>,
    rate_limit_changed: bool,
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
            rate_limit_changed: true,
            sleep_until,
        }
    }
}

impl<TP, SU> AsyncRateLimiter for AsyncTokenBucket<TP, SU>
where
    TP: TimeProvider,
    SU: SleepUntil + Send,
{
    fn rate_limit(&mut self, requested: u64) -> Option<RateLimitResult> {
        let result = TokenBucket::rate_limit(&mut self.token_bucket, requested)?;
        self.next_deadline = Some(Deadline::Instant(result.delay));
        Some(result)
    }

    fn set_rate(&mut self, rate: NonZeroRatePerSecond) {
        if self.token_bucket.rate() != rate {
            self.token_bucket.set_rate(rate.into());
            self.rate_limit_changed = true;
        }
    }

    fn wait(&mut self) -> impl std::future::Future<Output = ()> + std::marker::Send {
        if self.rate_limit_changed {
            self.next_deadline = self
                .token_bucket
                .rate_limit(0)
                .map(|result| Deadline::Instant(result.delay));
            self.rate_limit_changed = false;
        }
        async {
            match self.next_deadline {
                Some(Deadline::Instant(deadline)) => self.sleep_until.sleep_until(deadline).await,
                Some(Deadline::Never) => pending().await,
                _ => {}
            }
        }
    }
}

pub trait AsyncRateLimiter {
    fn rate_limit(&mut self, requested: u64) -> Option<RateLimitResult>;
    fn set_rate(&mut self, rate: NonZeroRatePerSecond);
    fn wait(&mut self) -> impl std::future::Future<Output = ()> + std::marker::Send;
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

// zeby ojciec nie musial wysylac sygnalu do wszystkich w sytuacji
// async condvar
#[derive(Clone)]
pub struct HierarchicalTokenBucket<
    BD = SharedBandwidthImpl,
    ARL = AsyncTokenBucket<DefaultTimeProvider, TokioSleepUntil>,
> where
    BD: SharedBandwidth,
{
    shared_parent: BD,
    rate_limiter: ARL,
    need_to_notify_parent: bool,
}

impl<BD, ARL> From<NonZeroRatePerSecond> for HierarchicalTokenBucket<BD, ARL>
where
    BD: SharedBandwidth + From<NonZeroRatePerSecond>,
    ARL: From<NonZeroRatePerSecond>,
{
    fn from(rate: NonZeroRatePerSecond) -> Self {
        HierarchicalTokenBucket::new(rate)
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
            shared_parent: BD::from(rate),
            rate_limiter: ARL::from(rate),
            need_to_notify_parent: false,
        }
    }

    fn request_bandwidth(&mut self, requested: u64) -> NonZeroRatePerSecond
    where
        BD: SharedBandwidth,
    {
        self.need_to_notify_parent = true;
        self.shared_parent.request_bandwidth(requested)
    }

    fn notify_idle(&mut self)
    where
        BD: SharedBandwidth,
    {
        if self.need_to_notify_parent {
            self.need_to_notify_parent = false;
            self.shared_parent.notify_idle();
        }
    }

    pub async fn rate_limit(mut self, requested: u64) -> Self
    where
        BD: SharedBandwidth,
        ARL: AsyncRateLimiter,
    {
        let rate = self.request_bandwidth(requested);
        self.rate_limiter.set_rate(rate);

        self.rate_limiter.rate_limit(requested);

        loop {
            futures::select! {
                _ = self.rate_limiter.wait().fuse() => {
                    self.notify_idle();
                    return self;
                },
                rate = self.shared_parent.await_bandwidth_change().fuse() => {
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

impl<BD, ARL> RateLimiterSleeper for HierarchicalTokenBucket<BD, ARL>
where
    BD: SharedBandwidth + Send,
    ARL: AsyncRateLimiter + Send,
{
    fn rate_limit(self, read_size: usize) -> impl Future<Output = Self> + Send {
        async move {
            HierarchicalTokenBucket::rate_limit(self, read_size.try_into().unwrap_or(u64::MAX))
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        cmp::max,
        iter::repeat,
        rc::Rc,
        sync::Arc,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use futures::Future;
    use parking_lot::Mutex;
    use rand::distributions::Uniform;

    use crate::{
        token_bucket::{
            AsyncTokenBucket, AtomicSharedBandwidthDivider, Deadline, HierarchicalTokenBucket,
            NonZeroRatePerSecond, SharedBandwidthImpl,
        },
        RateLimiterT,
    };

    use super::{SleepUntil, TimeProvider, TokenBucket};

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

    #[test]
    fn rate_limiter_sanity_check() {
        token_bucket_sanity_check_test::<TokenBucket<_>>();
        // token_bucket_sanity_check_test::<HierarchicalRateLimiter<TokenBucket<_>, TokenBucket<_>>>()
    }

    fn token_bucket_sanity_check_test<RL>()
    where
        RL: RateLimiterT + From<(NonZeroRatePerSecond, Rc<Box<dyn TimeProvider>>)>,
    {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> = Box::new(move || *time_provider.borrow());
        let mut rate_limiter = RL::from((limit_per_second, time_provider.into()));

        *time_to_return.borrow_mut() = now + Duration::from_secs(1);
        assert_eq!(rate_limiter.rate_limit(9), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(1);
        assert!(rate_limiter.rate_limit(12).is_some());

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(rate_limiter.rate_limit(8), None);
    }

    #[test]
    fn no_slowdown_while_within_rate_limit() {
        no_slowdown_while_within_rate_limit_test::<TokenBucket<_>>();
        // no_slowdown_while_within_rate_limit_test::<
        //     HierarchicalRateLimiter<TokenBucket<_>, TokenBucket<_>>,
        // >()
    }

    fn no_slowdown_while_within_rate_limit_test<RL>()
    where
        RL: RateLimiterT + From<(NonZeroRatePerSecond, Rc<Box<dyn TimeProvider>>)>,
    {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> = Box::new(move || *time_provider.borrow());
        let mut rate_limiter = RL::from((limit_per_second, time_provider.into()));

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
        // slowdown_when_limit_reached_test::<HierarchicalRateLimiter<TokenBucket<_>, TokenBucket<_>>>(
        // )
    }

    fn slowdown_when_limit_reached_test<RL>()
    where
        RL: RateLimiterT + From<(NonZeroRatePerSecond, Rc<Box<dyn TimeProvider>>)>,
    {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> = Box::new(move || *time_provider.borrow());
        let mut rate_limiter = RL::from((limit_per_second, time_provider.into()));

        *time_to_return.borrow_mut() = now;
        assert_eq!(rate_limiter.rate_limit(10), None);

        // we should wait some time after reaching the limit
        *time_to_return.borrow_mut() = now;
        assert!(rate_limiter.rate_limit(1).is_some());

        *time_to_return.borrow_mut() = now;
        assert_eq!(
            rate_limiter.rate_limit(19),
            Some(Deadline::Instant(now + Duration::from_secs(2))),
            "we should wait exactly 2 seconds"
        );
    }

    #[test]
    fn buildup_tokens_but_no_more_than_limit_token_bucket() {
        buildup_tokens_but_no_more_than_limit_test::<TokenBucket<_>>();
        // buildup_tokens_but_no_more_than_limit_test::<
        //     HierarchicalRateLimiter<TokenBucket<_>, TokenBucket<_>>,
        // >()
    }

    fn buildup_tokens_but_no_more_than_limit_test<RL>()
    where
        RL: RateLimiterT + From<(NonZeroRatePerSecond, Rc<Box<dyn TimeProvider>>)>,
    {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> = Box::new(move || *time_provider.borrow());
        let mut rate_limiter = RL::from((limit_per_second, time_provider.into()));

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
        // multiple_calls_buildup_wait_time_test::<
        //     HierarchicalRateLimiter<TokenBucket<_>, TokenBucket<_>>,
        // >()
    }

    fn multiple_calls_buildup_wait_time_test<RL>()
    where
        RL: RateLimiterT + From<(NonZeroRatePerSecond, Rc<Box<dyn TimeProvider>>)>,
    {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> = Box::new(move || *time_provider.borrow());
        let mut rate_limiter = RL::from((limit_per_second, time_provider.into()));

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(rate_limiter.rate_limit(10), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(rate_limiter.rate_limit(10), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(
            rate_limiter.rate_limit(10),
            Some(Deadline::Instant(
                now + Duration::from_secs(3) + Duration::from_secs(1)
            ))
        );

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(
            rate_limiter.rate_limit(50),
            Some(Deadline::Instant(
                now + Duration::from_secs(3) + Duration::from_secs(6)
            ))
        );
    }

    // #[test]
    // fn two_peers_can_share_rate_limiter() {
    //     let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
    //     let now = Instant::now();
    //     let time_to_return = Rc::new(RefCell::new(now));
    //     let time_provider = time_to_return.clone();
    //     let time_provider: Rc<Box<dyn TimeProvider>> =
    //         Rc::new(Box::new(move || *time_provider.borrow()));

    //     let rate_limiter =
    //         HierarchicalRateLimiter::<TokenBucket<_>, TokenBucket<_>>::new_with_time_provider(
    //             limit_per_second,
    //             time_provider,
    //         );

    //     let rate_limiter_cloned = rate_limiter.clone();

    //     assert_eq!(rate_limiter.rate_limit(5), None);
    //     assert_eq!(rate_limiter_cloned.rate_limit(5), None);

    //     assert_eq!(
    //         rate_limiter.rate_limit(10),
    //         Some(Deadline::Instant(now + Duration::from_secs(2)))
    //     );
    //     assert_eq!(
    //         rate_limiter_cloned.rate_limit(10),
    //         Some(Deadline::Instant(now + Duration::from_secs(2)))
    //     );
    // }

    // #[test]
    // fn single_peer_can_use_whole_bandwidth_when_needed() {
    //     let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
    //     let now = Instant::now();
    //     let time_to_return = Rc::new(RefCell::new(now));
    //     let time_provider = time_to_return.clone();
    //     let time_provider: Rc<Box<dyn TimeProvider>> =
    //         Rc::new(Box::new(move || *time_provider.borrow()));

    //     let rate_limiter =
    //         HierarchicalRateLimiter::<TokenBucket<_>, TokenBucket<_>>::new_with_time_provider(
    //             limit_per_second,
    //             time_provider,
    //         );

    //     let rate_limiter_cloned = rate_limiter.clone();

    //     assert_eq!(rate_limiter.rate_limit(5), None);
    //     assert_eq!(rate_limiter_cloned.rate_limit(5), None);

    //     *time_to_return.borrow_mut() = now + Duration::from_secs(1);

    //     assert_eq!(rate_limiter.rate_limit(10), None);
    //     assert_eq!(
    //         rate_limiter_cloned.rate_limit(10),
    //         Some(Deadline::Instant(now + Duration::from_secs(2)))
    //     );
    // }

    // #[test]
    // fn peers_receive_at_least_one_token_per_second() {
    //     let limit_per_second = NonZeroRatePerSecond(1.try_into().expect("10 > 0 qed"));
    //     let now = Instant::now();
    //     let time_to_return = Rc::new(RefCell::new(now));
    //     let time_provider = time_to_return.clone();
    //     let time_provider: Rc<Box<dyn TimeProvider>> =
    //         Rc::new(Box::new(move || *time_provider.borrow()));

    //     let rate_limiter =
    //         HierarchicalRateLimiter::<TokenBucket<_>, TokenBucket<_>>::new_with_time_provider(
    //             limit_per_second,
    //             time_provider,
    //         );

    //     let rate_limiter_cloned = rate_limiter.clone();

    //     assert_eq!(rate_limiter.rate_limit(1), None);
    //     // TODO
    //     assert_eq!(rate_limiter_cloned.rate_limit(1), None);

    //     // // TODO
    //     // assert_eq!(
    //     //     rate_limiter_cloned.rate_limit(1),
    //     //     Some(Deadline::Instant(now + Duration::from_secs(1)))
    //     // );
    //     // assert_eq!(
    //     //     rate_limiter_cloned.rate_limit(1),
    //     //     Some(Deadline::Instant(now + Duration::from_secs(2)))
    //     // );

    //     *time_to_return.borrow_mut() = now + Duration::from_secs(1);

    //     assert_eq!(rate_limiter.rate_limit(1), None);
    //     assert_eq!(
    //         rate_limiter_cloned.rate_limit(2),
    //         Some(Deadline::Instant(now + Duration::from_secs(2)))
    //     );
    // }

    // #[test]
    // fn peers_share_bandwidth_on_fifo_basis() {
    //     let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
    //     let now = Instant::now();
    //     let time_to_return = Rc::new(RefCell::new(now));
    //     let time_provider = time_to_return.clone();
    //     let time_provider: Rc<Box<dyn TimeProvider>> =
    //         Rc::new(Box::new(move || *time_provider.borrow()));

    //     let rate_limiter =
    //         HierarchicalRateLimiter::<TokenBucket<_>, TokenBucket<_>>::new_with_time_provider(
    //             limit_per_second,
    //             time_provider,
    //         );

    //     let rate_limiter_cloned = rate_limiter.clone();

    //     assert!(
    //         rate_limiter.rate_limit(5).is_none(),
    //         "we should be able to use our initial tokens"
    //     );
    //     assert!(
    //         rate_limiter_cloned.rate_limit(5).is_none(),
    //         "cloned rate-limiter should be able to use its initial tokens"
    //     );

    //     *time_to_return.borrow_mut() = now + Duration::from_secs(1);

    //     assert_eq!(
    //         rate_limiter.rate_limit(8),
    //         None,
    //         "we should be able to borrow bandwidth from the shared parent rate-limiter"
    //     );
    //     assert_eq!(
    //         rate_limiter_cloned.rate_limit(5),
    //         None,
    //         "second rate-limiter is still within its dedicated bandwidth, i.e. 10/2 peers = 5"
    //     );
    //     assert!(
    //         rate_limiter.rate_limit(7).is_some(),
    //         "we definitely not within expected bandwidth"
    //     );

    //     // reset internal state of rate-limiters
    //     *time_to_return.borrow_mut() = now + Duration::from_secs(10);
    //     assert!(
    //         rate_limiter.rate_limit(10).is_none(),
    //         "we should be able to borrow bandwidth from our parent"
    //     );
    //     assert!(rate_limiter_cloned.rate_limit(10).is_some(), "previous call should zero parent's bandwidth and 10 is greater than our dedicated bandwidth");
    // }

    // #[test]
    // fn avarage_bandwidth_should_be_within_some_bounds() {
    //     use rand::{
    //         distributions::{Distribution, Uniform},
    //         seq::SliceRandom,
    //         SeedableRng,
    //     };

    //     let mut test_state = vec![];

    //     let rate_limit = 4 * 1024 * 1024;
    //     let limit_per_second =
    //         NonZeroRatePerSecond(rate_limit.try_into().expect("(4 * 1024 * 1024) > 0 qed"));
    //     let rate_limit = rate_limit.into();
    //     let initial_time = Instant::now();
    //     let mut current_time = initial_time;
    //     let time_to_return = Rc::new(RefCell::new(initial_time));
    //     let time_provider = time_to_return.clone();
    //     let time_provider: Rc<Box<dyn TimeProvider>> =
    //         Rc::new(Box::new(move || *time_provider.borrow()));

    //     let rate_limiter =
    //         HierarchicalRateLimiter::<TokenBucket<_>, TokenBucket<_>>::new_with_time_provider(
    //             limit_per_second,
    //             time_provider,
    //         );

    //     let mut rand_gen = rand::rngs::StdRng::seed_from_u64(
    //         SystemTime::now()
    //             .duration_since(UNIX_EPOCH)
    //             .expect("back to the future")
    //             .as_secs(),
    //     );

    //     let limiters_count = Uniform::from(1..=64).sample(&mut rand_gen);
    //     let rate_limiters = repeat(())
    //         .scan((0u64, rate_limiter), |(id, rate_limiter), _| {
    //             let new_rate_limiter = rate_limiter.clone();
    //             let new_state = rate_limiter.clone();
    //             let limiter_id = *id;
    //             *rate_limiter = new_state;
    //             *id += 1;
    //             Some((limiter_id, new_rate_limiter))
    //         })
    //         .take(limiters_count)
    //         .collect::<Vec<_>>();
    //     // let rate_limiters = vec![rate_limiter, rate_limiter_cloned];

    //     let mut total_data_scheduled = 0u128;
    //     let mut last_deadline = initial_time;

    //     let data_gen = Uniform::from(0..100 * 1024 * 1024);
    //     let time_gen = Uniform::from(0..1000 * 10);

    //     let mut calculated_rate_limit = rate_limit;

    //     for _ in 0..100000 {
    //         let (selected_limiter_id, selected_rate_limiter) = rate_limiters
    //             .choose(&mut rand_gen)
    //             .expect("we should be able to randomly choose a rate-limiter from our collection");
    //         let data_read = data_gen.sample(&mut rand_gen);
    //         let next_deadline = selected_rate_limiter.rate_limit(data_read);
    //         let next_deadline = Option::<Instant>::from(next_deadline.unwrap_or(Deadline::Never))
    //             .unwrap_or(last_deadline);
    //         last_deadline = max(last_deadline, next_deadline);
    //         total_data_scheduled += u128::from(data_read);

    //         test_state.push((*selected_limiter_id, data_read));

    //         // let time_passed = rand_gen.gen_range(0..=10);
    //         let time_passed = time_gen.sample(&mut rand_gen);
    //         current_time += Duration::from_millis(time_passed);
    //         *time_to_return.borrow_mut() = current_time;

    //         // check if used bandwidth was within expected bounds
    //         let time_passed = last_deadline - initial_time;
    //         calculated_rate_limit = total_data_scheduled * 1000 / time_passed.as_millis();

    //         if calculated_rate_limit > rate_limit {
    //             let diff = calculated_rate_limit - rate_limit;
    //             assert!(
    //                 diff <= rate_limit,
    //                 "used bandwidth should be smaller that twice the rate-limit - rate_limit = {rate_limit}; diff = {diff}; rate_limiters.len() = {}; state: {:?}",
    //                 rate_limiters.len(),
    //                 test_state,
    //             );
    //         }
    //     }
    //     println!(
    //         "expected rate-limit = {rate_limit} calculated rate limit = {calculated_rate_limit}"
    //     );
    // }

    // #[test]
    // fn no_more_bandwidth_than_double_rate_limit() {
    //     let rate_limit = 128;
    //     let limit_per_second = NonZeroRatePerSecond(rate_limit.try_into().expect("128 > 0 qed"));
    //     let now = Instant::now();
    //     let time_to_return = Rc::new(RefCell::new(now));
    //     let time_provider = time_to_return.clone();
    //     let time_provider: Rc<Box<dyn TimeProvider>> =
    //         Rc::new(Box::new(move || *time_provider.borrow()));

    //     let rate_limiter =
    //         HierarchicalRateLimiter::<TokenBucket<_>, TokenBucket<_>>::new_with_time_provider(
    //             limit_per_second,
    //             time_provider,
    //         );

    //     let limiters_count = 8;

    //     let rate_limiters = repeat(())
    //         .scan((0u64, rate_limiter), |(id, rate_limiter), _| {
    //             let new_rate_limiter = rate_limiter.clone();
    //             let new_state = rate_limiter.clone();
    //             let limiter_id = *id;
    //             *rate_limiter = new_state;
    //             *id += 1;
    //             Some((limiter_id, new_rate_limiter))
    //         })
    //         .take(limiters_count)
    //         .collect::<Vec<_>>();

    //     let initial_tokens = 128 / limiters_count as u64;

    //     assert!(
    //         rate_limiters[0].1.rate_limit(rate_limit).is_none(),
    //         "we should be able to use whole bandwidth"
    //     );

    //     for (id, rate_limiter) in rate_limiters.iter().skip(1) {
    //         assert!(
    //             rate_limiter.rate_limit(initial_tokens).is_none(),
    //             "we should be able to use all of our minimum bandwidth at rate_limiter {id}"
    //         );
    //     }

    //     assert!(
    //         rate_limiters[rate_limiters.len() - 1]
    //             .1
    //             .rate_limit(1)
    //             .is_some(),
    //         "without updating rate-limiter's time, we should not be able to schedule more units"
    //     );

    //     // // reset state of the `rate_limiters`
    //     // *time_to_return.borrow_mut() = now + Duration::from_secs(256);
    //     // let left_shared_bandwidth
    // }

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
            *self.last_instant.lock() = instant;
            async {}
        }
    }

    // TODO make it to use async, but make everything super short
    #[tokio::test]
    async fn htb_avarage_bandwidth_should_be_within_some_bounds() {
        use rand::{
            distributions::{Distribution, Uniform},
            seq::SliceRandom,
            SeedableRng,
        };

        // let mut test_state = vec![];
        let test_state = vec![
            (2, 69368249, 3496),
            (5, 55111143, 4387),
            (3, 13730828, 842),
            (4, 1655360, 5985),
        ];

        let rate_limit = 4 * 1024 * 1024;
        let rate_limit_nonzero = rate_limit.try_into().expect("(4 * 1024 * 1024) > 0 qed");
        let rate_limit = rate_limit.into();
        let initial_time = Instant::now();
        let mut current_time = initial_time;
        let time_to_return = Rc::new(RefCell::new(initial_time));
        let time_provider = time_to_return.clone();
        let time_provider: Rc<Box<dyn TimeProvider>> =
            Rc::new(Box::new(move || *time_provider.borrow()));

        let shared_parent = SharedBandwidthImpl::from(NonZeroRatePerSecond(rate_limit_nonzero));
        let rate_limiter = TokenBucket::new_with_time_provider(rate_limit_nonzero, time_provider);

        let sleep_until_last_instant = Arc::new(Mutex::new(initial_time));
        let sleep_until = TestSleepUntil::new(sleep_until_last_instant.clone());
        let rate_limiter = AsyncTokenBucket::new(rate_limiter, sleep_until);
        let rate_limiter = HierarchicalTokenBucket {
            shared_parent,
            rate_limiter,
            need_to_notify_parent: false,
        };

        let mut rand_gen = rand::rngs::StdRng::seed_from_u64(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("back to the future")
                .as_secs(),
        );

        let limiters_count = Uniform::from(1..=64).sample(&mut rand_gen);
        let mut rate_limiters = repeat(())
            .scan((0u64, rate_limiter), |(id, rate_limiter), _| {
                let new_rate_limiter = rate_limiter.clone();
                let new_state = rate_limiter.clone();
                let limiter_id = *id;
                *rate_limiter = new_state;
                *id += 1;
                Some((limiter_id, Some(new_rate_limiter)))
            })
            // .take(limiters_count)
            .take(8)
            .collect::<Vec<_>>();

        let mut total_data_scheduled = 0;

        let data_gen = Uniform::from(0..100 * 1024 * 1024);
        let time_gen = Uniform::from(0..1000 * 10);

        let mut calculated_rate_limit = rate_limit;
        let mut last_deadline = initial_time;

        // TODO with test-impl of sleep_until, bandwidth sharing doesn't work properly - easy solution would be to increase current_time to last_deadline, otherwise peers are getting too much bandwidth
        for ix in 0..100000 {
            // let (selected_limiter_id, selected_rate_limiter) = rate_limiters
            //     .choose_mut(&mut rand_gen)
            //     .expect("we should be able to randomly choose a rate-limiter from our collection");
            // let data_read = data_gen.sample(&mut rand_gen);
            // let time_passed = time_gen.sample(&mut rand_gen);

            let (selected_limiter_id, data_read, time_passed) = test_state[ix];
            let selected_rate_limiter_pointer: &mut Option<HierarchicalTokenBucket<_, _>> =
                &mut rate_limiters[selected_limiter_id].1;
            let mut selected_rate_limiter = selected_rate_limiter_pointer
                .take()
                .expect("we should be able to retrieve a rate_limiter");

            // test_state.push((*selected_limiter_id, data_read, time_passed));

            selected_rate_limiter = selected_rate_limiter.rate_limit(data_read).await;
            let _ = selected_rate_limiter_pointer.insert(selected_rate_limiter);
            // let last_deadline = *sleep_until_last_instant.borrow();
            let next_deadline = *sleep_until_last_instant.lock();
            last_deadline = max(last_deadline, next_deadline);

            total_data_scheduled += u128::from(data_read);
            current_time += Duration::from_millis(time_passed);
            *time_to_return.borrow_mut() = current_time;

            // check if used bandwidth was within expected bounds
            let time_passed_for_limiter = last_deadline - initial_time;
            if time_passed_for_limiter.is_zero() {
                continue;
            }

            calculated_rate_limit =
                total_data_scheduled * 1000 / (time_passed_for_limiter.as_millis() + 1000);

            if calculated_rate_limit > rate_limit {
                let diff = calculated_rate_limit - rate_limit;
                assert!(
                    diff <= rate_limit,
                    "used bandwidth should be smaller that twice the rate-limit - rate_limit = {rate_limit}; diff = {diff}; rate_limiters.len() = {}; state: {:?}",
                    rate_limiters.len(),
                    test_state,
                );
            }
        }
        // TODO sometime it passes
        println!(
            "expected rate-limit = {rate_limit} calculated rate limit = {calculated_rate_limit}"
        );
    }

    // ------------------------------------------------------------------------------

    // dane na ktorych sie wywalilo:
    // used bandwidth should be smaller that twice the rate-limit - rate_limit = 4194304; diff = 4693010; rate_limiters.len() = 62; state: [(31, 46186434), (34, 72924691), (33, 99942899), (44, 56090363), (23, 63645015), (54, 87157354), (38, 101035927), (35, 92565943), (30, 10366467)]
    // used bandwidth should be smaller that twice the rate-limit - rate_limit = 4194304; diff = 5832772; rate_limiters.len() = 35; state: [(23, 63493327), (18, 80575244), (9, 10889868)]
    // [(53, 79450328, 6255), (41, 71683386, 5414), (12, 102766851, 5002), (35, 41795159, 2869), (6, 4608297, 9832)]

    // rate_limiters.len() = 8; state: [(2, 69368249, 3496), (5, 55111143, 4387), (3, 13730828, 842), (4, 1655360, 5985)]

    // fn generate_test_data<D>(rate_limiters: Vec<D>) -> impl Iterator<Item = (D, usize, u64)> {
    //     let mut rand_gen = rand::rngs::StdRng::seed_from_u64(
    //         SystemTime::now()
    //             .duration_since(UNIX_EPOCH)
    //             .expect("back to the future")
    //             .as_secs(),
    //     );

    //     let data_gen = Uniform::from(0..100 * 1024 * 1024);
    //     let time_gen = Uniform::from(0..1000 * 10);

    //     (0..100000).scan(move |state, _| {
    //         let (selected_limiter_id, selected_rate_limiter) = rate_limiters
    //             .choose_mut(&mut rand_gen)
    //             .expect("we should be able to randomly choose a rate-limiter from our collection");
    //         let data_read = data_gen.sample(&mut rand_gen);
    //     })

    //         for _ in 0..100000 {
    //             let (selected_limiter_id, selected_rate_limiter) = rate_limiters
    //                 .choose_mut(&mut rand_gen)
    //                 .expect("we should be able to randomly choose a rate-limiter from our collection");
    //         let data_read = data_gen.sample(&mut rand_gen);

    //         selected_rate_limiter.rate_limit(data_read).await;
    //         let last_deadline = *sleep_until_last_instant.borrow();
    //         // let next_deadline = *sleep_until_last_instant.borrow();
    //         // last_deadline = max(last_deadline, next_deadline);

    //         total_data_scheduled += u128::from(data_read);

    //         test_state.push((*selected_limiter_id, data_read));

    //         let time_passed = time_gen.sample(&mut rand_gen);
    //         current_time += Duration::from_millis(time_passed);
    //         *time_to_return.borrow_mut() = current_time;

    //         // check if used bandwidth was within expected bounds
    //         let time_passed_for_limiter = last_deadline - initial_time;
    //         if time_passed_for_limiter.is_zero() {
    //             continue;
    //         }

    //         calculated_rate_limit =
    //             total_data_scheduled * 1000 / (time_passed_for_limiter.as_millis() + 1000);

    //         if calculated_rate_limit > rate_limit {
    //             let diff = calculated_rate_limit - rate_limit;
    //             assert!(
    //                 diff <= rate_limit,
    //                 "used bandwidth should be smaller that twice the rate-limit - rate_limit = {rate_limit}; diff = {diff}; rate_limiters.len() = {}; state: {:?}",
    //                 rate_limiters.len(),
    //                 test_state,
    //             );
    //         }
    //     }
    // }
}
