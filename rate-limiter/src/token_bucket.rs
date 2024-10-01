use crate::{
    rate_limiter::{Deadline, RateLimiter},
    NonZeroRatePerSecond, SleepingRateLimiter, LOG_TARGET,
};
use futures::{future::pending, Future, FutureExt};
use log::trace;
use std::{
    cmp::min,
    num::NonZeroU64,
    ops::Deref,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

pub trait TimeProvider {
    fn now(&self) -> Instant;
}

// impl<AsTP> TimeProvider for AsTP
// where
//     AsTP: Deref,
//     AsTP::Target: TimeProvider,
// {
//     fn now(&self) -> Instant {
//         self.deref().now()
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
            // requested: 0,
            requested: rate_per_second.into(),
            time_provider,
        }
    }

    fn max_possible_available(&self) -> u64 {
        self.rate_per_second.into()
    }

    fn available(&self) -> Option<u64> {
        (self.requested <= self.max_possible_available())
            .then(|| u64::from(self.rate_per_second).saturating_sub(self.requested))
    }

    fn account_requested(&mut self, requested: u64) {
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

    pub fn rate(&self) -> NonZeroRatePerSecond {
        self.rate_per_second.into()
    }

    pub fn set_rate(&mut self, rate_per_second: NonZeroRatePerSecond) {
        self.update_tokens();
        let available = self.available();
        let previous_rate_per_second = self.rate_per_second.get();
        self.rate_per_second = rate_per_second.into();
        if available.is_some() {
            let max_for_available = self.max_possible_available();
            let available_after_rate_update = min(available.unwrap_or(0), max_for_available);
            self.requested = self.rate_per_second.get() - available_after_rate_update;
        } else {
            self.requested =
                self.requested - previous_rate_per_second + self.max_possible_available();
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
        self.account_requested(requested);
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

pub trait SharedBandwidth {
    // TODO perhaps return something that holds the counter and sub when dropped - meh, nope
    fn request_bandwidth(&mut self, requested: u64) -> NonZeroRatePerSecond;
    fn notify_idle(&mut self);
    fn await_bandwidth_change(&mut self) -> impl Future<Output = NonZeroRatePerSecond> + Send;
}

struct SharedBandwidthManagerImpl {
    max_rate: NonZeroRatePerSecond,
    active_children: Arc<AtomicU64>,
    // bandwidth_change: tokio::sync::Notify,
    // notified: tokio::sync::Notified,
    bandwidth_change: tokio::sync::broadcast::Receiver<()>,
    bandwidth_change_notify: tokio::sync::broadcast::Sender<()>,
}

impl Clone for SharedBandwidthManagerImpl {
    fn clone(&self) -> Self {
        Self {
            max_rate: self.max_rate.clone(),
            active_children: self.active_children.clone(),
            bandwidth_change: self.bandwidth_change_notify.subscribe(),
            bandwidth_change_notify: self.bandwidth_change_notify.clone(),
        }
    }
}

impl SharedBandwidthManagerImpl {
    pub fn new(rate: NonZeroRatePerSecond) -> Self {
        let (notify_sender, notify_receiver) = tokio::sync::broadcast::channel(2);
        Self {
            max_rate: rate,
            active_children: Arc::new(AtomicU64::new(0)),
            // bandwidth_change: tokio::sync::Notify::new(),
            bandwidth_change: notify_receiver,
            bandwidth_change_notify: notify_sender,
        }
    }
}

#[derive(Clone)]
pub struct SharedBandwidthManager(SharedBandwidthManagerImpl);

impl SharedBandwidthManager {
    fn request_bandwidth_without_children_increament(&self) -> NonZeroRatePerSecond {
        let active_children = self.0.active_children.load(Ordering::Relaxed);
        let rate = u64::from(self.0.max_rate) / active_children;
        NonZeroRatePerSecond(NonZeroU64::new(rate).unwrap_or(NonZeroU64::MIN))
    }
}

impl From<NonZeroRatePerSecond> for SharedBandwidthManager {
    fn from(rate: NonZeroRatePerSecond) -> Self {
        Self(SharedBandwidthManagerImpl::new(rate))
    }
}

impl SharedBandwidth for SharedBandwidthManager {
    // TODO make it grow exponentially on per-connection basis. It won't depend on number of children, but only how often they are calling it.
    fn request_bandwidth(&mut self, _requested: u64) -> NonZeroRatePerSecond {
        // let active_children = self.active_children.load(Ordering::Relaxed);
        let active_children = self.0.active_children.fetch_add(1, Ordering::Relaxed) + 1;
        let rate = u64::from(self.0.max_rate) / active_children;

        // notify everyone else
        let _ = self.0.bandwidth_change_notify.send(());
        // let _ = self.0.bandwidth_change.try_recv();

        NonZeroRatePerSecond(NonZeroU64::new(rate).unwrap_or(NonZeroU64::MIN))
    }

    fn notify_idle(&mut self) {
        self.0.active_children.fetch_sub(1, Ordering::Relaxed);
        // self.0.bandwidth_change.notify_waiters();
        let _ = self.0.bandwidth_change_notify.send(());
    }

    async fn await_bandwidth_change(&mut self) -> NonZeroRatePerSecond {
        // self.0.bandwidth_change.notified().await;
        let _ = self.0.bandwidth_change.recv().await;
        self.request_bandwidth_without_children_increament()
    }
}

pub trait AsyncRateLimiter {
    fn rate_limit(&mut self, requested: u64) -> Option<Deadline>;
    fn set_rate(&mut self, rate: NonZeroRatePerSecond);
    fn wait(&mut self) -> impl std::future::Future<Output = ()> + std::marker::Send;
}

#[derive(Clone)]
pub struct AsyncTokenBucket<TP, SU>
// where
//     TP: TimeProvider,
{
    token_bucket: TokenBucket<TP>,
    next_deadline: Option<Deadline>,
    sleep_until: SU,
}

// impl<IntoTB, TP, IntoSU, SU> From<(IntoTB, IntoSU)> for AsyncTokenBucket<TP, SU>
// where
//     IntoTB: Into<TokenBucket<TP>>,
//     SU: From<IntoSU>,
// {
//     fn from((into_token_bucket, into_sleep_until): (IntoTB, IntoSU)) -> Self {
//         let token_bucket = into_token_bucket.into();
//         let sleep_until = into_sleep_until.into();
//         Self {
//             token_bucket,
//             next_deadline: None,
//             rate_limit_changed: false,
//             sleep_until,
//         }
//     }
// }

// impl<TP, SU> From<NonZeroRatePerSecond> for AsyncTokenBucket<TP, SU>
// where
//     TP: TimeProvider + Default,
//     SU: Default,
// {
//     fn from(value: NonZeroRatePerSecond) -> Self {
//         Self::new(TokenBucket::from(value), SU::default())
//     }
// }

// impl<TP, SU> From<TP> for AsyncTokenBucket<TP, SU> {
//     fn from(value: TP) -> Self {
//         let token_bucket = TokenBucket::new_with_time_provider(NonZeroU64::MIN, value);
//     }
// }

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
            self.token_bucket.set_rate(rate.into());
            self.next_deadline = self.token_bucket.rate_limit(0);
        }
    }

    fn wait(&mut self) -> impl std::future::Future<Output = ()> + std::marker::Send {
        async {
            match self.next_deadline {
                Some(Deadline::Instant(deadline)) => self.sleep_until.sleep_until(deadline).await,
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

// zeby ojciec nie musial wysylac sygnalu do wszystkich w sytuacji
// async condvar
#[derive(Clone)]
pub struct HierarchicalTokenBucket<
    BD = SharedBandwidthManager,
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
            shared_parent: shared_bandwidth,
            rate_limiter: async_token_bucket,
            need_to_notify_parent: false,
        }
    }
}

// impl<BD, ARL, TP> From<(NonZeroRatePerSecond, TP)> for HierarchicalTokenBucket<BD, ARL>
// where
//     BD: SharedBandwidth + From<NonZeroRatePerSecond>,
//     // ARL: From<NonZeroRatePerSecond>,
//     ARL: From<NonZeroRatePerSecond>,
//     TP: TimeProvider,
// {
//     fn from((rate, time_provider): (NonZeroRatePerSecond, TP)) -> Self {

//     }
// }

// impl<A, B, BD, ARL> From<(A, B)> for HierarchicalTokenBucket<BD, ARL>
// where
//     BD: SharedBandwidth + From<A>,
//     ARL: From<B>,
// {
//     fn from((a, b): (A, B)) -> Self {
//         let shared_parent = BD::from(a);
//         let rate_limiter = ARL::from(b);
//         Self {
//             shared_parent,
//             rate_limiter,
//             need_to_notify_parent: false,
//         }
//     }
// }

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

    fn notify_idle(&mut self) {
        if self.need_to_notify_parent {
            self.need_to_notify_parent = false;
            self.shared_parent.notify_idle();
        }
    }

    pub async fn rate_limit(mut self, requested: u64) -> Self
    where
        ARL: AsyncRateLimiter,
    {
        let rate = self.request_bandwidth(requested);
        self.rate_limiter.set_rate(rate);

        self.rate_limiter.rate_limit(requested);

        let mut debug_called = false;

        loop {
            futures::select! {
                _ = self.rate_limiter.wait().fuse() => {
                    println!("debug_called = {debug_called}");
                    self.notify_idle();
                    return self;
                },
                rate = self.shared_parent.await_bandwidth_change().fuse() => {
                    self.rate_limiter.set_rate(rate);
                    debug_called = true;
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
        ops::Deref,
        rc::Rc,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use futures::Future;
    use parking_lot::{Condvar, Mutex};
    use rand::distributions::Uniform;
    use tokio::sync::{futures::Notified, oneshot::Receiver, Barrier, Notify, Semaphore};

    use super::{AsyncRateLimiter, SharedBandwidth, SleepUntil, TimeProvider, TokenBucket};
    use crate::{
        rate_limiter::RateLimiter,
        token_bucket::{
            AsyncTokenBucket, Deadline, HierarchicalTokenBucket, NonZeroRatePerSecond,
            SharedBandwidthManager, SharedBandwidthManagerImpl,
        },
        SleepingRateLimiter,
    };

    // impl<BDI, BD, ARLI, ARL> From<(BDI, ARLI)> for HierarchicalTokenBucket<BD, ARL>
    // where
    //     BD: SharedBandwidth + From<BDI>,
    //     ARL: From<ARLI>,
    // {
    //     fn from((into_shared_bandwidth, into_rate_limiter): (BDI, ARLI)) -> Self {
    //         let shared_parent = into_shared_bandwidth.into();
    //         let rate_limiter = into_rate_limiter.into();
    //         Self {
    //             shared_parent,
    //             rate_limiter,
    //             need_to_notify_parent: false,
    //         }
    //     }
    // }
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
                shared_parent: shared_bandwidth,
                rate_limiter: async_token_bucket,
                need_to_notify_parent: false,
            }
        }
    }

    // impl<BD, ARL, SU> From<(NonZeroRatePerSecond, Rc<Box<(dyn TimeProvider)>>)>
    //     for HierarchicalTokenBucket<BD, ARL>
    // where
    //     BD: SharedBandwidth + From<NonZeroRatePerSecond>,
    // {
    //     fn from(
    //         value@(rate, time_provider): (NonZeroRatePerSecond, Rc<Box<(dyn TimeProvider)>>),
    //     ) -> Self {
    //         let shared_parent = BD::from(rate);
    //         let rate_limiter = ARL::from(value);
    //         Self {
    //             shared_parent: todo!(),
    //             rate_limiter: todo!(),
    //             need_to_notify_parent: todo!(),
    //         }
    //     }
    // }

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

    // impl<AsTP> TimeProvider for AsTP
    // where
    //     AsTP: Deref,
    //     AsTP::Target: TimeProvider,
    // {
    //     fn now(&self) -> Instant {
    //         self.deref().now()
    //     }
    // }

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

        pub fn last_used_instant(&self) -> Instant {
            *self.last_instant.lock()
        }
    }

    impl SleepUntil for TestSleepUntilShared {
        fn sleep_until(&mut self, instant: Instant) -> impl Future<Output = ()> + Send {
            *self.last_instant.lock() = instant;
            async {}
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
            (NonZeroRatePerSecond(rate), time_provider, _): (
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
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> = Box::new(move || *time_provider.borrow());
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
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> = Box::new(move || *time_provider.borrow());
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
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> = Box::new(move || *time_provider.borrow());
        let mut rate_limiter = RL::from((
            limit_per_second,
            time_provider.into(),
            TestSleepUntilShared::new(now),
        ));

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
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> = Box::new(move || *time_provider.borrow());
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
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Rc::new(RefCell::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Box<dyn TimeProvider> = Box::new(move || *time_provider.borrow());
        let mut rate_limiter = RL::from((
            limit_per_second,
            time_provider.into(),
            TestSleepUntilShared::new(now),
        ));

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
        BM: SharedBandwidth,
    {
        fn request_bandwidth(&mut self, requested: u64) -> NonZeroRatePerSecond {
            self.wrapped.request_bandwidth(requested)
        }

        fn notify_idle(&mut self) {
            self.wait.read().wait();
            self.wrapped.notify_idle();
        }

        fn await_bandwidth_change(&mut self) -> impl Future<Output = NonZeroRatePerSecond> + Send {
            self.wrapped.await_bandwidth_change()
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

    // #[derive(Clone)]
    // struct CondvarSleepUntil<SU> {
    //     wait: Arc<tokio::sync::RwLock<tokio::sync::Barrier>>,
    //     // proceed: Arc<tokio::>,
    //     wrapped: SU,
    // }

    #[derive(Clone)]
    struct JoiningSleepUntil<SU> {
        // wait: Arc<tokio::sync::RwLock<tokio::sync::Barrier>>,
        wait: Arc<tokio::sync::Barrier>,
        wrapped: SU,
    }

    impl<SU> JoiningSleepUntil<SU> {
        pub fn new(waiters_count: usize, sleep_until: SU) -> Self {
            // let wait = Arc::new(tokio::sync::RwLock::new(tokio::sync::Barrier::new(
            //     waiters_count,
            // )));
            let wait = Arc::new(tokio::sync::Barrier::new(waiters_count));
            Self {
                wait,
                wrapped: sleep_until,
            }
        }
    }

    impl<SU> SleepUntil for JoiningSleepUntil<SU>
    where
        SU: SleepUntil + Send,
    {
        fn sleep_until(&mut self, instant: Instant) -> impl Future<Output = ()> + Send {
            async move {
                // let _ = self.wait.read().await.wait().await;
                let _ = self.wait.wait().await;
                self.wrapped.sleep_until(instant).await;
            }
        }
    }

    struct NotifyingSharedBandwidth<BM> {
        cloned: Arc<parking_lot::Mutex<Option<tokio::sync::broadcast::Sender<()>>>>,
        notifier: tokio::sync::broadcast::Sender<()>,
        wrapped: BM,
    }

    impl<BM> NotifyingSharedBandwidth<BM> {
        pub fn new(
            wrapped: BM,
            notifier: tokio::sync::broadcast::Sender<()>,
            shared_for_clone: Arc<parking_lot::Mutex<Option<tokio::sync::broadcast::Sender<()>>>>,
        ) -> Self {
            Self {
                notifier,
                wrapped,
                cloned: shared_for_clone,
            }
        }
    }

    impl<BM> Clone for NotifyingSharedBandwidth<BM>
    where
        BM: Clone,
    {
        fn clone(&self) -> Self {
            let new_notifier = {
                let mut locked_cloned = self.cloned.lock();
                match locked_cloned.take() {
                    Some(notifier) => notifier,
                    None => locked_cloned
                        .insert(tokio::sync::broadcast::channel(1).0)
                        .clone(),
                }
            };

            Self {
                cloned: self.cloned.clone(),
                notifier: new_notifier,
                wrapped: self.wrapped.clone(),
            }
        }
    }

    impl<BM> SharedBandwidth for NotifyingSharedBandwidth<BM>
    where
        BM: SharedBandwidth + Send,
    {
        fn request_bandwidth(&mut self, requested: u64) -> NonZeroRatePerSecond {
            self.wrapped.request_bandwidth(requested)
        }

        fn notify_idle(&mut self) {
            self.wrapped.notify_idle()
        }

        fn await_bandwidth_change(&mut self) -> impl Future<Output = NonZeroRatePerSecond> + Send {
            async {
                let result = self.wrapped.await_bandwidth_change().await;
                let _ = self.notifier.send(());
                println!("broadcast called");
                result
            }
        }
    }

    struct NotifiedSleepUntil<SU> {
        cloned: Arc<parking_lot::Mutex<Option<tokio::sync::broadcast::Sender<()>>>>,
        // notifier: Fut,
        notifications: tokio::sync::broadcast::Receiver<()>,
        wrapped: SU,
    }

    impl<SU> NotifiedSleepUntil<SU> {
        pub fn new(
            wrapped: SU,
            notifications: tokio::sync::broadcast::Receiver<()>,
            shared_for_clone: Arc<parking_lot::Mutex<Option<tokio::sync::broadcast::Sender<()>>>>,
        ) -> Self {
            Self {
                notifications,
                wrapped,
                cloned: shared_for_clone,
            }
        }
    }

    impl<SU> Clone for NotifiedSleepUntil<SU>
    where
        SU: Clone,
    {
        fn clone(&self) -> Self {
            let new_notifier = match self.cloned.lock().take() {
                Some(notifier) => notifier,
                None => self
                    .cloned
                    .lock()
                    .insert(tokio::sync::broadcast::channel(1).0)
                    .clone(),
            };

            Self {
                cloned: self.cloned.clone(),
                notifications: new_notifier.subscribe(),
                wrapped: self.wrapped.clone(),
            }
        }
    }

    impl<SU> SleepUntil for NotifiedSleepUntil<SU>
    where
        SU: SleepUntil + Send,
    {
        fn sleep_until(&mut self, instant: Instant) -> impl Future<Output = ()> + Send {
            async move {
                let _ = self.notifications.recv().await;
                self.wrapped.sleep_until(instant).await
            }
        }
    }

    #[tokio::test]
    async fn two_peers_can_share_bandwidth() {
        let limit_per_second = NonZeroRatePerSecond(10.try_into().expect("10 > 0 qed"));
        let now = Instant::now();
        let time_to_return = Arc::new(Mutex::new(now));
        let time_provider = time_to_return.clone();
        let time_provider: Arc<Box<dyn TimeProvider + Send + Sync>> =
            Arc::new(Box::new(move || *time_provider.lock()));

        let last_deadline = Arc::new(Mutex::new(now));
        let last_deadline_cloned = last_deadline.clone();

        // TODO time_provider and sleep until should share a clock?
        // let mut rate_limiter = HierarchicalTokenBucket::<
        //     TestBandwidthManager<SharedBandwidthManager>,
        //     AsyncTokenBucket<_, _>,
        // >::from((
        //     limit_per_second,
        //     time_provider,
        //     // TODO debug this
        //     JoiningSleepUntil::new(2, TestSleepUntil::new(last_deadline.clone())),
        // ));

        let (notifier, notifications_receiver) = tokio::sync::broadcast::channel(2);
        let shared_for_cloning = Arc::new(parking_lot::Mutex::new(None));
        let shared_parent = NotifyingSharedBandwidth::new(
            SharedBandwidthManager::from(limit_per_second),
            notifier.clone(),
            shared_for_cloning.clone(),
        );
        // let sleep_until = JoiningSleepUntil::new(
        //     2,
        //     NotifiedSleepUntil::new(
        //         TestSleepUntil::new(last_deadline.clone()),
        //         notifier,
        //         shared_notifier,
        //     ),
        // );
        // let sleep_until = NotifiedSleepUntil::new(
        //     JoiningSleepUntil::new(2, TestSleepUntil::new(last_deadline.clone())),
        //     notifier,
        //     shared_notifier,
        // );
        let sleep_until = NotifiedSleepUntil::new(
            TestSleepUntil::new(last_deadline.clone()),
            notifications_receiver,
            shared_for_cloning,
        );
        let inner_rate_limiter =
            AsyncTokenBucket::from((limit_per_second, time_provider, sleep_until));

        let mut rate_limiter = HierarchicalTokenBucket::from((shared_parent, inner_rate_limiter));
        let mut rate_limiter_cloned = rate_limiter.clone();

        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let barrier_cloned = barrier.clone();

        // TODO potrzebuje czegos co dostanie sygnal od BandwidthManagera, po otrzymaniu ktorego na pewno sie wait na nim zakonczy - jakis nie-async sygnal, to tym jak juz cos zwroci

        // TODO don't do this, it's seems impossible to do correctly
        // instead, change it into a simple test where you measure average bandwidth
        thread::scope(|s| {
            s.spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                runtime.block_on(async move {
                    barrier.wait().await;
                    // assert_eq!(RateLimiter::rate_limit(&mut rate_limiter, 5), None);
                    rate_limiter = rate_limiter.rate_limit(10).await;

                    barrier.wait().await;
                    // TODO ewidentnie limiter najpierw dostaje wynik z 10/s, zanim zmieni mu sie rate na 5/s
                    assert_eq!(*last_deadline.lock() - now, Duration::from_secs(2));
                    *time_to_return.lock() = *last_deadline.lock();

                    barrier.wait().await;
                    // 500 ms - dostaja wiekszy rate, bo czas dla nich idzie osobno?
                    rate_limiter.rate_limit(10).await;

                    barrier.wait().await;
                    // TODO tutaj sie nadal wywala! - czasami
                    assert_eq!(*last_deadline.lock() - now, Duration::from_secs(4));
                });
            });

            s.spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                runtime.block_on(async move {
                    barrier_cloned.wait().await;
                    rate_limiter_cloned = rate_limiter_cloned.rate_limit(10).await;

                    barrier_cloned.wait().await;
                    assert_eq!(*last_deadline_cloned.lock() - now, Duration::from_secs(2));
                    barrier_cloned.wait().await;

                    rate_limiter_cloned.rate_limit(10).await;

                    barrier_cloned.wait().await;
                    assert_eq!(*last_deadline_cloned.lock() - now, Duration::from_secs(4));
                });
            });
        });
        // TODO we should build TestBandwidthDivider that allows to .await until there is more peers simultaneously
    }

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
            let mut last_instant = self.last_instant.lock();
            *last_instant = max(*last_instant, instant);
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

        let shared_parent = SharedBandwidthManager::from(NonZeroRatePerSecond(rate_limit_nonzero));
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
