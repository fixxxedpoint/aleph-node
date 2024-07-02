use std::{
    borrow::{Borrow, BorrowMut}, collections::{HashMap, HashSet}, fmt::{Debug, Display, Error as FmtError, Formatter}, sync::Arc
};

use futures::{channel::mpsc, Future, StreamExt};
use log::{debug, info, trace, warn};
use parity_scale_codec::DecodeAll;
use rand::{seq::IteratorRandom, thread_rng};
pub use sc_network::PeerId;
use sc_network::{
    service::traits::{NotificationEvent as SubstrateEvent, ValidationResult}, MessageSink, ProtocolName
};
use tokio::time;

use crate::{
    network::{Data, GossipNetwork, LOG_TARGET},
    STATUS_REPORT_INTERVAL,
};

pub struct ExploitedProtocNetwork<Exploit = ()> {
    protocol_network: ProtocolNetwork,
    exploit: Exploit,
}

impl ExploitedProtocNetwork {
    pub fn new(protocol_network: ProtocolNetwork, mut peer_filter: impl FnMut(&PeerId) -> bool + Send + 'static) -> (ExploitedProtocNetwork<impl FnMut(Vec<u8>, PeerId, &mut ProtocolNetwork) + Send + 'static>, impl Future<Output = Result<(), ()>>) {
        let (mut exploit_sender, exploit_receiver) = mpsc::channel(0);
        let mut filtered_peers = HashMap::new();
        let network = ExploitedProtocNetwork { protocol_network, exploit: move |data, peer_id, protocol_network: &mut ProtocolNetwork| {
            if !peer_filter(&peer_id) {
                filtered_peers.remove(&peer_id);
                return;
            }
            let message_sink = match filtered_peers.entry(peer_id) {
                std::collections::hash_map::Entry::Vacant(vacant) => {
                    let network_service: &Box<dyn sc_network::config::NotificationService> = protocol_network.borrow_mut();
                    let Some(message_sink) = network_service.message_sink(&peer_id) else { return };
                    vacant.insert(Arc::new(message_sink)).clone()
                },
                std::collections::hash_map::Entry::Occupied(message_sink) => message_sink.get().clone(),
            };
            exploit_sender.try_send((data, peer_id, message_sink));
        }};
        let exploit = ExploitedSendNetwork::new(exploit_receiver).run();
        (network, exploit)
    }
}

#[async_trait::async_trait]
impl<Exploit: FnMut(Vec<u8>, PeerId, &mut ProtocolNetwork) + Send + 'static, D: Data> GossipNetwork<D> for ExploitedProtocNetwork<Exploit> {
    type Error = <ProtocolNetwork as GossipNetwork<D>>::Error;
    type PeerId = <ProtocolNetwork as GossipNetwork<D>>::PeerId;

    fn send_to(&mut self, data: D, peer_id: Self::PeerId) -> Result<(), Self::Error> {
        (self.exploit)(data.encode(), peer_id.clone(), &mut self.protocol_network);
        self.protocol_network.send_to(data, peer_id)
    }

    fn send_to_random(&mut self, data: D, peer_ids: HashSet<Self::PeerId>) -> Result<(), Self::Error>  {
        self.protocol_network.send_to_random(data, peer_ids)
    }

    fn broadcast(&mut self, data: D) -> Result<(), Self::Error>  {
        self.protocol_network.broadcast(data)
    }

    async fn next(&mut self) -> Result<(D, PeerId), Self::Error> {
        self.protocol_network.next().await
    }
}

/// A thin wrapper around sc_network::config::NotificationService that stores a list
/// of all currently connected peers, and introduces a few convenience methods to
/// allow broadcasting messages and sending data to random peers.
pub struct ProtocolNetwork {
    service: Box<dyn sc_network::config::NotificationService>,
    connected_peers: HashSet<PeerId>,
    last_status_report: time::Instant,
}

impl Borrow<Box<dyn sc_network::config::NotificationService>> for ProtocolNetwork {
    fn borrow(&self) -> &Box<dyn sc_network::config::NotificationService> {
        &self.service
    }
}

impl BorrowMut<Box<dyn sc_network::config::NotificationService>> for ProtocolNetwork {
    fn borrow_mut(&mut self) -> &mut Box<dyn sc_network::config::NotificationService> {
        &mut self.service
    }
}

impl ProtocolNetwork {
    pub fn new(service: Box<dyn sc_network::config::NotificationService>) -> Self {
        Self {
            service,
            connected_peers: HashSet::new(),
            last_status_report: time::Instant::now(),
        }
    }

    pub fn name(&self) -> ProtocolName {
        self.service.protocol().clone()
    }

    fn random_peer<'a>(&'a self, peer_ids: &'a HashSet<PeerId>) -> Option<&'a PeerId> {
        peer_ids
            .intersection(&self.connected_peers)
            .choose(&mut thread_rng())
            .or_else(|| self.connected_peers.iter().choose(&mut thread_rng()))
    }

    fn handle_network_event(&mut self, event: SubstrateEvent) -> Option<(Vec<u8>, PeerId)> {
        use SubstrateEvent::*;
        match event {
            ValidateInboundSubstream {
                peer: _,
                handshake: _,
                result_tx,
            } => {
                let _ = result_tx.send(ValidationResult::Accept);
                None
            }
            NotificationStreamOpened { peer, .. } => {
                self.connected_peers.insert(peer);
                None
            }
            NotificationStreamClosed { peer } => {
                self.connected_peers.remove(&peer);
                None
            }
            NotificationReceived { peer, notification } => Some((notification, peer)),
        }
    }

