pub mod handler;
// Custom LSP types
pub mod msg;
pub mod types;

use std::{
    io,
    path::{Path, PathBuf},
};

use crossbeam::channel::{Receiver, Select};
use lsp_types::{
    request::{HoverRequest, Initialize},
    Position, TextDocumentIdentifier, Hover
};
use serde::{Deserialize, Serialize};
use url::Url;

use self::{
    handler::LangServerHandler,
    msg::LspMessage,
    types::{InlayHint, InlayHints},
};

#[derive(Debug, Serialize, Deserialize)]
pub struct LsConfig {
    pub command: Vec<String>,
    pub root_markers: Vec<String>,
}

#[derive(Debug)]
pub enum Event {
    Hello,
    StartServer {
        lang_id: String,
        config: LsConfig,
        cur_path: String,
    },
    Hover {
        lang_id: String,
        text_document: TextDocumentIdentifier,
        position: Position,
    },
    InlayHints {
        lang_id: String,
        text_document: TextDocumentIdentifier,
    },
}

#[derive(Debug)]
pub enum EditorError {
    Timeout,
    Parse(&'static str),
    CommandDataInvalid(&'static str),
    UnexpectedResponse(String),
    UnexpectedMessage(String),
    RootPathNotFound,
}

impl From<EditorError> for LspcError {
    fn from(e: EditorError) -> Self {
        LspcError::Editor(e)
    }
}

#[derive(Debug)]
pub enum LangServerError {
    Process(io::Error),
    ServerDisconnected,
    InvalidResponse,
}

impl From<LangServerError> for LspcError {
    fn from(lse: LangServerError) -> Self {
        LspcError::LangServer(lse)
    }
}

#[derive(Debug)]
pub enum LspcError {
    Editor(EditorError),
    LangServer(LangServerError),
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
        hover: &Hover,
    ) -> Result<(), EditorError>;
    fn inline_hints(
        &self,
        text_document: TextDocumentIdentifier,
        hints: &Vec<InlayHint>,
    ) -> Result<(), EditorError>;
}

pub struct Lspc<E: Editor> {
    editor: E,
    lsp_handlers: Vec<LangServerHandler<E>>,
}

#[derive(Debug)]
enum SelectedMsg {
    Editor(Event),
    Lsp(usize, LspMessage),
}

fn select<E: Editor>(
    event_receiver: &Receiver<Event>,
    handlers: &Vec<LangServerHandler<E>>,
) -> SelectedMsg {
    let mut sel = Select::new();
    sel.recv(event_receiver);
    for lsp_client in handlers.iter() {
        sel.recv(&lsp_client.receiver());
    }

    let oper = sel.select();
    match oper.index() {
        0 => {
            let nvim_msg = oper.recv(event_receiver).unwrap();
            SelectedMsg::Editor(nvim_msg)
        }
        i => {
            let lsp_msg = oper.recv(handlers[i - 1].receiver()).unwrap();

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
    fn handler_for(&mut self, lang_id: &str) -> Option<&mut LangServerHandler<E>> {
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
                let mut lsp_handler =
                    LangServerHandler::new(lang_id, &config.command[0], &config.command[1..])
                        .map_err(|e| LspcError::LangServer(e))?;
                let cur_path = PathBuf::from(cur_path);
                let root = find_root_path(&cur_path, &config.root_markers)
                    .map(|path| path.to_str())
                    .ok_or_else(|| LspcError::Editor(EditorError::RootPathNotFound))?
                    .ok_or_else(|| LspcError::Editor(EditorError::RootPathNotFound))?;

                let root_url =
                    to_file_url(&root).ok_or(LspcError::Editor(EditorError::RootPathNotFound))?;

                lsp_handler.initialize(
                    root.to_owned(),
                    root_url,
                    capabilities,
                    Box::new(move |editor: &mut E, handler, response| {
                        log::debug!("InitializeResponse callback");
                        let response = response
                            .cast::<Initialize>()
                            .map_err(|_| LspcError::LangServer(LangServerError::InvalidResponse))?;

                        handler.initialize_response(response)?;

                        editor.message("LangServer initialized")?;
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
                    Box::new(move |editor: &mut E, _handler, response| {
                        log::debug!("HoverResponse callback");
                        let response = response
                            .cast::<HoverRequest>()
                            .map_err(|_| LspcError::LangServer(LangServerError::InvalidResponse))?;
                        if let Some(hover) = response {
                            editor.show_hover(text_document_clone, &hover)?;
                        }

                        Ok(())
                    }),
                )?;
            }
            Event::InlayHints {
                lang_id,
                text_document,
            } => {
                let handler = self.handler_for(&lang_id).ok_or(LspcError::NotStarted)?;
                let text_document_clone = text_document.clone();
                handler.inlay_hints_request(
                    text_document,
                    Box::new(move |editor: &mut E, _handler, response| {
                        log::debug!("InlayHintsResponse callback");
                        let hints = response
                            .cast::<InlayHints>()
                            .map_err(|_| LspcError::LangServer(LangServerError::InvalidResponse))?;
                        editor.inline_hints(text_document_clone, &hints)?;

                        Ok(())
                    }),
                )?;
            }
        }

        Ok(())
    }

    fn handle_lsp_msg(&mut self, index: usize, msg: LspMessage) -> Result<(), LspcError> {
        let lsp_handler = &mut self.lsp_handlers[index];
        match msg {
            LspMessage::Request(_req) => {}
            LspMessage::Notification(_notification) => {}
            LspMessage::Response(res) => {
                if let Some(callback) = lsp_handler.callback_for(res.id) {
                    (callback.func)(&mut self.editor, lsp_handler, res)?;
                } else {
                    log::error!("not requested response: {:?}", res);
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
