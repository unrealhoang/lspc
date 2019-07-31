use std::{
    error::Error,
    fmt,
    io::{BufRead, Write},
    sync::atomic::{AtomicU64, Ordering},
    thread::{self, JoinHandle},
    time::Duration,
};

use crossbeam::channel::{self, Receiver, Sender};

use rmp_serde::Deserializer;
use rmpv::Value;
use serde::{
    self,
    de::{self, SeqAccess, Visitor},
    ser::SerializeSeq,
    Deserialize, Serialize,
};

use crate::lspc::{Editor, Event};
use crate::rpc::{self, Message, RpcError};

pub struct Neovim {
    rpc_client: rpc::Client<NvimMessage>,
    event_receiver: Receiver<Event>,
    next_id: AtomicU64,
    subscription_sender: Sender<(u64, Sender<NvimMessage>)>,
    thread: JoinHandle<()>,
}

fn to_event(msg: NvimMessage) -> Option<Event> {
    match msg {
        NvimMessage::RpcNotification { ref method, .. } if method == "hello" => Some(Event::Hello),
        _ => None,
    }
}

pub enum RequestError {
    Timeout,
}

impl Neovim {
    pub fn new(rpc_client: rpc::Client<NvimMessage>) -> Self {
        let (event_sender, event_receiver) = channel::unbounded();
        let (subscription_sender, subscription_receiver) =
            channel::bounded::<(u64, Sender<NvimMessage>)>(16);

        let rpc_receiver = rpc_client.receiver.clone();
        let thread = thread::spawn(move || {
            let mut subscriptions = Vec::<(u64, Sender<NvimMessage>)>::new();

            for nvim_msg in rpc_receiver {
                if let NvimMessage::RpcResponse { msgid, .. } = nvim_msg {
                    while let Ok(sub) = subscription_receiver.try_recv() {
                        subscriptions.push(sub);
                    }
                    if let Some(index) = subscriptions.iter().position(|item| item.0 == msgid) {
                        let sub = subscriptions.swap_remove(index);
                        sub.1.send(nvim_msg).unwrap();
                    } else {
                        log::error!("Received non-requested response: {}", msgid);
                    }
                } else {
                    if let Some(event) = to_event(nvim_msg) {
                        event_sender.send(event).unwrap();
                    } else {
                        log::error!("Cannot convert nvim msg to editor event");
                    }
                }
            }
        });

        Neovim {
            next_id: AtomicU64::new(1),
            subscription_sender,
            event_receiver,
            rpc_client,
            thread,
        }
    }

    pub fn request(&self, method: &str, params: Vec<Value>) -> Result<NvimMessage, RequestError> {
        let msgid = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = NvimMessage::RpcRequest {
            msgid,
            method: method.into(),
            params,
        };

        let (response_sender, response_receiver) = channel::bounded::<NvimMessage>(1);
        self.subscription_sender
            .send((msgid, response_sender))
            .unwrap();
        self.rpc_client.sender.send(req).unwrap();

        response_receiver
            .recv_timeout(Duration::from_secs(60))
            .map_err(|_| RequestError::Timeout)
    }

    pub fn receiver(&self) -> &Receiver<NvimMessage> {
        &self.rpc_client.receiver
    }

    pub fn close(self) {
        self.thread.join().unwrap();
    }
}

impl Editor for Neovim {
    fn events(&self) -> Receiver<Event> {
        self.event_receiver.clone()
    }

    fn say_hello(&self) -> Result<(), ()> {
        let params = vec!["echo 'hello from the other side'".into()];
        if let Err(_) = self.request("nvim_command", params) {
            log::error!("Timeout requesting");
        };
        Ok(())
    }
}

impl Message for NvimMessage {
    fn read(r: &mut impl BufRead) -> Result<Option<NvimMessage>, RpcError> {
        let mut deserializer = Deserializer::new(r);
        Ok(Some(Deserialize::deserialize(&mut deserializer).map_err(
            |e| match e {
                rmp_serde::decode::Error::InvalidMarkerRead(_)
                | rmp_serde::decode::Error::InvalidDataRead(_) => {
                    RpcError::Read(e.description().into())
                }
                _ => RpcError::Deserialize(e.description().into()),
            },
        )?))
    }

    fn write(self, w: &mut impl Write) -> Result<(), RpcError> {
        rmp_serde::encode::write(w, &self)
            .map_err(|e| RpcError::Serialize(e.description().into()))?;
        w.flush()
            .map_err(|e| RpcError::Write(e.description().into()))?;
        Ok(())
    }

    fn is_exit(&self) -> bool {
        match self {
            NvimMessage::RpcNotification { method, .. } => method == "exit",
            _ => false,
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum NvimMessage {
    RpcRequest {
        msgid: u64,
        method: String,
        params: Vec<Value>,
    }, // 0
    RpcResponse {
        msgid: u64,
        error: Value,
        result: Value,
    }, // 1
    RpcNotification {
        method: String,
        params: Vec<Value>,
    }, // 2
}

impl Serialize for NvimMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use NvimMessage::*;

        match self {
            RpcRequest {
                msgid,
                method,
                params,
            } => {
                let mut seq = serializer.serialize_seq(Some(4))?;
                seq.serialize_element(&Value::from(0))?;
                seq.serialize_element(&Value::from(*msgid))?;
                seq.serialize_element(&Value::from(method.clone()))?;
                seq.serialize_element(&Value::from(params.clone()))?;
                seq.end()
            }
            RpcResponse {
                msgid,
                error,
                result,
            } => {
                let mut seq = serializer.serialize_seq(Some(4))?;
                seq.serialize_element(&Value::from(1))?;
                seq.serialize_element(&Value::from(*msgid))?;
                seq.serialize_element(&Value::from(error.clone()))?;
                seq.serialize_element(&Value::from(result.clone()))?;
                seq.end()
            }
            RpcNotification { method, params } => {
                let mut seq = serializer.serialize_seq(Some(3))?;
                seq.serialize_element(&Value::from(2))?;
                seq.serialize_element(&Value::from(method.clone()))?;
                seq.serialize_element(&Value::from(params.clone()))?;
                seq.end()
            }
        }
    }
}

struct RpcVisitor;

impl<'de> Visitor<'de> for RpcVisitor {
    type Value = NvimMessage;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("seq (tag, [msgid], method, params)")
    }

    fn visit_seq<V>(self, mut seq: V) -> Result<Self::Value, V::Error>
    where
        V: SeqAccess<'de>,
    {
        use NvimMessage::*;

        let tag: i64 = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(0, &self))?;
        if tag == 0 {
            let msgid = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(1, &self))?;
            let method: String = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(2, &self))?;
            let params: Vec<Value> = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(3, &self))?;

            Ok(RpcRequest {
                msgid,
                method,
                params,
            })
        } else if tag == 1 {
            let msgid = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(1, &self))?;
            let error: Value = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(2, &self))?;
            let result: Value = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(3, &self))?;

            Ok(RpcResponse {
                msgid,
                error,
                result,
            })
        } else if tag == 2 {
            let method: String = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(1, &self))?;
            let params: Vec<Value> = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(2, &self))?;

            Ok(RpcNotification { method, params })
        } else {
            Err(de::Error::invalid_value(
                de::Unexpected::Other("invalid tag"),
                &self,
            ))
        }
    }
}

impl<'de> Deserialize<'de> for NvimMessage {
    fn deserialize<D>(deserializer: D) -> Result<NvimMessage, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(RpcVisitor)
    }
}
