pub mod handler;
// Custom LSP types
pub mod msg;
mod tracking_file;
pub mod types;

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crossbeam::channel::{tick, Receiver, Select};
use lsp_types::{
    self as lsp, notification as noti,
    request::{
        Formatting, GotoDefinition, GotoDefinitionResponse, HoverRequest, Initialize, References,
    },
    DocumentFormattingParams, FormattingOptions, Hover, Location, Position, ShowMessageParams,
    TextDocumentIdentifier, TextEdit,
};
use serde::{Deserialize, Serialize};
use url::Url;

use self::{
    handler::{LangServerHandler, LangSettings},
    msg::{LspMessage, RawNotification, RawRequest, RawResponse},
    tracking_file::TrackingFile,
    types::{InlayHint, InlayHints, InlayHintsParams},
};

pub const SYNC_DELAY_MS: u64 = 500;
pub const TIMER_TICK_MS: u64 = 100;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct LsConfig {
    pub command: Vec<String>,
    pub root_markers: Vec<String>,
    #[serde(default)]
    pub indentation: u64,
    #[serde(default)]
    pub indentation_with_space: bool,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Hello,
    StartServer {
        lang_id: String,
        config: LsConfig,
        cur_path: String,
    },
    Hover {
        text_document: TextDocumentIdentifier,
        position: Position,
    },
    GotoDefinition {
        text_document: TextDocumentIdentifier,
        position: Position,
    },
    InlayHints {
        text_document: TextDocumentIdentifier,
    },
    FormatDoc {
        text_document_lines: Vec<String>,
        text_document: TextDocumentIdentifier,
    },
    DidOpen {
        text_document: TextDocumentIdentifier,
    },
    DidChange {
        text_document: TextDocumentIdentifier,
        version: i64,
        content_change: lsp::TextDocumentContentChangeEvent,
    },
    DidClose {
        text_document: TextDocumentIdentifier,
    },
    References {
        text_document: TextDocumentIdentifier,
        position: Position,
        include_declaration: bool,
    },
}

