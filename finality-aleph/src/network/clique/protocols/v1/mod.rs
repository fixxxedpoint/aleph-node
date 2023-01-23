use codec::{Decode, Encode};
use futures::{channel::mpsc, StreamExt};
use log::{debug, info, trace, warn};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::{timeout, Duration},
};

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
    trace!(target: LOG_TARGET, "Waiting for extended hand...");
    let (sender, receiver, public_key) = v0_handshake_incoming(stream, secret_key).await?;
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
        io::Write,
        pin::Pin,
        task::{Context, Poll},
        thread::{sleep, spawn},
        time::Duration,
    };

    use futures::{
        channel::{
            mpsc::{self, UnboundedReceiver},
            oneshot,
        },
        pin_mut, FutureExt, StreamExt,
    };
    use tokio::{
        io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf},
        net::{TcpListener, TcpStream},
        task::spawn_blocking,
    };

    use super::{incoming, manage_connection, outgoing, ProtocolError};
    use crate::network::clique::{
        mock::{key, MockPrelims, MockPublicKey, MockSplittable},
        protocols::{v1::Message, ConnectionType},
        Authorizator, Data,
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
        let (authorizer, auth_handler) = Authorizator::new();
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

    #[tokio::test]
    async fn try_starving_sender() {
        let (user_sender, _user_receiver) = mpsc::unbounded();
        let (data_from_user_sender, data_from_user_receiver) = mpsc::unbounded();
        let mut data_from_user_sender = Some(data_from_user_sender);

        struct MockReader<A, R> {
            action: A,
            reader: R,
        }

        impl<A: FnMut() + Unpin, R: AsyncRead + Unpin> AsyncRead for MockReader<A, R> {
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
                            println!("how?");
                            return Poll::Pending;
                        }
                    }
                }
                buf.advance(remaining);
                Poll::Ready(Result::Ok(()))
            }
        }

        let mut data = Vec::<u8>::new();
        let data_to_send = Message::Data(vec![1u8, 2u8, 3u8]);
        data = crate::network::clique::protocols::v1::send_data(data, data_to_send)
            .await
            .expect("we should be able to encode data");
        println!("data: {:?}", data);
        let reader = MockReader {
            action: move || {
                data_from_user_sender
                    .take()
                    .map(|ch| drop(ch))
                    .unwrap_or(())
            },
            reader: IteratorWrapper(data.into_iter().cycle()),
        };

        struct MockWriter {}

        impl AsyncWrite for MockWriter {
            fn poll_write(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
                _: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                panic!("MockWriter `poll_write` should never be called");
                // Poll::Ready(std::io::Result::Ok(1))
            }

            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                panic!("MockWriter `poll_flush` should never be called");
                // Poll::Ready(std::io::Result::Ok(()))
            }

            fn poll_shutdown(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                panic!("MockWriter `poll_shutdown` should never be called");
                // Poll::Ready(std::io::Result::Ok(()))
            }
        }

        manage_connection::<MockPublicKey, Vec<u8>, _, _>(
            MockWriter {},
            reader,
            data_from_user_receiver,
            user_sender,
        )
        .await
        .expect("it should never return");

        panic!("this test should never end");
    }

    #[tokio::test]
    async fn try_starving_sender_with_tcp_stream() {
        let (user_sender, mut user_receiver) = mpsc::unbounded();
        let (data_from_user_sender, data_from_user_receiver): (_, UnboundedReceiver<Vec<u8>>) =
            mpsc::unbounded();
        let mut data_from_user_sender = Some(data_from_user_sender);

        let localhost_address = "127.0.0.1:6666";
        let listener = std::net::TcpListener::bind(localhost_address)
            .expect("we should be able to create a TcpListener");

        let mut data = Vec::<u8>::new();
        let data_to_send = Message::Data(vec![1u8, 2u8, 3u8]);
        data = crate::network::clique::protocols::v1::send_data(data, data_to_send)
            .await
            .expect("we should be able to encode data");
        let (sender, receiver) = oneshot::channel();
        // spawn_blocking(|| {
        std::thread::spawn(move || {
            sender
                .send(())
                .expect("we should be able to send thread initialization signal");

            let mut stream = listener
                .accept()
                .map(|(stream, _)| stream)
                .expect("we should be able to accept an connection");

            loop {
                // sleep(Duration::from_secs(1))
                stream
                    .write_all(&data[..])
                    .expect("we should be able to send data using TcpStream");
            }
        });

        receiver
            .await
            .expect("we should be able to receive thread initialization signal");

        struct MockReader<A, R> {
            action: A,
            reader: R,
        }

        impl<A: FnMut() + Unpin, R: AsyncRead + Unpin> AsyncRead for MockReader<A, R> {
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

        let mut out_connection = TcpStream::connect(localhost_address)
            .await
            .expect("we should be able to connect to our TcpListener");

        let (reader, writer) = out_connection.split();

        let reader = MockReader {
            action: move || {
                // println!("reading");
                // println!("dropping");
                // data_from_user_sender
                //     .take()
                //     .map(|ch| drop(ch))
                //     .unwrap_or(())
            },
            reader,
        };

        struct MockWriter<A, W> {
            action: A,
            writer: W,
        }

        impl<A: FnMut() + Unpin, W: AsyncWrite + Unpin> AsyncWrite for MockWriter<A, W> {
            fn poll_write(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                // panic!("MockWriter `poll_write` should never be called");
                // Poll::Ready(std::io::Result::Ok(1))
                let self_mut = self.get_mut();
                // Poll::Pending
                println!("write called");
                (self_mut.action)();
                return Poll::Pending;

                // println!("calling action");
                // (self_mut.action)();

                let result = <W as AsyncWrite>::poll_write(Pin::new(&mut self_mut.writer), cx, buf);
                if let Poll::Pending = result {
                    println!("calling action");
                    (self_mut.action)();
                }
                result
            }

            fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                // panic!("MockWriter `poll_flush` should never be called");
                // Poll::Ready(std::io::Result::Ok(()))
                // Poll::Pending
                // self.as_mut().poll_flush(cx)
                println!("flush called");
                <W as AsyncWrite>::poll_flush(Pin::new(&mut self.get_mut().writer), cx)
            }

            fn poll_shutdown(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                // panic!("MockWriter `poll_shutdown` should never be called");
                // Poll::Ready(std::io::Result::Ok(()))
                // Poll::Pending
                println!("shutdown called");
                <W as AsyncWrite>::poll_shutdown(Pin::new(&mut self.get_mut().writer), cx)
            }
        }

        tokio::spawn(async move {
            while let Some(_) = user_receiver.next().await {
                println!("received data on user-channel");
            }
        });
        manage_connection::<MockPublicKey, Vec<u8>, _, _>(
            MockWriter {
                action: move || {
                    println!("dropping");
                    data_from_user_sender
                        .take()
                        .map(|ch| drop(ch))
                        .unwrap_or(())
                },
                writer,
            },
            reader,
            data_from_user_receiver,
            user_sender,
        )
        .await
        .expect("it should never return");

        panic!("this test should never end");
    }

    #[tokio::test]
    async fn make_sender_never_call_timeout_again() {
        let (user_sender, mut user_receiver) = mpsc::unbounded();
        let (data_from_user_sender, data_from_user_receiver): (_, UnboundedReceiver<Vec<u8>>) =
            mpsc::unbounded();
        let mut data_from_user_sender = Some(data_from_user_sender);

        let localhost_address = "127.0.0.1:6666";
        let listener = std::net::TcpListener::bind(localhost_address)
            .expect("we should be able to create a TcpListener");

        let mut data = Vec::<u8>::new();
        let data_to_send = Message::Data(vec![1u8, 2u8, 3u8]);
        data = crate::network::clique::protocols::v1::send_data(data, data_to_send)
            .await
            .expect("we should be able to encode data");
        let (sender, receiver) = oneshot::channel();
        // spawn_blocking(|| {
        std::thread::spawn(move || {
            sender
                .send(())
                .expect("we should be able to send thread initialization signal");

            let mut stream = listener
                .accept()
                .map(|(stream, _)| stream)
                .expect("we should be able to accept an connection");

            loop {
                // sleep(Duration::from_secs(1))
                stream
                    .write_all(&data[..])
                    .expect("we should be able to send data using TcpStream");
            }
        });

        receiver
            .await
            .expect("we should be able to receive thread initialization signal");

        struct MockReader<A, R> {
            action: A,
            reader: R,
        }

        impl<A: FnMut() + Unpin, R: AsyncRead + Unpin> AsyncRead for MockReader<A, R> {
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

        let mut out_connection = TcpStream::connect(localhost_address)
            .await
            .expect("we should be able to connect to our TcpListener");

        let (reader, writer) = out_connection.split();

        let reader = MockReader {
            action: move || {
                // println!("reading");
                // println!("dropping");
                // data_from_user_sender
                //     .take()
                //     .map(|ch| drop(ch))
                //     .unwrap_or(())
            },
            reader,
        };

        struct MockWriter<A, W> {
            action: A,
            writer: W,
        }

        impl<A: FnMut() + Unpin, W: AsyncWrite + Unpin> AsyncWrite for MockWriter<A, W> {
            fn poll_write(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                // panic!("MockWriter `poll_write` should never be called");
                // Poll::Ready(std::io::Result::Ok(1))
                let self_mut = self.get_mut();
                // Poll::Pending
                println!("write called");
                (self_mut.action)();
                return Poll::Pending;
                // return Poll::Ready(Ok(buf.len()));

                // println!("calling action");
                // (self_mut.action)();

                let result = <W as AsyncWrite>::poll_write(Pin::new(&mut self_mut.writer), cx, buf);
                if let Poll::Pending = result {
                    println!("calling action");
                    (self_mut.action)();
                }
                result
            }

            fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                // panic!("MockWriter `poll_flush` should never be called");
                // Poll::Ready(std::io::Result::Ok(()))
                // Poll::Pending
                // self.as_mut().poll_flush(cx)
                println!("flush called");
                <W as AsyncWrite>::poll_flush(Pin::new(&mut self.get_mut().writer), cx)
            }

            fn poll_shutdown(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                // panic!("MockWriter `poll_shutdown` should never be called");
                // Poll::Ready(std::io::Result::Ok(()))
                // Poll::Pending
                println!("shutdown called");
                <W as AsyncWrite>::poll_shutdown(Pin::new(&mut self.get_mut().writer), cx)
            }
        }

        tokio::spawn(async move {
            while let Some(_) = user_receiver.next().await {
                // println!("received data on user-channel");
            }
        });
        manage_connection::<MockPublicKey, Vec<u8>, _, _>(
            MockWriter {
                action: move || {
                    println!("dropping");
                    data_from_user_sender
                        .take()
                        .map(|ch| drop(ch))
                        .unwrap_or(())
                },
                writer,
            },
            reader,
            data_from_user_receiver,
            user_sender,
        )
        .await
        .expect("it should never return");

        panic!("this test should never end");
    }
}
