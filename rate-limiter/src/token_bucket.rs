use crate::LOG_TARGET;
use log::trace;
use std::{
    cmp::min,
    sync::{
        atomic::{AtomicI64, AtomicU64},
        Arc,
    },
    time::{Duration, Instant},
};

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
        let mut time_provider = DefaultTimeProvider::default();
        let base_instant = time_provider.now();
        let last_update: u64 = time_provider
            .now()
            .duration_since(base_instant)
            .as_millis()
            .try_into()
            .expect("something's wrong with flux capacitor");
        TokenBucket {
            rate_per_second,
            available: Arc::new(AtomicU64::new(rate_per_second)),
            last_update: Arc::new(AtomicU64::new(last_update)),
            base_instant,
            time_provider,
        }
    }
}

impl<T> TokenBucket<T>
where
    T: TimeProvider,
{
    // #[cfg(test)]
    // pub fn new_with_now(rate_per_second: usize, now: Instant, time_provider: T) -> Self
    // where
    //     T: Clone,
    // {
    //     TokenBucket {
    //         rate_per_second,
    //         available: Arc::new(AtomicI64::new(
    //             rate_per_second.try_into().unwrap_or(i64::MAX),
    //         )),
    //         last_update: now,
    //         time_provider,
    //     }
    // }

    // fn calculate_delay(&self, available: i64, now: Instant) -> Instant {
    //     if self.rate_per_second == 0 {
    //         return Duration::MAX;
    //     }
    //     let delay_micros = (self.requested - self.available)
    //         .saturating_mul(1_000)
    //         .saturating_div(self.rate_per_second);
    //     Duration::from_micros(delay_micros.try_into().unwrap_or(u64::MAX))
    // }

    /// Calculates [Duration](time::Duration) by which we should delay next call to some governed resource in order to satisfy
    /// configured rate limit.
    pub fn rate_limit(&mut self, mut requested: u64) -> Option<Instant> {
        trace!(
            target: LOG_TARGET,
            "TokenBucket called for {} of requested bytes. Internal state: {:?}.",
            requested,
            self
        );
        // let requested: i64 = requested.try_into().expect("'request' is too big");
        let previous_available = self
            .available
            .fetch_add(requested, std::sync::atomic::Ordering::Acquire);
        let now_available = previous_available + requested;
        if now_available <= self.rate_per_second {
            return None;
        }
        let now: u64 = self
            .time_provider
            .now()
            .duration_since(self.base_instant)
            .as_millis()
            .try_into()
            .expect("'u64' should be enough to store milliseconds since 'base_instant'");
        let last_update = self
            .last_update
            .fetch_max(now, std::sync::atomic::Ordering::Acquire);

        if last_update > now {
            return None;
        }

        let since_last_update = now - last_update;
        let new_units = since_last_update
            .saturating_mul(self.rate_per_second)
            .saturating_mul(1_000)
            .try_into()
            .unwrap_or(u64::MAX);
        let last_available = self
            .available
            .fetch_sub(new_units, std::sync::atomic::Ordering::Release);
        if last_available > new_units {
            self.available
                .store(0, std::sync::atomic::Ordering::Release);
        }
        let now_available = last_available.saturating_sub(new_units);

        if now_available <= self.rate_per_second {
            return None;
        }

        let wait_milliseconds = (now_available - self.rate_per_second)
            .saturating_mul(1_000)
            .saturating_div(self.rate_per_second);
        Some(
            self.base_instant
                + Duration::from_millis(now)
                + Duration::from_millis(wait_milliseconds),
        )
        // let now_available = self.available.load(std::sync::atomic::Ordering::Acquire);
    }

    // fn token_limit(&self) -> usize {
    //     self.rate_per_second
    // }
}

// #[cfg(test)]
// mod tests {
//     use std::{
//         cell::RefCell,
//         time::{Duration, Instant},
//     };

//     use super::TokenBucket;

//     #[test]
//     fn token_bucket_sanity_check() {
//         let limit_per_second = 10;
//         let now = Instant::now();
//         let time_to_return = RefCell::new(now);
//         let time_provider = || *time_to_return.borrow();
//         let mut rate_limiter = TokenBucket::new_with_now(limit_per_second, now, time_provider);

