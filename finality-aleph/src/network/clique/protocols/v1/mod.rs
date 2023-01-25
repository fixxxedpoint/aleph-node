use codec::{Decode, Encode};
use futures::{channel::mpsc, StreamExt};
use log::{debug, info, trace, warn};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::{timeout, Duration},
};

use super::handshake::HandshakeError;
use crate::network::clique::{
    io::{receive_data, send_data},
    protocols::{
        handle_authorization,
        handshake::{v0_handshake_incoming, v0_handshake_outgoing},
        ConnectionType, ProtocolError, ResultForService,
    },
    Authorization, Data, PublicKey, SecretKey, Splittable, LOG_TARGET,
};

const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_MISSED_HEARTBEATS: u32 = 4;

#[derive(Debug, Clone, Encode, Decode)]
enum Message<D: Data> {
    Data(D),
    Heartbeat,
}

async fn sending<PK: PublicKey, D: Data, S: AsyncWrite + Unpin + Send>(
    mut sender: S,
    mut data_from_user: mpsc::UnboundedReceiver<D>,
) -> Result<(), ProtocolError<PK>> {
    use Message::*;
    loop {
        let to_send = match timeout(HEARTBEAT_TIMEOUT, data_from_user.next()).await {
            Ok(maybe_data) => match maybe_data {
                Some(data) => Data(data),
                // We have been closed by the parent service, all good.
                None => return Ok(()),
            },
            _ => Heartbeat,
        };
        sender = send_data(sender, to_send).await?;
    }
}

async fn receiving<PK: PublicKey, D: Data, S: AsyncRead + Unpin + Send>(
    mut stream: S,
    data_for_user: mpsc::UnboundedSender<D>,
) -> Result<(), ProtocolError<PK>> {
    use Message::*;
    loop {
        let (old_stream, message) = timeout(
            MAX_MISSED_HEARTBEATS * HEARTBEAT_TIMEOUT,
            receive_data(stream),
        )
        .await
        .map_err(|_| ProtocolError::CardiacArrest)??;
        stream = old_stream;
        match message {
            Data(data) => data_for_user
                .unbounded_send(data)
                .map_err(|_| ProtocolError::NoUserConnection)?,
            Heartbeat => (),
        }
    }
}

async fn manage_connection<
    PK: PublicKey,
    D: Data,
    S: AsyncWrite + Unpin + Send,
    R: AsyncRead + Unpin + Send,
>(
    sender: S,
    receiver: R,
    data_from_user: mpsc::UnboundedReceiver<D>,
    data_for_user: mpsc::UnboundedSender<D>,
) -> Result<(), ProtocolError<PK>> {
    let sending = sending(sender, data_from_user);
    let receiving = receiving(receiver, data_for_user);
    tokio::select! {
        result = receiving => result,
        result = sending => result,
    }
}

/// Performs the outgoing handshake, and then manages a connection sending and receiving data.
/// Exits on parent request, or in case of broken or dead network connection.
pub async fn outgoing<SK: SecretKey, D: Data, S: Splittable>(
    stream: S,
    secret_key: SK,
    public_key: SK::PublicKey,
    result_for_parent: mpsc::UnboundedSender<ResultForService<SK::PublicKey, D>>,
    data_for_user: mpsc::UnboundedSender<D>,
) -> Result<(), ProtocolError<SK::PublicKey>> {
    trace!(target: LOG_TARGET, "Extending hand to {}.", public_key);
    let (sender, receiver) = v0_handshake_outgoing(stream, secret_key, public_key.clone()).await?;
    info!(
        target: LOG_TARGET,
        "Outgoing handshake with {} finished successfully.", public_key
    );
    let (data_for_network, data_from_user) = mpsc::unbounded();
    result_for_parent
        .unbounded_send((
            public_key.clone(),
            Some(data_for_network),
            ConnectionType::New,
        ))
        .map_err(|_| ProtocolError::NoParentConnection)?;

    debug!(
        target: LOG_TARGET,
        "Starting worker for communicating with {}.", public_key
    );
    manage_connection(sender, receiver, data_from_user, data_for_user).await
}

#[async_trait::async_trait]
pub trait Handshake<SK: SecretKey> {
    async fn handshake<S: Splittable>(
        stream: S,
        secret_key: SK,
    ) -> Result<(S::Sender, S::Receiver, SK::PublicKey), HandshakeError<SK::PublicKey>>;
}

struct DefaultHandshake {}

