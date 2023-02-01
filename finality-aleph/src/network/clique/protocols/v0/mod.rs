use futures::{
    channel::{mpsc, oneshot},
    StreamExt,
};
use log::{debug, info, trace, warn};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::network::clique::{
    io::{receive_data, send_data},
    protocols::{
        handshake::{v0_handshake_incoming, v0_handshake_outgoing},
        ConnectionType, ProtocolError, ResultForService,
    },
    Data, PublicKey, SecretKey, Splittable, LOG_TARGET,
};

mod heartbeat;

use heartbeat::{heartbeat_receiver, heartbeat_sender};

/// Receives data from the parent service and sends it over the network.
/// Exits when the parent channel is closed, or if the network connection is broken.
async fn sending<PK: PublicKey, D: Data, S: AsyncWrite + Unpin + Send>(
    mut sender: S,
    mut data_from_user: mpsc::UnboundedReceiver<D>,
) -> Result<(), ProtocolError<PK>> {
    loop {
        sender = match data_from_user.next().await {
            Some(data) => send_data(sender, data).await?,
            // We have been closed by the parent service, all good.
            None => return Ok(()),
        };
    }
}

/// Performs the handshake, and then keeps sending data received from the parent service.
/// Exits on parent request, or in case of broken or dead network connection.
pub async fn outgoing<SK: SecretKey, D: Data, S: Splittable>(
    stream: S,
    secret_key: SK,
    public_key: SK::PublicKey,
    result_for_parent: mpsc::UnboundedSender<ResultForService<SK::PublicKey, D>>,
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
            ConnectionType::LegacyOutgoing,
        ))
        .map_err(|_| ProtocolError::NoParentConnection)?;

    let sending = sending(sender, data_from_user);
    let heartbeat = heartbeat_receiver(receiver);

    debug!(
        target: LOG_TARGET,
        "Starting worker for sending to {}.", public_key
    );
    loop {
        tokio::select! {
            _ = heartbeat => return Err(ProtocolError::CardiacArrest),
            result = sending => return result,
        }
    }
}

/// Receives data from the network and sends it to the parent service.
/// Exits when the parent channel is closed, or if the network connection is broken.
async fn receiving<PK: PublicKey, D: Data, S: AsyncRead + Unpin + Send>(
    mut stream: S,
    data_for_user: mpsc::UnboundedSender<D>,
) -> Result<(), ProtocolError<PK>> {
    loop {
        let (old_stream, data) = receive_data(stream).await?;
        stream = old_stream;
        data_for_user
            .unbounded_send(data)
            .map_err(|_| ProtocolError::NoUserConnection)?;
    }
}

/// Performs the handshake, and then keeps sending data received from the network to the parent service.
/// Exits on parent request, or in case of broken or dead network connection.
pub async fn incoming<SK: SecretKey, D: Data, S: Splittable>(
    stream: S,
    secret_key: SK,
    authorization_requests_sender: mpsc::UnboundedSender<(SK::PublicKey, oneshot::Sender<bool>)>,
    result_for_parent: mpsc::UnboundedSender<ResultForService<SK::PublicKey, D>>,
    data_for_user: mpsc::UnboundedSender<D>,
) -> Result<(), ProtocolError<SK::PublicKey>> {
    trace!(target: LOG_TARGET, "Waiting for extended hand...");
    let (sender, receiver, public_key) = v0_handshake_incoming(stream, secret_key).await?;
    info!(
        target: LOG_TARGET,
        "Incoming handshake with {} finished successfully.", public_key
    );

    let authorized = handle_authorization::<SK>(authorization_requests_sender, public_key.clone())
        .await
        .map_err(|_| ProtocolError::NotAuthorized)?;
    if !authorized {
        warn!(
            target: LOG_TARGET,
            "public_key={} was not authorized.", public_key
        );
        return Ok(());
    }

    let (tx_exit, mut exit) = mpsc::unbounded();
    result_for_parent
        .unbounded_send((
            public_key.clone(),
            Some(tx_exit),
            ConnectionType::LegacyIncoming,
        ))
        .map_err(|_| ProtocolError::NoParentConnection)?;

    let receiving = receiving(receiver, data_for_user);
    let heartbeat = heartbeat_sender(sender);

    debug!(
        target: LOG_TARGET,
        "Starting worker for receiving from {}.", public_key
    );
    loop {
        tokio::select! {
            _ = heartbeat => return Err(ProtocolError::CardiacArrest),
            result = receiving => return result,
            _ = exit.next() => return Ok(()),
        }
    }
}

