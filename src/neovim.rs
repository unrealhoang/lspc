use std::{
    error::Error,
    fmt,
    io::{BufRead, Write},
    sync::atomic::{AtomicU64, Ordering},
    thread::{self, JoinHandle},
    time::Duration,
};

use crossbeam::channel::{self, Receiver, Sender};

use lsp_types::{
    GotoCapability, Hover, HoverCapability, HoverContents, Location, MarkedString, MarkupContent,
    MarkupKind, Position, ShowMessageParams, TextDocumentClientCapabilities,
    TextDocumentIdentifier, TextEdit,
};
use rmp_serde::Deserializer;
use rmpv::Value;
use serde::{
    self,
    de::{self, SeqAccess, Visitor},
    ser::SerializeSeq,
    Deserialize, Serialize,
};
use url::Url;

use crate::lspc::{types::InlayHint, Editor, EditorError, Event, LsConfig};
use crate::rpc::{self, Message, RpcError};

pub struct Neovim {
    rpc_client: rpc::Client<NvimMessage>,
    event_receiver: Receiver<Event>,
    next_id: AtomicU64,
    subscription_sender: Sender<(u64, Sender<NvimMessage>)>,
    thread: JoinHandle<()>,
}

pub trait ToDisplay {
    fn to_display(&self) -> Vec<String>;
    fn vim_filetype(&self) -> Option<String> {
        None
    }
}

impl ToDisplay for MarkedString {
    fn to_display(&self) -> Vec<String> {
        let s = match self {
            MarkedString::String(ref s) => s,
            MarkedString::LanguageString(ref ls) => &ls.value,
        };
        s.lines().map(String::from).collect()
    }

    fn vim_filetype(&self) -> Option<String> {
        match self {
            MarkedString::String(_) => Some("markdown".to_string()),
            MarkedString::LanguageString(ref ls) => Some(ls.language.clone()),
        }
    }
}

impl ToDisplay for MarkupContent {
    fn to_display(&self) -> Vec<String> {
        self.value.lines().map(str::to_string).collect()
    }

    fn vim_filetype(&self) -> Option<String> {
        match self.kind {
            MarkupKind::Markdown => Some("markdown".to_string()),
            MarkupKind::PlainText => Some("text".to_string()),
        }
    }
}

impl ToDisplay for Hover {
    fn to_display(&self) -> Vec<String> {
        match self.contents {
            HoverContents::Scalar(ref ms) => ms.to_display(),
            HoverContents::Array(ref arr) => arr
                .iter()
                .flat_map(|ms| {
                    if let MarkedString::LanguageString(ref ls) = ms {
                        let mut buf = Vec::new();

                        buf.push(format!("```{}", ls.language));
                        buf.extend(ls.value.lines().map(String::from));
                        buf.push("```".to_string());

                        buf
                    } else {
                        ms.to_display()
                    }
                })
                .collect(),
            HoverContents::Markup(ref mc) => mc.to_display(),
        }
    }

    fn vim_filetype(&self) -> Option<String> {
        match self.contents {
            HoverContents::Scalar(ref ms) => ms.vim_filetype(),
            HoverContents::Array(_) => Some("markdown".to_string()),
            HoverContents::Markup(ref mc) => mc.vim_filetype(),
        }
    }
}

impl ToDisplay for str {
    fn to_display(&self) -> Vec<String> {
        self.lines().map(String::from).collect()
    }
}

// Todo: cut down these parsing logic by implement Deserializer for Value
pub fn from_value(config_value: &Value) -> Option<LsConfig> {
    let mut root_markers = None;
    let mut command = None;
    let mut indentation = 4;
    for (k, v) in config_value.as_map()?.iter().filter_map(|(key, value)| {
        let k = key.as_str()?;
        Some((k, value))
    }) {
        if k == "command" {
            let data = v
                .as_array()?
                .iter()
                .filter_map(|item| item.as_str())
                .map(|s| String::from(s))
                .collect::<Vec<String>>();

            if data.len() > 1 {
                command = Some(data);
            }
        } else if k == "root_markers" {
            let data = v
                .as_array()?
                .iter()
                .filter_map(|item| item.as_str())
                .map(|s| String::from(s))
                .collect::<Vec<String>>();
            root_markers = Some(data);
        } else if k == "indentation" {
            indentation = v.as_u64()?;
        }
    }
    if let (Some(root_markers), Some(command)) = (root_markers, command) {
        Some(LsConfig {
            root_markers,
            command,
            indentation
        })
    } else {
        None
    }
}