#[async_trait::async_trait]
impl<SK: SecretKey> Handshake<SK> for DefaultHandshake {
    async fn handshake<S: Splittable>(
        stream: S,
        secret_key: SK,
    ) -> Result<(S::Sender, S::Receiver, SK::PublicKey), HandshakeError<SK::PublicKey>> {
        v0_handshake_incoming(stream, secret_key).await
    }
}

/// Performs the incoming handshake, and then manages a connection sending and receiving data.
/// Exits on parent request (when the data source is dropped), or in case of broken or dead
/// network connection.
pub async fn incoming<SK: SecretKey, D: Data, S: Splittable, A: Authorization<SK::PublicKey>>(
    stream: S,
    secret_key: SK,
    authorizator: A,
    result_for_parent: mpsc::UnboundedSender<ResultForService<SK::PublicKey, D>>,
    data_for_user: mpsc::UnboundedSender<D>,
) -> Result<(), ProtocolError<SK::PublicKey>> {
    handle_incoming::<_, _, _, _, DefaultHandshake>(
        stream,
        secret_key,
        authorizator,
        result_for_parent,
        data_for_user,
    )
    .await
}

pub async fn handle_incoming<
    SK: SecretKey,
    D: Data,
    S: Splittable,
    A: Authorization<SK::PublicKey>,
    H: Handshake<SK>,
>(
    stream: S,
    secret_key: SK,
    authorizator: A,
    result_for_parent: mpsc::UnboundedSender<ResultForService<SK::PublicKey, D>>,
    data_for_user: mpsc::UnboundedSender<D>,
) -> Result<(), ProtocolError<SK::PublicKey>> {
    trace!(target: LOG_TARGET, "Waiting for extended hand...");
    let (sender, receiver, public_key) = H::handshake(stream, secret_key).await?;
    info!(
        target: LOG_TARGET,
        "Incoming handshake with {} finished successfully.", public_key
    );

    let authorized = handle_authorization::<SK, A>(authorizator, public_key.clone())
        .await
        .map_err(|_| ProtocolError::NotAuthorized)?;
    if !authorized {
        warn!(
            target: LOG_TARGET,
            "public_key={} was not authorized.", public_key
        );
        return Ok(());
    }

    let (data_for_network, data_from_user) = mpsc::unbounded();
    result_for_parent
        .unbounded_send((
            public_key.clone(),
            Some(data_for_network),
            ConnectionType::New,
        ))
        .map_err(|_| ProtocolError::NoParentConnection)?;
    debug!(
        target: LOG_TARGET,
        "Starting worker for communicating with {}.", public_key
    );
    manage_connection(sender, receiver, data_from_user, data_for_user).await
}

#[cfg(test)]
mod tests {
    use std::{
        pin::Pin,
        sync::Mutex,
        task::{Context, Poll},
    };

    use futures::{channel::mpsc, pin_mut, FutureExt, StreamExt};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    use super::{incoming, outgoing, Handshake, ProtocolError};
    use crate::network::clique::{
        mock::{
            key, new_authorizer, MockAuthorizer, MockPrelims, MockSecretKey, MockSplittable,
            MockWrappedSplittable,
        },
        protocols::{handshake::HandshakeError, v1::handle_incoming, ConnectionType},
        ConnectionInfo, Data, SecretKey, Splittable,
    };

    fn prepare<D: Data>() -> MockPrelims<D> {
        let (stream_incoming, stream_outgoing) = MockSplittable::new(4096);
        let (id_incoming, pen_incoming) = key();
        let (id_outgoing, pen_outgoing) = key();
        assert_ne!(id_incoming, id_outgoing);
        let (incoming_result_for_service, result_from_incoming) = mpsc::unbounded();
        let (outgoing_result_for_service, result_from_outgoing) = mpsc::unbounded();
        let (incoming_data_for_user, data_from_incoming) = mpsc::unbounded::<D>();
        let (outgoing_data_for_user, data_from_outgoing) = mpsc::unbounded::<D>();
        let authorizer = new_authorizer();
        let incoming_handle = Box::pin(incoming(
            stream_incoming,
            pen_incoming.clone(),
            authorizer,
            incoming_result_for_service,
            incoming_data_for_user,
        ));
        let outgoing_handle = Box::pin(outgoing(
            stream_outgoing,
            pen_outgoing.clone(),
            id_incoming.clone(),
            outgoing_result_for_service,
            outgoing_data_for_user,
        ));
        MockPrelims {
            id_incoming,
            pen_incoming,
            id_outgoing,
            pen_outgoing,
            incoming_handle,
            outgoing_handle,
            data_from_incoming,
            data_from_outgoing: Some(data_from_outgoing),
            result_from_incoming,
            result_from_outgoing,
        }
    }

