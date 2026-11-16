//! Fork: the headless Claude Code chat host (macOS only). See the chat plan's
//! Milestone 2.

mod args;
mod boundary;
mod codex;
mod context;
mod env;
pub(in crate::session) mod flush;
mod host;
mod normalize;
mod recovery;
mod reserve;
mod state;
mod submissions;
mod supervisor;
mod transcript;

pub(crate) use host::{Event, Host, Services};

#[cfg(test)]
mod tests;
