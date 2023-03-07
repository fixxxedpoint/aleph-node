use std::{
    collections::HashMap,
    fmt,
    iter::{self, Sum},
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures::{
    stream::{Stream, StreamExt},
    Future,
};
use log::{error, trace};
use sc_consensus::JustificationSyncLink;
use sc_network::{
    multiaddr::Protocol as MultiaddressProtocol, Event as SubstrateEvent, Multiaddr,
    NetworkService, NetworkSyncForkRequest, PeerId,
};
use sc_network_common::{
    protocol::ProtocolName,
    service::{NetworkEventStream as _, NetworkNotification, NetworkPeers, NotificationSender},
    ExHashT,
};
use sp_api::NumberFor;
use sp_consensus::SyncOracle;
use sp_runtime::traits::Block;
use tokio::time::Sleep;

use super::clique::rate_limiter::RateLimiter;
use crate::network::{
    gossip::{Event, EventStream, NetworkSender, Protocol, RawNetwork},
    RequestBlocks,
};

impl<B: Block, H: ExHashT> RequestBlocks<B> for Arc<NetworkService<B, H>> {
    fn request_justification(&self, hash: &B::Hash, number: NumberFor<B>) {
        NetworkService::request_justification(self, hash, number)
    }

    fn request_stale_block(&self, hash: B::Hash, number: NumberFor<B>) {
        // The below comment is adapted from substrate:
        // Notifies the sync service to try and sync the given block from the given peers. If the given vector
        // of peers is empty (as in our case) then the underlying implementation should make a best effort to fetch
        // the block from any peers it is connected to.
        NetworkService::set_sync_fork_request(self, Vec::new(), hash, number)
    }

    /// Clear all pending justification requests.
    fn clear_justification_requests(&self) {
        NetworkService::clear_justification_requests(self)
    }

    fn is_major_syncing(&self) -> bool {
        NetworkService::is_major_syncing(self)
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

pub struct RateLimitedNetworkEventStream<P, ES: EventStream<P>, RL> {
    stream: ES,
    rate_limiter: Option<SleepingRateLimiter<RL>>,
    _phantom_data: PhantomData<P>,
}

impl<P, ES: EventStream<P>, RL> RateLimitedNetworkEventStream<P, ES, RL> {
    pub fn new(stream: ES, rate_limiter: RL) -> Self {
        Self {
            stream,
            rate_limiter: Some(SleepingRateLimiter::new(rate_limiter)),
            _phantom_data: PhantomData,
        }
    }
}

pub struct SleepingRateLimiter<RL> {
    rate_limiter: RL,
    rate_limiter_sleep: Pin<Box<Sleep>>,
}

impl<RL> SleepingRateLimiter<RL> {
    fn new(rate_limiter: RL) -> Self {
        Self {
            rate_limiter,
            rate_limiter_sleep: Box::pin(tokio::time::sleep(Duration::ZERO)),
        }
    }
}

impl<RL: RateLimiter> SleepingRateLimiter<RL> {
    pub fn rate_limit(mut self, read_size: usize) -> RateLimiterTask<RL> {
        let mut now = None;
        let mut now_closure = || now.get_or_insert_with(|| Instant::now()).clone();
        let next_wait = self.rate_limiter.rate_limit(read_size, &mut now_closure);
        let next_wait = now_closure() + next_wait.unwrap_or(Duration::ZERO);
        let mut next_sleep = self.rate_limiter_sleep;
        next_sleep.set(tokio::time::sleep_until(next_wait.into()));

        RateLimiterTask {
            rate_limiter: Some(self.rate_limiter),
            rate_limiter_sleep: Some(next_sleep),
        }
    }
}

pub struct RateLimiterTask<RL> {
    rate_limiter: Option<RL>,
    rate_limiter_sleep: Option<Pin<Box<Sleep>>>,
}

impl<RL: RateLimiter + Unpin> Future for RateLimiterTask<RL> {
    type Output = SleepingRateLimiter<RL>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let deref_self = self.get_mut();
        match &mut deref_self.rate_limiter_sleep {
            Some(rate_limiter_sleep) => {
                if rate_limiter_sleep.as_mut().poll(cx).is_pending() {
                    return std::task::Poll::Pending;
                }
            }
            None => return std::task::Poll::Pending,
        }
        match (
            deref_self.rate_limiter.take(),
            deref_self.rate_limiter_sleep.take(),
        ) {
            (Some(rate_limiter), Some(rate_limiter_sleep)) => {
                std::task::Poll::Ready(SleepingRateLimiter {
                    rate_limiter,
                    rate_limiter_sleep,
                })
            }
            _ => std::task::Poll::Pending,
        }
    }
}

#[async_trait]
impl<P: Send + Sync, ES: EventStream<P> + Send, RL: RateLimiter + Unpin + Send + Sync>
    EventStream<P> for RateLimitedNetworkEventStream<P, ES, RL>
{
    async fn next_event(&mut self) -> Option<Event<P>> {
        let event = self.stream.next_event().await?;
        if let Event::Messages(_, messages) = &event {
            struct SaturatingUsize(usize);
            impl Sum<SaturatingUsize> for SaturatingUsize {
                fn sum<I: Iterator<Item = SaturatingUsize>>(iter: I) -> Self {
                    let sum = 0;
                    iter.fold(sum, |sum: usize, item| sum.saturating_add(item.0));
                    SaturatingUsize(sum)
                }
            }

            let size_received = messages
                .iter()
                .map(|(_, bytes)| SaturatingUsize(bytes.len()))
                .sum::<SaturatingUsize>()
                .0;

            let rate_limiter = self.rate_limiter.take()?;
            self.rate_limiter = Some(rate_limiter.rate_limit(size_received).await);
        }
        Some(event)
    }
}

#[derive(Clone)]
pub struct RateLimitedRawNetwork<RN, RL> {
    raw_network: RN,
    rate_limiter: RL,
}

impl<RN: RawNetwork, RL: RateLimiter + Clone + Send + Sync + Unpin + 'static> RawNetwork
    for RateLimitedRawNetwork<RN, RL>
where
    RN::EventStream: Send,
    RN::PeerId: Sync,
{
    type SenderError = RN::SenderError;
    type NetworkSender = RN::NetworkSender;
    type PeerId = RN::PeerId;
    type EventStream = RateLimitedNetworkEventStream<RN::PeerId, RN::EventStream, RL>;

    fn event_stream(&self) -> Self::EventStream {
        RateLimitedNetworkEventStream::new(
            self.raw_network.event_stream(),
            self.rate_limiter.clone(),
        )
    }

    fn sender(
        &self,
        peer_id: Self::PeerId,
        protocol: Protocol,
    ) -> Result<Self::NetworkSender, Self::SenderError> {
        self.raw_network.sender(peer_id, protocol)
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
