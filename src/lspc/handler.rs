use std::{
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use crossbeam::channel::Receiver;
use lsp_types::{
    notification::Initialized,
    request::{HoverRequest, Initialize},
    ClientCapabilities, InitializeResult, Position, ServerCapabilities, TextDocumentIdentifier,
};
use url::Url;

use super::{
    msg::{LspMessage, RawNotification, RawRequest, RawResponse},
    types::{InlayHints, InlayHintsParams},
    Editor, LangServerError, LspcError,
};
use crate::rpc;

type LspCallback<E> =
    Box<FnOnce(&mut E, &mut LangServerHandler<E>, RawResponse) -> Result<(), LspcError>>;

pub struct Callback<E: Editor> {
    pub id: u64,
    pub func: LspCallback<E>,
}

pub struct LangServerHandler<E: Editor> {
    pub lang_id: String,
    rpc_client: rpc::Client<LspMessage>,
    callbacks: Vec<Callback<E>>,
    next_id: AtomicU64,
    // None if server is not started
    server_capabilities: Option<ServerCapabilities>,
}

impl<E: Editor> LangServerHandler<E> {
    pub fn new(
        lang_id: String,
        command: &String,
        args: &[String],
    ) -> Result<Self, LangServerError> {
        let child_process = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| LangServerError::Process(e))?;

        let _child_pid = child_process.id();
        let child_stdout = child_process.stdout.unwrap();
        let child_stdin = child_process.stdin.unwrap();

        let rpc_client = rpc::Client::<LspMessage>::new(move || child_stdout, move || child_stdin);

        Ok(LangServerHandler {
            rpc_client,
            lang_id,
            next_id: AtomicU64::new(1),
            callbacks: Vec::new(),
            server_capabilities: None,
        })
    }

    fn send_msg(&self, msg: LspMessage) -> Result<(), LangServerError> {
        self.rpc_client
            .sender
            .send(msg)
            .map_err(|_| LangServerError::ServerDisconnected)?;

        Ok(())
    }

    pub fn receiver(&self) -> &Receiver<LspMessage> {
        &self.rpc_client.receiver
    }

    fn fetch_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn callback_for(&mut self, id: u64) -> Option<Callback<E>> {
        let cb_index = self.callbacks.iter().position(|cb| cb.id == id);
        if let Some(index) = cb_index {
            let callback = self.callbacks.swap_remove(index);
            Some(callback)
        } else {
            None
        }
    }

    pub fn initialize(
        &mut self,
        root: String,
        root_url: Url,
        capabilities: ClientCapabilities,
        cb: LspCallback<E>,
    ) -> Result<(), LangServerError> {
        log::debug!(
            "Initialize language server with capabilities: {:?}",
            capabilities
        );

        let id = self.fetch_id();

        let init_params = lsp_types::InitializeParams {
            process_id: Some(std::process::id() as u64),
            root_path: Some(root),
            root_uri: Some(root_url),
            initialization_options: None,
            capabilities,
            trace: None,
            workspace_folders: None,
        };
        let init_request = RawRequest::new::<Initialize>(id, &init_params);
        self.callbacks.push(Callback { id, func: cb });
        self.request(init_request)
    }

    pub fn initialize_response(
        &mut self,
        response: InitializeResult,
    ) -> Result<(), LangServerError> {
        let server_capabilities = response.capabilities;
        self.server_capabilities = Some(server_capabilities);

        self.initialized()?;

        Ok(())
    }

    pub fn initialized(&mut self) -> Result<(), LangServerError> {
        log::debug!("Sending initialized notification");
        let initialized_params = lsp_types::InitializedParams {};
        let initialized_notification = RawNotification::new::<Initialized>(&initialized_params);

        self.notify(initialized_notification)
    }

    pub fn hover_request(
        &mut self,
        text_document: TextDocumentIdentifier,
        position: Position,
        cb: LspCallback<E>,
    ) -> Result<(), LangServerError> {
        log::debug!("Send hover request: {:?} at {:?}", text_document, position);

        let id = self.fetch_id();
        let hover_params = lsp_types::TextDocumentPositionParams {
            text_document,
            position,
        };
        let hover_request = RawRequest::new::<HoverRequest>(id, &hover_params);
        self.callbacks.push(Callback { id, func: cb });
        self.request(hover_request)
    }

    pub fn inlay_hints_request(
        &mut self,
        text_document: TextDocumentIdentifier,
        cb: LspCallback<E>,
    ) -> Result<(), LangServerError> {
        log::debug!("Send inlay hints request: {:?}", text_document);

        let id = self.fetch_id();
        let inlay_hints_params = InlayHintsParams { text_document };
        let inlay_hints_request = RawRequest::new::<InlayHints>(id, &inlay_hints_params);
        self.callbacks.push(Callback { id, func: cb });
        self.request(inlay_hints_request)
    }

    fn request(&mut self, request: RawRequest) -> Result<(), LangServerError> {
        self.send_msg(LspMessage::Request(request))
    }

    fn notify(&mut self, not: RawNotification) -> Result<(), LangServerError> {
        self.send_msg(LspMessage::Notification(not))
    }
}