    #[tokio::test]
    async fn send_data() {
        let MockPrelims {
            incoming_handle,
            outgoing_handle,
            mut data_from_incoming,
            data_from_outgoing,
            mut result_from_incoming,
            mut result_from_outgoing,
            ..
        } = prepare::<Vec<i32>>();
        let mut data_from_outgoing = data_from_outgoing.expect("No data from outgoing!");
        let incoming_handle = incoming_handle.fuse();
        let outgoing_handle = outgoing_handle.fuse();
        pin_mut!(incoming_handle);
        pin_mut!(outgoing_handle);
        let _data_for_outgoing = tokio::select! {
            _ = &mut incoming_handle => panic!("incoming process unexpectedly finished"),
            _ = &mut outgoing_handle => panic!("outgoing process unexpectedly finished"),
            result = result_from_outgoing.next() => {
                let (_, maybe_data_for_outgoing, connection_type) = result.expect("the channel shouldn't be dropped");
                assert_eq!(connection_type, ConnectionType::New);
                let data_for_outgoing = maybe_data_for_outgoing.expect("successfully connected");
                data_for_outgoing
                    .unbounded_send(vec![4, 3, 43])
                    .expect("should send");
                data_for_outgoing
                    .unbounded_send(vec![2, 1, 3, 7])
                    .expect("should send");
                data_for_outgoing
            },
        };
        let _data_for_incoming = tokio::select! {
            _ = &mut incoming_handle => panic!("incoming process unexpectedly finished"),
            _ = &mut outgoing_handle => panic!("outgoing process unexpectedly finished"),
            result = result_from_incoming.next() => {
                let (_, maybe_data_for_incoming, connection_type) = result.expect("the channel shouldn't be dropped");
                assert_eq!(connection_type, ConnectionType::New);
                let data_for_incoming = maybe_data_for_incoming.expect("successfully connected");
                data_for_incoming
                    .unbounded_send(vec![5, 4, 44])
                    .expect("should send");
                data_for_incoming
                    .unbounded_send(vec![3, 2, 4, 8])
                    .expect("should send");
                data_for_incoming
            },
        };
        tokio::select! {
            _ = &mut incoming_handle => panic!("incoming process unexpectedly finished"),
            _ = &mut outgoing_handle => panic!("outgoing process unexpectedly finished"),
            v = data_from_incoming.next() => {
                assert_eq!(v, Some(vec![4, 3, 43]));
            },
        };
        tokio::select! {
            _ = &mut incoming_handle => panic!("incoming process unexpectedly finished"),
            _ = &mut outgoing_handle => panic!("outgoing process unexpectedly finished"),
            v = data_from_incoming.next() => {
                assert_eq!(v, Some(vec![2, 1, 3, 7]));
            },
        };
        tokio::select! {
            _ = &mut incoming_handle => panic!("incoming process unexpectedly finished"),
            _ = &mut outgoing_handle => panic!("outgoing process unexpectedly finished"),
            v = data_from_outgoing.next() => {
                assert_eq!(v, Some(vec![5, 4, 44]));
            },
        };
        tokio::select! {
            _ = &mut incoming_handle => panic!("incoming process unexpectedly finished"),
            _ = &mut outgoing_handle => panic!("outgoing process unexpectedly finished"),
            v = data_from_outgoing.next() => {
                assert_eq!(v, Some(vec![3, 2, 4, 8]));
            },
        };
    }

    #[tokio::test]
    async fn closed_by_parent_service() {
        let MockPrelims {
            id_outgoing,
            incoming_handle,
            outgoing_handle,
            data_from_incoming: _data_from_incoming,
            data_from_outgoing: _data_from_outgoing,
            mut result_from_incoming,
            result_from_outgoing: _result_from_outgoing,
            ..
        } = prepare::<Vec<i32>>();
        let incoming_handle = incoming_handle.fuse();
        let outgoing_handle = outgoing_handle.fuse();
        pin_mut!(incoming_handle);
        pin_mut!(outgoing_handle);
        tokio::select! {
            _ = &mut incoming_handle => panic!("incoming process unexpectedly finished"),
            _ = &mut outgoing_handle => panic!("outgoing process unexpectedly finished"),
            received = result_from_incoming.next() => {
                // we drop the data sending channel, thus finishing incoming_handle
                let (received_id, _, connection_type) = received.expect("the channel shouldn't be dropped");
                assert_eq!(connection_type, ConnectionType::New);
                assert_eq!(received_id, id_outgoing);
            },
        };
        incoming_handle
            .await
            .expect("closed manually, should finish with no error");
    }