    fn status_report(&self) {
        let mut status = String::from("Network status report: ");
        status.push_str(&format!(
            "{} connected peers - {:?}; ",
            self.service.protocol(),
            self.connected_peers.len()
        ));
        info!(target: LOG_TARGET, "{}", status);
    }
}

#[derive(Debug)]
pub enum ProtocolNetworkError {
    NetworkStreamTerminated,
}

impl Display for ProtocolNetworkError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), FmtError> {
        match self {
            ProtocolNetworkError::NetworkStreamTerminated => {
                write!(f, "Notifications event stream ended.")
            }
        }
    }
}

pub struct ExploitedSendNetwork {
    data: futures::channel::mpsc::Receiver<(Vec<u8>, PeerId, Arc<Box<dyn MessageSink>>)>,
}

impl ExploitedSendNetwork {
    pub fn new(receiver: futures::channel::mpsc::Receiver<(Vec<u8>, PeerId, Arc<Box<dyn MessageSink>>)>) -> Self {
        Self { data: receiver }
    }
}

impl ExploitedSendNetwork {
    pub async fn run(mut self) -> Result<(), ()> {
        let (mut message, mut peer_id, mut sink) = self.data.next().await.ok_or_else(|| panic!("where data?!"))?;
        loop {
            sink.send_sync_notification(message.clone());
            if let Some(new_message) = self.data.try_next().map_err(|_| panic!("yyyy koniec"))? {
                if new_message.1 != peer_id {
                    continue;
                }
                (message, peer_id, sink) = new_message;
            }
        }
    }
}

// pub trait Spawner {
//     fn spawn<Fut: Future<Output = Out>, Out>(future: Fut) -> impl Future<Output = Result<Out, ()>>;
// }

pub struct ExploitedNetwork<N, F> {
    inner: N,
    exploit: F,
}

impl<N, F> ExploitedNetwork<N, F>
{
    pub fn new(network: N, exploit: F) -> Self {
        Self { inner: network, exploit }
    }
}

#[async_trait::async_trait]
impl<D: Data, N: GossipNetwork<D>, F: FnMut(Vec<u8>, N::PeerId) + Send + 'static> GossipNetwork<D> for ExploitedNetwork<N, F> {
    type Error = N::Error;
    type PeerId = N::PeerId;

    fn send_to(&mut self, data: D, peer_id: Self::PeerId) -> Result<(), Self::Error> {
        (self.exploit)(data.encode(), peer_id.clone());
        self.inner.send_to(data, peer_id)
    }

    fn send_to_random(&mut self, data: D, peer_ids: HashSet<Self::PeerId>) -> Result<(), Self::Error> {
        self.inner.send_to_random(data, peer_ids)
    }

    fn broadcast(&mut self, data: D) -> Result<(), Self::Error> {
        self.inner.broadcast(data)
    }

    async fn next(&mut self) -> Result<(D, Self::PeerId), Self::Error> {
        self.inner.next().await
    }
}

#[async_trait::async_trait]
impl<D: Data> GossipNetwork<D> for ProtocolNetwork {
    type Error = ProtocolNetworkError;
    type PeerId = PeerId;

    fn send_to(&mut self, data: D, peer_id: PeerId) -> Result<(), Self::Error> {
        trace!(
            target: LOG_TARGET,
            "Sending block sync data to peer {:?}.",
            peer_id,
        );
        self.service.send_sync_notification(&peer_id, data.encode());
        Ok(())
    }

    fn send_to_random(&mut self, data: D, peer_ids: HashSet<PeerId>) -> Result<(), Self::Error> {
        trace!(
            target: LOG_TARGET,
            "Sending data to random peer among {:?}.",
            peer_ids,
        );
        let peer_id = match self.random_peer(&peer_ids) {
            Some(peer_id) => *peer_id,
            None => {
                debug!(
                    target: LOG_TARGET,
                    "Failed to send message to random peer, no peers are available."
                );
                return Ok(());
            }
        };
        self.send_to(data, peer_id)
    }

    fn broadcast(&mut self, data: D) -> Result<(), Self::Error> {
        for peer in self.connected_peers.clone() {
            // in the current version send_to never returns an error
            let _ = self.send_to(data.clone(), peer);
        }
        Ok(())
    }

    async fn next(&mut self) -> Result<(D, PeerId), Self::Error> {
        let mut status_ticker = time::interval_at(
            self.last_status_report
                .checked_add(STATUS_REPORT_INTERVAL)
                .unwrap_or(time::Instant::now()),
            STATUS_REPORT_INTERVAL,
        );
        loop {
            tokio::select! {
                maybe_event = self.service.next_event() => {
                    let event = maybe_event.ok_or(Self::Error::NetworkStreamTerminated)?;
                    let Some((message, peer_id)) = self.handle_network_event(event) else { continue };
                    match D::decode_all(&mut &message[..]) {
                        Ok(message) => return Ok((message, peer_id)),
                        Err(e) => {
                            warn!(
                                target: LOG_TARGET,
                                "Error decoding message: {}", e
                            )
                        },
                    }
                },
                _ = status_ticker.tick() => {
                    self.status_report();
                    self.last_status_report = time::Instant::now();
                },
            }
        }
    }
}
