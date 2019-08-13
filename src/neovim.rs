use std::{
    collections::HashMap,
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
use rmpv::{
    decode::read_value,
    encode::write_value,
    ext::{from_value, to_value},
    Value,
};
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
    // Map from bufnr to LangId
    buf_mapper: HashMap<u64, String>,
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

fn apply_edits(lines: &Vec<String>, edits: &Vec<TextEdit>) -> String {
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
    editted_content
}

fn to_document_offset(lines: &Vec<String>, pos: Position) -> usize {
    lines[..pos.line as usize]
        .iter()
        .map(String::len)
        .fold(0, |acc, current| acc + current + 1)
        + pos.character as usize
}

fn text_document_from_path_str<'de, D>(deserializer: D) -> Result<TextDocumentIdentifier, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: String = Deserialize::deserialize(deserializer)?;
    let uri = Url::from_file_path(s)
        .map_err(|_| <D::Error as de::Error>::custom("could not convert path to URI"))?;

    Ok(TextDocumentIdentifier::new(uri))
}

fn to_event(msg: NvimMessage) -> Result<Event, EditorError> {
    log::debug!("Trying to convert msg: {:?} to event", msg);
    match msg {
        NvimMessage::RpcNotification { method, params } => {
            if method == "hello" {
                Ok(Event::Hello)
            } else if method == "start_lang_server" {
                #[derive(Deserialize)]
                struct StartLangServerParams(String, LsConfig, String);

                let start_lang_params: StartLangServerParams = Deserialize::deserialize(params)
                    .map_err(|_e| EditorError::Parse("failed to parse start lang server params"))?;

                Ok(Event::StartServer {
                    lang_id: start_lang_params.0,
                    config: start_lang_params.1,
                    cur_path: start_lang_params.2,
                })
            } else if method == "hover" {
                #[derive(Deserialize)]
                struct HoverParams(
                    String,
                    #[serde(deserialize_with = "text_document_from_path_str")]
                    TextDocumentIdentifier,
                    Position,
                );

                let hover_params: HoverParams = Deserialize::deserialize(params)
                    .map_err(|_e| EditorError::Parse("failed to parse hover params"))?;

                Ok(Event::Hover {
                    lang_id: hover_params.0,
                    text_document: hover_params.1,
                    position: hover_params.2,
                })
            } else if method == "goto_definition" {
                #[derive(Deserialize)]
                struct GotoDefinitionParams(
                    String,
                    #[serde(deserialize_with = "text_document_from_path_str")]
                    TextDocumentIdentifier,
                    Position,
                );

                let goto_definition_params: GotoDefinitionParams = Deserialize::deserialize(params)
                    .map_err(|_e| EditorError::Parse("failed to parse goto definition params"))?;

                Ok(Event::GotoDefinition {
                    lang_id: goto_definition_params.0,
                    text_document: goto_definition_params.1,
                    position: goto_definition_params.2,
                })
            } else if method == "inlay_hints" {
                #[derive(Deserialize)]
                struct InlayHintsParams(
                    String,
                    #[serde(deserialize_with = "text_document_from_path_str")]
                    TextDocumentIdentifier,
                );

                let inlay_hints_params: InlayHintsParams = Deserialize::deserialize(params)
                    .map_err(|_e| EditorError::Parse("failed to parse inlay hints params"))?;

                Ok(Event::InlayHints {
                    lang_id: inlay_hints_params.0,
                    text_document: inlay_hints_params.1,
                })
            } else if method == "format_doc" {
                #[derive(Deserialize)]
                struct FormatDocParams(
                    String,
                    #[serde(deserialize_with = "text_document_from_path_str")]
                    TextDocumentIdentifier,
                    Vec<String>,
                );

                let format_doc_params: FormatDocParams = Deserialize::deserialize(params)
                    .map_err(|_e| EditorError::Parse("failed to parse goto definition params"))?;

                Ok(Event::FormatDoc {
                    lang_id: format_doc_params.0,
                    text_document: format_doc_params.1,
                    text_document_lines: format_doc_params.2,
                })
            } else if method == "did_open" {
                #[derive(Deserialize)]
                struct DidOpenParams(
                    #[serde(deserialize_with = "text_document_from_path_str")]
                    TextDocumentIdentifier,
                );
                let did_open_params: DidOpenParams = Deserialize::deserialize(params)
                    .map_err(|_e| EditorError::Parse("failed to parse did_open params"))?;

                Ok(Event::DidOpen {
                    text_document: did_open_params.0,
                })
            } else {
                Err(EditorError::UnexpectedMessage(format!(
                    "unexpected notification {:?} {:?}",
                    method, params
                )))
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
            buf_mapper: HashMap::new(),
        }
    }

    // using nvim_call_atomic rpc call
    fn call_atomic(&self, calls: &[Value]) -> Result<Vec<Value>, EditorError> {
        let response = self.request("nvim_call_atomic", calls.into());
        if let NvimMessage::RpcResponse { result, .. } = response? {
            let mut tuple = if let Value::Array(arr) = result {
                arr
            } else {
                return Err(EditorError::UnexpectedResponse("Expected tuple"));
            };
            let error = tuple
                .pop()
                .ok_or(EditorError::UnexpectedResponse("Expect error value"))?;

            if let Some(error) = error.as_array() {
                let error_msg = error
                    .get(2)
                    .ok_or(EditorError::UnexpectedResponse("Expected error message"))?
                    .as_str()
                    .ok_or(EditorError::UnexpectedResponse(
                        "Expected String error message",
                    ))?;

                return Err(EditorError::Failed(error_msg.into()));
            }

            let results = tuple
                .pop()
                .ok_or(EditorError::UnexpectedResponse("Expect result array"))?;
            if let Value::Array(results) = results {
                Ok(results)
            } else {
                Err(EditorError::UnexpectedResponse("Expect result array"))
            }
        } else {
            Err(EditorError::UnexpectedResponse("Expected response"))
        }
    }

    pub fn request(&self, method: &str, params: &[Value]) -> Result<NvimMessage, EditorError> {
        let msgid = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = NvimMessage::RpcRequest {
            msgid,
            method: method.into(),
            params: Value::from(params.to_owned()),
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

    pub fn notify(&self, method: &str, params: &[Value]) -> Result<(), EditorError> {
        let noti = NvimMessage::RpcNotification {
            method: method.into(),
            params: Value::from(params.to_owned()),
        };
        // FIXME: add RpcQueueFull to EditorError??
        self.rpc_client.sender.send(noti).unwrap();

        Ok(())
    }

    pub fn command(&self, command: &str) -> Result<NvimMessage, EditorError> {
        self.request("nvim_command", &vec![command.into()])
    }

    // Call VimL function
    pub fn call_function(&self, func: &str, args: Vec<Value>) -> Result<NvimMessage, EditorError> {
        self.request("nvim_call_function", &vec![func.into(), args.into()])
    }

    pub fn create_namespace(&self, ns_name: &str) -> Result<u64, EditorError> {
        let response = self.request("nvim_create_namespace", &vec![ns_name.into()])?;
        log::debug!("Create namespace response: {:?}", response);
        if let NvimMessage::RpcResponse { ref result, .. } = response {
            Ok(result.as_u64().ok_or(EditorError::UnexpectedResponse(
                "Expected nvim_create_namespace respsonse",
            ))?)
        } else {
            Err(EditorError::UnexpectedResponse(
                "Expected nvim_create_namespace respsonse",
            ))
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
            &vec![
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
        self.request("nvim_command", &params)
            .map_err(|_| EditorError::Timeout)?;

        Ok(())
    }

    fn message(&mut self, msg: &str) -> Result<(), EditorError> {
        self.command(&format!("echo '{}'", msg))?;
        Ok(())
    }

    fn show_hover(
        &mut self,
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
        &mut self,
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

    fn show_message(&mut self, params: &ShowMessageParams) -> Result<(), EditorError> {
        self.command(&format!("echo '[LS-{:?}] {}'", params.typ, params.message))?;

        Ok(())
    }

    fn goto(&mut self, location: &Location) -> Result<(), EditorError> {
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
        let editted_content = apply_edits(lines, edits);
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

    fn watch_file_events(
        &mut self,
        _text_document: &TextDocumentIdentifier,
        lang_id: &str,
    ) -> Result<(), EditorError> {
        // FIXME: check current buffer is `text_document`
        let mut results = self.call_atomic(&[
            vec![
                "nvim_buf_attach".into(),
                Value::Array(vec![
                    0.into(), // Current buffer
                    Value::Boolean(true),
                    Value::Map(Vec::new()),
                ]),
            ]
            .into(),
            vec!["nvim_get_current_buf".into(), Value::Array(Vec::new())].into(),
        ])?;

        if results.len() != 2 {
            Err(EditorError::UnexpectedResponse("Wrong number of response"))
        } else {
            let cur_bufnr = results
                .pop()
                .unwrap()
                .as_u64()
                .ok_or(EditorError::UnexpectedResponse("Expect integer bufnr"))?;

            self.buf_mapper.insert(cur_bufnr, lang_id.to_owned());
            Ok(())
        }
    }
}

impl Message for NvimMessage {
    fn read(r: &mut impl BufRead) -> Result<Option<NvimMessage>, RpcError> {
        let value = read_value(r).map_err(|e| RpcError::Read(e.description().into()))?;
        log::debug!("< Nvim: {:?}", value);
        let inner: NvimMessage =
            from_value(value).map_err(|e| RpcError::Deserialize(e.description().into()))?;
        let r = Some(inner);

        Ok(r)
    }

    fn write(self, w: &mut impl Write) -> Result<(), RpcError> {
        log::debug!("> Nvim: {:?}", self);

        let value = to_value(self).map_err(|e| RpcError::Serialize(e.description().into()))?;
        write_value(w, &value).map_err(|e| RpcError::Write(e.description().into()))?;
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
        params: Value,
    }, // 0
    RpcResponse {
        msgid: u64,
        error: Value,
        result: Value,
    }, // 1
    RpcNotification {
        method: String,
        params: Value,
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
                seq.serialize_element(&0)?;
                seq.serialize_element(&msgid)?;
                seq.serialize_element(&method)?;
                seq.serialize_element(&params)?;
                seq.end()
            }
            RpcResponse {
                msgid,
                error,
                result,
            } => {
                let mut seq = serializer.serialize_seq(Some(4))?;
                seq.serialize_element(&1)?;
                seq.serialize_element(&msgid)?;
                seq.serialize_element(&error)?;
                seq.serialize_element(&result)?;
                seq.end()
            }
            RpcNotification { method, params } => {
                let mut seq = serializer.serialize_seq(Some(3))?;
                seq.serialize_element(&2)?;
                seq.serialize_element(&method)?;
                seq.serialize_element(&params)?;
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
            let params: Value = seq
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
            let params: Value = seq
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

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Position, Range, TextEdit};

    #[test]
    fn test_apply_edits() {
        let original_content = String::from("fn   a() {\n  print!(\"hello\");\n}");
        let lines = original_content
            .split("\n")
            .map(String::from)
            .collect::<Vec<String>>();
        let edits = vec![
            TextEdit::new(
                Range::new(Position::new(0, 3), Position::new(0, 5)),
                String::from(""),
            ),
            TextEdit::new(
                Range::new(Position::new(1, 0), Position::new(1, 0)),
                String::from("  "),
            ),
        ];
        let editted_content = apply_edits(&lines, &edits);
        let expected_content = String::from("fn a() {\n    print!(\"hello\");\n}");
        assert_eq!(editted_content, expected_content);
    }

    #[test]
    fn test_deserialize_ls_config() {
        let value = Value::Map(vec![
            (
                Value::from("root_markers"),
                Value::from(vec![Value::from("Cargo.lock")]),
            ),
            (
                Value::from("command"),
                Value::from(vec![Value::from("rustup"), Value::from("run")]),
            ),
            (Value::from("indentation"), Value::from(4)),
            (Value::from("indentation_with_space"), Value::from(true)),
        ]);

        let ls_config: LsConfig = Deserialize::deserialize(value).unwrap();
        let expected = LsConfig {
            command: vec!["rustup".to_owned(), "run".to_owned()],
            root_markers: vec!["Cargo.lock".to_owned()],
            indentation: 4,
            indentation_with_space: true,
        };

        assert_eq!(expected, ls_config);
    }

    #[test]
    fn test_deserialize_start_lang_server_params() {
        let start_lang_server_msg = NvimMessage::RpcNotification {
            method: String::from("start_lang_server"),
            params: Value::from(vec![
                Value::from("rust"),
                Value::Map(vec![
                    (
                        Value::from("root_markers"),
                        Value::from(vec![Value::from("Cargo.lock")]),
                    ),
                    (
                        Value::from("command"),
                        Value::from(vec![Value::from("rustup")]),
                    ),
                    (Value::from("indentation"), Value::from(4)),
                    (Value::from("indentation_with_space"), Value::from(true)),
                ]),
                Value::from("/abc"),
            ]),
        };
        let expected = Event::StartServer {
            lang_id: String::from("rust"),
            config: LsConfig {
                command: vec![String::from("rustup")],
                root_markers: vec![String::from("Cargo.lock")],
                indentation: 4,
                indentation_with_space: true,
            },
            cur_path: String::from("/abc"),
        };
        assert_eq!(expected, to_event(start_lang_server_msg).unwrap());
    }

    fn to_text_document(s: &str) -> Option<TextDocumentIdentifier> {
        let uri = Url::from_file_path(s).ok()?;
        Some(TextDocumentIdentifier::new(uri))
    }

    #[test]
    fn test_deserialize_inlay_hints_params() {
        let inlay_hints_msg = NvimMessage::RpcNotification {
            method: String::from("inlay_hints"),
            params: Value::from(vec![Value::from("rust"), Value::from("/abc/d.rs")]),
        };
        let text_document = to_text_document("/abc/d.rs").unwrap();
        let expected = Event::InlayHints {
            lang_id: String::from("rust"),
            text_document,
        };

        assert_eq!(expected, to_event(inlay_hints_msg).unwrap());
    }
}
