use std::{
    error::Error,
    fmt,
    io::{BufRead, Write},
    sync::atomic::{AtomicU64, Ordering},
};

use crossbeam::channel::Receiver;

use rmp_serde::Deserializer;
use rmpv::Value;
use serde::{
    self,
    de::{self, SeqAccess, Visitor},
    ser::SerializeSeq,
    Deserialize, Serialize,
};

use crate::rpc::{self, RpcError, Message};

pub struct Neovim {
    rpc_client: rpc::Client<NvimMessage>,
    next_id: AtomicU64,
}

impl Neovim {
    pub fn new(rpc_client: rpc::Client<NvimMessage>) -> Self {
        Neovim {
            next_id: AtomicU64::new(1),
            rpc_client,
        }
    }

    pub fn request(&self, method: &str, params: Vec<Value>) -> Result<NvimMessage, RpcError> {
        let msgid = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = NvimMessage::RpcRequest {
            msgid,
            method: method.into(),
            params,
        };
        self.rpc_client.request(req)
    }

    pub fn receiver(&self) -> &Receiver<NvimMessage> {
        &self.rpc_client.receiver
    }
}

impl Message for NvimMessage {
    type Id = u64;

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

    fn is_response(&self) -> bool {
        match self {
            NvimMessage::RpcResponse { .. } => true,
            _ => false,
        }
    }

    fn id(&self) -> Option<u64> {
        match self {
            NvimMessage::RpcRequest { msgid, .. } | NvimMessage::RpcResponse { msgid, .. } => {
                Some(*msgid)
            }
            NvimMessage::RpcNotification { .. } => None,
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
