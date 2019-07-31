pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub mod lspc;
pub mod neovim;
pub mod rpc;

pub use lspc::Lspc;
