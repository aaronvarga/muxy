//! Session summaries for the macOS menu bar. No terminal views are attached here.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Attention {
    Input,
    Unread,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    /// Stable identity, carried by the native item even if the menu refreshes.
    pub id: String,
    pub label: String,
    pub attention: Option<Attention>,
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::StatusMenu;
