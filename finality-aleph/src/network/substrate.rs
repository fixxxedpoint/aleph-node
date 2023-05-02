use std::{
    cmp::max,
    collections::HashMap,
    fmt, iter,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use aleph_primitives::BlockNumber;
use async_trait::async_trait;
use futures::stream::{Stream, StreamExt};
use log::{error, trace};
use rate_limiter::TokenBucket;
use sc_consensus::JustificationSyncLink;
use sc_network::{
    multiaddr::Protocol as MultiaddressProtocol, Event as SubstrateEvent, Multiaddr,
    NetworkService, NetworkSyncForkRequest, PeerId, ReputationChange,
};
use sc_network_common::{
    protocol::ProtocolName,
    service::{NetworkEventStream as _, NetworkNotification, NetworkPeers, NotificationSender},
    ExHashT,
};
use sc_peerset::BANNED_THRESHOLD;
use sp_runtime::traits::{Block, Header};

use crate::{
    network::{
        gossip::{Event, EventStream, NetworkSender, Protocol, RawNetwork},
        RequestBlocks,
    },
    IdentifierFor,
};

impl<B: Block, H: ExHashT> RequestBlocks<IdentifierFor<B>> for Arc<NetworkService<B, H>>
where
    B::Header: Header<Number = BlockNumber>,
{
    fn request_justification(&self, block_id: IdentifierFor<B>) {
        NetworkService::request_justification(self, &block_id.hash, block_id.number)
    }

    fn request_stale_block(&self, block_id: IdentifierFor<B>) {
        // The below comment is adapted from substrate:
        // Notifies the sync service to try and sync the given block from the given peers. If the given vector
        // of peers is empty (as in our case) then the underlying implementation should make a best effort to fetch
        // the block from any peers it is connected to.
        NetworkService::set_sync_fork_request(self, Vec::new(), block_id.hash, block_id.number)
    }

    /// Clear all pending justification requests.
    fn clear_justification_requests(&self) {
        NetworkService::clear_justification_requests(self)
    }
}

/// Name of the network protocol used by Aleph Zero to disseminate validator
/// authentications.
const AUTHENTICATION_PROTOCOL_NAME: &str = "/auth/0";

/// Legacy name of the network protocol used by Aleph Zero to disseminate validator
/// authentications. Might be removed after some updates.
const LEGACY_AUTHENTICATION_PROTOCOL_NAME: &str = "/aleph/1";

/// Name of the network protocol used by Aleph Zero to synchronize the block state.
const BLOCK_SYNC_PROTOCOL_NAME: &str = "/sync/0";

/// Convert protocols to their names and vice versa.
#[derive(Clone)]
pub struct ProtocolNaming {
    authentication_name: ProtocolName,
    authentication_fallback_names: Vec<ProtocolName>,
    block_sync_name: ProtocolName,
    protocols_by_name: HashMap<ProtocolName, Protocol>,
}

impl ProtocolNaming {
    /// Create a new protocol naming scheme with the given chain prefix.
    pub fn new(chain_prefix: String) -> Self {
        let authentication_name: ProtocolName =
            format!("{}{}", chain_prefix, AUTHENTICATION_PROTOCOL_NAME).into();
        let mut protocols_by_name = HashMap::new();
        protocols_by_name.insert(authentication_name.clone(), Protocol::Authentication);
        let authentication_fallback_names: Vec<ProtocolName> =
            vec![LEGACY_AUTHENTICATION_PROTOCOL_NAME.into()];
        for protocol_name in &authentication_fallback_names {
            protocols_by_name.insert(protocol_name.clone(), Protocol::Authentication);
        }
        let block_sync_name: ProtocolName =
            format!("{}{}", chain_prefix, BLOCK_SYNC_PROTOCOL_NAME).into();
        protocols_by_name.insert(block_sync_name.clone(), Protocol::BlockSync);
        ProtocolNaming {
            authentication_name,
            authentication_fallback_names,
            block_sync_name,
            protocols_by_name,
        }
    }

    /// Returns the canonical name of the protocol.
    pub fn protocol_name(&self, protocol: &Protocol) -> ProtocolName {
        use Protocol::*;
        match protocol {
            Authentication => self.authentication_name.clone(),
            BlockSync => self.block_sync_name.clone(),
        }
    }

    /// Returns the fallback names of the protocol.
    pub fn fallback_protocol_names(&self, protocol: &Protocol) -> Vec<ProtocolName> {
        use Protocol::*;
        match protocol {
            Authentication => self.authentication_fallback_names.clone(),
            _ => Vec::new(),
        }
    }

    /// Attempts to convert the protocol name to a protocol.
    fn to_protocol(&self, protocol_name: &str) -> Option<Protocol> {
        self.protocols_by_name.get(protocol_name).copied()
    }
}

#[derive(Debug)]
pub enum SenderError {
    CannotCreateSender(PeerId, Protocol),
    LostConnectionToPeer(PeerId),
    LostConnectionToPeerReady(PeerId),
}

impl fmt::Display for SenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SenderError::CannotCreateSender(peer_id, protocol) => {
                write!(
                    f,
                    "Can not create sender to peer {:?} with protocol {:?}",
                    peer_id, protocol
                )
            }
            SenderError::LostConnectionToPeer(peer_id) => {
                write!(
                    f,
                    "Lost connection to peer {:?} while preparing sender",
                    peer_id
                )
            }
            SenderError::LostConnectionToPeerReady(peer_id) => {
                write!(
                    f,
                    "Lost connection to peer {:?} after sender was ready",
                    peer_id
                )
            }
        }
    }
}

