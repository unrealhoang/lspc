use std::{
    error::Error,
    io::{self, BufRead, Write},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use crossbeam::channel::Receiver;
use log;
use serde::{Deserialize, Serialize};
use serde_json::{from_str, from_value, to_string, to_value, Value};

use lsp_types::{
    notification::{Exit, Notification},
    request::{Initialize, Request},
    ClientCapabilities, ServerCapabilities,
};

use crate::rpc::{self, Message, RpcError};

#[derive(Debug)]
pub enum LspError {
    Process(io::Error),
    QueueDisconnected,
    InvalidResponse
}

pub struct LspChannel {
    lang_id: String,
    pid: u32,
    rpc_client: rpc::Client<LspMessage>,

    next_id: AtomicU64,
}

impl LspChannel {
    pub fn new(lang_id: String, command: String, args: Vec<String>) -> Result<Self, LspError> {
        log::debug!(
            "Create new LspChannel with lang_id: {}, command: {}, args: {:?}",
            lang_id,
            command,
            args,
        );
        let child_process = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| LspError::Process(e))?;

        let child_pid = child_process.id();
        let child_stdout = child_process.stdout.unwrap();
        let child_stdin = child_process.stdin.unwrap();

        let client = rpc::Client::<LspMessage>::new(move || child_stdout, move || child_stdin);

        Ok(LspChannel {
            lang_id: lang_id.into(),
            pid: child_pid,
            rpc_client: client,
            next_id: AtomicU64::new(1),
        })
    }

    pub fn send_request(&self, request: RawRequest) -> Result<(), LspError> {
        self.rpc_client
            .sender
            .send(LspMessage::Request(request))
            .map_err(|e| LspError::QueueDisconnected)?;

        Ok(())
    }

    pub fn receiver(&self) -> &Receiver<LspMessage> {
        &self.rpc_client.receiver
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum LspMessage {
    Request(RawRequest),
    Notification(RawNotification),
    Response(RawResponse),
}

impl From<RawRequest> for LspMessage {
    fn from(raw: RawRequest) -> LspMessage {
        LspMessage::Request(raw)
    }
}

impl From<RawNotification> for LspMessage {
    fn from(raw: RawNotification) -> LspMessage {
        LspMessage::Notification(raw)
    }
}

impl From<RawResponse> for LspMessage {
    fn from(raw: RawResponse) -> LspMessage {
        LspMessage::Response(raw)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RawRequest {
    pub id: u64,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RawResponse {
    // JSON RPC allows this to be null if it was impossible
    // to decode the request's id. Ignore this special case
    // and just die horribly.
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RawResponseError>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RawResponseError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Clone, Copy, Debug)]
#[allow(unused)]
pub enum ErrorCode {
    ParseError = -32700,
    InvalidRequest = -32600,
    MethodNotFound = -32601,
    InvalidParams = -32602,
    InternalError = -32603,
    ServerErrorStart = -32099,
    ServerErrorEnd = -32000,
    ServerNotInitialized = -32002,
    UnknownErrorCode = -32001,
    RequestCanceled = -32800,
    ContentModified = -32801,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RawNotification {
    pub method: String,
    pub params: Value,
}
impl Message for LspMessage {
    fn read(r: &mut impl BufRead) -> Result<Option<LspMessage>, RpcError> {
        let text = match read_msg_text(r).map_err(|e| RpcError::Read(e))? {
            None => return Ok(None),
            Some(text) => text,
        };
        let msg = from_str(&text).map_err(|e| RpcError::Deserialize(e.description().into()))?;
        Ok(Some(msg))
    }

    fn write(self, w: &mut impl Write) -> Result<(), RpcError> {
        #[derive(Serialize)]
        struct JsonRpc {
            jsonrpc: &'static str,
            #[serde(flatten)]
            msg: LspMessage,
        }
        let text = to_string(&JsonRpc {
            jsonrpc: "2.0",
            msg: self,
        })
        .map_err(|e| RpcError::Serialize(e.description().into()))?;
        write_msg_text(w, &text)?;
        Ok(())
    }

    fn is_exit(&self) -> bool {
        match self {
            LspMessage::Notification(n) => n.is::<Exit>(),
            _ => false,
        }
    }
}

impl RawRequest {
    pub fn new<R>(id: u64, params: &R::Params) -> RawRequest
    where
        R: Request,
        R::Params: serde::Serialize,
    {
        RawRequest {
            id,
            method: R::METHOD.to_string(),
            params: to_value(params).unwrap(),
        }
    }
    pub fn cast<R>(self) -> ::std::result::Result<(u64, R::Params), RawRequest>
    where
        R: Request,
        R::Params: serde::de::DeserializeOwned,
    {
        if self.method != R::METHOD {
            return Err(self);
        }
        let id = self.id;
        let params: R::Params = from_value(self.params).unwrap();
        Ok((id, params))
    }
}

impl RawResponse {
    pub fn ok<R>(id: u64, result: &R::Result) -> RawResponse
    where
        R: Request,
        R::Result: serde::Serialize,
    {
        RawResponse {
            id,
            result: Some(to_value(&result).unwrap()),
            error: None,
        }
    }
    pub fn err(id: u64, code: i32, message: String) -> RawResponse {
        let error = RawResponseError {
            code,
            message,
            data: None,
        };
        RawResponse {
            id,
            result: None,
            error: Some(error),
        }
    }

    pub fn cast<R>(self) -> ::std::result::Result<R::Result, RawResponse>
    where
        R: Request,
        R::Result: serde::de::DeserializeOwned,
    {
        if let Some(result) = self.result {
            let result: R::Result = from_value(result).unwrap();
            return Ok(result);
        }

        Err(self)
    }
}

impl RawNotification {
    pub fn new<N>(params: &N::Params) -> RawNotification
    where
        N: Notification,
        N::Params: serde::Serialize,
    {
        RawNotification {
            method: N::METHOD.to_string(),
            params: to_value(params).unwrap(),
        }
    }
    pub fn is<N>(&self) -> bool
    where
        N: Notification,
    {
        self.method == N::METHOD
    }
    pub fn cast<N>(self) -> ::std::result::Result<N::Params, RawNotification>
    where
        N: Notification,
        N::Params: serde::de::DeserializeOwned,
    {
        if !self.is::<N>() {
            return Err(self);
        }
        Ok(from_value(self.params).unwrap())
    }
}

fn read_msg_text(inp: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut size = None;
    let mut buf = String::new();
    loop {
        buf.clear();
        let read_count = inp
            .read_line(&mut buf)
            .map_err(|e| e.description().to_owned())?;
        if read_count == 0 {
            return Ok(None);
        }
        if !buf.ends_with("\r\n") {
            Err(format!("malformed header: {:?}", buf))?;
        }
        let buf = &buf[..buf.len() - 2];
        if buf.is_empty() {
            break;
        }
        let mut parts = buf.splitn(2, ": ");
        let header_name = parts.next().unwrap();
        let header_value = parts
            .next()
            .ok_or_else(|| format!("malformed header: {:?}", buf))?;
        if header_name == "Content-Length" {
            size = Some(
                header_value
                    .parse::<usize>()
                    .map_err(|_| "Failed to parse header size".to_owned())?,
            );
        }
    }
    let size = size.ok_or("no Content-Length")?;
    let mut buf = buf.into_bytes();
    buf.resize(size, 0);
    inp.read_exact(&mut buf)
        .map_err(|e| e.description().to_owned())?;
    let buf = String::from_utf8(buf).map_err(|e| e.description().to_owned())?;
    log::debug!("< {}", buf);
    Ok(Some(buf))
}

fn write_msg_text(out: &mut impl Write, msg: &str) -> Result<(), RpcError> {
    log::debug!("> {}", msg);
    write!(out, "Content-Length: {}\r\n\r\n", msg.len())
        .map_err(|e| RpcError::Write(e.description().into()))?;
    out.write_all(msg.as_bytes())
        .map_err(|e| RpcError::Write(e.description().into()))?;
    out.flush()
        .map_err(|e| RpcError::Write(e.description().into()))?;
    Ok(())
}
