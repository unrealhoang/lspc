use std::{
    fmt::Debug,
    path::Path,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use crossbeam::channel::Receiver;
use lsp_types::{
    self as lsp,
    notification::{Initialized, Notification},
    request::Request,
    InitializeResult, ServerCapabilities,
};
use serde::{de::DeserializeOwned, Serialize};

use super::{
    msg::{LspMessage, RawNotification, RawRequest, RawResponse},
    Editor, LangServerError, LspcError,
};
use crate::rpc;

pub type RawCallback<E> =
    Box<dyn FnOnce(&mut E, &mut LangServerHandler<E>, RawResponse) -> Result<(), LspcError>>;

pub struct Callback<E: Editor> {
    pub id: u64,
    pub func: RawCallback<E>,
}

pub struct LangSettings {
    pub indentation: u64,
    pub indentation_with_space: bool,
}

pub struct LangServerHandler<E: Editor> {
    pub id: u64,
    pub lang_id: String,
    rpc_client: rpc::Client<LspMessage>,
    callbacks: Vec<Callback<E>>,
    next_id: AtomicU64,
    root_path: String,
    // None if server is not started
    server_capabilities: Option<ServerCapabilities>,
    pub lang_settings: LangSettings,
}

impl<E: Editor> LangServerHandler<E> {
    pub fn new(
        id: u64,
        lang_id: String,
        command: &String,
        lang_settings: LangSettings,
        args: &[String],
        root_path: String,
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
            id,
            rpc_client,
            lang_id,
            next_id: AtomicU64::new(1),
            root_path,
            callbacks: Vec::new(),
            server_capabilities: None,
            lang_settings,
        })
    }

    pub fn include_file(&self, file_path: &str) -> bool {
        let file_path = Path::new(file_path);

        file_path.starts_with(&self.root_path)
    }

    pub fn sync_kind(&self) -> lsp::TextDocumentSyncKind {
        if let Some(ref cap) = self.server_capabilities {
            match cap.text_document_sync {
                Some(lsp::TextDocumentSyncCapability::Kind(kind)) => return kind,
                Some(lsp::TextDocumentSyncCapability::Options(ref opts)) => {
                    if let Some(kind) = opts.change {
                        return kind;
                    }
                }
                _ => {}
            }
        }
        lsp::TextDocumentSyncKind::Full
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

        self.lsp_notify::<Initialized>(&lsp_types::InitializedParams {})
    }

    pub fn lsp_request<R: Request>(
        &mut self,
        params: &R::Params,
        cb: Box<dyn FnOnce(&mut E, &mut LangServerHandler<E>, R::Result) -> Result<(), LspcError>>,
    ) -> Result<(), LangServerError>
    where
        R::Params: Serialize + Debug,
        R::Result: DeserializeOwned + 'static,
        E: 'static,
    {
        log::debug!("Send LSP request: {} with {:?}", R::METHOD, params);

        let id = self.fetch_id();
        let request = RawRequest::new::<R>(id, params);
        let raw_callback: RawCallback<E> =
            Box::new(move |e, handler, raw_response: RawResponse| {
                log::debug!("{} callback", R::METHOD);
                let response = raw_response.cast::<R>()?;
                cb(e, handler, response)
            });
        let func = Box::new(raw_callback);
        self.callbacks.push(Callback { id, func });
        self.request(request)
    }

    fn request(&mut self, request: RawRequest) -> Result<(), LangServerError> {
        self.send_msg(LspMessage::Request(request))
    }

    pub fn lsp_notify<R: Notification>(&mut self, params: &R::Params) -> Result<(), LangServerError>
    where
        R::Params: Serialize + Debug,
    {
        let noti = RawNotification::new::<R>(params);
        self.send_msg(LspMessage::Notification(noti))
    }
}