impl std::error::Error for SenderError {}

pub struct SubstrateNetworkSender {
    notification_sender: Box<dyn NotificationSender>,
    peer_id: PeerId,
}

#[async_trait]
impl NetworkSender for SubstrateNetworkSender {
    type SenderError = SenderError;

    async fn send<'a>(
        &'a self,
        data: impl Into<Vec<u8>> + Send + Sync + 'static,
    ) -> Result<(), SenderError> {
        self.notification_sender
            .ready()
            .await
            .map_err(|_| SenderError::LostConnectionToPeer(self.peer_id))?
            .send(data.into())
            .map_err(|_| SenderError::LostConnectionToPeerReady(self.peer_id))
    }
}

pub struct NetworkEventStream<B: Block, H: ExHashT> {
    stream: Pin<Box<dyn Stream<Item = SubstrateEvent> + Send>>,
    naming: ProtocolNaming,
    network: Arc<NetworkService<B, H>>,
}

#[async_trait]
impl<B: Block, H: ExHashT> EventStream<PeerId> for NetworkEventStream<B, H> {
    async fn next_event(&mut self) -> Option<Event<PeerId>> {
        use Event::*;
        use SubstrateEvent::*;
        loop {
            match self.stream.next().await {
                Some(event) => match event {
                    SyncConnected { remote } => {
                        let multiaddress: Multiaddr =
                            iter::once(MultiaddressProtocol::P2p(remote.into())).collect();
                        trace!(target: "aleph-network", "Connected event from address {:?}", multiaddress);
                        if let Err(e) = self.network.add_peers_to_reserved_set(
                            self.naming.protocol_name(&Protocol::Authentication),
                            iter::once(multiaddress.clone()).collect(),
                        ) {
                            error!(target: "aleph-network", "add_reserved failed for authentications: {}", e);
                        }
                        if let Err(e) = self.network.add_peers_to_reserved_set(
                            self.naming.protocol_name(&Protocol::BlockSync),
                            iter::once(multiaddress).collect(),
                        ) {
                            error!(target: "aleph-network", "add_reserved failed for block sync: {}", e);
                        }
                        continue;
                    }
                    SyncDisconnected { remote } => {
                        trace!(target: "aleph-network", "Disconnected event for peer {:?}", remote);
                        let addresses: Vec<_> = iter::once(remote).collect();
                        self.network.remove_peers_from_reserved_set(
                            self.naming.protocol_name(&Protocol::Authentication),
                            addresses.clone(),
                        );
                        self.network.remove_peers_from_reserved_set(
                            self.naming.protocol_name(&Protocol::BlockSync),
                            addresses,
                        );
                        continue;
                    }
                    NotificationStreamOpened {
                        remote, protocol, ..
                    } => match self.naming.to_protocol(protocol.as_ref()) {
                        Some(protocol) => return Some(StreamOpened(remote, protocol)),
                        None => continue,
                    },
                    NotificationStreamClosed { remote, protocol } => {
                        match self.naming.to_protocol(protocol.as_ref()) {
                            Some(protocol) => return Some(StreamClosed(remote, protocol)),
                            None => continue,
                        }
                    }
                    NotificationsReceived { messages, remote } => {
                        return Some(Messages(
                            remote,
                            messages
                                .into_iter()
                                .filter_map(|(protocol, data)| {
                                    self.naming
                                        .to_protocol(protocol.as_ref())
                                        .map(|protocol| (protocol, data))
                                })
                                .collect(),
                        ));
                    }
                    Dht(_) => continue,
                },
                None => return None,
            }
        }
    }
}

/// A wrapper around the substrate network that includes information about protocol names.
#[derive(Clone)]
pub struct SubstrateNetwork<B: Block, H: ExHashT> {
    network: Arc<NetworkService<B, H>>,
    naming: ProtocolNaming,
}

impl<B: Block, H: ExHashT> SubstrateNetwork<B, H> {
    /// Create a new substrate network wrapper.
    pub fn new(network: Arc<NetworkService<B, H>>, naming: ProtocolNaming) -> Self {
        SubstrateNetwork { network, naming }
    }
}

impl<B: Block, H: ExHashT> RawNetwork for SubstrateNetwork<B, H> {
    type SenderError = SenderError;
    type NetworkSender = SubstrateNetworkSender;
    type PeerId = PeerId;
    type EventStream = NetworkEventStream<B, H>;

