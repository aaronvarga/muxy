use super::Event;

#[derive(Debug)]
pub enum Script {}

impl Script {
    pub fn start(_source: String) -> std::io::Result<(Self, async_channel::Receiver<Event>)> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            crate::tr!("Extension scripts are not available on Linux yet.").to_string(),
        ))
    }

    pub fn evaluate(&self, _source: String) {
        match *self {}
    }
}
