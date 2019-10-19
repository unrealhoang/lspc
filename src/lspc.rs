pub mod handler;
// Custom LSP types
pub mod msg;
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
    request::{Formatting, GotoDefinition, GotoDefinitionResponse, HoverRequest, Initialize},
    DocumentFormattingParams, FormattingOptions, Hover, Location, Position, ShowMessageParams,
    TextDocumentIdentifier, TextEdit,
};
use serde::{Deserialize, Serialize};
use url::Url;

use self::{
    handler::{LangServerHandler, LangSettings},
    msg::{LspMessage, RawNotification, RawRequest, RawResponse},
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
pub enum Event<B: BufferId> {
    Hello,
    StartServer {
        lang_id: String,
        config: LsConfig,
        cur_path: String,
    },
    Hover {
        buf_id: B,
        text_document: TextDocumentIdentifier,
        position: Position,
    },
    GotoDefinition {
        buf_id: B,
        text_document: TextDocumentIdentifier,
        position: Position,
    },
    InlayHints {
        buf_id: B,
        text_document: TextDocumentIdentifier,
    },
    FormatDoc {
        buf_id: B,
        text_document_lines: Vec<String>,
        text_document: TextDocumentIdentifier,
    },
    DidOpen {
        buf_id: B,
        text_document: TextDocumentIdentifier,
    },
    DidChange {
        buf_id: B,
        version: i64,
        content_change: lsp::TextDocumentContentChangeEvent,
    },
    DidClose {
        buf_id: B,
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

    fn events(&self) -> Receiver<Event<Self::BufferId>>;
    fn capabilities(&self) -> lsp_types::ClientCapabilities;
    fn say_hello(&self) -> Result<(), EditorError>;
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
    fn goto(&mut self, location: &Location) -> Result<(), EditorError>;
    fn apply_edits(&self, lines: &Vec<String>, edits: &Vec<TextEdit>) -> Result<(), EditorError>;
    fn track_all_buffers(&self) -> Result<(), EditorError>;
    fn watch_file_events(
        &mut self,
        text_document: &TextDocumentIdentifier,
    ) -> Result<(), EditorError>;
}

struct DelayedSync {
    scheduled_at: Option<Instant>,
    sync_content: lsp::DidChangeTextDocumentParams,
}

impl DelayedSync {
    fn new(uri: Url) -> Self {
        DelayedSync {
            scheduled_at: None,
            sync_content: lsp::DidChangeTextDocumentParams {
                text_document: lsp::VersionedTextDocumentIdentifier {
                    uri: uri,
                    version: None,
                },
                content_changes: Vec::new(),
            },
        }
    }

    fn update_sync_content(
        &mut self,
        version: i64,
        content_change: lsp::TextDocumentContentChangeEvent,
    ) {
        self.sync_content.text_document.version = Some(version);
        let last_content_change = self.sync_content.content_changes.iter_mut().last();
        if let Some(last_content_change) = last_content_change {
            if last_content_change.range == content_change.range {
                std::mem::replace(last_content_change, content_change);
            } else {
                self.sync_content.content_changes.push(content_change);
            }
        } else {
            self.sync_content.content_changes.push(content_change);
        }
    }
}

struct TrackingBuffer {
    lang_id: String,
    text_document: TextDocumentIdentifier,
    sent_did_open: bool,
    delayed_sync: DelayedSync,
}

impl TrackingBuffer {
    fn new(lang_id: String, text_document: TextDocumentIdentifier) -> Self {
        TrackingBuffer {
            lang_id,
            delayed_sync: DelayedSync::new(text_document.uri.clone()),
            text_document,
            sent_did_open: false,
        }
    }

    fn sync_pending_changes<E: Editor>(
        &mut self,
        lsp_handler: &mut LangServerHandler<E>,
    ) -> Result<(), LspcError> {
        let mut sync_content = lsp::DidChangeTextDocumentParams {
            text_document: lsp::VersionedTextDocumentIdentifier {
                uri: self.delayed_sync.sync_content.text_document.uri.clone(),
                version: self.delayed_sync.sync_content.text_document.version,
            },
            content_changes: Vec::new(),
        };
        std::mem::swap(&mut self.delayed_sync.sync_content, &mut sync_content);

        if !sync_content.content_changes.is_empty() {
            lsp_handler.lsp_notify::<noti::DidChangeTextDocument>(sync_content)?;
            self.delayed_sync.scheduled_at = None;
        }

        Ok(())
    }

    fn delay_sync_in(
        &mut self,
        duration: Duration,
        version: i64,
        content_change: lsp::TextDocumentContentChangeEvent,
    ) {
        if let None = self.delayed_sync.scheduled_at {
            self.delayed_sync.scheduled_at = Some(Instant::now() + duration);
        }

        self.delayed_sync
            .update_sync_content(version, content_change);
    }
}

pub struct Lspc<E: Editor> {
    editor: E,
    lsp_handlers: Vec<LangServerHandler<E>>,
    tracking_buffers: HashMap<E::BufferId, TrackingBuffer>,
}

#[derive(Debug)]
enum SelectedMsg<B: BufferId> {
    Editor(Event<B>),
    Lsp(usize, LspMessage),
    TimerTick,
}

fn select<E: Editor>(
    event_receiver: &Receiver<Event<E::BufferId>>,
    timer_tick: &Receiver<Instant>,
    handlers: &Vec<LangServerHandler<E>>,
) -> SelectedMsg<E::BufferId> {
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
    fn handler_for_buffer(
        &mut self,
        buf_id: &E::BufferId,
    ) -> Option<(&mut LangServerHandler<E>, &mut TrackingBuffer)> {
        let tracking_buffer = self.tracking_buffers.get_mut(buf_id)?;
        let handler = self
            .lsp_handlers
            .iter_mut()
            .find(|handler| handler.lang_id == tracking_buffer.lang_id)?;
        Some((handler, tracking_buffer))
    }

    fn handle_editor_event(&mut self, event: Event<E::BufferId>) -> Result<(), LspcError> {
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

                let mut lsp_handler = LangServerHandler::new(
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
                    init_params,
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
                buf_id,
                text_document,
                position,
            } => {
                let (handler, _) = self.handler_for_buffer(&buf_id).ok_or_else(|| {
                    log::info!("Nontracking buffer: {:?}", buf_id);
                    MainLoopError::IgnoredMessage
                })?;
                let text_document_clone = text_document.clone();
                let params = lsp_types::TextDocumentPositionParams {
                    text_document,
                    position,
                };
                handler.lsp_request::<HoverRequest>(
                    params,
                    Box::new(move |editor: &mut E, _handler, response| {
                        if let Some(hover) = response {
                            editor.show_hover(&text_document_clone, &hover)?;
                        }
                        Ok(())
                    }),
                )?;
            }
            Event::GotoDefinition {
                buf_id,
                text_document,
                position,
            } => {
                let (handler, _) = self.handler_for_buffer(&buf_id).ok_or_else(|| {
                    log::info!("Nontracking buffer: {:?}", buf_id);
                    MainLoopError::IgnoredMessage
                })?;
                let params = lsp_types::TextDocumentPositionParams {
                    text_document,
                    position,
                };
                handler.lsp_request::<GotoDefinition>(
                    params,
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
            Event::InlayHints {
                buf_id,
                text_document,
            } => {
                let (handler, _) = self.handler_for_buffer(&buf_id).ok_or_else(|| {
                    log::info!("Nontracking buffer: {:?}", buf_id);
                    MainLoopError::IgnoredMessage
                })?;
                let text_document_clone = text_document.clone();
                let params = InlayHintsParams { text_document };
                handler.lsp_request::<InlayHints>(
                    params,
                    Box::new(move |editor: &mut E, _handler, response| {
                        editor.inline_hints(&text_document_clone, &response)?;

                        Ok(())
                    }),
                )?;
            }
            Event::FormatDoc {
                buf_id,
                text_document_lines,
                text_document,
            } => {
                let (handler, _) = self.handler_for_buffer(&buf_id).ok_or_else(|| {
                    log::info!("Nontracking buffer: {:?}", buf_id);
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
                    params,
                    Box::new(move |editor: &mut E, _handler, response| {
                        if let Some(edits) = response {
                            editor.apply_edits(&text_document_lines, &edits)?;
                        }

                        Ok(())
                    }),
                )?;
            }
            Event::DidOpen {
                buf_id,
                text_document,
            } => {
                let file_path = text_document.uri.path();
                let handler = handler_of(&mut self.lsp_handlers, &file_path).ok_or_else(|| {
                    log::info!("Unmanaged file: {:?}", text_document.uri);
                    MainLoopError::IgnoredMessage
                })?;

                self.editor.watch_file_events(&text_document)?;
                self.tracking_buffers.insert(
                    buf_id,
                    TrackingBuffer::new(handler.lang_id.clone(), text_document.clone()),
                );
            }
            Event::DidChange {
                buf_id,
                version,
                content_change,
            } => {
                let (handler, tracking_buf) =
                    self.handler_for_buffer(&buf_id).ok_or_else(|| {
                        log::info!(
                            "Received changed event for nontracking buffer: {:?}",
                            buf_id
                        );
                        MainLoopError::IgnoredMessage
                    })?;

                if !tracking_buf.sent_did_open {
                    handler.lsp_notify::<noti::DidOpenTextDocument>(
                        lsp::DidOpenTextDocumentParams {
                            text_document: lsp::TextDocumentItem {
                                uri: tracking_buf.text_document.uri.clone(),
                                language_id: tracking_buf.lang_id.clone(),
                                version,
                                text: content_change.text,
                            },
                        },
                    )?;
                    tracking_buf.sent_did_open = true;
                } else {
                    tracking_buf.delay_sync_in(
                        Duration::from_millis(SYNC_DELAY_MS),
                        version,
                        content_change,
                    );
                }
            }
            Event::DidClose { buf_id } => {
                let (handler, tracking_buf) =
                    self.handler_for_buffer(&buf_id).ok_or_else(|| {
                        log::info!(
                            "Received changed event for nontracking buffer: {:?}",
                            buf_id
                        );
                        MainLoopError::IgnoredMessage
                    })?;

                tracking_buf.sync_pending_changes(handler)?;
                handler.lsp_notify::<noti::DidCloseTextDocument>(
                    lsp::DidCloseTextDocumentParams {
                        text_document: tracking_buf.text_document.clone(),
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
        let sync_due_buffers = self
            .tracking_buffers
            .iter()
            .filter(|(_, buf)| {
                if let Some(instant) = buf.delayed_sync.scheduled_at {
                    instant <= now
                } else {
                    false
                }
            })
            .map(|(buf_id, _)| buf_id)
            .copied()
            .collect::<Vec<_>>();

        for buf_id in sync_due_buffers {
            log::debug!("Buffer changes due: {:?}", buf_id);
            let (handler, tracking_buf) = self.handler_for_buffer(&buf_id).ok_or_else(|| {
                log::info!(
                    "Received changed event for nontracking buffer: {:?}",
                    buf_id
                );
                MainLoopError::IgnoredMessage
            })?;
            tracking_buf.sync_pending_changes(handler)?;
        }
        Ok(())
    }
}

impl<E: Editor> Lspc<E> {
    pub fn new(editor: E) -> Self {
        Lspc {
            editor,
            lsp_handlers: Vec::new(),
            tracking_buffers: HashMap::new(),
        }
    }

    pub fn main_loop(mut self) {
        let event_receiver = self.editor.events();
        let timer_tick = tick(Duration::from_millis(TIMER_TICK_MS));

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