    fn event_stream(&self) -> Self::EventStream {
        NetworkEventStream {
            stream: Box::pin(self.network.as_ref().event_stream("aleph-network")),
            naming: self.naming.clone(),
            network: self.network.clone(),
        }
    }

    fn sender(
        &self,
        peer_id: Self::PeerId,
        protocol: Protocol,
    ) -> Result<Self::NetworkSender, Self::SenderError> {
        Ok(SubstrateNetworkSender {
            // Currently method `notification_sender` does not distinguish whether we are not connected to the peer
            // or there is no such protocol so we need to have this worthless `SenderError::CannotCreateSender` error here
            notification_sender: self
                .network
                .notification_sender(peer_id, self.naming.protocol_name(&protocol))
                .map_err(|_| SenderError::CannotCreateSender(peer_id, protocol))?,
            peer_id,
        })
    }
}

pub struct ReputationRateLimiter {
    rate_limiter: TokenBucket,
    duration_to_reputation_ratio: i32,
}

impl ReputationRateLimiter {
    pub fn new(rate_limiter: TokenBucket, time_to_ban: Duration) -> Self {
        let duration_to_reputation_ratio =
            BANNED_THRESHOLD / i32::try_from(max(time_to_ban.as_secs(), 1)).unwrap_or(i32::MAX);
        Self {
            rate_limiter,
            duration_to_reputation_ratio,
        }
    }

    fn map_to_reputation(&self, duration: Duration) -> ReputationChange {
        let reputation_delta = self.duration_to_reputation_ratio
            * i32::try_from(duration.as_secs()).unwrap_or(i32::MAX);
        ReputationChange::new(reputation_delta, "Peer exceeded bandwidth limit.")
    }
}

impl ReputationRateLimiter {
    pub fn rate_limit(&mut self, read_size: usize) -> Option<ReputationChange> {
        let wait_time = self.rate_limiter.rate_limit(read_size, || Instant::now());
        wait_time.map(|time| self.map_to_reputation(time))
    }
}

pub struct ReputationRateLimitedEventStream<NP, ES> {
    rate_limiter: ReputationRateLimiter,
    rate_limiters: HashMap<PeerId, ReputationRateLimiter>,
    stream: ES,
    network: NP,
}

impl<NP, ES> ReputationRateLimitedEventStream<NP, ES> {
    pub fn new(rate_limiter: ReputationRateLimiter, stream: ES, network: NP) -> Self {
        Self {
            rate_limiter,
            stream,
            network,
        }
    }

    fn compute_size<P>(&self, event: &Event<P>) -> usize {
        if let Event::Messages(_, messages) = event {
            let mut size = 0;
            for message in messages {
                size = message.1.len().saturating_add(size);
            }
            size
        } else {
            0
        }
    }
}

#[async_trait]
impl<NP, ES> EventStream<PeerId> for ReputationRateLimitedEventStream<NP, ES>
where
    NP: NetworkPeers + Send,
    ES: EventStream<PeerId> + Send,
{
    async fn next_event(&mut self) -> Option<Event<PeerId>> {
        let event = self.stream.next_event().await?;
        let size = self.compute_size(&event);
        // TODO implement this on per peer basis
        if let Some(reputation_change) = self.rate_limiter.rate_limit(size) {
            let who = event.peer();
            self.network.report_peer(who.clone(), reputation_change);
        }
        Some(event)
    }
}

#[derive(Clone)]
pub struct MapNetwork<RN, F> {
    network: RN,
    event_stream_map: F,
}

impl<RN, F> MapNetwork<RN, F> {
    pub fn new(network: RN, event_stream_map: F) -> Self {
        Self {
            network,
            event_stream_map,
        }
    }
}

impl<RN, F, ES> RawNetwork for MapNetwork<RN, F>
where
    RN: RawNetwork,
    ES: EventStream<RN::PeerId>,
    F: Fn(RN::EventStream) -> ES + Send + Sync + Clone + 'static,
{
    type SenderError = RN::SenderError;
    type NetworkSender = RN::NetworkSender;
    type PeerId = RN::PeerId;
    type EventStream = ES;

    fn event_stream(&self) -> Self::EventStream {
        (self.event_stream_map)(self.network.event_stream())
    }

    fn sender(
        &self,
        peer_id: Self::PeerId,
        protocol: Protocol,
    ) -> Result<Self::NetworkSender, Self::SenderError> {
        self.network.sender(peer_id, protocol)
    }
}

pub fn new_reputation_rate_limited_network<B: Block, H: ExHashT, NP: NetworkPeers>(
    network: SubstrateNetwork<B, H>,
) -> MapNetwork<
    SubstrateNetwork<B, H>,
    impl Fn(NetworkEventStream<B, H>) -> ReputationRateLimitedEventStream<NP, NetworkEventStream<B, H>>,
> {
    MapNetwork::new(network)
}
