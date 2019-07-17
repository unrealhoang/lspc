use std::io::{self, Stdin, StdinLock, Stdout, StdoutLock};

use lspc::neovim::NvimMsg;
use lspc::rpc::Client;
use std::error::Error;

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

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let client = Client::<NvimMsg>::new(stdinlock, stdoutlock)?;

    let mut msg_id = 1;
    for msg in client.receiver {
        match msg {
            NvimMsg::RpcNotification { method, .. } => {
                if method == "hello" {
                    client.sender.send(NvimMsg::RpcRequest {
                        msgid: msg_id,
                        method: "nvim_command".into(),
                        params: vec!["echo 'hello from the other side'".into()],
                    })?;
                    msg_id += 1;
                }
            }
            _ => (),
        }
    }
    Ok(())
}
