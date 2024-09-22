use crate::{NonZeroRatePerSecond, RatePerSecond, LOG_TARGET};
use futures::{future::pending, FutureExt};
use log::{trace, warn};
use parking_lot::Mutex;
use std::{
    cmp::{max, min},
    iter::empty,
    num::NonZeroU64,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const SECOND_IN_MILLIS: u64 = 1_000;

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
    pub delay: Option<Instant>,
    pub dropped: u64,
}

pub struct RateLimitTryResult {
    scheduled_for_later: u64,
}

pub trait RateLimiterController {
    fn rate(&self) -> RatePerSecond;
    fn set_rate(&self, rate_per_second: RatePerSecond);
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

pub trait RateLimiter {
    fn rate_limit(&self, requested: u64) -> Option<Deadline>;
    fn try_rate_limit_without_delay(&self, requested: u64) -> RateLimitResult;
}

// pub trait AsyncRateLimiter {
//     async fn rate_limit(&self, requested: u64);
// }

/// Implementation of the `Token Bucket` algorithm for the purpose of rate-limiting access to some abstract resource.
pub struct TokenBucket<T = DefaultTimeProvider> {
    rate_per_second: AtomicU64,
    available: AtomicU64,
    last_update: AtomicU64,
    last_update_lock: Mutex<()>,
    initial_time: Instant,
    time_provider: T,
}

impl<T> Clone for TokenBucket<T>
where
    T: TimeProvider + Clone,
{
    fn clone(&self) -> Self {
        Self::new_with_time_provider(
            self.rate_per_second
                .load(Ordering::Relaxed)
                .try_into()
                .unwrap_or(NonZeroU64::MIN),
            self.time_provider.clone(),
        )
    }
}

impl<T> std::fmt::Debug for TokenBucket<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenBucket")
            .field("rate_per_second", &self.rate_per_second)
            .field("available", &self.available)
            .field("last_update", &self.last_update)
            .field("initial_time", &self.initial_time)
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
        let base_instant = time_provider.now();
        Self {
            rate_per_second: rate_per_second.get().into(),
            // we have `rate_per_second` tokens at start
            available: AtomicU64::new(0),
            last_update: AtomicU64::new(0),
            last_update_lock: Mutex::new(()),
            initial_time: base_instant,
            time_provider,
        }
    }

    fn available(&self) -> u64 {
        self.rate_per_second
            .load(Ordering::Relaxed)
            .saturating_sub(self.available.load(std::sync::atomic::Ordering::Relaxed))
    }

    fn rate_limit_internal(&self, requested: u64) -> Option<(Instant, u64)> {
        trace!(
            target: LOG_TARGET,
            "TokenBucket called for {} of requested bytes. Internal state: {:?}.",
            requested,
            self
        );
        let now_available = self.available();
        let now = if now_available < requested {
            self.update_tokens()
        } else {
            self.last_update.load(Ordering::Relaxed)
        };

        self.calculate_delay(requested, now)
    }

    fn calculate_delay(&self, requested: u64, now: u64) -> Option<(Instant, u64)> {
        let now_available = self
            .available
            .fetch_add(requested, std::sync::atomic::Ordering::Relaxed)
            + requested;

        let rate_per_second = self.rate_per_second.load(Ordering::Relaxed);
        let scheduled_for_later = now_available.saturating_sub(rate_per_second);
        if scheduled_for_later == 0 {
            return None;
        }
        let wait_milliseconds =
            scheduled_for_later.saturating_mul(SECOND_IN_MILLIS) / rate_per_second;

        trace!(
            target: LOG_TARGET,
            "TokenBucket about to wait for {} milliseconds after requesting {} - scheduled_for_later {}; now_available {}; rate_per_second {}; {:?}.",
            wait_milliseconds,
            requested,
            scheduled_for_later,
            now_available,
            rate_per_second,
            self,
        );

        let now = Duration::from_millis(now);
        let wait_duration = Duration::from_millis(wait_milliseconds);
        Some((self.initial_time + now + wait_duration, scheduled_for_later))
    }

    fn update_tokens(&self) -> u64 {
        let Some(_guard) = self.last_update_lock.try_lock() else {
            trace!(
                target: LOG_TARGET,
                "TokenBucket tried to update, but failed {:?}",
                self,
            );
            let now = self.last_update.load(std::sync::atomic::Ordering::Relaxed);
            return now;
        };
        trace!(
            target: LOG_TARGET,
            "TokenBucket about to update it tokens {:?}.",
            self,
        );
        let rate_per_second = self.rate_per_second.load(Ordering::Relaxed);
        let updated_now = self
            .time_provider
            .now()
            .duration_since(self.initial_time)
            .as_millis()
            .try_into()
            .ok();
        let Some(updated_now) = updated_now else {
            warn!(target: LOG_TARGET, "'u64' should be enough to store milliseconds since 'initial_time' - rate limiting will be turned off.");
            let now = self.last_update.load(std::sync::atomic::Ordering::Relaxed);
            return now;
        };
        let now = updated_now;
        let since_last_update = now
            - self
                .last_update
                .fetch_max(now, std::sync::atomic::Ordering::Relaxed);
        let new_units = since_last_update.saturating_mul(rate_per_second) / SECOND_IN_MILLIS;
        trace!(
            target: LOG_TARGET,
            "TokenBucket new_units: {}; since_last_update {}; {:?}.",
            new_units,
            since_last_update,
            self,
        );
        let now_available = self.available.load(std::sync::atomic::Ordering::Relaxed);
        let new_units = min(now_available, new_units);
        trace!(
            target: LOG_TARGET,
            "TokenBucket new_units after processing: {}; {:?}.",
            new_units,
            self,
        );

        // this can't underflow, since all other operations can only `fetch_add` to `available`
        self.available
            .fetch_sub(new_units, std::sync::atomic::Ordering::Relaxed);

        now
    }

    pub fn rate_per_second(&mut self, rate_per_second: u64) -> u64 {
        self.rate_per_second
            .swap(rate_per_second, Ordering::Relaxed)
    }

    /// Calculates [Duration](time::Duration) by which we should delay next call to some governed resource in order to satisfy
    /// configured rate limit.
    pub fn rate_limit(&mut self, requested: u64) -> Option<Instant> {
        self.rate_limit_internal(requested).map(|(delay, _)| delay)
    }

    pub fn try_rate_limit_without_delay(&self, requested: u64) -> RateLimitResult {
        if requested == 0 {
            return RateLimitResult {
                delay: None,
                dropped: 0,
            };
        }
        let mut now_available = self.available();
        let mut to_request = min(now_available, requested);
        if to_request < requested {
            self.update_tokens();
            now_available = self.available();
            to_request = min(now_available, requested);
        }
        let result = self.rate_limit_internal(to_request);
        let dropped = requested - to_request;
        match result {
            Some((delay, _)) => RateLimitResult {
                delay: Some(delay),
                dropped,
            },
            None => RateLimitResult {
                delay: None,
                dropped,
            },
        }
    }
}