fn to_document_offset(lines: &Vec<String>, pos: Position) -> usize {
    lines[..pos.line as usize]
        .iter()
        .map(String::len)
        .fold(0, |acc, current| acc + current + 1)
        + pos.character as usize
}

fn to_text_document(s: &str) -> Option<TextDocumentIdentifier> {
    let uri = Url::from_file_path(s).ok()?;
    Some(TextDocumentIdentifier::new(uri))
}

fn to_position(s: &Vec<(Value, Value)>) -> Option<Position> {
    let mut line = None;
    let mut character = None;

    for (k, v) in s.iter().filter_map(|(key, value)| {
        let k = key.as_str()?;
        Some((k, value))
    }) {
        if k == "line" {
            let data = v.as_u64()?;
            line = Some(data);
        } else if k == "character" {
            let data = v.as_u64()?;
            character = Some(data);
        }
    }
    if let (Some(line), Some(character)) = (line, character) {
        Some(Position::new(line, character))
    } else {
        None
    }
}

fn to_event(msg: NvimMessage) -> Result<Event, EditorError> {
    log::debug!("Trying to convert msg: {:?} to event", msg);
    match msg {
        NvimMessage::RpcNotification { ref method, .. } if method == "hello" => Ok(Event::Hello),
        NvimMessage::RpcNotification {
            ref method,
            ref params,
        } if method == "start_lang_server" => {
            if params.len() < 3 {
                Err(EditorError::Parse(
                    "Wrong amount of params for start_lang_server",
                ))
            } else {
                let lang_id = params[0]
                    .as_str()
                    .ok_or(EditorError::Parse(
                        "Invalid lang_id param for start_lang_server",
                    ))?
                    .to_owned();
                let config =
                    from_value(&params[1]).ok_or(EditorError::Parse("Failed to parse Config"))?;
                let cur_path = params[2]
                    .as_str()
                    .ok_or(EditorError::Parse(
                        "Invalid path param for start_lang_server",
                    ))?
                    .to_owned();
                Ok(Event::StartServer {
                    lang_id,
                    config,
                    cur_path,
                })
            }
        }
        NvimMessage::RpcNotification {
            ref method,
            ref params,
        } if method == "hover" => {
            if params.len() < 3 {
                Err(EditorError::Parse("Wrong amount of params for hover"))
            } else {
                let lang_id = params[0]
                    .as_str()
                    .ok_or(EditorError::Parse("Invalid lang_id param for hover"))?
                    .to_owned();
                let text_document_str = params[1]
                    .as_str()
                    .ok_or(EditorError::Parse("Invalid text_document param for hover"))?;
                let text_document = to_text_document(text_document_str).ok_or(
                    EditorError::Parse("Can't parse text_document param for hover"),
                )?;
                let position_map = params[2]
                    .as_map()
                    .ok_or(EditorError::Parse("Invalid position param for hover"))?;
                let position = to_position(position_map)
                    .ok_or(EditorError::Parse("Can't parse position param for hover"))?;

                Ok(Event::Hover {
                    lang_id,
                    text_document,
                    position,
                })
            }
        }
        NvimMessage::RpcNotification {
            ref method,
            ref params,
        } if method == "goto_definition" => {
            if params.len() < 3 {
                Err(EditorError::Parse("Wrong amount of params for hover"))
            } else {
                let lang_id = params[0]
                    .as_str()
                    .ok_or(EditorError::Parse("Invalid lang_id param for hover"))?
                    .to_owned();
                let text_document_str = params[1]
                    .as_str()
                    .ok_or(EditorError::Parse("Invalid text_document param for hover"))?;
                let text_document = to_text_document(text_document_str).ok_or(
                    EditorError::Parse("Can't parse text_document param for hover"),
                )?;
                let position_map = params[2]
                    .as_map()
                    .ok_or(EditorError::Parse("Invalid position param for hover"))?;
                let position = to_position(position_map)
                    .ok_or(EditorError::Parse("Can't parse position param for hover"))?;

                Ok(Event::GotoDefinition {
                    lang_id,
                    text_document,
                    position,
                })
            }
        }
        NvimMessage::RpcNotification {
            ref method,
            ref params,
        } if method == "inlay_hints" => {
            if params.len() < 2 {
                Err(EditorError::Parse("Wrong amount of params for hover"))
            } else {
                let lang_id = params[0]
                    .as_str()
                    .ok_or(EditorError::Parse("Invalid lang_id param for hover"))?
                    .to_owned();
                let text_document_str = params[1]
                    .as_str()
                    .ok_or(EditorError::Parse("Invalid text_document param for hover"))?;
                let text_document = to_text_document(text_document_str).ok_or(
                    EditorError::Parse("Can't parse text_document param for hover"),
                )?;

                Ok(Event::InlayHints {
                    lang_id,
                    text_document,
                })
            }
        }
        NvimMessage::RpcNotification {
            ref method,
            ref params,
        } if method == "format_doc" => {
            if params.len() < 1 {
                Err(EditorError::Parse(
                    "Wrong amount of params for format document",
                ))
            } else {
                let lang_id = params[0]
                    .as_str()
                    .ok_or(EditorError::Parse(
                        "Invalid lang_id param for format document",
                    ))?
                    .to_owned();
                
                let text_document_str = params[1].as_str().ok_or(EditorError::Parse(
                    "Invalid text_document param for format document",
                ))?;
                let text_document = to_text_document(text_document_str).ok_or(
                    EditorError::Parse("Can't parse text_document param for format document"),
                )?;

                let text_document_lines: Vec<String> = params[2]
                    .as_array()
                    .ok_or(EditorError::Parse(
                        "Invalid text_document_lines param for format document",
                    ))?
                    .into_iter()
                    .map(|line| line.as_str().unwrap().to_owned())
                    .collect();

                Ok(Event::FormatDoc {
                    lang_id,
                    text_document,
                    text_document_lines,
                })
            }
        }
        _ => Err(EditorError::UnexpectedMessage(format!("{:?}", msg))),
    }
}

