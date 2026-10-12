pub mod assets;

/// Premultiplied BGRA pixels of a page at the display's scale.
pub struct Snapshot {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Snapshot({}x{})", self.width, self.height)
    }
}
type AssetResult = Result<(Vec<u8>, &'static str), String>;

#[derive(Debug)]
pub enum Event {
    Message {
        id: u64,
        generation: u64,
        json: String,
    },
    Navigated,
    Loaded,
    Failed(String),
    Escape,
    Shortcut(gpui::Keystroke),
    Snapshot {
        id: u64,
        generation: u64,
        image: Snapshot,
    },
    Asset {
        id: u64,
        result: AssetResult,
    },
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;
#[cfg(target_os = "macos")]
pub use macos::NativeWebview;
#[cfg(not(target_os = "macos"))]
pub use unsupported::NativeWebview;