impl<TP> RateLimiterController for TokenBucket<TP> {
    fn rate(&self) -> RatePerSecond {
        self.rate_per_second.load(Ordering::Relaxed).into()
    }

    fn set_rate(&self, rate_per_second: RatePerSecond) {
        let rate_per_second = match rate_per_second {
            RatePerSecond::Block => return,
            RatePerSecond::Rate(value) => value.into(),
        };
        self.rate_per_second
            .store(rate_per_second, Ordering::Relaxed)
    }
}

impl<TP> RateLimiter for TokenBucket<TP>
where
    TP: TimeProvider,
{
    fn rate_limit(&self, requested: u64) -> Option<Deadline> {
        Some(Deadline::Instant(
            self.rate_limit_internal(requested)
                .map(|(delay, _)| delay)?,
        ))
    }

    fn try_rate_limit_without_delay(&self, requested: u64) -> RateLimitResult {
        self.try_rate_limit_without_delay(requested)
    }
}

pub struct HierarchicalRateLimiter<
    ParentRL = TokenBucket<DefaultTimeProvider>,
    ThisRL = TokenBucket<DefaultTimeProvider>,
> {
    last_children_count: AtomicU64,
    parent: Arc<ParentRL>,
    rate_limiter: ThisRL,
}

impl From<NonZeroRatePerSecond> for HierarchicalRateLimiter {
    fn from(rate_per_second: NonZeroRatePerSecond) -> Self {
        HierarchicalRateLimiter::new(rate_per_second)
    }
}

impl<TP> From<(NonZeroRatePerSecond, TP)>
    for HierarchicalRateLimiter<TokenBucket<TP>, TokenBucket<TP>>
