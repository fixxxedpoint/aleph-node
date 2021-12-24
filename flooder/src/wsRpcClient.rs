use std::sync::mpsc::channel;
use std::sync::mpsc::Sender as ThreadOut;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::thread::JoinHandle;

use log::info;
use serde_json::Value;
use sp_core::H256 as Hash;
use ws::Message;
use ws::{connect, CloseCode, Handler, Result as WsResult, Sender as WsSender};

use substrate_api_client::rpc::ws_client::on_extrinsic_msg_submit_only;
use substrate_api_client::std::rpc::json_req;
use substrate_api_client::std::rpc::ws_client::Subscriber;
use substrate_api_client::std::rpc::ws_client::{
    on_extrinsic_msg_until_broadcast, on_extrinsic_msg_until_finalized,
    on_extrinsic_msg_until_in_block, on_extrinsic_msg_until_ready, on_get_request_msg,
    on_subscription_msg, OnMessageFn, RpcClient,
};
use substrate_api_client::std::ApiClientError;
use substrate_api_client::std::ApiResult;
use substrate_api_client::std::FromHexString;
use substrate_api_client::std::RpcClient as RpcClientTrait;
use substrate_api_client::std::XtStatus;

pub struct WsRpcClient {
    next_handler: Arc<Mutex<Option<RpcClient>>>,
    join_handle: Option<thread::JoinHandle<WsResult<()>>>,
    out: WsSender,
}

impl WsRpcClient {
    pub fn new(url: &str) -> WsRpcClient {
        let (sender, join_handle, rpc_client) = start_rpc_client_thread(url.to_string())
            .expect("unable to initialized WebSocket's thread");
        WsRpcClient {
            next_handler: rpc_client,
            join_handle: Some(join_handle),
            out: sender,
        }
    }
}

impl Drop for WsRpcClient {
    fn drop(&mut self) {
        self.close();
    }
}

impl RpcClientTrait for WsRpcClient {
    fn get_request(&self, jsonreq: Value) -> ApiResult<String> {
        let (result_in, result_out) = channel();
        self.get(jsonreq.to_string(), result_in)?;

        let str = result_out.recv()?;
        Ok(str)
    }

    fn send_extrinsic(
        &self,
        xthex_prefixed: String,
        exit_on: XtStatus,
    ) -> ApiResult<Option<sp_core::H256>> {
        // Todo: Make all variants return a H256: #175.

        let jsonreq = match exit_on {
            XtStatus::SubmitOnly => json_req::author_submit_extrinsic(&xthex_prefixed).to_string(),
            _ => json_req::author_submit_and_watch_extrinsic(&xthex_prefixed).to_string(),
        };

        let (result_in, result_out) = channel();
        let result = match exit_on {
            XtStatus::Finalized => {
                self.send_extrinsic_and_wait_until_finalized(jsonreq, result_in)?;
                let res = result_out.recv()?;
                info!("finalized: {}", res);
                Ok(Some(Hash::from_hex(res)?))
            }
            XtStatus::InBlock => {
                self.send_extrinsic_and_wait_until_in_block(jsonreq, result_in)?;
                let res = result_out.recv()?;
                info!("inBlock: {}", res);
                Ok(Some(Hash::from_hex(res)?))
            }
            XtStatus::Broadcast => {
                self.send_extrinsic_and_wait_until_broadcast(jsonreq, result_in)?;
                let res = result_out.recv()?;
                info!("broadcast: {}", res);
                Ok(None)
            }
            XtStatus::Ready => {
                self.send_extrinsic_until_ready(jsonreq, result_in)?;
                let res = result_out.recv()?;
                info!("ready: {}", res);
                Ok(None)
            }
            XtStatus::SubmitOnly => {
                self.send_extrinsic(jsonreq, result_in)?;
                let res = result_out.recv()?;
                info!("submitted xt: {}", res);
                Ok(None)
            }
            _ => Err(ApiClientError::UnsupportedXtStatus(exit_on)),
        };

        // reset the RpcClient handler used by the WebSocket's thread
        *self
            .next_handler
            .lock()
            .expect("unable to acquire a lock on RpcClient") = None;
        result
    }
}

impl Subscriber for WsRpcClient {
    fn start_subscriber(
        &self,
        json_req: String,
        result_in: ThreadOut<String>,
    ) -> Result<(), ws::Error> {
        self.start_subscriber(json_req, result_in)
    }
}

impl WsRpcClient {
    pub fn get(&self, json_req: String, result_in: ThreadOut<String>) -> WsResult<()> {
        self.start_rpc_client_thread(json_req, result_in, on_get_request_msg)
    }

