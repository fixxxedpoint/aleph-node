use libp2p::core::muxing::StreamMuxer;
use libp2p::core::transport::Transport;
use libp2p::identity::PeerId;
use rate_limiter::{RateLimitedAsyncRead, RateLimiter, SleepingRateLimiter};
use sc_network::service::NetworkConfig;

pub trait TransportBuilder<Config> {
    fn build_transport(
        self,
        config: impl Into<Config>,
    ) -> impl Transport<
        Output = (
            PeerId,
            impl StreamMuxer<Substream = impl Unpin, Error = impl Send> + Send + Unpin,
        ),
        Dial = impl Send,
        ListenerUpgrade = impl Send,
        Error = impl Send + Sync,
    > + Send;
}

pub struct SubstrateTransportBuilder {}

impl SubstrateTransportBuilder {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for SubstrateTransportBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TransportBuilder<NetworkConfig> for SubstrateTransportBuilder {
    fn build_transport(
        self,
        config: impl Into<NetworkConfig>,
    ) -> impl Transport<
        Output = (
            PeerId,
            impl StreamMuxer<Substream = impl Unpin, Error = impl Send> + Send + Unpin,
        ),
        Dial = impl Send,
        ListenerUpgrade = impl Send,
        Error = impl Send + Sync,
    > + Send {
        let config = config.into();
        sc_network::transport::build_default_transport(
            config.keypair,
            config.memory_only,
            config.muxer_window_size,
            config.muxer_maximum_buffer_size,
        )
    }
}

pub struct RateLimitedTransportBuilder<TB> {
    rate_per_second: usize,
    inner: TB,
}

impl<TB> RateLimitedTransportBuilder<TB> {
    pub fn new(transport_builder: TB, rate_per_second: usize) -> Self {
        Self {
            rate_per_second,
            inner: transport_builder,
        }
    }
}

impl<TC, TB: TransportBuilder<TC>> TransportBuilder<TC> for RateLimitedTransportBuilder<TB> {
    fn build_transport(
        self,
        config: impl Into<TC>,
    ) -> impl Transport<
        Output = (
            PeerId,
            impl StreamMuxer<Substream = impl Unpin, Error = impl Send> + Send + Unpin,
        ),
        Dial = impl Send,
        ListenerUpgrade = impl Send,
        Error = impl Send + Sync,
    > + Send {
        let rate_per_second = self.rate_per_second;
        let transport = self.inner.build_transport(config.into());
        transport.map(move |(peer_id, stream_muxer), _| {
            (
                peer_id,
                StreamMuxerWrapper::new(stream_muxer, rate_per_second),
            )
        })
    }
}

struct StreamMuxerWrapper<SM> {
    rate_per_second: usize,
    stream_muxer: SM,
}

impl<SM> StreamMuxerWrapper<SM> {
    pub fn new(stream_muxer: SM, rate_per_second: usize) -> Self {
        Self {
            rate_per_second,
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

impl<SM> StreamMuxer for StreamMuxerWrapper<SM>
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
        self.get_inner().poll_inbound(cx).map(|result| {
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
        self.get_inner().poll_outbound(cx).map(|result| {
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
        self.get_inner().poll_close(cx)
    }

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<libp2p::core::muxing::StreamMuxerEvent, Self::Error>> {
        self.get_inner().poll(cx)
    }
}

// impl TransportBuilder for RateLimiterConfig {
//     fn build_transport(self) -> impl Transport<Output = (PeerId, impl StreamMuxer)> {
//         let default_transport_builder = SubstrateTransportBuilder {
//             keypair: todo!(),
//             memory_only: todo!(),
//             yamux_window_size: todo!(),
//             yamux_maximum_buffer_size: todo!(),
//         };
//         let rate_limiter = RateLimiter::new(SleepingRateLimiter::new(
//             self.substrate_bit_rate_per_connection,
//         ));
//         RateLimitedTransportBuilder::new(default_transport_builder, rate_limiter).build_transport()
//     }
// }