where
    TP: TimeProvider + Clone,
{
    fn from((rate_per_second, time_provider): (NonZeroRatePerSecond, TP)) -> Self {
        HierarchicalRateLimiter::new_with_time_provider(rate_per_second, time_provider)
    }
}

impl<ParentRL, ThisRL> Clone for HierarchicalRateLimiter<ParentRL, ThisRL>
where
    ThisRL: Clone,
{
    fn clone(&self) -> Self {
        Self {
            last_children_count: AtomicU64::new(self.last_children_count.load(Ordering::Relaxed)),
            parent: Arc::clone(&self.parent),
            rate_limiter: self.rate_limiter.clone(),
        }
    }
}

impl HierarchicalRateLimiter {
    pub fn new(rate_per_second: NonZeroRatePerSecond) -> Self {
        Self::new_with_time_provider(rate_per_second, DefaultTimeProvider)
    }
}

impl<ParentRL, ThisRL> HierarchicalRateLimiter<ParentRL, ThisRL> {
    fn new_with_time_provider<TP>(rate_per_second: NonZeroRatePerSecond, time_provider: TP) -> Self
    where
        TP: TimeProvider + Clone,
        ParentRL: From<(NonZeroRatePerSecond, TP)>,
        ThisRL: From<(NonZeroRatePerSecond, TP)>,
    {
        let parent = Arc::new(ParentRL::from((rate_per_second, time_provider.clone())));
        let token_bucket = ThisRL::from((rate_per_second, time_provider));
        Self {
            last_children_count: Arc::strong_count(&parent)
                .try_into()
                .unwrap_or(u64::MAX)
                .into(),
            parent,
            rate_limiter: token_bucket,
        }
    }
}

impl<ParentRL, ThisRL> HierarchicalRateLimiter<ParentRL, ThisRL>
where
    ParentRL: RateLimiter + RateLimiterController,
    ThisRL: RateLimiter + RateLimiterController,
{
    fn set_rate(&self) {
        let children_count = Arc::strong_count(&self.parent)
            .try_into()
            .unwrap_or(u64::MAX);
        if self.last_children_count.load(Ordering::Relaxed) == children_count {
            return;
        }
        self.last_children_count
            .store(children_count, Ordering::Relaxed);
        let parent_rate: u64 = self.parent.rate().into();
        let rate_per_second_for_children = max(parent_rate / children_count, 1);
        self.rate_limiter
            .set_rate(rate_per_second_for_children.into());
    }

    pub fn try_rate_limit_without_delay(&self, requested: u64) -> RateLimitResult {
        self.set_rate();

        let result = self.rate_limiter.try_rate_limit_without_delay(requested);
        let left_tokens = result.dropped;
        let for_parent = requested - left_tokens;
        // account all tokens that we used
        self.parent.rate_limit(for_parent);

        // try to borrow from parent
        let parent_result = self.parent.try_rate_limit_without_delay(left_tokens);
        let left_tokens = parent_result.dropped;

        let required_delay = empty().chain(result.delay).chain(parent_result.delay).max();
        RateLimitResult {
            delay: required_delay,
            dropped: left_tokens,
        }
    }

    pub fn rate_limit(&self, requested: u64) -> Option<Deadline> {
        let RateLimitResult {
            dropped: left_tokens,
            delay: required_delay,
        } = self.try_rate_limit_without_delay(requested);

        // account all tokens that we are about to use
        // let parent_result = self
        //     .parent
        //     .rate_limit(left_tokens)
        //     .map(|value| value.into());
        self.parent.rate_limit(left_tokens);

        let result = empty()
            .chain(required_delay)
            .chain(
                self.rate_limiter
                    .rate_limit(left_tokens)
                    .and_then(|value| value.into()),
            )
            // .chain(parent_result.flatten())
            .max();

        Some(Deadline::Instant(result?))
    }
}

impl<ParentRL, ThisRL> RateLimiterController for HierarchicalRateLimiter<ParentRL, ThisRL>
where
    ParentRL: RateLimiterController,
    ThisRL: RateLimiterController,
{
    fn rate(&self) -> RatePerSecond {
        self.parent.rate()
    }

    fn set_rate(&self, rate_per_second: RatePerSecond) {
        self.parent.set_rate(rate_per_second)
    }
}

