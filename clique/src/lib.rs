//! A network for maintaining direct connections between all nodes.
use std::{
    fmt::{Debug, Display},
    hash::Hash,
    pin::Pin,
};

use futures::FutureExt;
use parity_scale_codec::Codec;
use rate_limiter::{RateLimiterTask, SleepingRateLimiter, TokenBucket};
use tokio::io::{AsyncRead, AsyncWrite};

mod crypto;
mod incoming;
mod io;
mod manager;
pub mod mock;
mod outgoing;
mod protocols;
mod service;
#[cfg(test)]
mod testing;

pub use crypto::{PublicKey, SecretKey};
pub use service::{Service, SpawnHandleT};

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
    rate_limiter: RateLimiterTask,
    read: A,
}

impl<A> RateLimitedAsyncRead<A> {
    pub fn new(read: A, rate_limiter: RateLimiterTask) -> Self {
        Self { rate_limiter, read }
    }
}

impl<A: AsyncRead + Unpin> AsyncRead for RateLimitedAsyncRead<A> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let sleeping_rate_limiter = match this.rate_limiter.poll_unpin(cx) {
            std::task::Poll::Ready(rate_limiter) => rate_limiter,
            _ => return std::task::Poll::Pending,
        };

        let filled_before = buf.filled().len();
        let result = Pin::new(&mut this.read).poll_read(cx, buf);
        let filled_after = buf.filled().len();
        let last_read_size = filled_after - filled_before;
        let last_read_size = last_read_size.try_into().unwrap_or(u64::MAX);

        this.rate_limiter = sleeping_rate_limiter.rate_limit(last_read_size);

        result
    }
}

pub struct Splitted<I, O>(I, O);

impl<I: AsyncRead + Unpin, O: Unpin> AsyncRead for Splitted<I, O> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.0).poll_read(cx, buf)
    }
}

impl<I: Unpin, O: AsyncWrite + Unpin> AsyncWrite for Splitted<I, O> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.1).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.1).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.1).poll_shutdown(cx)
    }
}

impl<I, O: ConnectionInfo> ConnectionInfo for Splitted<I, O> {
    fn peer_address_info(&self) -> PeerAddressInfo {
        self.1.peer_address_info()
    }
}

impl<
        I: AsyncRead + ConnectionInfo + Unpin + Send,
        O: AsyncWrite + ConnectionInfo + Unpin + Send,
    > Splittable for Splitted<I, O>
{
    type Sender = O;
    type Receiver = I;

    fn split(self) -> (Self::Sender, Self::Receiver) {
        (self.1, self.0)
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
impl<A, D> Dialer<A> for RateLimitingDialer<D>
where
    A: Data,
    D: Dialer<A>,
    <D::Connection as Splittable>::Sender: Unpin,
    <D::Connection as Splittable>::Receiver: Unpin,
{
    type Connection = Splitted<
        RateLimitedAsyncRead<<D::Connection as Splittable>::Receiver>,
        <D::Connection as Splittable>::Sender,
    >;
    type Error = D::Error;

    async fn connect(&mut self, address: A) -> Result<Self::Connection, Self::Error> {
        let connection = self.dialer.connect(address).await?;
        let (sender, receiver) = connection.split();
        Ok(Splitted(
            RateLimitedAsyncRead::new(
                receiver,
                RateLimiterTask::new(SleepingRateLimiter::new(self.rate_limiter.clone())),
            ),
            sender,
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
    type Connection = Splitted<
        RateLimitedAsyncRead<<L::Connection as Splittable>::Receiver>,
        <L::Connection as Splittable>::Sender,
    >;
    type Error = L::Error;

    async fn accept(&mut self) -> Result<Self::Connection, Self::Error> {
        let connection = self.listener.accept().await?;
        let (sender, receiver) = connection.split();
        Ok(Splitted(
            RateLimitedAsyncRead::new(
                receiver,
                RateLimiterTask::new(SleepingRateLimiter::new(self.rate_limiter.clone())),
            ),
            sender,
        ))
    }
}
