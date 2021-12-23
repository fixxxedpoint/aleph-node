use std::sync::mpsc::{Receiver, SendError, Sender as ThreadOut};

use ac_node_api::events::{EventsDecoder, Raw, RawEvent};
use codec::Decode;
use log::{debug, error, info, warn};
use serde_json::Value;
use sp_core::Pair;
use sp_runtime::MultiSignature;
use ws::{CloseCode, Error, Handler, Handshake, Message, Result as WsResult, Sender};

use substrate_api_client::std::rpc::RpcClientError;
use substrate_api_client::std::{json_req, FromHexString, RpcClient as RpcClientTrait, XtStatus};
use substrate_api_client::std::{Api, ApiResult};
use substrate_api_client::utils;

pub type OnMessageFn = fn(msg: Message, result: &ThreadOut<String>) -> WsResult<()>;

type RpcResult<T> = Result<T, RpcClientError>;

pub struct RpcClient {
    pub result: ThreadOut<String>,
    pub on_message_fn: OnMessageFn,
}

impl Handler for RpcClient {
    fn on_message(&mut self, msg: Message) -> WsResult<()> {
        (self.on_message_fn)(msg, &self.result)
    }
}

pub fn on_get_request_msg(msg: Message, result: &ThreadOut<String>) -> WsResult<()> {
    info!("Got get_request_msg {}", msg);
    let result_str = serde_json::from_str(msg.as_text()?)
        .map(|v: serde_json::Value| v["result"].to_string())
        .map_err(|e| Box::new(RpcClientError::Serde(e)))?;

    result
        .send(result_str)
        .map_err(|e| Box::new(RpcClientError::Send(e)).into())
}

pub fn on_subscription_msg(msg: Message, result: &ThreadOut<String>) -> WsResult<()> {
    info!("got on_subscription_msg {}", msg);
    let value: serde_json::Value =
        serde_json::from_str(msg.as_text()?).map_err(|e| Box::new(RpcClientError::Serde(e)))?;

    match value["id"].as_str() {
        Some(_idstr) => {}
        _ => {
            // subscriptions
            debug!("no id field found in response. must be subscription");
            debug!("method: {:?}", value["method"].as_str());
            match value["method"].as_str() {
                Some("state_storage") => {
                    let changes = &value["params"]["result"]["changes"];
                    match changes[0][1].as_str() {
                        Some(change_set) => {
                            if let Err(SendError(e)) = result.send(change_set.to_owned()) {
                                debug!("SendError on `result` channel: {}.", e);
                            }
                        }
                        None => println!("No events happened"),
                    };
                }
                Some("chain_finalizedHead") => {
                    let head = serde_json::to_string(&value["params"]["result"])
                        .map_err(|e| Box::new(RpcClientError::Serde(e)))?;

                    if let Err(e) = result.send(head) {
                        debug!("SendError on `result` channel: {}.", e);
                    }
                }
                _ => error!("unsupported method"),
            }
        }
    };
    Ok(())
}

pub fn on_extrinsic_msg_until_finalized(msg: Message, result: &ThreadOut<String>) -> WsResult<()> {
    let retstr = msg.as_text().unwrap();
    debug!("got msg {}", retstr);
    match parse_status(retstr) {
        Ok((XtStatus::Finalized, val)) => end_process(result, val),
        Ok((XtStatus::Future, _)) => {
            warn!("extrinsic has 'future' status. aborting");
            end_process(result, None)
        }
        Err(e) => {
            end_process(result, None)?;
            Err(Box::new(e).into())
        }
        _ => Ok(()),
    }
}

pub fn on_extrinsic_msg_until_in_block(msg: Message, result: &ThreadOut<String>) -> WsResult<()> {
    let retstr = msg.as_text().unwrap();
    debug!("got msg {}", retstr);
    match parse_status(retstr) {
        Ok((XtStatus::Finalized, val)) => end_process(result, val),
        Ok((XtStatus::InBlock, val)) => end_process(result, val),
        Ok((XtStatus::Future, _)) => end_process(result, None),
        Err(e) => {
            end_process(result, None)?;
            Err(Box::new(e).into())
        }
        _ => Ok(()),
    }
}