impl<ParentRL, ThisRL> RateLimiter for HierarchicalRateLimiter<ParentRL, ThisRL>
where
    ParentRL: RateLimiter + RateLimiterController,
    ThisRL: RateLimiter + RateLimiterController,
{
    fn rate_limit(&self, requested: u64) -> Option<Deadline> {
        HierarchicalRateLimiter::rate_limit(self, requested)
    }

    fn try_rate_limit_without_delay(&self, requested: u64) -> RateLimitResult {
        HierarchicalRateLimiter::try_rate_limit_without_delay(self, requested)
    }
}

#[derive(Clone)]
pub enum RateLimiterFacade<RL> {
    NoTraffic,
    RateLimiter(RL),
}

impl<RL> RateLimiterFacade<RL>
where
    RL: RateLimiter,
{
    pub fn new(rate: RatePerSecond) -> Self
    where
        RL: From<NonZeroRatePerSecond>,
    {
        match rate {
            RatePerSecond::Block => Self::NoTraffic,
            RatePerSecond::Rate(rate) => Self::RateLimiter(rate.into()),
        }
    }

    pub fn rate_limit(&self, requested: u64) -> Option<Deadline> {
        match self {
            RateLimiterFacade::NoTraffic => Some(Deadline::Never),
            RateLimiterFacade::RateLimiter(limiter) => limiter.rate_limit(requested),
        }
    }
}

struct SharedParent {
    max_rate: NonZeroRatePerSecond,
    active_children: AtomicU64,
    bandwidth_change: tokio::sync::Notify,
}

impl SharedParent {
    pub fn new(rate: NonZeroRatePerSecond) -> Self {
        Self {
            max_rate: rate,
            active_children: AtomicU64::new(0),
            bandwidth_change: tokio::sync::Notify::new(),
        }
    }
}

impl BandwidthDivider for Arc<SharedParent> {
    fn request_bandwidth(&mut self, _requestd: u64) -> NonZeroRatePerSecond {
        // let active_children = self.active_children.load(Ordering::Relaxed);
        let active_children = self.active_children.fetch_add(1, Ordering::Relaxed) + 1;
        let rate = u64::from(self.max_rate) / active_children;
        NonZeroRatePerSecond(NonZeroU64::new(rate).unwrap_or(NonZeroU64::MIN))
    }

    fn notify_idle(&mut self) {
        self.active_children.fetch_sub(1, Ordering::Relaxed);
        // todo!();
        // self.bandwidth_change.notify_all();
        // self.cond_var.no
        self.bandwidth_change.notify_waiters();
    }

    async fn await_bandwidth_change(&mut self) -> NonZeroRatePerSecond {
        self.bandwidth_change.notified().await;
        self.request_bandwidth(0)
    }
}

pub trait BandwidthDivider {
    // TODO perhaps return something that holds the counter and sub when dropped - meh, nope
    fn request_bandwidth(&mut self, requestd: u64) -> NonZeroRatePerSecond;
    fn notify_idle(&mut self);
    async fn await_bandwidth_change(&mut self) -> NonZeroRatePerSecond;
}

// pub trait RateLimiterWaiter {
//     async fn wait(&mut self);
//     fn set_rate(&mut self, rate: NonZeroRatePerSecond);
// }

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
    SU: SleepUntil,
{
    fn rate_limit(&mut self, requested: u64) -> RateLimitTryResult {
        // let rate_limiter_try_result = self.token_bucket.try_rate_limit_without_delay(requested);
        // let deadline = self
        //     .token_bucket
        //     .rate_limit(rate_limiter_try_result.dropped);

        let deadline = self
            .token_bucket
            .rate_limit(requested);
        self.next_deadline = *max(&deadline, &self.next_deadline);
        RateLimitTryResult {
            scheduled_for_later: rate_limiter_try_result.dropped,
        }
    }

    fn try_rate_limit(&mut self, requested: u64) -> RateLimitResult {
        self.token_bucket.try_rate_limit_without_delay(requested)
    }

    fn set_rate(&mut self, rate: NonZeroRatePerSecond) {
        self.token_bucket.set_rate(rate.into());
        self.rate_limit_changed = true;
    }

    async fn wait(&mut self) {
        if self.rate_limit_changed {
            self.next_deadline = self.token_bucket.rate_limit(0);
            self.rate_limit_changed = false;
        }
        match self.next_deadline {
            Some(Deadline::Instant(deadline)) => self.sleep_until.sleep_until(deadline).await,
            Some(Deadline::Never) => pending().await,
            None => {}
        }
    }
}

