use async_channel::Receiver;
use muxy_core::capability::Capability;

pub const CAPABILITY: Capability = Capability::Unsupported;

#[derive(Debug)]
pub(super) enum Service {}

impl Service {
    pub(super) fn start() -> Result<(Self, Receiver<String>), Capability> {
        Err(CAPABILITY)
    }

    pub(super) fn deliver(&self, _id: &str, _title: &str, _body: &str) {
        match *self {}
    }

    pub(super) fn clear(&self, _ids: &[String]) {
        match *self {}
    }
}
