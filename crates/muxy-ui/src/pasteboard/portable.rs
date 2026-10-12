use super::Content;

pub fn read_content(cx: &gpui::App) -> Result<Content, String> {
    if let Some(item) = cx.read_from_clipboard() {
        for entry in item.entries() {
            if let gpui::ClipboardEntry::Image(image) = entry {
                if image.bytes().len() > 25 * 1024 * 1024 {
                    return Err(crate::tr!("Copied image is larger than 25 MiB").to_string());
                }
                return Ok(Content::Image(image.bytes().to_vec()));
            }
        }
    }
    Ok(Content::Text)
}

#[derive(Debug)]
pub enum Lease {}

impl Lease {
    pub fn capture() -> Result<Self, String> {
        Err(crate::tr!("Clipboard image submission is not available on Linux. Select inline image paths in Composer settings.").to_string())
    }

    pub fn write_png(&mut self, _png: Vec<u8>) -> Result<(), String> {
        match *self {}
    }
}
