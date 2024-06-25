use libp2p::core::muxing::StreamMuxer;
use rate_limiter::{RateLimitedAsyncRead, RateLimiter, SleepingRateLimiter};

pub struct RateLimitedStreamMuxer<SM> {
    rate_per_second: usize,
    stream_muxer: SM,
}

impl<SM> RateLimitedStreamMuxer<SM> {
    pub fn new(stream_muxer: SM, rate_per_second: usize) -> Self {
        Self {
            rate_per_second,
            stream_muxer,
        }
    }

    fn inner(self: std::pin::Pin<&mut Self>) -> std::pin::Pin<&mut SM>
    where
        SM: Unpin,
    {
        let this = self.get_mut();
        std::pin::Pin::new(&mut this.stream_muxer)
    }
}

impl<SM> StreamMuxer for RateLimitedStreamMuxer<SM>
where
    SM: StreamMuxer + Unpin,
    SM::Substream: Unpin,
{
    type Substream = RateLimitedAsyncRead<SM::Substream>;

    type Error = SM::Error;

    fn poll_inbound(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::Substream, Self::Error>> {
        let rate_per_second = self.rate_per_second;
        self.inner().poll_inbound(cx).map(|result| {
            result.map(|substream| {
                RateLimitedAsyncRead::new(
                    substream,
                    RateLimiter::new(SleepingRateLimiter::new(rate_per_second)),
                )
            })
        })
    }

    fn poll_outbound(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::Substream, Self::Error>> {
        let rate_per_second = self.rate_per_second;
        self.inner().poll_outbound(cx).map(|result| {
            result.map(|substream| {
                RateLimitedAsyncRead::new(
                    substream,
                    RateLimiter::new(SleepingRateLimiter::new(rate_per_second)),
                )
            })
        })
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner().poll_close(cx)
    }

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<libp2p::core::muxing::StreamMuxerEvent, Self::Error>> {
        self.inner().poll(cx)
    }
}
