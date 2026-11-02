//! Session summaries for the macOS menu bar. No terminal views are attached here.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Attention {
    Input,
    Unread,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ProjectIcon {
    pub logo: Option<std::sync::Arc<[u8]>>,
    /// An SF Symbol prefixed with `sf:`, an emoji, or the project initial.
    pub glyph: String,
    pub color: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    /// Stable identity, carried by the native item even if the menu refreshes.
    pub id: String,
    pub label: String,
    pub attention: Option<Attention>,
    pub icon: ProjectIcon,
}

/// Sessions with unread updates or waiting for input, counted once per entry.
pub fn attention_count(entries: &[Entry]) -> usize {
    entries
        .iter()
        .filter(|entry| entry.attention.is_some())
        .count()
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::StatusMenu;
#[cfg(target_os = "macos")]
pub(crate) use macos::project_icon;
