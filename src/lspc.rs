mod msg;
mod transport;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

use self::msg::{RawMessage, RawRequest};
use self::transport::{piped_process_transport, Threads};

use std::sync::atomic::{Ordering, AtomicU64};

use serde_json::{Value, to_value};

use crossbeam::channel::{ bounded, Receiver, Sender};

struct LspClient {
    name: String,
    command: String,
    sender: Sender<RawMessage>,
    receiver: Receiver<RawMessage>,
    threads: Threads,
    id_counter: AtomicU64,
}

struct LspClients {
    clients: Vec<LspClient>,
}

impl LspClient {
    fn new(name: &str, command: &str, args: Vec<String>) -> Result<Self> {
        let (receiver, sender, threads) = piped_process_transport(command, args)?;

        let capabilities = lsp_types::ClientCapabilities {
            workspace: None,
            text_document: None,
            window: None,
            experimental: None
        };
        let init_params = lsp_types::InitializeParams {
            process_id: Some(std::process::id() as u64),
            root_path: Some("".into()),
            root_uri: None,
            initialization_options: None,
            capabilities,
            trace: None,
            workspace_folders: None
        };

        let init_request = RawRequest {
            id: 1,
            method: "".into(),
            params: to_value(init_params)?
        };
        sender.send(RawMessage::Request(init_request)).unwrap();

        Ok(LspClient {
            name: name.into(),
            command: command.into(),
            receiver,
            sender,
            threads,
            id_counter: AtomicU64::new(2)
        })
    }

    // TODO: wait for response from this method
    fn request(&self, method: String, params: Value) {
        let id = self.id_counter.fetch_add(1, Ordering::Relaxed);
        let request = RawRequest {
            id, method, params
        };

        self.sender.send(RawMessage::Request(request)).unwrap();
    }

}