pub trait AsyncRateLimiter {
    fn rate_limit(&mut self, requested: u64) -> RateLimitTryResult;
    fn try_rate_limit(&mut self, requested: u64) -> RateLimitResult;
    fn set_rate(&mut self, rate: NonZeroRatePerSecond);
    async fn wait(&mut self);
}

pub trait SleepUntil {
    async fn sleep_until(&mut self, instant: Instant);
}

#[derive(Clone)]
pub struct TokioSleepUntil;

impl SleepUntil for TokioSleepUntil {
    async fn sleep_until(&mut self, instant: Instant) {
        tokio::time::sleep_until(instant.into()).await;
    }
}

// zeby ojciec nie musial wysylac sygnalu do wszystkich w sytuacji
// async condvar
#[derive(Clone)]
pub struct LinuxHierarchicalTokenBucket<BD, ARL> {
    shared_parent: BD,
    rate_limiter: ARL,
}

impl<BD, ARL> LinuxHierarchicalTokenBucket<BD, ARL>
where
    BD: BandwidthDivider,
    ARL: AsyncRateLimiter,
{
    pub async fn rate_limit(&mut self, requested: u64) {
        let rate_limiter_result = self.rate_limiter.rate_limit(requested);
        if rate_limiter_result.scheduled_for_later == 0 {
            return;
        }

        let left_from_request = rate_limiter_result.scheduled_for_later;
        let rate = self.shared_parent.request_bandwidth(left_from_request);
        self.rate_limiter.set_rate(rate);

        loop {
            futures::select! {
                _ = self.rate_limiter.wait().fuse() => {
                    self.shared_parent.notify_idle();
                    return;
                },
                rate = self.shared_parent.await_bandwidth_change().fuse() => {
                    self.rate_limiter.set_rate(rate);
                },
            }
        }
    }

    // fn rate_limit_internal(&mut self, requested: u64) -> RateLimitTryResult {
    // }
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

    use crate::token_bucket::{
        AsyncTokenBucket, Deadline, HierarchicalRateLimiter, LinuxHierarchicalTokenBucket,
        NonZeroRatePerSecond, SharedParent,
    };

    use super::{RateLimiter, SleepUntil, TimeProvider, TokenBucket};

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
        token_bucket_sanity_check_test::<HierarchicalRateLimiter<TokenBucket<_>, TokenBucket<_>>>()
    }

    fn token_bucket_sanity_check_test<RL>()
    where
        RL: RateLimiter + From<(NonZeroRatePerSecond, Rc<Box<dyn TimeProvider>>)>,
    {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> = Box::new(move || *time_provider.borrow());
        let rate_limiter = RL::from((limit_per_second, time_provider.into()));

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
        no_slowdown_while_within_rate_limit_test::<
            HierarchicalRateLimiter<TokenBucket<_>, TokenBucket<_>>,
        >()
    }

    fn no_slowdown_while_within_rate_limit_test<RL>()
    where
        RL: RateLimiter + From<(NonZeroRatePerSecond, Rc<Box<dyn TimeProvider>>)>,
    {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> = Box::new(move || *time_provider.borrow());
        let rate_limiter = RL::from((limit_per_second, time_provider.into()));

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
        slowdown_when_limit_reached_test::<HierarchicalRateLimiter<TokenBucket<_>, TokenBucket<_>>>(
        )
    }

    fn slowdown_when_limit_reached_test<RL>()
    where
        RL: RateLimiter + From<(NonZeroRatePerSecond, Rc<Box<dyn TimeProvider>>)>,
    {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> = Box::new(move || *time_provider.borrow());
        let rate_limiter = RL::from((limit_per_second, time_provider.into()));

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
        buildup_tokens_but_no_more_than_limit_test::<
            HierarchicalRateLimiter<TokenBucket<_>, TokenBucket<_>>,
        >()
    }

    fn buildup_tokens_but_no_more_than_limit_test<RL>()
    where
        RL: RateLimiter + From<(NonZeroRatePerSecond, Rc<Box<dyn TimeProvider>>)>,
    {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> = Box::new(move || *time_provider.borrow());
        let rate_limiter = RL::from((limit_per_second, time_provider.into()));

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
            HierarchicalRateLimiter<TokenBucket<_>, TokenBucket<_>>,
        >()
    }

    fn multiple_calls_buildup_wait_time_test<RL>()
    where
        RL: RateLimiter + From<(NonZeroRatePerSecond, Rc<Box<dyn TimeProvider>>)>,
    {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> = Box::new(move || *time_provider.borrow());
        let rate_limiter = RL::from((limit_per_second, time_provider.into()));

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

    #[test]
    fn two_peers_can_share_rate_limiter() {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Rc<Box<dyn TimeProvider>> =
            Rc::new(Box::new(move || *time_provider.borrow()));

        let rate_limiter =
            HierarchicalRateLimiter::<TokenBucket<_>, TokenBucket<_>>::new_with_time_provider(
                limit_per_second,
                time_provider,
            );

        let rate_limiter_cloned = rate_limiter.clone();

        assert_eq!(rate_limiter.rate_limit(5), None);
        assert_eq!(rate_limiter_cloned.rate_limit(5), None);

        assert_eq!(
            rate_limiter.rate_limit(10),
            Some(Deadline::Instant(now + Duration::from_secs(2)))
        );
        assert_eq!(
            rate_limiter_cloned.rate_limit(10),
            Some(Deadline::Instant(now + Duration::from_secs(2)))
        );
    }

    #[test]
    fn single_peer_can_use_whole_bandwidth_when_needed() {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Rc<Box<dyn TimeProvider>> =
            Rc::new(Box::new(move || *time_provider.borrow()));

        let rate_limiter =
            HierarchicalRateLimiter::<TokenBucket<_>, TokenBucket<_>>::new_with_time_provider(
                limit_per_second,
                time_provider,
            );

        let rate_limiter_cloned = rate_limiter.clone();

        assert_eq!(rate_limiter.rate_limit(5), None);
        assert_eq!(rate_limiter_cloned.rate_limit(5), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(1);

        assert_eq!(rate_limiter.rate_limit(10), None);
        assert_eq!(
            rate_limiter_cloned.rate_limit(10),
            Some(Deadline::Instant(now + Duration::from_secs(2)))
        );
    }

    #[test]
    fn peers_receive_at_least_one_token_per_second() {
        let limit_per_second = NonZeroRatePerSecond(1.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Rc<Box<dyn TimeProvider>> =
            Rc::new(Box::new(move || *time_provider.borrow()));

        let rate_limiter =
            HierarchicalRateLimiter::<TokenBucket<_>, TokenBucket<_>>::new_with_time_provider(
                limit_per_second,
                time_provider,
            );

        let rate_limiter_cloned = rate_limiter.clone();

        assert_eq!(rate_limiter.rate_limit(1), None);
        // TODO
        assert_eq!(rate_limiter_cloned.rate_limit(1), None);

        // // TODO
        // assert_eq!(
        //     rate_limiter_cloned.rate_limit(1),
        //     Some(Deadline::Instant(now + Duration::from_secs(1)))
        // );
        // assert_eq!(
        //     rate_limiter_cloned.rate_limit(1),
        //     Some(Deadline::Instant(now + Duration::from_secs(2)))
        // );

        *time_to_return.borrow_mut() = now + Duration::from_secs(1);

        assert_eq!(rate_limiter.rate_limit(1), None);
        assert_eq!(
            rate_limiter_cloned.rate_limit(2),
            Some(Deadline::Instant(now + Duration::from_secs(2)))
        );
    }

    #[test]
    fn peers_share_bandwidth_on_fifo_basis() {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Rc<Box<dyn TimeProvider>> =
            Rc::new(Box::new(move || *time_provider.borrow()));

        let rate_limiter =
            HierarchicalRateLimiter::<TokenBucket<_>, TokenBucket<_>>::new_with_time_provider(
                limit_per_second,
                time_provider,
            );

        let rate_limiter_cloned = rate_limiter.clone();

        assert!(
            rate_limiter.rate_limit(5).is_none(),
            "we should be able to use our initial tokens"
        );
        assert!(
            rate_limiter_cloned.rate_limit(5).is_none(),
            "cloned rate-limiter should be able to use its initial tokens"
        );

        *time_to_return.borrow_mut() = now + Duration::from_secs(1);

        assert_eq!(
            rate_limiter.rate_limit(8),
            None,
            "we should be able to borrow bandwidth from the shared parent rate-limiter"
        );
        assert_eq!(
            rate_limiter_cloned.rate_limit(5),
            None,
            "second rate-limiter is still within its dedicated bandwidth, i.e. 10/2 peers = 5"
        );
        assert!(
            rate_limiter.rate_limit(7).is_some(),
            "we definitely not within expected bandwidth"
        );

        // reset internal state of rate-limiters
        *time_to_return.borrow_mut() = now + Duration::from_secs(10);
        assert!(
            rate_limiter.rate_limit(10).is_none(),
            "we should be able to borrow bandwidth from our parent"
        );
        assert!(rate_limiter_cloned.rate_limit(10).is_some(), "previous call should zero parent's bandwidth and 10 is greater than our dedicated bandwidth");
    }

    #[test]
    fn avarage_bandwidth_should_be_within_some_bounds() {
        use rand::{
            distributions::{Distribution, Uniform},
            seq::SliceRandom,
            SeedableRng,
        };

        let mut test_state = vec![];

        let rate_limit = 4 * 1024 * 1024;
        let limit_per_second =
            NonZeroRatePerSecond(rate_limit.try_into().expect("(4 * 1024 * 1024) > 0 qed"));
        let rate_limit = rate_limit.into();
        let initial_time = Instant::now();
        let mut current_time = initial_time;
        let time_to_return = Rc::new(RefCell::new(initial_time));
        let time_provider = time_to_return.clone();
        let time_provider: Rc<Box<dyn TimeProvider>> =
            Rc::new(Box::new(move || *time_provider.borrow()));

        let rate_limiter =
            HierarchicalRateLimiter::<TokenBucket<_>, TokenBucket<_>>::new_with_time_provider(
                limit_per_second,
                time_provider,
            );

        let mut rand_gen = rand::rngs::StdRng::seed_from_u64(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("back to the future")
                .as_secs(),
        );

        let limiters_count = Uniform::from(1..=64).sample(&mut rand_gen);
        let rate_limiters = repeat(())
            .scan((0u64, rate_limiter), |(id, rate_limiter), _| {
                let new_rate_limiter = rate_limiter.clone();
                let new_state = rate_limiter.clone();
                let limiter_id = *id;
                *rate_limiter = new_state;
                *id += 1;
                Some((limiter_id, new_rate_limiter))
            })
            .take(limiters_count)
            .collect::<Vec<_>>();
        // let rate_limiters = vec![rate_limiter, rate_limiter_cloned];

        let mut total_data_scheduled = 0u128;
        let mut last_deadline = initial_time;

        let data_gen = Uniform::from(0..100 * 1024 * 1024);
        let time_gen = Uniform::from(0..1000 * 10);

        let mut calculated_rate_limit = rate_limit;

        for _ in 0..100000 {
            let (selected_limiter_id, selected_rate_limiter) = rate_limiters
                .choose(&mut rand_gen)
                .expect("we should be able to randomly choose a rate-limiter from our collection");
            let data_read = data_gen.sample(&mut rand_gen);
            let next_deadline = selected_rate_limiter.rate_limit(data_read);
            let next_deadline = Option::<Instant>::from(next_deadline.unwrap_or(Deadline::Never))
                .unwrap_or(last_deadline);
            last_deadline = max(last_deadline, next_deadline);
            total_data_scheduled += u128::from(data_read);

            test_state.push((*selected_limiter_id, data_read));

            // let time_passed = rand_gen.gen_range(0..=10);
            let time_passed = time_gen.sample(&mut rand_gen);
            current_time += Duration::from_millis(time_passed);
            *time_to_return.borrow_mut() = current_time;

            // check if used bandwidth was within expected bounds
            let time_passed = last_deadline - initial_time;
            calculated_rate_limit = total_data_scheduled * 1000 / time_passed.as_millis();

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
        println!(
            "expected rate-limit = {rate_limit} calculated rate limit = {calculated_rate_limit}"
        );
    }

    #[test]
    fn no_more_bandwidth_than_double_rate_limit() {
        let rate_limit = 128;
        let limit_per_second = NonZeroRatePerSecond(rate_limit.try_into().expect("128 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Rc<Box<dyn TimeProvider>> =
            Rc::new(Box::new(move || *time_provider.borrow()));

        let rate_limiter =
            HierarchicalRateLimiter::<TokenBucket<_>, TokenBucket<_>>::new_with_time_provider(
                limit_per_second,
                time_provider,
            );

        let limiters_count = 8;

        let rate_limiters = repeat(())
            .scan((0u64, rate_limiter), |(id, rate_limiter), _| {
                let new_rate_limiter = rate_limiter.clone();
                let new_state = rate_limiter.clone();
                let limiter_id = *id;
                *rate_limiter = new_state;
                *id += 1;
                Some((limiter_id, new_rate_limiter))
            })
            .take(limiters_count)
            .collect::<Vec<_>>();

        let initial_tokens = 128 / limiters_count as u64;

        assert!(
            rate_limiters[0].1.rate_limit(rate_limit).is_none(),
            "we should be able to use whole bandwidth"
        );

        for (id, rate_limiter) in rate_limiters.iter().skip(1) {
            assert!(
                rate_limiter.rate_limit(initial_tokens).is_none(),
                "we should be able to use all of our minimum bandwidth at rate_limiter {id}"
            );
        }

        assert!(
            rate_limiters[rate_limiters.len() - 1]
                .1
                .rate_limit(1)
                .is_some(),
            "without updating rate-limiter's time, we should not be able to schedule more units"
        );

        // // reset state of the `rate_limiters`
        // *time_to_return.borrow_mut() = now + Duration::from_secs(256);
        // let left_shared_bandwidth
    }

    #[derive(Clone)]
    struct TestSleepUntil {
        last_instant: Rc<RefCell<Instant>>,
    }

    impl TestSleepUntil {
        pub fn new(initial_instant: Rc<RefCell<Instant>>) -> Self {
            Self {
                last_instant: initial_instant,
            }
        }
    }

    impl SleepUntil for TestSleepUntil {
        async fn sleep_until(&mut self, instant: Instant) {
            *self.last_instant.borrow_mut() = instant;
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

        let mut test_state = vec![];

        let rate_limit = 4 * 1024 * 1024;
        let rate_limit_nonzero = rate_limit.try_into().expect("(4 * 1024 * 1024) > 0 qed");
        let rate_limit = rate_limit.into();
        let initial_time = Instant::now();
        let mut current_time = initial_time;
        let time_to_return = Rc::new(RefCell::new(initial_time));
        let time_provider = time_to_return.clone();
        let time_provider: Rc<Box<dyn TimeProvider>> =
            Rc::new(Box::new(move || *time_provider.borrow()));

        let shared_parent = Arc::new(SharedParent::new(NonZeroRatePerSecond(rate_limit_nonzero)));
        let rate_limiter = TokenBucket::new_with_time_provider(rate_limit_nonzero, time_provider);

        let sleep_until_last_instant = Rc::new(RefCell::new(initial_time));
        let sleep_until = TestSleepUntil::new(sleep_until_last_instant.clone());
        let rate_limiter = AsyncTokenBucket::new(rate_limiter, sleep_until);
        let rate_limiter = LinuxHierarchicalTokenBucket {
            shared_parent,
            rate_limiter,
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
                Some((limiter_id, new_rate_limiter))
            })
            .take(limiters_count)
            .collect::<Vec<_>>();

        let mut total_data_scheduled = 0;

        let data_gen = Uniform::from(0..100 * 1024 * 1024);
        let time_gen = Uniform::from(0..1000 * 10);

        let mut calculated_rate_limit = rate_limit;

        for _ in 0..100000 {
            let (selected_limiter_id, selected_rate_limiter) = rate_limiters
                .choose_mut(&mut rand_gen)
                .expect("we should be able to randomly choose a rate-limiter from our collection");
            let data_read = data_gen.sample(&mut rand_gen);

            selected_rate_limiter.rate_limit(data_read).await;
            let last_deadline = *sleep_until_last_instant.borrow();
            // let next_deadline = *sleep_until_last_instant.borrow();
            // last_deadline = max(last_deadline, next_deadline);

            total_data_scheduled += u128::from(data_read);

            test_state.push((*selected_limiter_id, data_read));

            let time_passed = time_gen.sample(&mut rand_gen);
            current_time += Duration::from_millis(time_passed);
            *time_to_return.borrow_mut() = current_time;

            // check if used bandwidth was within expected bounds
            let time_passed_for_limiter = last_deadline - initial_time;
            if time_passed_for_limiter.is_zero() {
                continue;
            }

            calculated_rate_limit = total_data_scheduled * 1000 / (time_passed_for_limiter.as_millis() + 1000);

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
        println!(
            "expected rate-limit = {rate_limit} calculated rate limit = {calculated_rate_limit}"
        );
    }
}