pub async fn handle_authorization<SK: SecretKey>(
    authorization_requests_sender: mpsc::UnboundedSender<(SK::PublicKey, oneshot::Sender<bool>)>,
    public_key: SK::PublicKey,
) -> Result<bool, ()> {
    let (sender, receiver) = oneshot::channel();
    if let Err(err) = authorization_requests_sender.unbounded_send((public_key.clone(), sender)) {
        warn!(
            target: LOG_TARGET,
            "Unable to send authorization request for public-key {}: disconnected.",
            err.into_inner().0,
        );
        return Err(());
    }
    match receiver.await {
        Ok(result) => Ok(result),
        Err(_) => {
            warn!(
                target: LOG_TARGET,
                "Unable to receive an answer to authorization request for public-key {}: disconnected.",
                public_key,
            );
            Err(())
        }
    }
}

#[cfg(test)]
pub mod tests {
    use futures::{
        channel::{mpsc, oneshot},
        pin_mut, Future, FutureExt, StreamExt,
    };

    use crate::network::clique::{
        mock::{key, MockPrelims, MockSplittable},
        protocols::{
            v0::{incoming, outgoing},
            ConnectionType, ProtocolError,
        },
        Data,
    };

    fn prepare<D: Data>() -> MockPrelims<D> {
        let (stream_incoming, stream_outgoing) = MockSplittable::new(4096);
        let (id_incoming, pen_incoming) = key();
        let (id_outgoing, pen_outgoing) = key();
        assert_ne!(id_incoming, id_outgoing);
        let (incoming_result_for_service, result_from_incoming) = mpsc::unbounded();
        let (outgoing_result_for_service, result_from_outgoing) = mpsc::unbounded();
        let (data_for_user, data_from_incoming) = mpsc::unbounded::<D>();
        let (authorization_requests_sender, authorization_requests) = mpsc::unbounded();
        let incoming_handle = Box::pin(incoming(
            stream_incoming,
            pen_incoming.clone(),
            authorization_requests_sender,
            incoming_result_for_service,
            data_for_user,
        ));
        let outgoing_handle = Box::pin(outgoing(
            stream_outgoing,
            pen_outgoing.clone(),
            id_incoming.clone(),
            outgoing_result_for_service,
        ));
        MockPrelims {
            id_incoming,
            pen_incoming,
            id_outgoing,
            pen_outgoing,
            incoming_handle,
            outgoing_handle,
            data_from_incoming,
            data_from_outgoing: None,
            result_from_incoming,
            result_from_outgoing,
            authorization_requests,
        }
    }

