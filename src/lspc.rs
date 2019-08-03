mod handler;
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crossbeam::channel::{Receiver, Select};
use lsp_types::{
    request::{HoverRequest, Initialize},
    notification::{Initialized},
    ClientCapabilities, Position, ServerCapabilities, TextDocumentIdentifier,
};
use url::Url;

use self::handler::{LspChannel, LspError, LspMessage, RawNotification, RawRequest, RawResponse};
use crate::neovim::Config;

#[derive(Debug)]
pub enum Event {
    Hello,
    StartServer {
        lang_id: String,
        config: Config,
        cur_path: String,
    },
    Hover {
        lang_id: String,
        text_document: TextDocumentIdentifier,
        position: Position,
    },
}

#[derive(Debug)]
pub enum EditorError {
    Timeout,
    Parse(&'static str),
    UnexpectedMessage,
    RootPathNotFound,
}

#[derive(Debug)]
pub enum LspcError {
    Editor(EditorError),
    LangServer(LspError),
    // Requested lang_id server is not started
    NotStarted,
}

pub trait Editor {
    fn events(&self) -> Receiver<Event>;
    fn capabilities(&self) -> lsp_types::ClientCapabilities;
    fn say_hello(&self) -> Result<(), EditorError>;
    fn message(&self, msg: &str) -> Result<(), EditorError>;
    fn show_hover(
        &self,
        text_document: TextDocumentIdentifier,
        range: Option<lsp_types::Range>,
        contents: lsp_types::HoverContents,
    ) -> Result<(), EditorError>;
}

type LspCallback<E> =
    Box<FnOnce(&mut E, &mut LspHandler<E>, RawResponse) -> Result<(), LspcError>>;

pub struct Callback<E: Editor> {
    pub id: u64,
    pub func: LspCallback<E>,
}

pub struct LspHandler<E: Editor> {
    lang_id: String,
    channel: LspChannel,
    callbacks: Vec<Callback<E>>,
    next_id: AtomicU64,
    // None if server is not started
    server_capabilities: Option<ServerCapabilities>,
}

impl<E: Editor> LspHandler<E> {
    fn new(lang_id: String, channel: LspChannel) -> Self {
        LspHandler {
            lang_id,
            channel,
            next_id: AtomicU64::new(1),
            callbacks: Vec::new(),
            server_capabilities: None,
        }
    }

    fn fetch_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn initialize(
        &mut self,
        root: String,
        capabilities: ClientCapabilities,
        cb: LspCallback<E>,
    ) -> Result<(), LspcError> {
        log::debug!(
            "Initialize language server with capabilities: {:?}",
            capabilities
        );

        let id = self.fetch_id();
        let root_url =
            to_file_url(&root).ok_or(LspcError::Editor(EditorError::RootPathNotFound))?;

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

    fn initialized(&mut self) -> Result<(), LspcError> {
        log::debug!("Sending initialized notification");
        let initialized_params = lsp_types::InitializedParams{};
        let initialized_notification = RawNotification::new::<Initialized>(&initialized_params);

        self.notify(initialized_notification)
    }

    fn hover_request(
        &mut self,
        text_document: TextDocumentIdentifier,
        position: Position,
        cb: LspCallback<E>,
    ) -> Result<(), LspcError> {
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

    fn request(&mut self, request: RawRequest) -> Result<(), LspcError> {
        self.channel
            .send_msg(LspMessage::Request(request))
            .map_err(|e| LspcError::LangServer(e))?;

        Ok(())
    }

    fn notify(&mut self, not: RawNotification) -> Result<(), LspcError> {
        self.channel
            .send_msg(LspMessage::Notification(not))
            .map_err(|e| LspcError::LangServer(e))?;

        Ok(())
    }
}

pub struct Lspc<E: Editor> {
    editor: E,
    lsp_handlers: Vec<LspHandler<E>>,
}

#[derive(Debug)]
enum SelectedMsg {
    Editor(Event),
    Lsp(usize, LspMessage),
}

fn select<E: Editor>(
    event_receiver: &Receiver<Event>,
    handlers: &Vec<LspHandler<E>>,
) -> SelectedMsg {
    let mut sel = Select::new();
    sel.recv(event_receiver);
    for lsp_client in handlers.iter() {
        sel.recv(&lsp_client.channel.receiver());
    }

    let oper = sel.select();
    match oper.index() {
        0 => {
            let nvim_msg = oper.recv(event_receiver).unwrap();
            SelectedMsg::Editor(nvim_msg)
        }
        i => {
            let lsp_msg = oper.recv(handlers[i - 1].channel.receiver()).unwrap();

            SelectedMsg::Lsp(i - 1, lsp_msg)
        }
    }
}

fn find_root_path<'a>(mut cur_path: &'a Path, root_marker: &Vec<String>) -> Option<&'a Path> {
    if cur_path.is_file() {
        cur_path = cur_path.parent()?;
    }
    loop {
        if root_marker
            .iter()
            .any(|marker| cur_path.join(marker).exists())
        {
            return Some(cur_path);
        }
        cur_path = cur_path.parent()?;
    }
}

fn to_file_url(s: &str) -> Option<Url> {
    Url::from_file_path(s).ok()
}

impl<E: Editor> Lspc<E> {
    fn handler_for(&mut self, lang_id: &str) -> Option<&mut LspHandler<E>> {
        self.lsp_handlers
            .iter_mut()
            .find(|handler| handler.lang_id == lang_id)
    }

