use crate::{ConnectionInfo, Data, Dialer, Listener, Splittable};

pub struct RateLimitedAsyncRead<A> {
    rate_limiter: SleepingRateLimiter,
    read: Pin<Box<A>>,
}

impl<A: AsyncRead> RateLimitedAsyncRead<A> {
    pub fn new(read: A, rate_limiter: TokenBucket) -> Self {
        Self {
            rate_limiter: SleepingRateLimiter::new(rate_limiter),
            read: Box::pin(read),
        }
    }
}

impl<A: AsyncRead> AsyncRead for RateLimitedAsyncRead<A> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let deref_self = self.get_mut();
        match Pin::new(&mut deref_self.rate_limiter.current_sleep()).poll(cx) {
            std::task::Poll::Ready(rate_limiter) => rate_limiter,
            std::task::Poll::Pending => return std::task::Poll::Pending,
        };

        let filled_before = buf.filled().len();
        let result = deref_self.read.as_mut().poll_read(cx, buf);
        let filled_after = buf.filled().len();
        let last_read_size = filled_after - filled_before;

        deref_self.rate_limiter.rate_limit(last_read_size);

        result
    }
}

impl<A: AsyncWrite + Unpin> AsyncWrite for RateLimitedAsyncRead<A> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        self.read.as_mut().poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        self.read.as_mut().poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        self.read.as_mut().poll_shutdown(cx)
    }
}

impl<R: Splittable> Splittable for RateLimitedAsyncRead<R> {
    type Sender = R::Sender;
    type Receiver = RateLimitedAsyncRead<R::Receiver>;

    fn split(self) -> (Self::Sender, Self::Receiver) {
        let (sender, receiver) = Pin::<Box<R>>::into_inner(self.read).split();
        let rate_limiter = self.rate_limiter;
        let receiver = RateLimitedAsyncRead::new(receiver, rate_limiter.into_inner());
        (sender, receiver)
    }
}

impl<A: ConnectionInfo> ConnectionInfo for RateLimitedAsyncRead<A> {
    fn peer_address_info(&self) -> super::PeerAddressInfo {
        self.read.peer_address_info()
    }
}

#[derive(Clone)]
pub struct RateLimitingDialer<D> {
    dialer: D,
    rate_limiter: TokenBucket,
}

impl<D> RateLimitingDialer<D> {
    pub fn new(dialer: D, rate_limiter: TokenBucket) -> Self {
        Self {
            dialer,
            rate_limiter,
        }
    }
}

#[async_trait::async_trait]
impl<A: Data, D: Dialer<A>> Dialer<A> for RateLimitingDialer<D> {
    type Connection = RateLimitedAsyncRead<D::Connection>;
    type Error = D::Error;

    async fn connect(&mut self, address: A) -> Result<Self::Connection, Self::Error> {
        let connection = self.dialer.connect(address).await?;
        Ok(RateLimitedAsyncRead::new(
            connection,
            self.rate_limiter.clone(),
        ))
    }
}

pub struct RateLimitingListener<L> {
    listener: L,
    rate_limiter: TokenBucket,
}

impl<L> RateLimitingListener<L> {
    pub fn new(listener: L, rate_limiter: TokenBucket) -> Self {
        Self {
            listener,
            rate_limiter,
        }
    }
}

#[async_trait::async_trait]
impl<L: Listener + Send> Listener for RateLimitingListener<L> {
    type Connection = RateLimitedAsyncRead<L::Connection>;
    type Error = L::Error;

    async fn accept(&mut self) -> Result<Self::Connection, Self::Error> {
        let connection = self.listener.accept().await?;
        Ok(RateLimitedAsyncRead::new(
            connection,
            self.rate_limiter.clone(),
        ))
    }
}
