use futures::{
    channel::{mpsc, oneshot},
    StreamExt,
};

pub enum AuthorizatorError {
    MissingService,
    ServiceDisappeared,
}

/// Allows one to authorize incoming public-keys.
#[async_trait::async_trait]
pub trait Authorization<PK> {
    async fn is_authorized(&self, value: PK) -> Result<bool, AuthorizatorError>;
}

struct AuthorizationHandler<PK> {
    identifier: PK,
    result_sender: oneshot::Sender<bool>,
}

impl<PK> AuthorizationHandler<PK> {
    fn new(result: PK) -> (Self, oneshot::Receiver<bool>) {
        let (auth_sender, auth_receiver) = oneshot::channel();
        (
            Self {
                identifier: result,
                result_sender: auth_sender,
            },
            auth_receiver,
        )
    }

    pub fn handle_authorization(
        self,
        mut handler: impl FnMut(PK) -> bool,
    ) -> Result<(), AuthorizatorError> {
        let auth_result = handler(self.identifier);
        self.result_sender
            .send(auth_result)
            .map_err(|_| AuthorizatorError::MissingService)
    }
}

/// Used for validation of authorization requests. One should call [handle_authorization](Self::handle_authorization) and
/// provide a callback responsible for authorization. Each such call should be matched with call to
/// [Authorizator::is_authorized](Authorizator::is_authorized).
pub struct AuthorizationRequestHandler<PK> {
    receiver: mpsc::UnboundedReceiver<AuthorizationHandler<PK>>,
}

impl<PK> AuthorizationRequestHandler<PK> {
    fn new(receiver: mpsc::UnboundedReceiver<AuthorizationHandler<PK>>) -> Self {
        Self { receiver }
    }

    pub async fn handle_authorization<F: FnMut(PK) -> bool>(
        &mut self,
        handler: F,
    ) -> Result<(), AuthorizatorError> {
        let next = self
            .receiver
            .next()
            .await
            .ok_or(AuthorizatorError::MissingService)?;

        next.handle_authorization(handler)
    }
}

#[derive(Clone)]
pub struct Authorizator<PK> {
    sender: mpsc::UnboundedSender<AuthorizationHandler<PK>>,
}

/// `Authorizator` is responsible for public-key authorization.
/// Each call to [is_authorized](Authorizator::is_authorized) should be followed by a call of
/// [handle_authorization](AuthorizationHandler::handle_authorization).
impl<PK> Authorizator<PK> {
    pub fn new() -> (Self, AuthorizationRequestHandler<PK>) {
        let (sender, receiver) = mpsc::unbounded();
        (Self { sender }, AuthorizationRequestHandler::new(receiver))
    }
}

#[async_trait::async_trait]
impl<PK: Send> Authorization<PK> for Authorizator<PK> {
    async fn is_authorized(&self, value: PK) -> Result<bool, AuthorizatorError> {
        let (handler, receiver) = AuthorizationHandler::new(value);
        self.sender
            .unbounded_send(handler)
            .map_err(|_| AuthorizatorError::MissingService)?;
        receiver
            .await
            .map_err(|_| AuthorizatorError::ServiceDisappeared)
    }
}