    pub fn send_extrinsic(&self, json_req: String, result_in: ThreadOut<String>) -> WsResult<()> {
        self.start_rpc_client_thread(json_req, result_in, on_extrinsic_msg_submit_only)
    }

    pub fn send_extrinsic_until_ready(
        &self,
        json_req: String,
        result_in: ThreadOut<String>,
    ) -> WsResult<()> {
        self.start_rpc_client_thread(json_req, result_in, on_extrinsic_msg_until_ready)
    }

    pub fn send_extrinsic_and_wait_until_broadcast(
        &self,
        json_req: String,
        result_in: ThreadOut<String>,
    ) -> WsResult<()> {
        self.start_rpc_client_thread(json_req, result_in, on_extrinsic_msg_until_broadcast)
    }

    pub fn send_extrinsic_and_wait_until_in_block(
        &self,
        json_req: String,
        result_in: ThreadOut<String>,
    ) -> WsResult<()> {
        self.start_rpc_client_thread(json_req, result_in, on_extrinsic_msg_until_in_block)
    }

    pub fn send_extrinsic_and_wait_until_finalized(
        &self,
        json_req: String,
        result_in: ThreadOut<String>,
    ) -> WsResult<()> {
        self.start_rpc_client_thread(json_req, result_in, on_extrinsic_msg_until_finalized)
    }

    pub fn start_subscriber(&self, json_req: String, result_in: ThreadOut<String>) -> WsResult<()> {
        self.start_rpc_client_thread(json_req, result_in, on_subscription_msg)
    }

    fn start_rpc_client_thread(
        &self,
        jsonreq: String,
        result_in: ThreadOut<String>,
        on_message_fn: OnMessageFn,
    ) -> WsResult<()> {
        // todo!();
        // send the request using the `out` channel
        // create RpcClient like in the original code, but without its `on_open` method (it was responsible for sending request, which we already did)
        // set that RpcClient to WsHandler on the other thread so it can use it to handle responses

        // or slightly better idea (?)
        // use https://docs.rs/ws/0.9.1/src/ws/communication.rs.html#86
        // use similar approach like above, but do not copy the RpcClient from that library, just provide it a fake Sender
        // when it finishes (ThreadOut) just switch it with NoOp Handler until a new request arrives

        // let (tx, rx) = sync_channel(2);
        // token: Token,
        // channel: mio::channel::SyncSender<Command>,
        // connection_id: u32,

        // 1 used by on_open of RpcClient + 1 extra buffer
        const MAGIC_SEND_CONST: usize = 2;
        let (ws_tx, _ws_rx) = mio::channel::sync_channel(MAGIC_SEND_CONST);
        let ws_sender = ws::Sender::new(0.into(), ws_tx, 0);

        let rpc_client = RpcClient {
            out: ws_sender,
            request: jsonreq.clone(),
            result: result_in,
            on_message_fn,
        };
        // force lock to be released before we send a message on ws::Sender, otherwise we might get a deadlock
        {
            let next_handler = self
                .next_handler
                .lock()
                .expect("unable to acquire a lock on RpcClient");
            *next_handler = Some(rpc_client);
        }
        self.out.send(jsonreq)
    }

    pub fn close(&mut self) {
        self.out
            .close(CloseCode::Normal)
            .expect("unable to send close on the WebSocket");
        self.join_handle
            .take()
            .map(|handle| handle.join().expect("unable to join WebSocket's thread"));
    }
}

fn start_rpc_client_thread(
    url: String,
) -> Result<
    (
        WsSender,
        JoinHandle<WsResult<()>>,
        Arc<Mutex<Option<RpcClient>>>,
    ),
    (),
> {
    let (tx, rx) = std::sync::mpsc::sync_channel(0);
    let join = thread::Builder::new()
        .name("client".to_owned())
        .spawn(move || -> WsResult<()> {
            connect(url, |out| -> WsHandler {
                let rpc_client = Arc::new(Mutex::new(None));
                tx.send((out, rpc_client.clone()))
                    .expect("main thread was already stopped");
                WsHandler {
                    next_handler: rpc_client,
                }
            })
        })
        .map_err(|_| ())?;
    let (out, rpc_client) = rx.recv().map_err(|_| ())?;
    Ok((out, join, rpc_client))
}

struct WsHandler {
    next_handler: Arc<Mutex<Option<RpcClient>>>,
}

impl Handler for WsHandler {
    fn on_message(&mut self, msg: Message) -> WsResult<()> {
        if let Some(handler) = self
            .next_handler
            .lock()
            .expect("main thread probably died")
            .as_mut()
        {
            handler.on_message(msg)
        } else {
            Ok(())
        }
    }
}