pub fn on_extrinsic_msg_until_broadcast(msg: Message, result: &ThreadOut<String>) -> WsResult<()> {
    let retstr = msg.as_text().unwrap();
    debug!("got msg {}", retstr);
    match parse_status(retstr) {
        Ok((XtStatus::Finalized, val)) => end_process(result, val),
        Ok((XtStatus::Broadcast, _)) => end_process(result, None),
        Ok((XtStatus::Future, _)) => end_process(result, None),
        Err(e) => {
            end_process(result, None)?;
            Err(Box::new(e).into())
        }
        _ => Ok(()),
    }
}

pub fn on_extrinsic_msg_until_ready(msg: Message, result: &ThreadOut<String>) -> WsResult<()> {
    let retstr = msg.as_text().unwrap();
    debug!("got msg {}", retstr);
    match parse_status(retstr) {
        Ok((XtStatus::Finalized, val)) => end_process(result, val),
        Ok((XtStatus::Ready, _)) => end_process(result, None),
        Ok((XtStatus::Future, _)) => end_process(result, None),
        Err(e) => {
            end_process(result, None)?;
            Err(Box::new(e).into())
        }
        _ => Ok(()),
    }
}

pub fn on_extrinsic_msg_submit_only(msg: Message, result: &ThreadOut<String>) -> WsResult<()> {
    let retstr = msg.as_text().unwrap();
    debug!("got msg {}", retstr);
    match result_from_json_response(retstr) {
        Ok(val) => end_process(result, Some(val)),
        Err(e) => {
            end_process(result, None)?;
            Err(Box::new(e).into())
        }
    }
}

fn end_process(result: &ThreadOut<String>, value: Option<String>) -> WsResult<()> {
    // return result to calling thread
    debug!("Thread end result :{:?} value:{:?}", result, value);
    let val = value.unwrap_or_else(|| "".to_string());

    result
        .send(val)
        .map_err(|e| Box::new(RpcClientError::Send(e)).into())
}

fn parse_status(msg: &str) -> RpcResult<(XtStatus, Option<String>)> {
    let value: serde_json::Value = serde_json::from_str(msg)?;

    if value["error"].as_object().is_some() {
        return Err(into_extrinsic_err(&value));
    }

    match value["params"]["result"].as_object() {
        Some(obj) => {
            if let Some(hash) = obj.get("finalized") {
                info!("finalized: {:?}", hash);
                Ok((XtStatus::Finalized, Some(hash.to_string())))
            } else if let Some(hash) = obj.get("inBlock") {
                info!("inBlock: {:?}", hash);
                Ok((XtStatus::InBlock, Some(hash.to_string())))
            } else if let Some(array) = obj.get("broadcast") {
                info!("broadcast: {:?}", array);
                Ok((XtStatus::Broadcast, Some(array.to_string())))
            } else {
                Ok((XtStatus::Unknown, None))
            }
        }
        None => match value["params"]["result"].as_str() {
            Some("ready") => Ok((XtStatus::Ready, None)),
            Some("future") => Ok((XtStatus::Future, None)),
            Some(&_) => Ok((XtStatus::Unknown, None)),
            None => Ok((XtStatus::Unknown, None)),
        },
    }
}

/// Todo: this is the code that was used in `parse_status` Don't we want to just print the
/// error as is instead of introducing our custom format here?
fn into_extrinsic_err(resp_with_err: &Value) -> RpcClientError {
    let err_obj = resp_with_err["error"].as_object().unwrap();

    let error = err_obj
        .get("message")
        .map_or_else(|| "", |e| e.as_str().unwrap());
    let code = err_obj
        .get("code")
        .map_or_else(|| -1, |c| c.as_i64().unwrap());
    let details = err_obj
        .get("data")
        .map_or_else(|| "", |d| d.as_str().unwrap());

    RpcClientError::Extrinsic(format!(
        "extrinsic error code {}: {}: {}",
        code, error, details
    ))
}

fn result_from_json_response(resp: &str) -> RpcResult<String> {
    let value: serde_json::Value = serde_json::from_str(resp)?;

    let resp = value["result"]
        .as_str()
        .ok_or_else(|| into_extrinsic_err(&value))?;

    Ok(resp.to_string())
}
