use std::fmt;
use std::io::{self, Read, Write, Stdin, StdinLock, Stdout, StdoutLock};
use std::thread::{self, JoinHandle};

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
use lspc::neovim::NvimMsg;
use lspc::rpc::{Client};

use lazy_static::lazy_static;

lazy_static! {
    static ref STDIN: Stdin = io::stdin();
    static ref STDOUT: Stdout = io::stdout();
}

pub fn stdinlock() -> StdinLock<'static> {
    STDIN.lock()
}

pub fn stdoutlock() -> StdoutLock<'static> {
    STDOUT.lock()
}

fn main() -> Result<(), Box<dyn Error>> {
    let stdin = stdinlock();
    let stdout = stdoutlock();
    let client = Client::<NvimMsg>::new(stdin, stdout)?;

    for msg in rx {
        match msg {
            RpcNotification { method, params } => {
                if method == "hello" {
                    client.send_request(
                        "nvim_command",
                        vec!["echo 'hello from the other side'".into()],
                    );
                }
            }
            _ => (),
        }
    }
    Ok(())
}
