mod lsp_msg;

use std::{
    error::Error,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use serde_json::{to_value, Value};

use crossbeam::channel::Receiver;

use crate::rpc;
pub use lsp_msg::LspMessage;
use lsp_msg::RawRequest;

pub struct LspClient {
    name: String,
    command: String,
    rpc_client: rpc::Client<LspMessage>,

    next_id: AtomicU64,
}

impl LspClient {
    pub fn new(name: &str, command: &str, args: Vec<String>) -> Result<Self, String> {
        let child_process = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn().map_err(|e| format!("Cannot spawn child process: {}", e.description()))?;

        let child_stdout = child_process.stdout.unwrap();
        let child_stdin = child_process.stdin.unwrap();

        let client = rpc::Client::<LspMessage>::new(move || child_stdout, move || child_stdin);

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

        let params = to_value(init_params).map_err(|e| format!("Failed to serialize init params: {}", e.description()))?;
        let init_request = RawRequest {
            id: 1,
            method: "".into(),
            params
        };

        client
            .sender
            .send(LspMessage::Request(init_request))
            .unwrap();

        Ok(LspClient {
            name: name.into(),
            command: command.into(),
            rpc_client: client,
            next_id: AtomicU64::new(2),
        })
    }

    pub fn send_request(&self, method: String, params: Value) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = RawRequest { id, method, params };

        self.rpc_client
            .sender
            .send(LspMessage::Request(request))
            .unwrap();
    }

    pub fn receiver(&self) -> &Receiver<LspMessage> {
        &self.rpc_client.receiver
    }
}
