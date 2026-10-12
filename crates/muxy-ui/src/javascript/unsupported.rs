use super::Event;

pub const CAPABILITY: muxy_core::capability::Capability =
    muxy_core::capability::Capability::Unsupported;

#[derive(Debug)]
pub enum Script {}

impl Script {
    pub fn start(_source: String) -> std::io::Result<(Self, async_channel::Receiver<Event>)> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            muxy_core::capability::Feature::ExtensionScripts.unsupported_reason(),
        ))
    }

    pub fn evaluate(&self, _source: String) {
        match *self {}
    }
}