    #[tokio::test]
    async fn parent_service_dead() {
        let MockPrelims {
            incoming_handle,
            outgoing_handle,
            data_from_incoming: _data_from_incoming,
            data_from_outgoing: _data_from_outgoing,
            result_from_incoming,
            result_from_outgoing: _result_from_outgoing,
            ..
        } = prepare::<Vec<i32>>();
        std::mem::drop(result_from_incoming);
        let incoming_handle = incoming_handle.fuse();
        let outgoing_handle = outgoing_handle.fuse();
        pin_mut!(incoming_handle);
        pin_mut!(outgoing_handle);
        tokio::select! {
            e = &mut incoming_handle => match e {
                Err(ProtocolError::NoParentConnection) => (),
                Err(e) => panic!("unexpected error: {}", e),
                Ok(_) => panic!("successfully finished when parent dead"),
            },
            _ = &mut outgoing_handle => panic!("outgoing process unexpectedly finished"),
        };
    }

    #[tokio::test]
    async fn parent_user_dead() {
        let MockPrelims {
            incoming_handle,
            outgoing_handle,
            data_from_incoming,
            data_from_outgoing: _data_from_outgoing,
            result_from_incoming: _result_from_incoming,
            mut result_from_outgoing,
            ..
        } = prepare::<Vec<i32>>();
        std::mem::drop(data_from_incoming);
        let incoming_handle = incoming_handle.fuse();
        let outgoing_handle = outgoing_handle.fuse();
        pin_mut!(incoming_handle);
        pin_mut!(outgoing_handle);
        let _data_for_outgoing = tokio::select! {
            _ = &mut incoming_handle => panic!("incoming process unexpectedly finished"),
            _ = &mut outgoing_handle => panic!("outgoing process unexpectedly finished"),
            result = result_from_outgoing.next() => {
                let (_, maybe_data_for_outgoing, connection_type) = result.expect("the channel shouldn't be dropped");
                assert_eq!(connection_type, ConnectionType::New);
                let data_for_outgoing = maybe_data_for_outgoing.expect("successfully connected");
                data_for_outgoing
                    .unbounded_send(vec![2, 1, 3, 7])
                    .expect("should send");
                data_for_outgoing
            },
        };
        tokio::select! {
            e = &mut incoming_handle => match e {
                Err(ProtocolError::NoUserConnection) => (),
                Err(e) => panic!("unexpected error: {}", e),
                Ok(_) => panic!("successfully finished when user dead"),
            },
            _ = &mut outgoing_handle => panic!("outgoing process unexpectedly finished"),
        };
    }

    #[tokio::test]
    async fn sender_dead_before_handshake() {
        let MockPrelims {
            incoming_handle,
            outgoing_handle,
            data_from_incoming: _data_from_incoming,
            data_from_outgoing: _data_from_outgoing,
            result_from_incoming: _result_from_incoming,
            result_from_outgoing: _result_from_outgoing,
            ..
        } = prepare::<Vec<i32>>();
        std::mem::drop(outgoing_handle);
        match incoming_handle.await {
            Err(ProtocolError::HandshakeError(_)) => (),
            Err(e) => panic!("unexpected error: {}", e),
            Ok(_) => panic!("successfully finished when connection dead"),
        };
    }

    #[tokio::test]
    async fn sender_dead_after_handshake() {
        let MockPrelims {
            incoming_handle,
            outgoing_handle,
            data_from_incoming: _data_from_incoming,
            data_from_outgoing: _data_from_outgoing,
            mut result_from_incoming,
            result_from_outgoing: _result_from_outgoing,
            ..
        } = prepare::<Vec<i32>>();
        let incoming_handle = incoming_handle.fuse();
        pin_mut!(incoming_handle);
        let (_, _exit, connection_type) = tokio::select! {
            _ = &mut incoming_handle => panic!("incoming process unexpectedly finished"),
            _ = outgoing_handle => panic!("outgoing process unexpectedly finished"),
            out = result_from_incoming.next() => out.expect("should receive"),
        };
        assert_eq!(connection_type, ConnectionType::New);
        // outgoing_handle got consumed by tokio::select!, the sender is dead
        match incoming_handle.await {
            Err(ProtocolError::ReceiveError(_)) => (),
            Err(e) => panic!("unexpected error: {}", e),
            Ok(_) => panic!("successfully finished when connection dead"),
        };
    }

