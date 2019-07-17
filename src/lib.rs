pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

mod lspc;
pub mod neovim;
pub mod rpc;
