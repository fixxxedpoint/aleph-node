use libp2p::core::muxing::StreamMuxer;
use libp2p::core::transport::{upgrade::Multiplexed, Transport};
use libp2p::identity::PeerId;
use rate_limiter::{RateLimitedAsyncRead, RateLimiter, SleepingRateLimiter};

use crate::RateLimiterConfig;

pub trait TransportBuilder {
    fn build_transport(self) -> impl Transport<Output = (PeerId, impl StreamMuxer)>;
}

pub struct SubstrateTransportBuilder {
    pub keypair: identity::Keypair,
    pub memory_only: bool,
    pub yamux_window_size: Option<u32>,
    pub yamux_maximum_buffer_size: usize,
}

impl TransportBuilder for SubstrateTransportBuilder {
    fn build_transport(self) -> impl Transport<Output = (PeerId, impl StreamMuxer)> {
        sc_network::transport::build_default_transport(
            self.keypair,
            self.memory_only,
            self.yamux_window_size,
            self.yamux_maximum_buffer_size,
        )
    }
}

pub struct RateLimitedTransportBuilder<TB> {
    rate_limiter: RateLimiter,
    inner: TB,
}

impl<TB> RateLimitedTransportBuilder<TB> {
    pub fn new(transport_builder: TB, rate_limiter: RateLimiter) -> Self {
        Self {
            rate_limiter,
            inner: transport_builder,
        }
    }
}

impl<TB: TransportBuilder> TransportBuilder for RateLimitedTransportBuilder<TB> {
    fn build_transport(self) -> impl Transport<Output = (PeerId, impl StreamMuxer)> {
        let transport = self.inner.build_transport();
        transport.map(|(peer_id, stream_muxer), _| {
            (
                peer_id,
                StreamMuxerWrapper::new(stream_muxer, self.rate_limiter.clone()),
            )
        })
    }
}

struct StreamMuxerWrapper<SM> {
    rate_limiter: RateLimiter,
    stream_muxer: SM,
}

impl<SM> StreamMuxerWrapper<SM> {
    pub fn new(stream_muxer: SM, rate_limiter: RateLimiter) -> Self {
        Self {
            rate_limiter,
            stream_muxer,
        }
    }

    fn get_inner(self: std::pin::Pin<&mut Self>) -> std::pin::Pin<&mut SM>
    where
        SM: Unpin,
    {
        let this = self.get_mut();
        std::pin::Pin::new(&mut this.stream_muxer)
    }
}

impl<SM: StreamMuxer> StreamMuxer for StreamMuxerWrapper<SM> {
    type Substream = RateLimitedAsyncRead<SM::Substream>;

    type Error = SM::Error;

    fn poll_inbound(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::Substream, Self::Error>> {
        self.get_inner().poll_inbound(cx).map(|result| {
            result.map(|substream| {
                RateLimitedAsyncRead::new(substream, self.get_mut().rate_limiter.clone())
            })
        })
    }

    fn poll_outbound(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::Substream, Self::Error>> {
        self.get_inner()
            .poll_outbound(cx)
            .map(|result| result.map(RateLimitedAsyncRead::new))
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.get_inner().poll_close(cx)
    }

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<libp2p::core::muxing::StreamMuxerEvent, Self::Error>> {
        self.get_inner().poll(cx)
    }
}

impl TransportBuilder for RateLimiterConfig {
    fn build_transport(self) -> impl Transport<Output = (PeerId, impl StreamMuxer)> {
        let default_transport_builder = SubstrateTransportBuilder {
            keypair: todo!(),
            memory_only: todo!(),
            yamux_window_size: todo!(),
            yamux_maximum_buffer_size: todo!(),
        };
        let rate_limiter = RateLimiter::new(SleepingRateLimiter::new(
            self.substrate_bit_rate_per_connection,
        ));
        RateLimitedTransportBuilder::new(default_transport_builder, rate_limiter).build_transport()
    }
}