//         *time_to_return.borrow_mut() = now + Duration::from_secs(1);
//         assert_eq!(rate_limiter.rate_limit(9), None);

//         *time_to_return.borrow_mut() = now + Duration::from_secs(1);
//         assert!(rate_limiter.rate_limit(12).is_some());

//         *time_to_return.borrow_mut() = now + Duration::from_secs(3);
//         assert_eq!(rate_limiter.rate_limit(8), None);
//     }

//     #[test]
//     fn no_slowdown_while_within_rate_limit() {
//         let limit_per_second = 10;
//         let now = Instant::now();
//         let time_to_return = RefCell::new(now);
//         let time_provider = || *time_to_return.borrow();
//         let mut rate_limiter = TokenBucket::new_with_now(limit_per_second, now, time_provider);

//         *time_to_return.borrow_mut() = now + Duration::from_secs(1);
//         assert_eq!(rate_limiter.rate_limit(9), None);

//         *time_to_return.borrow_mut() = now + Duration::from_secs(2);
//         assert_eq!(rate_limiter.rate_limit(5), None);

//         *time_to_return.borrow_mut() = now + Duration::from_secs(3);
//         assert_eq!(rate_limiter.rate_limit(1), None);

//         *time_to_return.borrow_mut() = now + Duration::from_secs(3);
//         assert_eq!(rate_limiter.rate_limit(9), None);
//     }

//     #[test]
//     fn slowdown_when_limit_reached() {
//         let limit_per_second = 10;
//         let now = Instant::now();
//         let time_to_return = RefCell::new(now);
//         let time_provider = || *time_to_return.borrow();
//         let mut rate_limiter = TokenBucket::new_with_now(limit_per_second, now, time_provider);

//         *time_to_return.borrow_mut() = now;
//         assert_eq!(rate_limiter.rate_limit(10), None);

//         // we should wait some time after reaching the limit
//         *time_to_return.borrow_mut() = now;
//         assert!(rate_limiter.rate_limit(1).is_some());

//         *time_to_return.borrow_mut() = now;
//         assert_eq!(
//             rate_limiter.rate_limit(19),
//             Some(Duration::from_secs(2)),
//             "we should wait exactly 2 seconds"
//         );
//     }

//     #[test]
//     fn buildup_tokens_but_no_more_than_limit() {
//         let limit_per_second = 10;
//         let now = Instant::now();
//         let time_to_return = RefCell::new(now);
//         let time_provider = || *time_to_return.borrow();
//         let mut rate_limiter = TokenBucket::new_with_now(limit_per_second, now, time_provider);

//         *time_to_return.borrow_mut() = now + Duration::from_secs(2);
//         assert_eq!(rate_limiter.rate_limit(10), None);

//         *time_to_return.borrow_mut() = now + Duration::from_secs(10);
//         assert_eq!(rate_limiter.rate_limit(40), Some(Duration::from_secs(3)),);

//         *time_to_return.borrow_mut() = now + Duration::from_secs(11);
//         assert_eq!(rate_limiter.rate_limit(40), Some(Duration::from_secs(6)));
//     }

//     #[test]
//     fn multiple_calls_buildup_wait_time() {
//         let limit_per_second = 10;
//         let now = Instant::now();
//         let time_to_return = RefCell::new(now);
//         let time_provider = || *time_to_return.borrow();
//         let mut rate_limiter = TokenBucket::new_with_now(limit_per_second, now, time_provider);

//         *time_to_return.borrow_mut() = now + Duration::from_secs(3);
//         assert_eq!(rate_limiter.rate_limit(10), None);

//         *time_to_return.borrow_mut() = now + Duration::from_secs(3);
//         assert_eq!(rate_limiter.rate_limit(10), None);

//         *time_to_return.borrow_mut() = now + Duration::from_secs(3);
//         assert_eq!(rate_limiter.rate_limit(10), Some(Duration::from_secs(1)));

//         *time_to_return.borrow_mut() = now + Duration::from_secs(3);
//         assert_eq!(rate_limiter.rate_limit(50), Some(Duration::from_secs(6)));
//     }
// }
