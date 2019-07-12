use std::fmt;
use std::io::{self, Read, Write};
use std::thread::{self, JoinHandle};

use crossbeam;
use crossbeam::channel::{self, Receiver, Sender};
use rmp_serde as rmps;
use rmp_serde::{Deserializer, Serializer};
use rmpv::Value;
use serde::{
    self,
    de::{self, SeqAccess, Visitor},
    ser::SerializeSeq,
    Deserialize, Serialize,
};

use std::error::Error;

#[derive(Debug, PartialEq, Clone)]
pub enum RpcMessage {
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

impl Serialize for RpcMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
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
            RpcNotification {
                method,
                params,
            } => {
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
    type Value = RpcMessage;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("seq (tag, [msgid], method, params)")
    }

    fn visit_seq<V>(self, mut seq: V) -> Result<Self::Value, V::Error>
    where
        V: SeqAccess<'de>,
    {
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
            Err(de::Error::invalid_value(de::Unexpected::Other("invalid tag"), &self))
        }
    }
}

impl<'de> Deserialize<'de> for RpcMessage {
    fn deserialize<D>(deserializer: D) -> Result<RpcMessage, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(RpcVisitor)
    }
}

use RpcMessage::*;

struct Client {
    tx: Sender<RpcMessage>,
    tx_thread: JoinHandle<()>,
    rx_thread: JoinHandle<()>,
    msgid_counter: u64,
}

impl Client {
    fn new<R, W>(mut reader: R, mut writer: W) -> (Self, Receiver<RpcMessage>)
    where
        R: Read + Send + 'static,
        W: Write + Send + 'static,
    {
        let (tx, io_rx): (Sender<RpcMessage>, Receiver<RpcMessage>) = channel::unbounded();
        let (io_tx, rx): (Sender<RpcMessage>, Receiver<RpcMessage>) = channel::unbounded();
        let rx_thread = thread::spawn(move || {
            for msg in io_rx {
                // TODO: Handle err! Log is good
                rmp_serde::encode::write(&mut writer, &msg).unwrap();
                writer.flush().unwrap();
                let mut buf = Vec::new();
                msg.serialize(&mut Serializer::new(&mut buf)).unwrap();
                eprintln!("Sent: {:?}", buf);
            }
        });

        let tx_thread = thread::spawn(move || {
            let mut value_reader = Deserializer::new(reader);
            while let Ok(value) = Deserialize::deserialize(&mut value_reader) {
                eprintln!("Received: {:?}", value);
                io_tx.send(value).unwrap();
            }
        });

        let msgid_counter = 0;

        (Client {
            tx,
            tx_thread,
            rx_thread,
            msgid_counter,
        }, rx)
    }

    fn send_request(&mut self, method: &str, params: Vec<Value>) {
        self.msgid_counter += 1;
        let request = RpcRequest {
            msgid: self.msgid_counter,
            method: method.into(),
            params,
        };
        self.tx.send(request).unwrap();
    }

    fn stop(mut self) {
      self.tx_thread.join().unwrap();
      self.rx_thread.join().unwrap();
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let (mut client, rx) = Client::new(io::stdin(), io::stdout());

    for msg in rx {
        match msg {
            RpcNotification { method, params } => {
                if method == "hello" {
                    client.send_request(
                        "nvim_command",
                        vec!["echo 'hello from the other side'".into()],
                    );
                }
            },
            _ => ()
        }
    }
    Ok(())
}