impl Neovim {
    pub fn new(rpc_client: rpc::Client<NvimMessage>) -> Self {
        let (event_sender, event_receiver) = channel::unbounded();
        let (subscription_sender, subscription_receiver) =
            channel::bounded::<(u64, Sender<NvimMessage>)>(16);

        let rpc_receiver = rpc_client.receiver.clone();
        let thread = thread::spawn(move || {
            let mut subscriptions = Vec::<(u64, Sender<NvimMessage>)>::new();

            for nvim_msg in rpc_receiver {
                if let NvimMessage::RpcResponse { msgid, .. } = nvim_msg {
                    while let Ok(sub) = subscription_receiver.try_recv() {
                        subscriptions.push(sub);
                    }
                    if let Some(index) = subscriptions.iter().position(|item| item.0 == msgid) {
                        let sub = subscriptions.swap_remove(index);
                        sub.1.send(nvim_msg).unwrap();
                    } else {
                        log::error!("Received non-requested response: {}", msgid);
                    }
                } else {
                    match to_event(nvim_msg) {
                        Ok(event) => event_sender.send(event).unwrap(),
                        Err(e) => log::error!("Cannot convert nvim msg to editor event: {:?}", e),
                    }
                }
            }
        });

        Neovim {
            next_id: AtomicU64::new(1),
            subscription_sender,
            event_receiver,
            rpc_client,
            thread,
        }
    }

    pub fn request(&self, method: &str, params: Vec<Value>) -> Result<NvimMessage, EditorError> {
        let msgid = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = NvimMessage::RpcRequest {
            msgid,
            method: method.into(),
            params,
        };

        let (response_sender, response_receiver) = channel::bounded::<NvimMessage>(1);
        self.subscription_sender
            .send((msgid, response_sender))
            .unwrap();
        self.rpc_client.sender.send(req).unwrap();

        response_receiver
            .recv_timeout(Duration::from_secs(60))
            .map_err(|_| EditorError::Timeout)
    }

    pub fn notify(&self, method: &str, params: Vec<Value>) -> Result<(), EditorError> {
        let noti = NvimMessage::RpcNotification {
            method: method.into(),
            params,
        };
        // FIXME: add RpcQueueFull to EditorError??
        self.rpc_client.sender.send(noti).unwrap();

        Ok(())
    }

    pub fn command(&self, command: &str) -> Result<NvimMessage, EditorError> {
        self.request("nvim_command", vec![command.into()])
    }

    // Call VimL function
    pub fn call_function(&self, func: &str, args: Vec<Value>) -> Result<NvimMessage, EditorError> {
        self.request("nvim_call_function", vec![func.into(), args.into()])
    }

    pub fn create_namespace(&self, ns_name: &str) -> Result<u64, EditorError> {
        let response = self.request("nvim_create_namespace", vec![ns_name.into()])?;
        log::debug!("Create namespace response: {:?}", response);
        if let NvimMessage::RpcResponse { ref result, .. } = response {
            Ok(result
                .as_u64()
                .ok_or(EditorError::UnexpectedResponse(format!("{:?}", response)))?)
        } else {
            Err(EditorError::UnexpectedResponse(format!("{:?}", response)))
        }
    }

