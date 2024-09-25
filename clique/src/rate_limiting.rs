use rate_limiter::{
    NonZeroRatePerSecond, RateLimitedAsyncRead, RateLimiter, SleepingRateLimiter, RateLimiterT,
    SleepingRateLimiter,
};

use crate::{ConnectionInfo, Data, Dialer, Listener, PeerAddressInfo, Splittable, Splitted};

impl<Read: ConnectionInfo, RL> ConnectionInfo for RateLimitedAsyncRead<Read, RL> {
    fn peer_address_info(&self) -> PeerAddressInfo {
        self.inner().peer_address_info()
    }
}

/// Implementation of the [Dialer] trait governing all returned [Dialer::Connection] instances by a rate-limiting wrapper.
#[derive(Clone)]
pub struct RateLimitingDialer<D, RL> {
    dialer: D,
    rate_limiter: RL,
}

impl<D, RL> RateLimitingDialer<D, RL> {
    pub fn new(dialer: D, rate_limiter: RL) -> Self {
        Self {
            dialer,
            rate_limiter,
        }
    }
}

#[async_trait::async_trait]
impl<A, D, RL> Dialer<A> for RateLimitingDialer<D, RL>
where
    A: Data,
    D: Dialer<A>,
    <D::Connection as Splittable>::Sender: Unpin,
    <D::Connection as Splittable>::Receiver: Unpin,
    RL: rate_limiter::SleepingRateLimiter
        // + From<rate_limiter::NonZeroRatePerSecond>
        + Send
        + Clone
        + 'static,
{
    type Connection = Splitted<
        RateLimitedAsyncRead<<D::Connection as Splittable>::Receiver, RL>,
        <D::Connection as Splittable>::Sender,
    >;
    type Error = D::Error;

    async fn connect(&mut self, address: A) -> Result<Self::Connection, Self::Error> {
        let connection = self.dialer.connect(address).await?;
        let (sender, receiver) = connection.split();
        Ok(Splitted(
            RateLimitedAsyncRead::new(receiver, RateLimiter::new(self.rate_limiter.clone())),
            sender,
        ))
    }
}

/// Implementation of the [Listener] trait governing all returned [Listener::Connection] instances by a rate-limiting wrapper.
pub struct RateLimitingListener<L, RL> {
    listener: L,
    rate_limiter: RL,
}

impl<L, RL> RateLimitingListener<L, RL> {
    pub fn new(listener: L, rate_limiter: RL) -> Self {
        Self {
            listener,
            rate_limiter,
        }
    }
}

#[async_trait::async_trait]
impl<L, RL> Listener for RateLimitingListener<L, RL>
where
    L: Listener + Send,
    RL: SleepingRateLimiter + Send + Clone + 'static,
{
    type Connection = Splitted<
        RateLimitedAsyncRead<<L::Connection as Splittable>::Receiver, RL>,
        <L::Connection as Splittable>::Sender,
    >;
    type Error = L::Error;

    async fn accept(&mut self) -> Result<Self::Connection, Self::Error> {
        let connection = self.listener.accept().await?;
        let (sender, receiver) = connection.split();
        Ok(Splitted(
            RateLimitedAsyncRead::new(receiver, RateLimiter::new(self.rate_limiter.clone())),
            sender,
        ))
    }
}
