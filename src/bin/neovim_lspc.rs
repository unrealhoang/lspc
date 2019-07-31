use std::io::{self, Stdin, StdinLock, Stdout, StdoutLock};

use lspc::neovim::{Neovim, NvimMessage};
use lspc::Lspc;
use lspc::rpc::Client;
use std::error::Error;

use lazy_static::lazy_static;


lazy_static! {
    static ref STDIN: Stdin = io::stdin();
    static ref STDOUT: Stdout = io::stdout();
}

pub fn stdinlock() -> StdinLock<'static> {
    STDIN.lock()
}

pub fn stdoutlock() -> StdoutLock<'static> {
    STDOUT.lock()
}

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    simple_logging::log_to_file("log.txt", log::LevelFilter::Debug);

    let nvim_rpc = Client::<NvimMessage>::new(stdinlock, stdoutlock);
    let neovim = Neovim::new(nvim_rpc);
    let lspc = Lspc::new(neovim);

    lspc.main_loop();

    Ok(())
}