    pub fn set_virtual_text(
        &self,
        buffer_id: u64,
        ns_id: u64,
        line: u64,
        chunks: Vec<(&str, &str)>,
    ) -> Result<(), EditorError> {
        let chunks = chunks
            .into_iter()
            .map(|(label, hl_group)| Value::Array(vec![Value::from(label), Value::from(hl_group)]))
            .collect::<Vec<_>>()
            .into();
        self.notify(
            "nvim_buf_set_virtual_text",
            vec![
                buffer_id.into(),
                ns_id.into(),
                line.into(),
                chunks,
                Value::Map(Vec::new()),
            ],
        )?;

        Ok(())
    }

    pub fn receiver(&self) -> &Receiver<NvimMessage> {
        &self.rpc_client.receiver
    }

    pub fn close(self) {
        self.thread.join().unwrap();
    }
}

impl Editor for Neovim {
    fn events(&self) -> Receiver<Event> {
        self.event_receiver.clone()
    }

    fn capabilities(&self) -> lsp_types::ClientCapabilities {
        lsp_types::ClientCapabilities {
            workspace: None,
            text_document: Some(TextDocumentClientCapabilities {
                hover: Some(HoverCapability {
                    dynamic_registration: None,
                    content_format: Some(vec![MarkupKind::PlainText, MarkupKind::Markdown]),
                }),
                definition: Some(GotoCapability {
                    dynamic_registration: None,
                    link_support: None,
                }),
                ..Default::default()
            }),
            window: None,
            experimental: None,
        }
    }

    fn say_hello(&self) -> Result<(), EditorError> {
        let params = vec!["echo 'hello from the other side'".into()];
        self.request("nvim_command", params)
            .map_err(|_| EditorError::Timeout)?;

        Ok(())
    }

    fn message(&self, msg: &str) -> Result<(), EditorError> {
        self.command(&format!("echo '{}'", msg))?;
        Ok(())
    }

    fn show_hover(
        &self,
        _text_document: &TextDocumentIdentifier,
        hover: &Hover,
    ) -> Result<(), EditorError> {
        // FIXME: check current buffer is `text_document`
        let bufname = "__LanguageClient__";
        let filetype = if let Some(ft) = &hover.vim_filetype() {
            ft.as_str().into()
        } else {
            Value::Nil
        };
        let lines = hover
            .to_display()
            .iter()
            .map(|item| Value::from(item.as_str()))
            .collect::<Vec<_>>()
            .into();
        self.call_function(
            "lspc#command#open_hover_preview",
            vec![bufname.into(), lines, filetype],
        )?;

        Ok(())
    }

    fn inline_hints(
        &self,
        text_document: &TextDocumentIdentifier,
        hints: &Vec<InlayHint>,
    ) -> Result<(), EditorError> {
        // FIXME: check current buffer is `text_document`
        let ns_id = self.create_namespace(text_document.uri.path())?;
        for hint in hints {
            self.set_virtual_text(
                0,
                ns_id,
                hint.range.start.line,
                vec![(&hint.label, "error")],
            )?;
        }

        Ok(())
    }

    fn show_message(&self, params: &ShowMessageParams) -> Result<(), EditorError> {
        self.command(&format!("echo '[LS-{:?}] {}'", params.typ, params.message))?;

        Ok(())
    }

    fn goto(&self, location: &Location) -> Result<(), EditorError> {
        let filepath = location
            .uri
            .to_file_path()
            .map_err(|_| EditorError::CommandDataInvalid("Location URI is not file path"))?;
        let filepath = filepath
            .to_str()
            .ok_or(EditorError::CommandDataInvalid("Filepath is not UTF-8"))?;
        self.command(&format!("edit {}", filepath))?;
        let line = location.range.start.line + 1;
        let col = location.range.start.character + 1;
        self.call_function("cursor", vec![line.into(), col.into()])?;

        Ok(())
    }

