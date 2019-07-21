use log::error;
use std::io::{self, Stdin, StdinLock, Stdout, StdoutLock};

use lspc::lspc::{LspMessage, LspClient};
use lspc::neovim::{Neovim, NvimMessage};
use lspc::rpc::Client;
use std::error::Error;

use lazy_static::lazy_static;

use crossbeam::channel::Select;

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

struct State {
    nvim_client: Neovim,
    lsp_clients: Vec<LspClient>,
}

fn handle_editor_msg(state: &mut State, msg: NvimMessage) -> Result<(), ()> {
    match msg {
        NvimMessage::RpcNotification { method, .. } => {
            if method == "hello" {
                state
                    .nvim_client
                    .request(
                        "nvim_command",
                        vec!["echo 'hello from the other side'".into()]
                    )
                    .unwrap();
            }
        }
        _ => (),
    }

    Ok(())
}

fn handle_lsp_msg(state: &mut State, msg: LspMessage) -> Result<(), ()> {
    match msg {
        _ => (),
    };

    Ok(())
}

enum SelectedMsg {
    Nvim(NvimMessage),
    Lsp(u64, LspMessage),
}

fn select(state: &State) -> SelectedMsg {
    let mut sel = Select::new();
    sel.recv(&state.nvim_client.receiver());
    for lsp_client in state.lsp_clients.iter() {
        sel.recv(&lsp_client.receiver());
    }

    let oper = sel.select();
    match oper.index() {
        0 => {
            let nvim_msg = oper.recv(&state.nvim_client.receiver()).unwrap();
            SelectedMsg::Nvim(nvim_msg)
        }
        i => {
            let lsp_msg = oper.recv(&state.lsp_clients[i - 1].receiver()).unwrap();

            SelectedMsg::Lsp((i - 1) as u64, lsp_msg)
        }
    }
}

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let nvim_rpc = Client::<NvimMessage>::new(stdinlock, stdoutlock);
    let lsp_clients = Vec::new();
    let nvim_client = Neovim::new(nvim_rpc);

    let mut lspc_state = State {
        nvim_client,
        lsp_clients,
    };

    loop {
        let selected_msg = select(&lspc_state);
        let result = match selected_msg {
            SelectedMsg::Nvim(msg) => handle_editor_msg(&mut lspc_state, msg),
            SelectedMsg::Lsp(index, msg) => handle_lsp_msg(&mut lspc_state, msg),
        };
        if let Err(_) = result {
            error!("Error when handling msg");
        }
    }

    Ok(())
}
