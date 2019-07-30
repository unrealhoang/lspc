mod lsp_msg;
mod handler;

use crossbeam::channel::Receiver;

use lspc::handler::LspHandler;
pub use lsp_msg::LspMessage;

pub enum Event {
    Hello
}

pub trait Editor {
    fn events(&self) -> Receiver<Event>;
    fn say_hello(&self) -> Result<(), ()>;
}

pub struct Lspc<E: Editor> {
    editor: E,
    lsp_handlers: Vec<LspHandler>,
}

enum SelectedMsg {
    Editor(Event),
    Lsp(usize, LspMessage),
}

fn select(event_receiver: &Receiver<Event>, handlers: &Vec<LspHandler>) -> SelectedMsg {
    let mut sel = Select::new();
    sel.recv(&state.nvim_client.receiver());
    for lsp_client in state.lsp_clients.iter() {
        sel.recv(&lsp_client.receiver());
    }

    let oper = sel.select();
    match oper.index() {
        0 => {
            let nvim_msg = oper.recv(&state.nvim_client.receiver()).unwrap();
            SelectedMsg::Nvim(nvim_msg)
        }
        i => {
            let lsp_msg = oper.recv(&state.lsp_clients[i - 1].receiver()).unwrap();

            SelectedMsg::Lsp(i - 1, lsp_msg)
        }
    }
}

impl<E: Editor> Lspc<E> {
    pub fn new(editor: E) -> Self {
        Lspc {
            editor,
            lsp_handlers: Vec::new()
        }
    }

    pub fn main_loop(mut self) -> Result<(), dyn std::error::Error> {
        let event_receiver = editor.events();
        loop {
            let selected = select(editor.events(), &lsp_handlers);

        }
    }
}
