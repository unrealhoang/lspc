mod lsp_msg;

use std::{
    sync::atomic::{AtomicU64, Ordering},
    process::{Stdio, Command}
};

use serde_json::{to_value, Value};

use crossbeam::channel::{bounded, Receiver, Sender};

use lsp_msg::{LspMessage, RawRequest};
use crate::rpc;

use lazy_static::lazy_static;

use std::io::{self, Stdin, StdinLock};

struct LspClient {
    name: String,
    command: String,
    rpc_client: rpc::Client<LspMessage>,

    id_counter: AtomicU64,
}

struct LspClients {
    clients: Vec<LspClient>,
}


impl LspClient {
    fn new(name: &str, command: &str, args: Vec<String>) -> rpc::Result<Self> {
        let child_process = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;

        let client = rpc::Client::<LspMessage>::new(
            child_process.stdout.unwrap(),
            child_process.stdin.unwrap()
        )?;

        let capabilities = lsp_types::ClientCapabilities {
            workspace: None,
            text_document: None,
            window: None,
            experimental: None,
        };
        let init_params = lsp_types::InitializeParams {
            process_id: Some(std::process::id() as u64),
            root_path: Some("".into()),
            root_uri: None,
            initialization_options: None,
            capabilities,
            trace: None,
            workspace_folders: None,
        };

        let init_request = RawRequest {
            id: 1,
            method: "".into(),
            params: to_value(init_params)?,
        };
        client.sender.send(LspMessage::Request(init_request)).unwrap();

        Ok(LspClient {
            name: name.into(),
            command: command.into(),
            rpc_client: client,
            id_counter: AtomicU64::new(2),
        })
    }

    // TODO: wait for response from this method
    fn request(&self, method: String, params: Value) {
        let id = self.id_counter.fetch_add(1, Ordering::Relaxed);
        let request = RawRequest { id, method, params };

        self.rpc_client.sender.send(LspMessage::Request(request)).unwrap();
    }
}
