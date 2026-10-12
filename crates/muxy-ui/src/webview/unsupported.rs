use super::{AssetResult, Event, assets::Source};
use gpui::{Bounds, CursorStyle, Pixels, Rgba, Window};

pub const CAPABILITY: muxy_core::capability::Capability =
    muxy_core::capability::Capability::Unsupported;

#[derive(Debug)]
pub enum NativeWebview {}

impl NativeWebview {
    pub fn new(
        _window: &Window,
        _source: Source,
        _script: &str,
        _background: Rgba,
        _transparent_background: bool,
    ) -> Result<(Self, async_channel::Receiver<Event>), String> {
        Err(muxy_core::capability::Feature::ExtensionWebviews
            .unsupported_reason()
            .into_owned())
    }
    pub fn set_shortcuts(&self, _modal: bool, _shortcuts: Vec<gpui::Keystroke>) {
        match *self {}
    }
    pub fn set_focused(&self, _focused: bool) {
        match *self {}
    }
    pub fn set_grips(&self, _grips: &[(Bounds<Pixels>, CursorStyle)]) {
        match *self {}
    }
    pub fn set_mouse_passthrough(&self, _enabled: bool) {
        match *self {}
    }
    pub fn generation(&self) -> u64 {
        match *self {}
    }
    pub fn install_script(&self, _script: &str) {
        match *self {}
    }
    pub fn evaluate(&self, _script: &str) {
        match *self {}
    }
    pub fn reply(&self, _id: u64, _generation: u64, _json: &str) {
        match *self {}
    }
    pub fn complete_asset(&self, _id: u64, _result: AssetResult) {
        match *self {}
    }
    pub fn sync(
        &self,
        _bounds: Bounds<Pixels>,
        _clip: Bounds<Pixels>,
        _background: Rgba,
        _radius: f64,
    ) {
        match *self {}
    }
    pub fn occlude(&self, _occlusions: &[Bounds<Pixels>]) {
        match *self {}
    }
    pub fn set_visible(&self, _visible: bool) {
        match *self {}
    }
    pub fn focus(&self) {
        match *self {}
    }
    pub fn blur(&self) {
        match *self {}
    }
    pub fn snapshot(&self) {
        match *self {}
    }
    pub fn is_current_snapshot(&self, _id: u64, _generation: u64) -> bool {
        match *self {}
    }
}
