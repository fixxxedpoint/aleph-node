use crate::LOG_TARGET;
use log::{trace, warn};
use parking_lot::Mutex;
use std::{
    cmp::{max, min},
    iter::empty,
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

/// Implementation of the `Token Bucket` algorithm for the purpose of rate-limiting access to some abstract resource.
// #[derive(Clone)]
pub struct TokenBucket<T = DefaultTimeProvider> {
    rate_per_second: AtomicU64,
    available: AtomicU64,
    last_update: AtomicU64,
    last_update_lock: Mutex<()>,
    initial_time: Instant,
    time_provider: T,
}

impl TokenBucket {
    pub fn deep_clone(&self) -> Self {
        Self::new(self.rate_per_second.load(Ordering::Relaxed))
    }
}

impl Clone for TokenBucket {
    fn clone(&self) -> Self {
        Self::new(self.rate_per_second.load(Ordering::Relaxed))
    }
}

pub trait RateLimiter {
    fn rate_per_second(&self) -> u64;
    fn set_rate_per_second(&self, rate_per_second: u64);
    fn rate_limit(&self, requested: u64) -> Option<Option<Instant>>;
    fn try_rate_limit_without_delay(&self, requested: u64) -> RateLimitResult;
}

#[derive(Clone)]
pub struct NoTrafficRateLimiter;

impl RateLimiter for NoTrafficRateLimiter {
    fn rate_per_second(&self) -> u64 {
        0
    }

    fn set_rate_per_second(&self, _rate_per_second: u64) {}

    fn rate_limit(&self, _requested: u64) -> Option<Option<Instant>> {
        Some(None)
    }

    fn try_rate_limit_without_delay(&self, requested: u64) -> RateLimitResult {
        RateLimitResult {
            delay: None,
            dropped: Some(requested),
        }
    }
}

#[derive(Clone)]
pub enum WrappingRateLimiter<RL> {
    NoTraffic(NoTrafficRateLimiter),
    RateLimiter(RL),
}

impl<RL> WrappingRateLimiter<RL>
where
    RL: RateLimiter,
{
    pub fn new(rate_per_second: u64) -> Self
    where
        RL: From<u64>,
    {
        match rate_per_second {
            0 => Self::NoTraffic(NoTrafficRateLimiter),
            rate_per_second => Self::RateLimiter(rate_per_second.into()),
        }
    }

    pub fn rate_limit(&self, requested: u64) -> Option<Option<Instant>> {
        match self {
            WrappingRateLimiter::NoTraffic(limiter) => limiter.rate_limit(requested),
            WrappingRateLimiter::RateLimiter(limiter) => limiter.rate_limit(requested),
        }
    }
}

impl RateLimiter for TokenBucket {
    fn rate_per_second(&self) -> u64 {
        self.rate_per_second.load(Ordering::Relaxed)
    }

    fn set_rate_per_second(&self, rate_per_second: u64) {
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

impl RateLimiter for HierarchicalTokenBucket {
    fn rate_per_second(&self) -> u64 {
        self.rate_limiter.rate_per_second()
    }

    fn set_rate_per_second(&self, rate_per_second: u64) {
        self.rate_limiter.set_rate_per_second(rate_per_second)
    }

    fn rate_limit(&self, requested: u64) -> Option<Option<Instant>> {
        HierarchicalTokenBucket::rate_limit(&self, requested)
    }

    fn try_rate_limit_without_delay(&self, _requested: u64) -> RateLimitResult {
        todo!()
    }
}

// #[derive(Clone)]
pub struct HierarchicalTokenBucket<
    ParentRL = TokenBucket<DefaultTimeProvider>,
    ThisRL = TokenBucket<DefaultTimeProvider>,
> {
    last_children_count: AtomicU64,
    parent: Arc<ParentRL>,
    rate_limiter: ThisRL,
}

impl From<u64> for HierarchicalTokenBucket {
    fn from(rate_per_second: u64) -> Self {
        HierarchicalTokenBucket::new(rate_per_second)
    }
}

impl Clone for HierarchicalTokenBucket {
    fn clone(&self) -> Self {
        Self {
            last_children_count: AtomicU64::new(self.last_children_count.load(Ordering::Relaxed)),
            parent: Arc::clone(&self.parent),
            rate_limiter: self.rate_limiter.deep_clone(),
        }
    }
}

impl HierarchicalTokenBucket {
    pub fn new(rate_per_second: u64) -> Self {
        let parent = Arc::new(TokenBucket::new(rate_per_second));
        let token_bucket = TokenBucket::new(rate_per_second);
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
        let rate_per_second_for_children = max(self.parent.rate_per_second() / children_count, 1);
        self.rate_limiter
            .set_rate_per_second(rate_per_second_for_children);
    }

    pub fn rate_limit(&self, requested: u64) -> Option<Option<Instant>> {
        self.set_rate();

        let result = self.rate_limiter.try_rate_limit_without_delay(requested);
        let left_tokens = result.dropped.unwrap_or(0);
        let for_parent = requested - left_tokens;
        // account all tokens that we used
        self.parent.rate_limit(for_parent);

        // try to borrow from parent
        let parent_result = self.parent.try_rate_limit_without_delay(left_tokens);
        let left_tokens = parent_result.dropped.unwrap_or(0);

        let required_delay = empty().chain(result.delay).chain(parent_result.delay).max();

        let result = empty()
            .chain(required_delay)
            .chain(self.rate_limiter.rate_limit(left_tokens).flatten())
            .max();
        // account all tokens that we used
        self.parent.rate_limit(left_tokens);

        Some(Some(result?))
    }
}

impl<T> std::fmt::Debug for TokenBucket<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenBucket")
            .field("rate_per_second", &self.rate_per_second)
            .field("available", &self.available)
            .field("last_update", &self.last_update)
            .finish()
    }
}

pub struct RateLimitResult {
    pub delay: Option<Instant>,
    pub dropped: Option<u64>,
}

impl TokenBucket {
    /// Constructs a instance of [`TokenBucket`] with given target rate-per-second.
    pub fn new(rate_per_second: u64) -> Self {
        Self::new_with_time_provider(rate_per_second, DefaultTimeProvider)
    }
}

impl<T> TokenBucket<T>
where
    T: TimeProvider,
{
    fn new_with_time_provider(rate_per_second: u64, time_provider: T) -> Self {
        let base_instant = time_provider.now();
        Self {
            rate_per_second: rate_per_second.into(),
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
        let mut now_available = self
            .available
            .fetch_add(requested, std::sync::atomic::Ordering::Relaxed)
            + requested;
        if now_available <= self.rate_per_second.load(Ordering::Relaxed) {
            return None;
        }

        let mut now = self.last_update.load(std::sync::atomic::Ordering::Relaxed);
        if let Some(_guard) = self.last_update_lock.try_lock() {
            if let Some(value) = self.update_tokens(&mut now, &mut now_available) {
                return value;
            }
        } else {
            trace!(
                target: LOG_TARGET,
                "TokenBucket tried to update, but failed {:?}",
                self,
            );
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

    fn update_tokens(
        &self,
        now: &mut u64,
        now_available: &mut u64,
    ) -> Option<Option<(Instant, u64)>> {
        trace!(
            target: LOG_TARGET,
            "TokenBucket about to update it tokens {:?}.",
            self,
        );
        let updated_now = self
            .time_provider
            .now()
            .duration_since(self.initial_time)
            .as_millis()
            .try_into()
            .ok();
        let Some(updated_now) = updated_now else {
            warn!(target: LOG_TARGET, "'u64' should be enough to store milliseconds since 'initial_time' - rate limiting will be turned off.");
            return Some(None);
        };
        *now = updated_now;
        let since_last_update = *now
            - self
                .last_update
                .fetch_max(*now, std::sync::atomic::Ordering::Relaxed);
        let new_units = since_last_update
            .saturating_mul(self.rate_per_second.load(Ordering::Relaxed))
            / SECOND_IN_MILLIS;
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

        if *now_available <= self.rate_per_second.load(Ordering::Relaxed) {
            return Some(None);
        }
        None
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
                dropped: None,
            };
        }
        let now_available = self.available();
        let to_request = min(now_available, requested);
        // let result = self.rate_limit_internal(requested);
        let result = self.rate_limit_internal(to_request);
        let dropped = requested - to_request;
        let dropped = match dropped {
            0 => None,
            some => Some(some),
        };
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

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        time::{Duration, Instant},
    };

    use super::TokenBucket;

    #[test]
    fn token_bucket_sanity_check() {
        let limit_per_second = 10;
        let now = Instant::now();
        let time_to_return = RefCell::new(now);
        let time_provider = || *time_to_return.borrow();
        let mut rate_limiter = TokenBucket::new_with_time_provider(limit_per_second, time_provider);

        *time_to_return.borrow_mut() = now + Duration::from_secs(1);
        assert_eq!(rate_limiter.rate_limit(9), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(1);
        assert!(rate_limiter.rate_limit(12).is_some());

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(rate_limiter.rate_limit(8), None);
    }

    #[test]
    fn no_slowdown_while_within_rate_limit() {
        let limit_per_second = 10;
        let now = Instant::now();
        let time_to_return = RefCell::new(now);
        let time_provider = || *time_to_return.borrow();
        let mut rate_limiter = TokenBucket::new_with_time_provider(limit_per_second, time_provider);

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
    fn slowdown_when_limit_reached() {
        let limit_per_second = 10;
        let now = Instant::now();
        let time_to_return = RefCell::new(now);
        let time_provider = || *time_to_return.borrow();
        let mut rate_limiter = TokenBucket::new_with_time_provider(limit_per_second, time_provider);

        *time_to_return.borrow_mut() = now;
        assert_eq!(rate_limiter.rate_limit(10), None);

        // we should wait some time after reaching the limit
        *time_to_return.borrow_mut() = now;
        assert!(rate_limiter.rate_limit(1).is_some());

        *time_to_return.borrow_mut() = now;
        assert_eq!(
            rate_limiter.rate_limit(19),
            Some(now + Duration::from_secs(2)),
            "we should wait exactly 2 seconds"
        );
    }

    #[test]
    fn buildup_tokens_but_no_more_than_limit() {
        let limit_per_second = 10;
        let now = Instant::now();
        let time_to_return = RefCell::new(now);
        let time_provider = || *time_to_return.borrow();
        let mut rate_limiter = TokenBucket::new_with_time_provider(limit_per_second, time_provider);

        *time_to_return.borrow_mut() = now + Duration::from_secs(2);
        assert_eq!(rate_limiter.rate_limit(10), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(10);
        assert_eq!(
            rate_limiter.rate_limit(40),
            Some(now + Duration::from_secs(10) + Duration::from_secs(3)),
        );

        *time_to_return.borrow_mut() = now + Duration::from_secs(11);
        assert_eq!(
            rate_limiter.rate_limit(40),
            Some(now + Duration::from_secs(11) + Duration::from_secs(6))
        );
    }

    #[test]
    fn multiple_calls_buildup_wait_time() {
        let limit_per_second = 10;
        let now = Instant::now();
        let time_to_return = RefCell::new(now);
        let time_provider = || *time_to_return.borrow();
        let mut rate_limiter = TokenBucket::new_with_time_provider(limit_per_second, time_provider);

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(rate_limiter.rate_limit(10), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(rate_limiter.rate_limit(10), None);

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(
            rate_limiter.rate_limit(10),
            Some(now + Duration::from_secs(3) + Duration::from_secs(1))
        );

        *time_to_return.borrow_mut() = now + Duration::from_secs(3);
        assert_eq!(
            rate_limiter.rate_limit(50),
            Some(now + Duration::from_secs(3) + Duration::from_secs(6))
        );
    }
}