#[derive(Debug)]
pub enum EditorError {
    Timeout,
    Parse(&'static str),
    CommandDataInvalid(&'static str),
    UnexpectedResponse(&'static str),
    UnexpectedMessage(String),
    Failed(String),
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
    InvalidRequest(String),
    InvalidNotification(String),
    InvalidResponse(String),
}

impl From<RawRequest> for LangServerError {
    fn from(r: RawRequest) -> Self {
        LangServerError::InvalidRequest(format!("{:?}", r))
    }
}

impl From<RawNotification> for LangServerError {
    fn from(r: RawNotification) -> Self {
        LangServerError::InvalidNotification(format!("{:?}", r))
    }
}

impl From<RawResponse> for LangServerError {
    fn from(r: RawResponse) -> Self {
        LangServerError::InvalidResponse(format!("{:?}", r))
    }
}

impl From<MainLoopError> for LspcError {
    fn from(e: MainLoopError) -> Self {
        LspcError::MainLoop(e)
    }
}

impl<T> From<T> for LspcError
where
    T: Into<LangServerError>,
{
    fn from(r: T) -> Self {
        LspcError::LangServer(r.into())
    }
}

#[derive(Debug)]
pub enum MainLoopError {
    IgnoredMessage,
}

#[derive(Debug)]
pub enum LspcError {
    Editor(EditorError),
    LangServer(LangServerError),
    MainLoop(MainLoopError),
    // Requested lang_id server is not started
    NotStarted,
}

pub trait BufferId: Eq + std::fmt::Debug + std::hash::Hash + Copy + 'static {}

pub trait Editor: 'static {
    type BufferId: BufferId;

    fn events(&self) -> Receiver<Event>;
    fn capabilities(&self) -> lsp_types::ClientCapabilities;
    fn say_hello(&self) -> Result<(), EditorError>;

    fn init(&mut self) -> Result<(), EditorError>;
    fn message(&mut self, msg: &str) -> Result<(), EditorError>;
    fn show_hover(
        &mut self,
        text_document: &TextDocumentIdentifier,
        hover: &Hover,
    ) -> Result<(), EditorError>;
    fn inline_hints(
        &mut self,
        text_document: &TextDocumentIdentifier,
        hints: &Vec<InlayHint>,
    ) -> Result<(), EditorError>;
    fn show_message(&mut self, show_message_params: &ShowMessageParams) -> Result<(), EditorError>;
    fn show_references(&mut self, locations: &Vec<Location>) -> Result<(), EditorError>;
    fn show_diagnostics(
        &mut self,
        text_document: &TextDocumentIdentifier,
        diagnostics: &[lsp_types::Diagnostic],
    ) -> Result<(), EditorError>;
    fn goto(&mut self, location: &Location) -> Result<(), EditorError>;
    fn apply_edits(&self, lines: &Vec<String>, edits: &Vec<TextEdit>) -> Result<(), EditorError>;
    fn track_all_buffers(&self) -> Result<(), EditorError>;
    fn watch_file_events(
        &mut self,
        text_document: &TextDocumentIdentifier,
    ) -> Result<(), EditorError>;
}

pub struct Lspc<E: Editor> {
    editor: E,
    lsp_handlers: Vec<LangServerHandler<E>>,
    tracking_files: HashMap<Url, TrackingFile>,
    next_handler_id: u64,
}

#[derive(Debug)]
enum SelectedMsg {
    Editor(Event),
    Lsp(usize, LspMessage),
    TimerTick,
}

fn select<E: Editor>(
    event_receiver: &Receiver<Event>,
    timer_tick: &Receiver<Instant>,
    handlers: &Vec<LangServerHandler<E>>,
) -> SelectedMsg {
    let mut sel = Select::new();

    sel.recv(event_receiver);
    sel.recv(timer_tick);

    for lsp_client in handlers.iter() {
        sel.recv(&lsp_client.receiver());
    }

    let oper = sel.select();
    match oper.index() {
        0 => {
            let nvim_msg = oper.recv(event_receiver).unwrap();
            SelectedMsg::Editor(nvim_msg)
        }
        1 => {
            oper.recv(timer_tick).unwrap();
            SelectedMsg::TimerTick
        }
        i => {
            let lsp_msg = oper.recv(handlers[i - 2].receiver()).unwrap();

            SelectedMsg::Lsp(i - 2, lsp_msg)
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

// Get the handler of a file by checking
// if that handler's root is ancestor of `file_path`
fn handler_of<'a, E>(
    handlers: &'a mut Vec<LangServerHandler<E>>,
    file_path: &str,
) -> Option<&'a mut LangServerHandler<E>>
where
    E: Editor,
{
    handlers
        .iter_mut()
        .find(|handler| handler.include_file(file_path))
}

impl<E: Editor> Lspc<E> {
    fn handler_for_file(
        &mut self,
        uri: &Url,
    ) -> Option<(&mut LangServerHandler<E>, &mut TrackingFile, &mut E)> {
        let tracking_file = self.tracking_files.get_mut(uri)?;
        let handler = self
            .lsp_handlers
            .iter_mut()
            .find(|handler| handler.id == tracking_file.handler_id)?;
        Some((handler, tracking_file, &mut self.editor))
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
                let lang_settings = LangSettings {
                    indentation: config.indentation,
                    indentation_with_space: config.indentation_with_space,
                };

                let cur_path = PathBuf::from(cur_path);
                let root = find_root_path(&cur_path, &config.root_markers)
                    .map(|path| path.to_str())
                    .ok_or_else(|| LspcError::Editor(EditorError::RootPathNotFound))?
                    .ok_or_else(|| LspcError::Editor(EditorError::RootPathNotFound))?;

                let root_url =
                    to_file_url(&root).ok_or(LspcError::Editor(EditorError::RootPathNotFound))?;

                self.next_handler_id += 1;
                let mut lsp_handler = LangServerHandler::new(
                    self.next_handler_id,
                    lang_id,
                    &config.command[0],
                    lang_settings,
                    &config.command[1..],
                    root.to_owned(),
                )
                .map_err(|e| LspcError::LangServer(e))?;

                let init_params = lsp_types::InitializeParams {
                    process_id: Some(std::process::id() as u64),
                    root_path: Some(root.into()),
                    root_uri: Some(root_url),
                    initialization_options: None,
                    capabilities,
                    trace: None,
                    workspace_folders: None,
                };
                lsp_handler.lsp_request::<Initialize>(
                    &init_params,
                    Box::new(|editor: &mut E, handler, response| {
                        handler.initialize_response(response)?;

                        editor.message("LangServer initialized")?;
                        editor.track_all_buffers()?;
                        Ok(())
                    }),
                )?;

                self.lsp_handlers.push(lsp_handler);
            }
            Event::Hover {
                text_document,
                position,
            } => {
                let (handler, _, _) =
                    self.handler_for_file(&text_document.uri).ok_or_else(|| {
                        log::info!("Nontracking file: {:?}", text_document);
                        MainLoopError::IgnoredMessage
                    })?;
                let text_document_clone = text_document.clone();
                let params = lsp_types::TextDocumentPositionParams {
                    text_document,
                    position,
                };
                handler.lsp_request::<HoverRequest>(
                    &params,
                    Box::new(move |editor: &mut E, _handler, response| {
                        if let Some(hover) = response {
                            editor.show_hover(&text_document_clone, &hover)?;
                        }
                        Ok(())
                    }),
                )?;
            }
            Event::GotoDefinition {
                text_document,
                position,
            } => {
                let (handler, _, _) =
                    self.handler_for_file(&text_document.uri).ok_or_else(|| {
                        log::info!("Nontracking file: {:?}", text_document);
                        MainLoopError::IgnoredMessage
                    })?;
                let params = lsp_types::TextDocumentPositionParams {
                    text_document,
                    position,
                };
                handler.lsp_request::<GotoDefinition>(
                    &params,
                    Box::new(move |editor: &mut E, _handler, response| {
                        if let Some(definition) = response {
                            match definition {
                                GotoDefinitionResponse::Scalar(location) => {
                                    editor.goto(&location)?;
                                }
                                GotoDefinitionResponse::Array(array) => {
                                    if array.len() == 1 {
                                        editor.goto(&array[0])?;
                                    }
                                }
                                _ => {
                                    // FIXME: support Array & Link
                                }
                            }
                        }

                        Ok(())
                    }),
                )?;
            }
            Event::InlayHints { text_document } => {
                let (handler, _, _) =
                    self.handler_for_file(&text_document.uri).ok_or_else(|| {
                        log::info!("Nontracking file: {:?}", text_document);
                        MainLoopError::IgnoredMessage
                    })?;
                let text_document_clone = text_document.clone();
                let params = InlayHintsParams { text_document };
                handler.lsp_request::<InlayHints>(
                    &params,
                    Box::new(move |editor: &mut E, _handler, response| {
                        editor.inline_hints(&text_document_clone, &response)?;

                        Ok(())
                    }),
                )?;
            }
            Event::FormatDoc {
                text_document_lines,
                text_document,
            } => {
                let (handler, _, _) =
                    self.handler_for_file(&text_document.uri).ok_or_else(|| {
                        log::info!("Nontracking file: {:?}", text_document);
                        MainLoopError::IgnoredMessage
                    })?;
                let options = FormattingOptions {
                    tab_size: handler.lang_settings.indentation,
                    insert_spaces: handler.lang_settings.indentation_with_space,
                    properties: HashMap::new(),
                };
                let params = DocumentFormattingParams {
                    text_document,
                    options,
                };
                handler.lsp_request::<Formatting>(
                    &params,
                    Box::new(move |editor: &mut E, _handler, response| {
                        if let Some(edits) = response {
                            editor.apply_edits(&text_document_lines, &edits)?;
                        }

                        Ok(())
                    }),
                )?;
            }
            Event::References {
                text_document,
                position,
                include_declaration,
            } => {
                let (handler, _, _) =
                    self.handler_for_file(&text_document.uri).ok_or_else(|| {
                        log::info!("Nontracking file: {:?}", text_document);
                        MainLoopError::IgnoredMessage
                    })?;
                let params = lsp::ReferenceParams {
                    text_document_position: lsp::TextDocumentPositionParams {
                        text_document,
                        position,
                    },
                    context: lsp::ReferenceContext {
                        include_declaration,
                    },
                };

                handler.lsp_request::<References>(
                    &params,
                    Box::new(move |editor: &mut E, _handler, response| {
                        if let Some(locations) = response {
                            editor.show_references(&locations)?;
                        }

                        Ok(())
                    }),
                )?;
            }
            Event::DidOpen { text_document } => {
                let file_path = text_document.uri.path();
                let handler = handler_of(&mut self.lsp_handlers, &file_path).ok_or_else(|| {
                    log::info!("Unmanaged file: {:?}", text_document.uri);
                    MainLoopError::IgnoredMessage
                })?;

                self.editor.watch_file_events(&text_document)?;
                self.tracking_files.insert(
                    text_document.uri.clone(),
                    TrackingFile::new(handler.id, text_document.uri, handler.sync_kind()),
                );
            }
            Event::DidChange {
                text_document,
                version,
                content_change,
            } => {
                log::debug!(
                    "Received did change event: {:?}, {}, {:?}",
                    text_document,
                    version,
                    content_change
                );
                let (handler, tracking_file, _) =
                    self.handler_for_file(&text_document.uri).ok_or_else(|| {
                        log::info!(
                            "Received changed event for nontracking file: {:?}",
                            text_document
                        );
                        MainLoopError::IgnoredMessage
                    })?;

                tracking_file.track_change(version, &content_change);

                if !tracking_file.sent_did_open {
                    handler.lsp_notify::<noti::DidOpenTextDocument>(
                        &lsp::DidOpenTextDocumentParams {
                            text_document: lsp::TextDocumentItem {
                                uri: text_document.uri.clone(),
                                language_id: handler.lang_id.clone(),
                                version,
                                text: content_change.text,
                            },
                        },
                    )?;
                    tracking_file.sent_did_open = true;
                } else {
                    tracking_file.delay_sync_in(Duration::from_millis(SYNC_DELAY_MS));
                }
            }
            Event::DidClose { text_document } => {
                let (handler, tracking_file, _) =
                    self.handler_for_file(&text_document.uri).ok_or_else(|| {
                        log::info!(
                            "Received changed event for nontracking file: {:?}",
                            text_document
                        );
                        MainLoopError::IgnoredMessage
                    })?;

                let pending_changes = tracking_file.fetch_pending_changes();
                if let Some(params) = pending_changes {
                    handler.lsp_notify::<noti::DidChangeTextDocument>(&params)?;
                }
                handler.lsp_notify::<noti::DidCloseTextDocument>(
                    &lsp::DidCloseTextDocumentParams {
                        text_document: text_document,
                    },
                )?;
            }
        }

        Ok(())
    }

    fn handle_lsp_msg(&mut self, index: usize, msg: LspMessage) -> Result<(), LspcError> {
        let lsp_handler = &mut self.lsp_handlers[index];
        match msg {
            LspMessage::Request(_req) => {}
            LspMessage::Notification(mut noti) => {
                noti = match noti.cast::<noti::ShowMessage>() {
                    Ok(params) => {
                        self.editor.show_message(&params)?;

                        return Ok(());
                    }
                    Err(noti) => noti,
                };
                noti = match noti.cast::<noti::PublishDiagnostics>() {
                    Ok(params) => {
                        let (_handler, _tracking_file, editor) =
                            self.handler_for_file(&params.uri).ok_or_else(|| {
                                log::info!(
                                    "Received changed event for nontracking file: {:?}",
                                    params.uri
                                );
                                MainLoopError::IgnoredMessage
                            })?;
                        let text_document = TextDocumentIdentifier::new(params.uri);
                        editor.show_diagnostics(&text_document, &params.diagnostics)?;

                        return Ok(());
                    }
                    Err(noti) => noti,
                };

                log::warn!("Not supported notification: {:?}", noti);
            }
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

    fn handle_timer_tick(&mut self) -> Result<(), LspcError> {
        let now = Instant::now();
        let sync_due_files = self
            .tracking_files
            .iter()
            .filter(|(_, buf)| {
                if let Some(instant) = buf.scheduled_sync_at {
                    instant <= now
                } else {
                    false
                }
            })
            .map(|(file_url, _)| file_url)
            .cloned()
            .collect::<Vec<_>>();

        for uri in sync_due_files {
            log::debug!("File changes due: {:?}", uri);
            let (handler, tracking_file, _) = self.handler_for_file(&uri).ok_or_else(|| {
                log::info!("Received changed event for nontracking file: {:?}", uri);
                MainLoopError::IgnoredMessage
            })?;
            let pending_changes = tracking_file.fetch_pending_changes();
            if let Some(params) = pending_changes {
                handler.lsp_notify::<noti::DidChangeTextDocument>(&params)?;
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
            tracking_files: HashMap::new(),
            next_handler_id: 0,
        }
    }

    pub fn main_loop(mut self) {
        let event_receiver = self.editor.events();
        let timer_tick = tick(Duration::from_millis(TIMER_TICK_MS));

        if let Err(e) = self.editor.init() {
            log::error!("Editor initialization error: {:?}", e);
            return;
        }

        loop {
            let selected = select(&event_receiver, &timer_tick, &self.lsp_handlers);
            let result = match selected {
                SelectedMsg::Editor(event) => self.handle_editor_event(event),
                SelectedMsg::Lsp(index, msg) => self.handle_lsp_msg(index, msg),
                SelectedMsg::TimerTick => self.handle_timer_tick(),
            };
            if let Err(e) = result {
                log::error!("Handle error: {:?}", e);
            }
        }
    }
}
