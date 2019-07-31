mod handler;

use crossbeam::channel::{Receiver, Select};

use self::handler::{LspHandler, LspMessage};

pub enum Event {
    Hello,
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

fn handle_editor_event<E: Editor>(state: &mut Lspc<E>, event: Event) -> Result<(), ()> {
    match event {
        Hello => {
            state.editor.say_hello();
        }
        _ => (),
    }

    Ok(())
}

fn handle_lsp_msg<E: Editor>(state: &mut Lspc<E>, index: usize, msg: LspMessage) -> Result<(), ()> {
    match msg {
        _ => (),
    };

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
            match selected {
                SelectedMsg::Editor(event) => {
                    handle_editor_event(&mut self, event);
                }
                SelectedMsg::Lsp(index, msg) => {
                    handle_lsp_msg(&mut self, index, msg);
                }
            }
        }
    }
}
