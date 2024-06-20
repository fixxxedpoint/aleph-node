use libp2p::core::muxing::StreamMuxer;
use libp2p::core::transport::{upgrade::Multiplexed, Transport};
use libp2p::identity::PeerId;

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
    inner: TB,
}

impl<TB: TransportBuilder> TransportBuilder for RateLimitedTransportBuilder<TB> {
    fn build_transport(self) -> impl Transport<Output = (PeerId, impl StreamMuxer)> {
        let transport = self.inner.build_transport();
        transport.map(|(peer_id, stream_muxer), _| {
            // TODO wrap with rate-limiter
            stream_muxer.
        })
    }
}

struct StreamMuxerWrapper<SM> {
    stream_muxer: SM,
}

impl<SM: StreamMuxer> StreamMuxer for StreamMuxerWrapper<SM> {
    type Substream;

    type Error;

    fn poll_inbound(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::Substream, Self::Error>> {
        todo!()
    }

    fn poll_outbound(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::Substream, Self::Error>> {
        todo!()
    }

    fn poll_close(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<libp2p::core::muxing::StreamMuxerEvent, Self::Error>> {
        todo!()
    }
                }
