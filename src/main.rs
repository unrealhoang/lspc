use neovim_lib::{Neovim, Session, RequestHandler, Handler, Value};
use neovim_lib::neovim_api::NeovimApi;
use crossbeam::channel::{self, Sender, Receiver};
use crossbeam;

use std::error::Error;

mod lspc;

struct Command;

impl Command {
    fn from_name_and_args(name: &str, args: Vec<Value>) -> Self {
        Command
    }
}
struct EditorHandler {
    sender: Sender<(String, Vec<Value>)>,
}

impl RequestHandler for EditorHandler {}

impl Handler for EditorHandler {
    fn handle_notify(&mut self, name: &str, args: Vec<Value>) {
        self.sender.send((name.to_owned(), args)).unwrap();
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let session = Session::new_parent()?;
    let mut nvim = Neovim::new(session);
    let (sender, receiver) = channel::unbounded();
    let lspc_handler = EditorHandler { sender };
    nvim.session.start_event_loop_handler(lspc_handler);
    let event_handler_thread = nvim.session.take_dispatch_guard();

    crossbeam::scope(|scope| {
        scope.spawn(move |_| {
            for message in receiver {
                if message.0 == "hello" {
                    nvim.command("echo 'hello from the other side'").unwrap();
                }
            }
        });
    }).unwrap();

    event_handler_thread.join().expect("Handler thread error");

    Ok(())
}