    fn apply_edits(&self, lines: &Vec<String>, edits: &Vec<TextEdit>) -> Result<(), EditorError> {
        let mut sorted_edits = edits.clone();
        let mut editted_content = lines.join("\n");
        sorted_edits.sort_by_key(|i| (i.range.start.line, i.range.start.character));

        let mut last_modified_offset = editted_content.len();
        for edit in sorted_edits.iter().rev() {
            let start_offset = to_document_offset(&lines, edit.range.start);
            let end_offset = to_document_offset(&lines, edit.range.end);

            if end_offset <= last_modified_offset {
                editted_content = format!(
                    "{}{}{}",
                    &editted_content[..start_offset],
                    edit.new_text,
                    &editted_content[end_offset..]
                );
            } else {
                log::debug!("Overlapping edit!");
            }

            last_modified_offset = start_offset;
        }

        let new_lines: Vec<Value> = editted_content.split("\n").map(|e| e.into()).collect();
        let end_line = if new_lines.len() > lines.len() {
            new_lines.len() - 1
        } else {
            lines.len() - 1
        }; 
        self.call_function(
            "nvim_buf_set_lines",
            vec![
                0.into(), // 0 for current buff
                0.into(),
                end_line.into(),
                false.into(),
                Value::Array(new_lines),
            ],
        )?;
        Ok(())
    }
}

impl Message for NvimMessage {
    fn read(r: &mut impl BufRead) -> Result<Option<NvimMessage>, RpcError> {
        let mut deserializer = Deserializer::new(r);
        Ok(Some(Deserialize::deserialize(&mut deserializer).map_err(
            |e| match e {
                rmp_serde::decode::Error::InvalidMarkerRead(_)
                | rmp_serde::decode::Error::InvalidDataRead(_) => {
                    RpcError::Read(e.description().into())
                }
                _ => RpcError::Deserialize(e.description().into()),
            },
        )?))
    }

    fn write(self, w: &mut impl Write) -> Result<(), RpcError> {
        rmp_serde::encode::write(w, &self)
            .map_err(|e| RpcError::Serialize(e.description().into()))?;
        w.flush()
            .map_err(|e| RpcError::Write(e.description().into()))?;
        Ok(())
    }

    fn is_exit(&self) -> bool {
        match self {
            NvimMessage::RpcNotification { method, .. } => method == "exit",
            _ => false,
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum NvimMessage {
    RpcRequest {
        msgid: u64,
        method: String,
        params: Vec<Value>,
    }, // 0
    RpcResponse {
        msgid: u64,
        error: Value,
        result: Value,
    }, // 1
    RpcNotification {
        method: String,
        params: Vec<Value>,
    }, // 2
}

impl Serialize for NvimMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use NvimMessage::*;

        match self {
            RpcRequest {
                msgid,
                method,
                params,
            } => {
                let mut seq = serializer.serialize_seq(Some(4))?;
                seq.serialize_element(&Value::from(0))?;
                seq.serialize_element(&Value::from(*msgid))?;
                seq.serialize_element(&Value::from(method.clone()))?;
                seq.serialize_element(&Value::from(params.clone()))?;
                seq.end()
            }
            RpcResponse {
                msgid,
                error,
                result,
            } => {
                let mut seq = serializer.serialize_seq(Some(4))?;
                seq.serialize_element(&Value::from(1))?;
                seq.serialize_element(&Value::from(*msgid))?;
                seq.serialize_element(&Value::from(error.clone()))?;
                seq.serialize_element(&Value::from(result.clone()))?;
                seq.end()
            }
            RpcNotification { method, params } => {
                let mut seq = serializer.serialize_seq(Some(3))?;
                seq.serialize_element(&Value::from(2))?;
                seq.serialize_element(&Value::from(method.clone()))?;
                seq.serialize_element(&Value::from(params.clone()))?;
                seq.end()
            }
        }
    }
}

struct RpcVisitor;

impl<'de> Visitor<'de> for RpcVisitor {
    type Value = NvimMessage;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("seq (tag, [msgid], method, params)")
    }

    fn visit_seq<V>(self, mut seq: V) -> Result<Self::Value, V::Error>
    where
        V: SeqAccess<'de>,
    {
        use NvimMessage::*;

        let tag: i64 = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(0, &self))?;
        if tag == 0 {
            let msgid = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(1, &self))?;
            let method: String = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(2, &self))?;
            let params: Vec<Value> = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(3, &self))?;

            Ok(RpcRequest {
                msgid,
                method,
                params,
            })
        } else if tag == 1 {
            let msgid = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(1, &self))?;
            let error: Value = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(2, &self))?;
            let result: Value = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(3, &self))?;

            Ok(RpcResponse {
                msgid,
                error,
                result,
            })
        } else if tag == 2 {
            let method: String = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(1, &self))?;
            let params: Vec<Value> = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(2, &self))?;

            Ok(RpcNotification { method, params })
        } else {
            Err(de::Error::invalid_value(
                de::Unexpected::Other("invalid tag"),
                &self,
            ))
        }
    }
}

impl<'de> Deserialize<'de> for NvimMessage {
    fn deserialize<D>(deserializer: D) -> Result<NvimMessage, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(RpcVisitor)
    }
}

