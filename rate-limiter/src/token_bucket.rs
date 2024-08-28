use crate::LOG_TARGET;
use log::{trace, warn};
use parking_lot::Mutex;
use std::{
    cmp::{max, min},
    iter::empty,
    num::NonZeroU64,
    rc::Rc,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const SECOND_IN_MILLIS: u64 = 1_000;

// pub trait TimeProviderBoxClone {
//     fn box_clone(&self) -> Box<dyn TimeProvider>;
// }

// pub trait TimeProvider: TimeProviderBoxClone {
//     fn now(&self) -> Instant;
// }

pub trait TimeProvider {
    fn now(&self) -> Instant;
}

// impl<F> TimeProviderBoxClone for F where F: Fn() -> Instant + Clone {
//     fn clone(&self) -> Box<dyn TimeProvider> {
//         Box::new(Clone::clone(self))
//     }
// }

// impl<TP> TimeProviderBoxClone for TP where TP: TimeProvider {
//     fn box_clone(&self) -> Box<dyn TimeProvider> {
//         TimeProviderBoxClone::box_clone(self)
//     }
// }

impl<F> TimeProvider for F
where
    F: Fn() -> Instant,
{
    fn now(&self) -> Instant {
        self()
    }
}

impl<TP> TimeProvider for Rc<TP>
where
    TP: TimeProvider,
{
    fn now(&self) -> Instant {
        self.as_ref().now()
    }
}

// impl Clone for Box<dyn TimeProvider> {
//     fn clone(&self) -> Self {
//         Box::clone(self)
//     }
// }

impl TimeProvider for Box<dyn TimeProvider> {
    fn now(&self) -> Instant {
        self.as_ref().now()
    }
}

#[derive(Clone, Default)]
pub struct DefaultTimeProvider;

impl TimeProvider for DefaultTimeProvider {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[derive(Clone)]
pub struct NonZeroRatePerSecond(NonZeroU64);

impl From<NonZeroRatePerSecond> for u64 {
    fn from(value: NonZeroRatePerSecond) -> Self {
        value.0.into()
    }
}

pub enum RatePerSecond {
    Block,
    Rate(NonZeroRatePerSecond),
}

impl From<u64> for RatePerSecond {
    fn from(value: u64) -> Self {
        match value {
            0 => Self::Block,
            _ => Self::Rate(NonZeroRatePerSecond(
                NonZeroU64::new(value).unwrap_or(NonZeroU64::MAX),
            )),
        }
    }
}

impl From<RatePerSecond> for u64 {
    fn from(value: RatePerSecond) -> Self {
        match value {
            RatePerSecond::Block => 0,
            RatePerSecond::Rate(NonZeroRatePerSecond(value)) => value.into(),
        }
    }
}

pub struct RateLimitResult {
    pub delay: Option<Instant>,
    pub dropped: u64,
}

pub trait RateLimiter {
    fn rate(&self) -> RatePerSecond;
    fn set_rate(&self, rate_per_second: RatePerSecond);
    fn rate_limit(&self, requested: u64) -> Option<Option<Instant>>;
    fn try_rate_limit_without_delay(&self, requested: u64) -> RateLimitResult;
}

/// Implementation of the `Token Bucket` algorithm for the purpose of rate-limiting access to some abstract resource.
pub struct TokenBucket<T = DefaultTimeProvider> {
    rate_per_second: AtomicU64,
    available: AtomicU64,
    last_update: AtomicU64,
    last_update_lock: Mutex<()>,
    initial_time: Instant,
    time_provider: T,
}

impl Clone for TokenBucket {
    fn clone(&self) -> Self {
        Self::new(
            self.rate_per_second
                .load(Ordering::Relaxed)
                .try_into()
                .unwrap_or(NonZeroU64::MIN),
        )
    }
}

impl<T> std::fmt::Debug for TokenBucket<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenBucket")
            .field("rate_per_second", &self.rate_per_second)
            .field("available", &self.available)
            .field("last_update", &self.last_update)
            .field("last_update_lock", &self.last_update_lock)
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
        if requested == 0 {
            return None;
        }
        let mut now_available = self.available();
        if now_available >= requested {
            self.available
                .fetch_add(requested, std::sync::atomic::Ordering::Relaxed);
            return None;
        }

        let mut now = self.last_update.load(std::sync::atomic::Ordering::Relaxed);
        if let Some(_guard) = self.last_update_lock.try_lock() {
            if self
                .update_tokens(&mut now, &mut now_available, requested)
                .is_none()
            {
                return None;
            }
        } else {
            trace!(
                target: LOG_TARGET,
                "TokenBucket tried to update, but failed {:?}",
                self,
            );
            now_available = self
                .available
                .fetch_add(requested, std::sync::atomic::Ordering::Relaxed)
                + requested;
        }

        let scheduled_for_later = now_available - self.rate_per_second.load(Ordering::Relaxed);
        let wait_milliseconds = scheduled_for_later.saturating_mul(SECOND_IN_MILLIS)
            / self.rate_per_second.load(Ordering::Relaxed);

        trace!(
                target: LOG_TARGET,
            "TokenBucket about to wait for {} milliseconds after requesting {} - scheduled_for_later {}; now_available {}; rate_per_second {}; {:?}.",
            wait_milliseconds,
            requested,
            scheduled_for_later,
            now_available,
            self.rate_per_second.load(Ordering::Relaxed),
            self,
        );

        let now = Duration::from_millis(now);
        let wait_duration = Duration::from_millis(wait_milliseconds);
        Some((self.initial_time + now + wait_duration, scheduled_for_later))
    }

    fn update_tokens(&self, now: &mut u64, now_available: &mut u64, requested: u64) -> Option<()> {
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
            return None;
        };
        *now = updated_now;
        let since_last_update = *now
            - self
                .last_update
                .fetch_max(*now, std::sync::atomic::Ordering::Relaxed);
        let new_units = since_last_update.saturating_mul(rate_per_second) / SECOND_IN_MILLIS;
        trace!(
            target: LOG_TARGET,
            "TokenBucket new_units: {}; since_last_update {}; {:?}.",
            new_units,
            since_last_update,
            self,
        );
        *now_available = self.available.load(std::sync::atomic::Ordering::Relaxed);
        let new_units = min(*now_available, new_units);
        trace!(
            target: LOG_TARGET,
            "TokenBucket new_units after processing: {}; {:?}.",
            new_units,
            self,
        );

        // this can't underflow, since all other operations can only `fetch_add` to `available`
        *now_available = self
            .available
            .fetch_sub(new_units, std::sync::atomic::Ordering::Relaxed)
            - new_units;

        *now_available = self.available.fetch_add(requested, Ordering::Relaxed) + requested;
        if *now_available <= rate_per_second {
            None
        } else {
            Some(())
        }
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
        let now_available = self.available();
        let to_request = min(now_available, requested);
        // let result = self.rate_limit_internal(requested);
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

impl<TP> RateLimiter for TokenBucket<TP>
where
    TP: TimeProvider,
{
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

    fn rate_limit(&self, requested: u64) -> Option<Option<Instant>> {
        Some(Some(
            self.rate_limit_internal(requested)
                .map(|(delay, _)| delay)?,
        ))
    }

    fn try_rate_limit_without_delay(&self, requested: u64) -> RateLimitResult {
        self.try_rate_limit_without_delay(requested)
    }
}

pub struct HierarchicalTokenBucket<
    ParentRL = TokenBucket<DefaultTimeProvider>,
    ThisRL = TokenBucket<DefaultTimeProvider>,
> {
    last_children_count: AtomicU64,
    parent: Arc<ParentRL>,
    rate_limiter: ThisRL,
}

impl From<NonZeroRatePerSecond> for HierarchicalTokenBucket {
    fn from(rate_per_second: NonZeroRatePerSecond) -> Self {
        HierarchicalTokenBucket::new(rate_per_second)
    }
}

impl<TP> From<(NonZeroRatePerSecond, TP)>
    for HierarchicalTokenBucket<TokenBucket<TP>, TokenBucket<TP>>
where
    TP: TimeProvider + Clone,
{
    fn from((rate_per_second, time_provider): (NonZeroRatePerSecond, TP)) -> Self {
        HierarchicalTokenBucket::new_with_time_provider(rate_per_second, time_provider)
    }
}

impl Clone for HierarchicalTokenBucket {
    fn clone(&self) -> Self {
        Self {
            last_children_count: AtomicU64::new(self.last_children_count.load(Ordering::Relaxed)),
            parent: Arc::clone(&self.parent),
            rate_limiter: self.rate_limiter.clone(),
        }
    }
}

impl HierarchicalTokenBucket {
    pub fn new(rate_per_second: NonZeroRatePerSecond) -> Self {
        Self::new_with_time_provider(rate_per_second, DefaultTimeProvider::default())
    }
}

impl<ParentRL, ThisRL> HierarchicalTokenBucket<ParentRL, ThisRL> {
    fn new_with_time_provider<TP>(rate_per_second: NonZeroRatePerSecond, time_provider: TP) -> Self
    where
        TP: TimeProvider + Clone,
        ParentRL: From<(NonZeroRatePerSecond, TP)>,
        ThisRL: From<(NonZeroRatePerSecond, TP)>,
    {
        let parent = Arc::new(ParentRL::from((
            rate_per_second.clone(),
            time_provider.clone(),
        )));
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

impl<ParentRL, ThisRL> HierarchicalTokenBucket<ParentRL, ThisRL>
where
    ParentRL: RateLimiter,
    ThisRL: RateLimiter,
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

    pub fn rate_limit(&self, requested: u64) -> Option<Option<Instant>> {
        let RateLimitResult {
            dropped: left_tokens,
            delay: required_delay,
        } = self.try_rate_limit_without_delay(requested);

        // account all tokens that we are about to use
        self.parent.rate_limit(left_tokens);
        let result = empty()
            .chain(required_delay)
            .chain(self.rate_limiter.rate_limit(left_tokens).flatten())
            .max();

        Some(Some(result?))
    }
}

impl<ParentRL, ThisRL> RateLimiter for HierarchicalTokenBucket<ParentRL, ThisRL>
where
    ParentRL: RateLimiter,
    ThisRL: RateLimiter,
{
    fn rate(&self) -> RatePerSecond {
        self.parent.rate()
    }

    fn set_rate(&self, rate_per_second: RatePerSecond) {
        self.parent.set_rate(rate_per_second)
    }

    fn rate_limit(&self, requested: u64) -> Option<Option<Instant>> {
        HierarchicalTokenBucket::rate_limit(self, requested)
    }

    fn try_rate_limit_without_delay(&self, requested: u64) -> RateLimitResult {
        HierarchicalTokenBucket::try_rate_limit_without_delay(self, requested)
    }
}

#[derive(Clone)]
pub struct BlockingRateLimiter;

impl RateLimiter for BlockingRateLimiter {
    fn rate(&self) -> RatePerSecond {
        0.into()
    }

    fn set_rate(&self, _rate_per_second: RatePerSecond) {}

    fn rate_limit(&self, _requested: u64) -> Option<Option<Instant>> {
        Some(None)
    }

    fn try_rate_limit_without_delay(&self, requested: u64) -> RateLimitResult {
        RateLimitResult {
            delay: None,
            dropped: requested,
        }
    }
}

#[derive(Clone)]
pub enum RateLimiterFacade<RL> {
    NoTraffic(BlockingRateLimiter),
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
            RatePerSecond::Block => Self::NoTraffic(BlockingRateLimiter),
            RatePerSecond::Rate(rate) => Self::RateLimiter(rate.into()),
        }
    }

    pub fn rate_limit(&self, requested: u64) -> Option<Option<Instant>> {
        match self {
            RateLimiterFacade::NoTraffic(limiter) => limiter.rate_limit(requested),
            RateLimiterFacade::RateLimiter(limiter) => limiter.rate_limit(requested),
        }
    }
}
#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        num::NonZeroU64,
        rc::Rc,
        time::{Duration, Instant},
    };

    use crate::token_bucket::{HierarchicalTokenBucket, NonZeroRatePerSecond};

    use super::{RateLimiter, TimeProvider, TokenBucket};

    #[test]
    fn token_bucket_sanity_check_token_bucket() {
        token_bucket_sanity_check::<TokenBucket<_>>()
    }

    #[test]
    fn token_bucket_sanity_check_hierarchical() {
        token_bucket_sanity_check::<HierarchicalTokenBucket<TokenBucket<_>, TokenBucket<_>>>()
    }

    fn token_bucket_sanity_check<RL>()
    where
        RL: RateLimiter + From<(NonZeroRatePerSecond, Rc<Box<dyn TimeProvider>>)>,
    {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().unwrap_or(NonZeroU64::MIN));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> =
            Box::new(move || *time_provider.as_ref().borrow());
        let time_provider = Rc::new(time_provider);
        let rate_limiter = RL::from((limit_per_second, time_provider));

        *time_to_return.borrow_mut() = now + Duration::from_secs(1);
        assert_eq!(rate_limiter.rate_limit(9), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(1);
        assert!(rate_limiter.rate_limit(12).is_some());

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(rate_limiter.rate_limit(8), None);
    }

    #[test]
    fn no_slowdown_while_within_rate_limit_token_bucket() {
        no_slowdown_while_within_rate_limit::<TokenBucket<_>>()
    }

    #[test]
    fn no_slowdown_while_within_rate_limit_hierarchical() {
        no_slowdown_while_within_rate_limit::<HierarchicalTokenBucket<_, _>>()
    }

    fn no_slowdown_while_within_rate_limit<RL>()
    where
        RL: RateLimiter + From<(NonZeroRatePerSecond, Rc<Box<dyn TimeProvider>>)>,
    {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().unwrap_or(NonZeroU64::MIN));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> =
            Box::new(move || *time_provider.as_ref().borrow());
        let time_provider = Rc::new(time_provider);
        let rate_limiter = RL::from((limit_per_second, time_provider));

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
        slowdown_when_limit_reached::<TokenBucket<_>>()
    }

    #[test]
    fn slowdown_when_limit_reached_hierarchical() {
        slowdown_when_limit_reached::<HierarchicalTokenBucket<_, _>>()
    }

    fn slowdown_when_limit_reached<RL>()
    where
        RL: RateLimiter + From<(NonZeroRatePerSecond, Rc<Box<dyn TimeProvider>>)>,
    {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().unwrap_or(NonZeroU64::MIN));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> =
            Box::new(move || *time_provider.as_ref().borrow());
        let time_provider = Rc::new(time_provider);
        let rate_limiter = RL::from((limit_per_second, time_provider));

        *time_to_return.borrow_mut() = now;
        assert_eq!(rate_limiter.rate_limit(10), None);

        // we should wait some time after reaching the limit
        *time_to_return.borrow_mut() = now;
        assert!(rate_limiter.rate_limit(1).is_some());

        *time_to_return.borrow_mut() = now;
        assert_eq!(
            rate_limiter.rate_limit(19),
            Some(Some(now + Duration::from_secs(2))),
            "we should wait exactly 2 seconds"
        );
    }

    #[test]
    fn buildup_tokens_but_no_more_than_limit_token_bucket() {
        buildup_tokens_but_no_more_than_limit::<TokenBucket<_>>()
    }

    #[test]
    fn buildup_tokens_but_no_more_than_limit_hierarchical() {
        buildup_tokens_but_no_more_than_limit::<HierarchicalTokenBucket<_, _>>()
    }

    fn buildup_tokens_but_no_more_than_limit<RL>()
    where
        RL: RateLimiter + From<(NonZeroRatePerSecond, Rc<Box<dyn TimeProvider>>)>,
    {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().unwrap_or(NonZeroU64::MIN));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> =
            Box::new(move || *time_provider.as_ref().borrow());
        let time_provider = Rc::new(time_provider);
        let rate_limiter = RL::from((limit_per_second, time_provider));

        *time_to_return.borrow_mut() = now + Duration::from_secs(2);
        assert_eq!(rate_limiter.rate_limit(10), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(10);
        assert_eq!(
            rate_limiter.rate_limit(40),
            Some(Some(now + Duration::from_secs(10) + Duration::from_secs(3))),
        );

        *time_to_return.borrow_mut() = now + Duration::from_secs(11);
        assert_eq!(
            rate_limiter.rate_limit(40),
            Some(Some(now + Duration::from_secs(11) + Duration::from_secs(6)))
        );
    }

    #[test]
    fn multiple_calls_buildup_wait_time_token_bucket() {
        multiple_calls_buildup_wait_time::<TokenBucket<_>>()
    }

    #[test]
    fn multiple_calls_buildup_wait_time_hierarchical() {
        multiple_calls_buildup_wait_time::<HierarchicalTokenBucket<_, _>>()
    }

    fn multiple_calls_buildup_wait_time<RL>()
    where
        RL: RateLimiter + From<(NonZeroRatePerSecond, Rc<Box<dyn TimeProvider>>)>,
    {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().unwrap_or(NonZeroU64::MIN));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> =
            Box::new(move || *time_provider.as_ref().borrow());
        let time_provider = Rc::new(time_provider);
        let rate_limiter = RL::from((limit_per_second, time_provider));

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(rate_limiter.rate_limit(10), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(rate_limiter.rate_limit(10), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(
            rate_limiter.rate_limit(10),
            Some(Some(now + Duration::from_secs(3) + Duration::from_secs(1)))
        );

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(
            rate_limiter.rate_limit(50),
            Some(Some(now + Duration::from_secs(3) + Duration::from_secs(6)))
        );
    }
}
