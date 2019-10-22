use std::io::{self, Stdin, StdinLock, Stdout, StdoutLock};

use lspc::neovim::{Neovim, NvimMessage};
use lspc::rpc::Client;
use lspc::Lspc;
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
    let mut log_dir = dirs::home_dir().expect("Home directory not found");
    log_dir.push(".vim");
    std::fs::create_dir_all(&log_dir).expect("Cannot create log directory");

    log_dir.push("lspc_log.txt");
    simple_logging::log_to_file(log_dir, log::LevelFilter::Debug).expect("Can not open log file");

    let nvim_rpc = Client::<NvimMessage>::new(stdinlock, stdoutlock);
    let neovim = Neovim::new(nvim_rpc);
    let lspc = Lspc::new(neovim);

    lspc.main_loop();

    Ok(())
}
