use libp2p::core::transport::{Transport, upgrade::Multiplexed};
use libp2p::core::muxing::StreamMuxer;
use libp2p::identity::PeerId;

// pub struct TransportBuilder {}

pub trait TransportBuilder {
    // type Muxer: StreamMuxer;
    // type AuthenticatedTransport: Transport<Output = (PeerId, Self::Muxer)>;

    // fn build_transport(self) -> Multiplexed<Self::AuthenticatedTransport>;

    fn build_transport(self) -> Multiplexed<impl Transport<Output = (PeerId, impl StreamMuxer)>>;
}

    // pub fn build_transport() -> Multiplexed<impl Transport<Output = (PeerId, impl StreamMuxer)>> {
    //     todo!()
    // }

pub struct SubstrateTransportBuilder {}

// type Muxer = impl StreamMuxer;

// type AuthTrans = impl Transport<Output = (PeerId, Muxer)>;

impl TransportBuilder for SubstrateTransportBuilder {
    fn build_transport(self) -> Multiplexed<impl Transport<Output = (PeerId, impl StreamMuxer)>> {
        todo!()
    }
}