    fn handle_authorization<PK: Send + 'static>(
        mut authorization_requests: mpsc::UnboundedReceiver<(PK, oneshot::Sender<bool>)>,
        handler: impl FnOnce(PK) -> bool + Send + 'static,
    ) -> impl Future<Output = Result<(), ()>> {
        tokio::spawn(async move {
            let (public_key, response_sender) = authorization_requests
                .next()
                .await
                .expect("We should recieve at least one authorization request.");
            let authorization_result = handler(public_key);
            response_sender
                .send(authorization_result)
                .expect("We should be able to send back an authorization response.");
            Result::<(), ()>::Ok(())
        })
        .map(|result| match result {
            Ok(ok) => ok,
            Err(_) => Err(()),
        })
    }

    fn all_pass_authorization_handler<PK: Send + 'static>(
        authorization_requests: mpsc::UnboundedReceiver<(PK, oneshot::Sender<bool>)>,
    ) -> impl Future<Output = Result<(), ()>> {
        handle_authorization(authorization_requests, |_| true)
    }

    fn no_go_authorization_handler<PK: Send + 'static>(
        authorization_requests: mpsc::UnboundedReceiver<(PK, oneshot::Sender<bool>)>,
    ) -> impl Future<Output = Result<(), ()>> {
        handle_authorization(authorization_requests, |_| false)
    }

    #[tokio::test]
    async fn send_data() {
        let MockPrelims {
            incoming_handle,
            outgoing_handle,
            mut data_from_incoming,
            result_from_incoming: _result_from_incoming,
            mut result_from_outgoing,
            authorization_requests,
            ..
        } = prepare::<Vec<i32>>();
        let _authorization_handle = all_pass_authorization_handler(authorization_requests);
        let incoming_handle = incoming_handle.fuse();
        let outgoing_handle = outgoing_handle.fuse();
        pin_mut!(incoming_handle);
        pin_mut!(outgoing_handle);
        let _data_for_outgoing = tokio::select! {
            _ = &mut incoming_handle => panic!("incoming process unexpectedly finished"),
            _ = &mut outgoing_handle => panic!("outgoing process unexpectedly finished"),
            result = result_from_outgoing.next() => {
                let (_, maybe_data_for_outgoing, connection_type) = result.expect("the channel shouldn't be dropped");
                assert_eq!(connection_type, ConnectionType::LegacyOutgoing);
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
    }

    #[tokio::test]
    async fn closed_by_parent_service() {
        let MockPrelims {
            id_outgoing,
            incoming_handle,
            outgoing_handle,
            data_from_incoming: _data_from_incoming,
            mut result_from_incoming,
            result_from_outgoing: _result_from_outgoing,
            authorization_requests,
            ..
        } = prepare::<Vec<i32>>();
        let _authorization_handle = all_pass_authorization_handler(authorization_requests);
        let incoming_handle = incoming_handle.fuse();
        let outgoing_handle = outgoing_handle.fuse();
        pin_mut!(incoming_handle);
        pin_mut!(outgoing_handle);
        tokio::select! {
            _ = &mut incoming_handle => panic!("incoming process unexpectedly finished"),
            _ = &mut outgoing_handle => panic!("outgoing process unexpectedly finished"),
            received = result_from_incoming.next() => {
                // we drop the exit oneshot channel, thus finishing incoming_handle
                let (received_id, _, connection_type) = received.expect("the channel shouldn't be dropped");
                assert_eq!(connection_type, ConnectionType::LegacyIncoming);
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
            result_from_incoming,
            result_from_outgoing: _result_from_outgoing,
            authorization_requests,
            ..
        } = prepare::<Vec<i32>>();
        let _authorization_handle = all_pass_authorization_handler(authorization_requests);
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
            result_from_incoming: _result_from_incoming,
            mut result_from_outgoing,
            authorization_requests,
            ..
        } = prepare::<Vec<i32>>();
        let _authorization_handle = all_pass_authorization_handler(authorization_requests);
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
                assert_eq!(connection_type, ConnectionType::LegacyOutgoing);
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
            result_from_incoming: _result_from_incoming,
            result_from_outgoing: _result_from_outgoing,
            authorization_requests,
            ..
        } = prepare::<Vec<i32>>();
        let _authorization_handle = all_pass_authorization_handler(authorization_requests);
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
            mut result_from_incoming,
            result_from_outgoing: _result_from_outgoing,
            authorization_requests,
            ..
        } = prepare::<Vec<i32>>();
        let _authorization_handle = all_pass_authorization_handler(authorization_requests);
        let incoming_handle = incoming_handle.fuse();
        pin_mut!(incoming_handle);
        let (_, _exit, connection_type) = tokio::select! {
            _ = &mut incoming_handle => panic!("incoming process unexpectedly finished"),
            _ = outgoing_handle => panic!("outgoing process unexpectedly finished"),
            out = result_from_incoming.next() => out.expect("should receive"),
        };
        assert_eq!(connection_type, ConnectionType::LegacyIncoming);
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
            result_from_incoming: _result_from_incoming,
            result_from_outgoing: _result_from_outgoing,
            authorization_requests,
            ..
        } = prepare::<Vec<i32>>();
        let _authorization_handle = all_pass_authorization_handler(authorization_requests);
        std::mem::drop(incoming_handle);
        match outgoing_handle.await {
            Err(ProtocolError::HandshakeError(_)) => (),
            Err(e) => panic!("unexpected error: {}", e),
            Ok(_) => panic!("successfully finished when connection dead"),
        };
    }

    #[tokio::test]
    async fn receiver_dead_after_handshake() {
        let MockPrelims {
            incoming_handle,
            outgoing_handle,
            data_from_incoming: _data_from_incoming,
            mut result_from_incoming,
            result_from_outgoing: _result_from_outgoing,
            authorization_requests,
            ..
        } = prepare::<Vec<i32>>();
        let _authorization_handle = all_pass_authorization_handler(authorization_requests);
        let outgoing_handle = outgoing_handle.fuse();
        pin_mut!(outgoing_handle);
        let (_, _exit, connection_type) = tokio::select! {
            _ = incoming_handle => panic!("incoming process unexpectedly finished"),
            _ = &mut outgoing_handle => panic!("outgoing process unexpectedly finished"),
            out = result_from_incoming.next() => out.expect("should receive"),
        };
        assert_eq!(connection_type, ConnectionType::LegacyIncoming);
        // incoming_handle got consumed by tokio::select!, the receiver is dead
        match outgoing_handle.await {
            // We never get the SendError variant here, because we did not send anything
            // through data_for_outgoing.
            Err(ProtocolError::CardiacArrest) => (),
            Err(e) => panic!("unexpected error: {}", e),
            Ok(_) => panic!("successfully finished when connection dead"),
        };
    }

    #[tokio::test]
    async fn receiver_dead_after_handshake_try_send_error() {
        let MockPrelims {
            incoming_handle,
            outgoing_handle,
            data_from_incoming: _data_from_incoming,
            mut result_from_incoming,
            result_from_outgoing: _result_from_outgoing,
            authorization_requests,
            ..
        } = prepare::<Vec<i32>>();
        let _authorization_handle = all_pass_authorization_handler(authorization_requests);
        let outgoing_handle = outgoing_handle.fuse();
        pin_mut!(outgoing_handle);
        let (_, _exit, connection_type) = tokio::select! {
            _ = incoming_handle => panic!("incoming process unexpectedly finished"),
            _ = &mut outgoing_handle => panic!("outgoing process unexpectedly finished"),
            out = result_from_incoming.next() => out.expect("should receive"),
        };
        assert_eq!(connection_type, ConnectionType::LegacyIncoming);
        match outgoing_handle.await {
            Err(ProtocolError::CardiacArrest) => (),
            Err(e) => panic!("unexpected error: {}", e),
            Ok(_) => panic!("successfully finished when connection dead"),
        };
    }

    #[tokio::test]
    async fn do_not_call_sender_and_receiver_until_authorized() {
        let MockPrelims {
            incoming_handle,
            outgoing_handle,
            mut data_from_incoming,
            mut result_from_incoming,
            authorization_requests,
            ..
        } = prepare::<Vec<i32>>();

        let incoming_handle = incoming_handle.fuse();
        let outgoing_handle = outgoing_handle.fuse();
        let authorization_handle = no_go_authorization_handler(authorization_requests);

        // since we are returning `NotAuthorized` all except `outgoing_handle` should finish hapilly
        let (incoming_result, outgoing_result, authorization_result) =
            tokio::join!(incoming_handle, outgoing_handle, authorization_handle);

        assert!(incoming_result.is_ok());
        assert!(outgoing_result.is_err());
        assert!(authorization_result.is_ok());

        let data_from_incoming = data_from_incoming.try_next();
        assert!(data_from_incoming.ok().flatten().is_none());

        let result_from_incoming = result_from_incoming.try_next();
        assert!(result_from_incoming.ok().flatten().is_none());
    }
}