    #[tokio::test]
    async fn receiver_dead_before_handshake() {
        let MockPrelims {
            incoming_handle,
            outgoing_handle,
            data_from_incoming: _data_from_incoming,
            data_from_outgoing: _data_from_outgoing,
            result_from_incoming: _result_from_incoming,
            result_from_outgoing: _result_from_outgoing,
            ..
        } = prepare::<Vec<i32>>();
        std::mem::drop(incoming_handle);
        match outgoing_handle.await {
            Err(ProtocolError::HandshakeError(_)) => (),
            Err(e) => panic!("unexpected error: {}", e),
            Ok(_) => panic!("successfully finished when connection dead"),
        };
    }

    struct WrappingReader<A, R> {
        action: A,
        reader: R,
    }

    impl<A, R> WrappingReader<A, R> {
        pub fn new_with_closure(reader: R, closure: A) -> Self {
            Self {
                action: closure,
                reader,
            }
        }
    }

    impl<A: FnMut() + Unpin, R: AsyncRead + Unpin> AsyncRead for WrappingReader<A, R> {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let self_mut = self.get_mut();
            (self_mut.action)();
            Pin::new(&mut self_mut.reader).poll_read(cx, buf)
        }
    }

    impl<A, W> ConnectionInfo for WrappingReader<A, W> {
        fn peer_address_info(&self) -> crate::network::clique::PeerAddressInfo {
            String::from("WRAPPING_READER")
        }
    }

    struct IteratorWrapper<I>(I);

    impl<I: Iterator<Item = u8> + Unpin> AsyncRead for IteratorWrapper<I> {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let iter = &mut self.get_mut().0;
            let buffer = buf.initialize_unfilled();
            let remaining = buffer.len();
            for cell in buffer.iter_mut() {
                match iter.next() {
                    Some(next) => *cell = next,
                    None => {
                        return Poll::Pending;
                    }
                }
            }
            buf.advance(remaining);
            Poll::Ready(Result::Ok(()))
        }
    }

    struct WrappingWriter<A, W> {
        action: A,
        writer: W,
    }

    impl<A, W> WrappingWriter<A, W> {
        pub fn new_with_closure(writer: W, action: A) -> Self {
            Self { action, writer }
        }
    }

    impl<A: FnMut() + Unpin, W: AsyncWrite + Unpin> AsyncWrite for WrappingWriter<A, W> {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<Result<usize, std::io::Error>> {
            let self_mut = self.get_mut();
            (self_mut.action)();
            AsyncWrite::poll_write(Pin::new(&mut self_mut.writer), cx, buf)
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Result<(), std::io::Error>> {
            AsyncWrite::poll_flush(Pin::new(&mut self.get_mut().writer), cx)
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Result<(), std::io::Error>> {
            AsyncWrite::poll_shutdown(Pin::new(&mut self.get_mut().writer), cx)
        }
    }

    impl<A, W> ConnectionInfo for WrappingWriter<A, W> {
        fn peer_address_info(&self) -> crate::network::clique::PeerAddressInfo {
            String::from("WRAPPING_WRITER")
        }
    }

    struct NoHandshake {}

    #[async_trait::async_trait]
    impl Handshake<MockSecretKey> for NoHandshake {
        async fn handshake<S: Splittable>(
            stream: S,
            _: MockSecretKey,
        ) -> Result<
            (
                S::Sender,
                S::Receiver,
                <MockSecretKey as SecretKey>::PublicKey,
            ),
            HandshakeError<<MockSecretKey as SecretKey>::PublicKey>,
        > {
            let (sender, receiver) = stream.split();
            Ok((sender, receiver, key().0))
        }
    }

    #[tokio::test]
    async fn do_not_call_sender_and_receiver_until_authorized() {
        let writer = WrappingWriter::new_with_closure(Vec::new(), move || {
            panic!("Writer should not be called.");
        });
        let reader =
            WrappingReader::new_with_closure(IteratorWrapper([0].into_iter().cycle()), move || {
                panic!("Reader should not be called.");
            });
        let stream = MockWrappedSplittable::new(reader, writer);
        let (result_for_parent, _) = mpsc::unbounded();
        let (data_for_user, _) = mpsc::unbounded::<Vec<i32>>();

        let authorizer_called = Mutex::new(false);
        let authorizer = MockAuthorizer::new_with_closure(|_| {
            *authorizer_called.lock().unwrap() = true;
            false
        });
        let (_, secret_key) = key();
        // it should exit immediately after we reject authorization
        // `NoHandshake` mocks the real handshake procedure. It does not call reader and writer.
        let result = handle_incoming::<_, _, _, _, NoHandshake>(
            stream,
            secret_key,
            authorizer,
            result_for_parent,
            data_for_user,
        )
        .await;
        assert!(result.is_ok());
        assert!(*authorizer_called.lock().unwrap());
    }
}
