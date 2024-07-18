use crate::LOG_TARGET;
use log::{trace, warn};
use parking_lot::Mutex;
use std::{
    cmp::min,
    sync::{atomic::AtomicU64, Arc},
    time::{Duration, Instant},
};

const SECOND_IN_MILLIS: u64 = 1_000;

pub trait TimeProvider {
    fn now(&mut self) -> Instant;
}

impl<F> TimeProvider for F
where
    F: FnMut() -> Instant,
{
    fn now(&mut self) -> Instant {
        self()
    }
}

#[derive(Clone, Default)]
pub struct DefaultTimeProvider;

impl TimeProvider for DefaultTimeProvider {
    fn now(&mut self) -> Instant {
        Instant::now()
    }
}

/// Implementation of the `Token Bucket` algorithm for the purpose of rate-limiting access to some abstract resource.
#[derive(Clone)]
pub struct TokenBucket<T = DefaultTimeProvider> {
    rate_per_second: u64,
    available: Arc<AtomicU64>,
    last_update: Arc<AtomicU64>,
    last_update_lock: Arc<Mutex<()>>,
    base_instant: Instant,
    time_provider: T,
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
    fn new_with_time_provider(rate_per_second: u64, mut time_provider: T) -> Self {
        let base_instant = time_provider.now();
        let last_update: u64 = base_instant
            .duration_since(base_instant)
            .as_millis()
            .try_into()
            .expect("something's wrong with the flux capacitor");
        Self {
            rate_per_second,
            available: Arc::new(AtomicU64::new(0)),
            last_update: Arc::new(AtomicU64::new(last_update)),
            last_update_lock: Arc::new(Mutex::new(())),
            base_instant,
            time_provider,
        }
    }

    fn available(&self) -> u64 {
        self.rate_per_second
            .saturating_sub(self.available.load(std::sync::atomic::Ordering::Relaxed))
    }

    // fn request(&mut self, requested: u64) {
    //     self.available.fetch_add(val, order)
    // }

    /// Calculates [Duration](time::Duration) by which we should delay next call to some governed resource in order to satisfy
    /// configured rate limit.
    pub fn rate_limit(&mut self, requested: u64) -> Option<Instant> {
        trace!(
            target: LOG_TARGET,
            "TokenBucket called for {} of requested bytes. Internal state: {:?}.",
            requested,
            self
        );
        let mut before_available = self
            .available
            .fetch_add(requested, std::sync::atomic::Ordering::Relaxed);
        let mut now_available = before_available.saturating_add(requested);

        // let mut now_available = self.available.load(std::sync::atomic::Ordering::Relaxed);

        if now_available <= self.rate_per_second {
            return None;
        }

        println!("need to update the TokenBucket");

        let mut now = None;
        if let Some(_guard) = self.last_update_lock.try_lock() {
            println!("updating TokenBucket");
            now = self
                .time_provider
                .now()
                .duration_since(self.base_instant)
                .as_millis()
                .try_into()
                .ok();
            let Some(now) = now else {
                warn!(target: LOG_TARGET, "'u64' should be enough to store milliseconds since 'base_instant' - rate limiting will be turned off.");
                return None;
            };
            let since_last_update = now
                - self
                    .last_update
                    .fetch_max(now, std::sync::atomic::Ordering::Relaxed);

            let new_units = since_last_update
                .saturating_mul(self.rate_per_second)
                .saturating_div(SECOND_IN_MILLIS);

            now_available = self
                .available
                .load(std::sync::atomic::Ordering::Relaxed)
                .saturating_sub(requested);
            let new_units = min(now_available, new_units);
            // this can't overflow, since all other operations can only `fetch_add` to `available`
            now_available = self
                .available
                .fetch_sub(new_units, std::sync::atomic::Ordering::Relaxed)
                - new_units;

            if now_available <= self.rate_per_second {
                println!(
                    "new_units={}; since_last_update={}",
                    new_units, since_last_update
                );
                return None;
            }
        }

        let wait_milliseconds = (now_available - self.rate_per_second)
            .saturating_mul(SECOND_IN_MILLIS)
            .saturating_div(self.rate_per_second);
        let now = match now {
            Some(now) => Duration::from_millis(now),
            None => {
                Duration::from_millis(self.last_update.load(std::sync::atomic::Ordering::Relaxed))
            }
        };
        let wait_duration = Duration::from_millis(wait_milliseconds);
        println!(
            "gonna wait until: {:?}",
            self.base_instant + now + wait_duration
        );
        Some(self.base_instant + now + wait_duration)
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
