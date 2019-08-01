mod handler;

use crossbeam::channel::{Receiver, Select};

use self::handler::{LspChannel, LspMessage, RawRequest, RawNotification, RawResponse};
use lsp_types::{
    request::Initialize,
    ClientCapabilities,
    ServerCapabilities
};

#[derive(Debug)]
pub enum Event {
    Hello,
    StartServer(String, String, Vec<String>),
}

pub trait Editor {
    fn events(&self) -> Receiver<Event>;
    fn capabilities(&self) -> lsp_types::ClientCapabilities;
    fn say_hello(&self) -> Result<(), ()>;
}

pub struct Callback<E: Editor> {
    pub id: u64,
    pub func: Box<FnOnce(&mut E, &mut LspHandler<E>, RawResponse) -> Result<(), String>>
}

pub struct LspHandler<E: Editor> {
    channel: LspChannel,
    callbacks: Vec<Callback<E>>,
    // None if server is not started
    server_capabilities: Option<ServerCapabilities>,
}

impl<E: Editor> LspHandler<E> {
    fn new(channel: LspChannel) -> Self {
        LspHandler {
            channel,
            callbacks: Vec::new(),
            server_capabilities: None
        }
    }

    pub fn initialize(&mut self, capabilities: ClientCapabilities, cb: Box<FnOnce(&mut E, &mut LspHandler<E>, RawResponse) -> Result<(), String>>) -> Result<(), String> {
        log::debug!("Initialize language server with capabilities: {:?}", capabilities);

        let id = self.channel.fetch_id();
        let init_params = lsp_types::InitializeParams {
            process_id: Some(std::process::id() as u64),
            root_path: Some("".into()),
            root_uri: None,
            initialization_options: None,
            capabilities,
            trace: None,
            workspace_folders: None,
        };
        let init_request = RawRequest::new::<Initialize>(1, &init_params);
        self.callbacks.push(Callback { id, func: cb });
        self.request(init_request)
    }

    fn request(&mut self, request: RawRequest) -> Result<(), String> {
        self.channel.send_request(request)
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

fn select<E: Editor>(event_receiver: &Receiver<Event>, handlers: &Vec<LspHandler<E>>) -> SelectedMsg {
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

fn handle_editor_event<E: Editor>(state: &mut Lspc<E>, event: Event) -> Result<(), String> {
    match event {
        Event::Hello => {
            state.editor.say_hello().unwrap();
        }
        Event::StartServer(lang_id, command, args) => {
            let capabilities = state.editor.capabilities();
            let channel = LspChannel::new(lang_id, command, args)?;
            let mut lsp_handler = LspHandler::new(channel);

            lsp_handler.initialize(capabilities, Box::new(move |editor, handler, response| {
                log::debug!("InitializeResponse callback");
                let response = response.cast::<Initialize>().map_err(|e| "Invalid response".to_owned())?;
                let server_capabilities = response.capabilities;
                handler.server_capabilities = Some(server_capabilities);
                Ok(())
            }));

            state.lsp_handlers.push(lsp_handler);
        }
        _ => (),
    }

    Ok(())
}

fn handle_lsp_msg<E: Editor>(
    state: &mut Lspc<E>,
    index: usize,
    msg: LspMessage,
) -> Result<(), String> {
    let lsp_handler = &mut state.lsp_handlers[index];
    match msg {
        LspMessage::Request(req) => {
        }
        LspMessage::Notification(notification) => {
        }
        LspMessage::Response(res) => {
            let cb_index = lsp_handler.callbacks.iter().position(|cb| cb.id == res.id);
            if let Some(index) = cb_index {
                let callback = lsp_handler.callbacks.swap_remove(index);
                (callback.func)(&mut state.editor, lsp_handler, res)?;
            } else {
                log::error!("Unhandled response: {:?}", res);
            }
        }
    }

    Ok(())
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
                SelectedMsg::Editor(event) => {
                    handle_editor_event(&mut self, event)
                }
                SelectedMsg::Lsp(index, msg) => {
                    handle_lsp_msg(&mut self, index, msg)
                }
            };
            if let Err(e) = result {
                log::error!("Handle error: {}", e);
            }
        }
    }
}
