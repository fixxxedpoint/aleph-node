//! A network for maintaining direct connections between all nodes.
use std::{
    fmt::{Debug, Display},
    hash::Hash,
    pin::Pin,
};

use codec::Codec;
use futures::Future;
use rate_limiter::{SleepingRateLimiter, TokenBucket};
use tokio::io::{AsyncRead, AsyncWrite};

mod crypto;
mod incoming;
mod io;
mod manager;
pub mod mock;
mod outgoing;
mod protocols;
pub mod rate_limiter;
mod service;
#[cfg(test)]
mod testing;

pub use crypto::{PublicKey, SecretKey};
pub use service::Service;

const LOG_TARGET: &str = "network-clique";
/// A basic alias for properties we expect basic data to satisfy.
pub trait Data: Clone + Codec + Send + Sync + 'static {}

impl<D: Clone + Codec + Send + Sync + 'static> Data for D {}

/// Represents the id of an arbitrary node.
pub trait PeerId: PartialEq + Eq + Clone + Debug + Display + Hash + Codec + Send {
    /// This function is used for logging. It implements a shorter version of `to_string` for ids implementing display.
    fn to_short_string(&self) -> String {
        let id = format!("{}", self);
        if id.len() <= 12 {
            return id;
        }

        let prefix: String = id.chars().take(4).collect();

        let suffix: String = id.chars().skip(id.len().saturating_sub(8)).collect();

        format!("{}…{}", &prefix, &suffix)
    }
}

/// Represents the address of an arbitrary node.
pub trait AddressingInformation: Debug + Hash + Codec + Clone + Eq + Send + Sync + 'static {
    type PeerId: PeerId;

    /// Returns the peer id associated with this address.
    fn peer_id(&self) -> Self::PeerId;

    /// Verify the information.
    fn verify(&self) -> bool;
}

/// Abstraction for requesting own network addressing information.
pub trait NetworkIdentity {
    type PeerId: PeerId;
    type AddressingInformation: AddressingInformation<PeerId = Self::PeerId>;

    /// The external identity of this node.
    fn identity(&self) -> Self::AddressingInformation;
}

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

use log::info;
use tokio::net::{
    tcp::{OwnedReadHalf, OwnedWriteHalf},
    TcpListener, TcpStream,
};

/// Accepts new connections. Usually will be created listening on a specific interface and this is
/// just the result.
#[async_trait::async_trait]
pub trait Listener {
    type Connection: Splittable + 'static;
    type Error: Display;

    /// Returns the next incoming connection.
    async fn accept(&mut self) -> Result<Self::Connection, Self::Error>;
}

impl ConnectionInfo for TcpStream {
    fn peer_address_info(&self) -> String {
        match self.peer_addr() {
            Ok(addr) => addr.to_string(),
            Err(e) => format!("unknown address: {}", e),
        }
    }
}

impl ConnectionInfo for OwnedWriteHalf {
    fn peer_address_info(&self) -> String {
        match self.peer_addr() {
            Ok(addr) => addr.to_string(),
            Err(e) => e.to_string(),
        }
    }
}

impl ConnectionInfo for OwnedReadHalf {
    fn peer_address_info(&self) -> String {
        match self.peer_addr() {
            Ok(addr) => addr.to_string(),
            Err(e) => e.to_string(),
        }
    }
}

impl Splittable for TcpStream {
    type Sender = OwnedWriteHalf;
    type Receiver = OwnedReadHalf;

    fn split(self) -> (Self::Sender, Self::Receiver) {
        let (receiver, sender) = self.into_split();
        (sender, receiver)
    }
}

#[async_trait::async_trait]
impl Listener for TcpListener {
    type Connection = TcpStream;
    type Error = std::io::Error;

    async fn accept(&mut self) -> Result<Self::Connection, Self::Error> {
        let stream = TcpListener::accept(self).await.map(|(stream, _)| stream)?;
        if stream.set_linger(None).is_err() {
            info!(target: LOG_TARGET, "stream.set_linger(None) failed.");
        };
        Ok(stream)
    }
}

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
        match deref_self
            .rate_limiter
            .current_sleep()
            .map(|mut sleep| Pin::new(&mut sleep).poll(cx))
        {
            Some(std::task::Poll::Pending) => return std::task::Poll::Pending,
            _ => {}
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
    fn peer_address_info(&self) -> PeerAddressInfo {
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
