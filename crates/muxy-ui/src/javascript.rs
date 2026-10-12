use std::sync::mpsc;

#[derive(Debug)]
pub struct Call {
    pub request: String,
    pub reply: mpsc::SyncSender<String>,
}

#[derive(Debug)]
pub enum Event {
    Call(Call),
    Error(String),
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;
#[cfg(target_os = "macos")]
pub use macos::{CAPABILITY, Script};
#[cfg(not(target_os = "macos"))]
pub use unsupported::{CAPABILITY, Script};
