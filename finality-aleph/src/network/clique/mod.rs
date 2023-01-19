//! A network for maintaining direct connections between all nodes.
use std::fmt::Display;

use futures::{
    channel::{mpsc, oneshot},
    StreamExt,
};
use log::debug;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::network::Data;

mod crypto;
mod incoming;
mod io;
mod manager;
#[cfg(test)]
pub mod mock;
mod outgoing;
mod protocols;
mod service;

pub use crypto::{PublicKey, SecretKey};
pub use service::Service;

const LOG_TARGET: &str = "clique-network";

/// Network represents an interface for opening and closing connections with other nodes,
/// and sending direct messages between them.
///
/// Note on Network reliability and security: it is neither assumed that the sent messages must be
/// always delivered, nor the established connections must be secure in any way. The Network
/// implementation might fail to deliver any specific message, so messages have to be resent while
/// they still should be delivered.
#[async_trait::async_trait]
pub trait Network<PK: PublicKey, A: Data, D: Data>: Send + 'static {
    /// Add the peer to the set of connected peers.
    fn add_connection(&mut self, peer: PK, address: A);

    /// Remove the peer from the set of connected peers and close the connection.
    fn remove_connection(&mut self, peer: PK);

    /// Send a message to a single peer.
    /// This function should be implemented in a non-blocking manner.
    fn send(&self, data: D, recipient: PK);

    /// Receive a message from the network.
    async fn next(&mut self) -> Option<D>;
}

pub type PeerAddressInfo = String;

/// Reports address of the peer that we are connected to.
pub trait ConnectionInfo {
    /// Return the address of the peer that we are connected to.
    fn peer_address_info(&self) -> PeerAddressInfo;
}

/// A stream that can be split into a sending and receiving part.
pub trait Splittable: AsyncWrite + AsyncRead + ConnectionInfo + Unpin + Send {
    type Sender: AsyncWrite + ConnectionInfo + Unpin + Send;
    type Receiver: AsyncRead + ConnectionInfo + Unpin + Send;

    /// Split into the sending and receiving part.
    fn split(self) -> (Self::Sender, Self::Receiver);
}

/// Can use addresses to connect to a peer.
#[async_trait::async_trait]
pub trait Dialer<A: Data>: Clone + Send + 'static {
    type Connection: Splittable;
    type Error: Display + Send;

    /// Attempt to connect to a peer using the provided addressing information.
    async fn connect(&mut self, address: A) -> Result<Self::Connection, Self::Error>;
}

/// Accepts new connections. Usually will be created listening on a specific interface and this is
/// just the result.
#[async_trait::async_trait]
pub trait Listener {
    type Connection: Splittable + 'static;
    type Error: Display;

    /// Returns the next incoming connection.
    async fn accept(&mut self) -> Result<Self::Connection, Self::Error>;
}

pub trait Continuation<In, Out, AllOut> {
    fn cont(self, continuation: impl FnMut(In) -> Out) -> AllOut;
}

// #[async_trait::async_trait]
// pub trait AuthorizationContinuation {
//     async fn authorize<PK, D, Cont, Ret>(&mut self, continuation: Cont)
//     where
//         Cont: Fn(PK, Option<mpsc::UnboundedSender<D>>, ConnectionType) -> Ret,
//         Ret: Future<Output = bool>;
// }

pub struct AuthContinuationHandler<PK> {
    result: PK,
    result_sender: oneshot::Sender<bool>,
}

impl<PK> AuthContinuationHandler<PK> {
    pub fn new(result: PK) -> (Self, oneshot::Receiver<bool>) {
        let (auth_sender, auth_receiver) = oneshot::channel();
        (
            Self {
                result,
                result_sender: auth_sender,
            },
            auth_receiver,
        )
    }

    pub fn handle_authorization(self, mut handler: impl FnMut(PK) -> bool) -> Result<(), ()> {
        let auth_result = handler(self.result);
        self.result_sender.send(auth_result).map_err(|_| ())
    }
}

impl<PK> Continuation<PK, bool, ()> for AuthContinuationHandler<PK> {
    fn cont(self, mut continuation: impl FnMut(PK) -> bool) -> () {
        let auth_result = continuation(self.result);
        if let Err(er) = self.result_sender.send(auth_result) {
            debug!(
                target: LOG_TARGET,
                "Unable to send a result from authorization."
            );
        }
    }
}

pub enum AuthorizatorError {
    MissingService,
    ServiceDisappeared,
}

pub struct AuthorizationHandler<PK> {
    receiver: mpsc::UnboundedReceiver<AuthContinuationHandler<PK>>,
}

impl<PK> AuthorizationHandler<PK> {
    pub fn new(receiver: mpsc::UnboundedReceiver<AuthContinuationHandler<PK>>) -> Self {
        Self { receiver }
    }

    pub async fn next_authorization_request(&mut self) -> Option<AuthContinuationHandler<PK>> {
        self.receiver.next().await
    }
}

#[derive(Clone)]
pub struct Authorizator<PK> {
    sender: mpsc::UnboundedSender<AuthContinuationHandler<PK>>,
}

impl<PK> Authorizator<PK> {
    pub fn new() -> (Self, AuthorizationHandler<PK>) {
        let (sender, receiver) = mpsc::unbounded();
        (Self { sender }, AuthorizationHandler::new(receiver))
    }

    pub async fn is_authorized(&self, value: PK) -> Result<bool, AuthorizatorError> {
        let (handler, receiver) = AuthContinuationHandler::new(value);
        self.sender
            .unbounded_send(handler)
            .map_err(|_| AuthorizatorError::MissingService)?;
        receiver
            .await
            .map_err(|_| AuthorizatorError::ServiceDisappeared)
    }
}

// #[async_trait::async_trait]
// impl AuthorizationContinuation for AuthContinuationHandler {
//     async fn authorize<PK, D, Cont, Ret>(&mut self, continuation: Cont)
//     where
//         Cont: Fn(PK, Option<mpsc::UnboundedSender<D>>, ConnectionType) -> Ret,
//         Ret: futures::Future<Output = bool>,
//     {
//         todo!()
//     }
// }