    fn handle_editor_event(&mut self, event: Event) -> Result<(), LspcError> {
        match event {
            Event::Hello => {
                self.editor.say_hello().map_err(|e| LspcError::Editor(e))?;
            }
            Event::StartServer {
                lang_id,
                config,
                cur_path,
            } => {
                let capabilities = self.editor.capabilities();
                let channel = LspChannel::new(lang_id.clone(), &config.command[0], &config.command[1..])
                    .map_err(|e| LspcError::LangServer(e))?;
                let mut lsp_handler = LspHandler::new(lang_id, channel);
                let cur_path = PathBuf::from(cur_path);
                let root = find_root_path(&cur_path, &config.root)
                    .map(|path| path.to_str())
                    .ok_or_else(|| LspcError::Editor(EditorError::RootPathNotFound))?
                    .ok_or_else(|| LspcError::Editor(EditorError::RootPathNotFound))?;

                lsp_handler.initialize(
                    root.to_owned(),
                    capabilities,
                    Box::new(move |editor: &mut E, handler, response| {
                        log::debug!("InitializeResponse callback");
                        let response = response
                            .cast::<Initialize>()
                            .map_err(|_| LspcError::LangServer(LspError::InvalidResponse))?;
                        let server_capabilities = response.capabilities;
                        handler.server_capabilities = Some(server_capabilities);
                        editor
                            .message("LangServer initialized")
                            .map_err(|e| LspcError::Editor(e))?;

                        handler.initialized()?;
                        Ok(())
                    }),
                )?;

                self.lsp_handlers.push(lsp_handler);
            }
            Event::Hover {
                lang_id,
                text_document,
                position,
            } => {
                let handler = self.handler_for(&lang_id).ok_or(LspcError::NotStarted)?;
                let text_document_clone = text_document.clone();
                handler.hover_request(
                    text_document,
                    position,
                    Box::new(move |editor: &mut E, handler, response| {
                        log::debug!("HoverResponse callback");
                        let response = response
                            .cast::<HoverRequest>()
                            .map_err(|_| LspcError::LangServer(LspError::InvalidResponse))?;
                        if let Some(hover) = response {
                            editor
                                .show_hover(text_document_clone, hover.range, hover.contents)
                                .map_err(|e| LspcError::Editor(e))?;
                        }

                        Ok(())
                    }),
                )?;
            }
        }

        Ok(())
    }

    fn handle_lsp_msg(
        &mut self,
        index: usize,
        msg: LspMessage,
    ) -> Result<(), LspcError> {
        let lsp_handler = &mut self.lsp_handlers[index];
        match msg {
            LspMessage::Request(req) => {}
            LspMessage::Notification(notification) => {}
            LspMessage::Response(res) => {
                let cb_index = lsp_handler.callbacks.iter().position(|cb| cb.id == res.id);
                if let Some(index) = cb_index {
                    let callback = lsp_handler.callbacks.swap_remove(index);
                    (callback.func)(&mut self.editor, lsp_handler, res)?;
                } else {
                    log::error!("Unhandled response: {:?}", res);
                }
            }
        }

        Ok(())
    }
}

impl<E: Editor> Lspc<E> {
    pub fn new(editor: E) -> Self {
        Lspc {
            editor,
            lsp_handlers: Vec::new(),
        }
    }

    pub fn main_loop(mut self) {
        let event_receiver = self.editor.events();
        loop {
            let selected = select(&event_receiver, &self.lsp_handlers);
            log::debug!("Received msg: {:?}", selected);
            let result = match selected {
                SelectedMsg::Editor(event) => self.handle_editor_event(event),
                SelectedMsg::Lsp(index, msg) => self.handle_lsp_msg(index, msg),
            };
            if let Err(e) = result {
                log::error!("Handle error: {:?}", e);
            }
        }
    }
}
