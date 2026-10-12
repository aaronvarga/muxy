use crate::quick_terminal::ShortcutRecordingEvent;
use crate::quick_terminal::shortcut_service::{
    ShortcutBackend, ShortcutBackendFactory, ShortcutState,
};
use muxy_core::quick_terminal::QuickTerminalShortcut;
use std::rc::Rc;

pub const CAPABILITY: muxy_core::capability::Capability =
    muxy_core::capability::Capability::Unsupported;

#[derive(Debug)]
pub struct SystemObservers;

#[derive(Debug)]
pub struct ShortcutRecorder;

impl ShortcutRecorder {
    pub fn start(_sender: async_channel::Sender<ShortcutRecordingEvent>) -> Result<Self, String> {
        Err(muxy_core::capability::Feature::QuickTerminal
            .unsupported_reason()
            .into_owned())
    }
}

pub(super) struct UnsupportedShortcutBackendFactory;

impl ShortcutBackendFactory for UnsupportedShortcutBackendFactory {
    fn create(&mut self, shortcut: &QuickTerminalShortcut) -> Option<Box<dyn ShortcutBackend>> {
        (!matches!(shortcut, QuickTerminalShortcut::Unassigned))
            .then(|| Box::new(UnsupportedShortcutBackend) as Box<dyn ShortcutBackend>)
    }
}

struct UnsupportedShortcutBackend;

impl ShortcutBackend for UnsupportedShortcutBackend {
    fn start(&mut self, _trigger: Rc<dyn Fn()>) -> Result<(), String> {
        Ok(())
    }

    fn stop(&mut self) {}

    fn state(&self) -> ShortcutState {
        ShortcutState::Unavailable
    }
}
