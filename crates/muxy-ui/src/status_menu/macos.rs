use async_channel::{Receiver, Sender};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained, sel,
};
use objc2_app_kit::{
    NSAccessibility, NSColor, NSImage, NSImageSymbolConfiguration, NSMenu, NSMenuItem,
    NSSquareStatusItemLength, NSStatusBar, NSStatusItem,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSSize, NSString};

use super::{Attention, Entry};
use crate::tr;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Sender<String>]
    #[name = "MuxyStatusMenuTarget"]
    #[derive(Debug)]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        #[unsafe(method(choose:))]
        fn choose(&self, item: &NSMenuItem) {
            if let Some(id) = item.representedObject().and_then(|object| object.downcast::<NSString>().ok()) {
                let _ = self.ivars().try_send(id.to_string());
            }
        }
    }
);

#[derive(Debug)]
pub struct StatusMenu {
    item: Retained<NSStatusItem>,
    target: Retained<Target>,
    normal: Retained<NSImage>,
    red: Retained<NSImage>,
    yellow: Retained<NSImage>,
    blue: Retained<NSImage>,
    entries: Option<Vec<Entry>>,
    mtm: MainThreadMarker,
}

impl StatusMenu {
    pub fn new() -> Option<(Self, Receiver<String>)> {
        let mtm = MainThreadMarker::new()?;
        let normal = symbol("square.grid.2x2", 16.0, None)?;
        let red = symbol("square.grid.2x2", 16.0, Some(&NSColor::systemRedColor()))?;
        let yellow = symbol("circle.fill", 9.0, Some(&NSColor::systemYellowColor()))?;
        let blue = symbol("circle.fill", 9.0, Some(&NSColor::systemBlueColor()))?;
        let (sender, receiver) = async_channel::bounded(32);
        let target = Target::alloc(mtm).set_ivars(sender);
        // SAFETY: Initialize our main-thread NSObject with its channel installed.
        let target: Retained<Target> = unsafe { msg_send![super(target), init] };
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSSquareStatusItemLength);
        item.setAutosaveName(Some(&NSString::from_str("MuxySessions")));
        let mut menu = Self {
            item,
            target,
            normal,
            red,
            yellow,
            blue,
            entries: None,
            mtm,
        };
        menu.update(Vec::new());
        Some((menu, receiver))
    }

    /// Native menus and images are reused until their displayed data changes.
    pub fn update(&mut self, entries: Vec<Entry>) {
        if self.entries.as_ref() == Some(&entries) {
            return;
        }
        let attention = entries.iter().any(|entry| entry.attention.is_some());
        if let Some(button) = self.item.button(self.mtm) {
            button.setImage(Some(if attention { &self.red } else { &self.normal }));
            let description = NSString::from_str(&if attention {
                tr!("Muxy · Needs your attention").to_string()
            } else {
                tr!("Muxy · All sessions").to_string()
            });
            button.setToolTip(Some(&description));
            button.setAccessibilityLabel(Some(&description));
        }
        let menu = NSMenu::initWithTitle(NSMenu::alloc(self.mtm), &NSString::from_str("Muxy"));
        menu.setAutoenablesItems(false);
        if entries.is_empty() {
            let empty = self.row("", &tr!("No running terminals"));
            empty.setEnabled(false);
            menu.addItem(&empty);
        }
        for entry in &entries {
            let row = self.row(&entry.id, &entry.label);
            match entry.attention {
                Some(Attention::Input) => row.setImage(Some(&self.yellow)),
                Some(Attention::Unread) => row.setImage(Some(&self.blue)),
                None => {}
            }
            menu.addItem(&row);
        }
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        menu.addItem(&self.row("command-center", &tr!("Command Center")));
        self.item.setMenu(Some(&menu));
        self.entries = Some(entries);
    }

    fn row(&self, id: &str, label: &str) -> Retained<NSMenuItem> {
        let mut title: String = label.chars().take(100).collect();
        if label.chars().count() > 100 {
            title.push('…');
        }
        // SAFETY: The target is retained by StatusMenu, and choose: accepts NSMenuItem.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm),
                &NSString::from_str(&title),
                Some(sel!(choose:)),
                &NSString::new(),
            )
        };
        unsafe {
            item.setTarget(Some(&self.target));
            item.setRepresentedObject(Some(&NSString::from_str(id)));
        }
        item.setToolTip(Some(&NSString::from_str(label)));
        item
    }
}

impl Drop for StatusMenu {
    fn drop(&mut self) {
        if let Some(menu) = self.item.menu(self.mtm) {
            menu.cancelTracking();
        }
        self.item.setMenu(None);
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}

fn symbol(name: &str, size: f64, color: Option<&NSColor>) -> Option<Retained<NSImage>> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        None,
    )?;
    let image = match color {
        Some(color) => image.imageWithSymbolConfiguration(
            &NSImageSymbolConfiguration::configurationWithHierarchicalColor(color),
        )?,
        None => image,
    };
    image.setSize(NSSize::new(size, size));
    image.setTemplate(color.is_none());
    Some(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_symbols_render_color_instead_of_menu_templates() {
        assert!(
            symbol("square.grid.2x2", 16.0, None)
                .expect("normal icon")
                .isTemplate()
        );
        for (name, color, channels) in [
            ("square.grid.2x2", NSColor::systemRedColor(), [0, 1]),
            ("circle.fill", NSColor::systemYellowColor(), [1, 2]),
            ("circle.fill", NSColor::systemBlueColor(), [2, 0]),
        ] {
            let image = symbol(name, 16.0, Some(&color)).expect("colored icon");
            assert!(!image.isTemplate());
            let pixels = crate::bitmap::render_rgba(&image, 32, 32).expect("native bitmap");
            assert!(
                pixels.chunks_exact(4).any(|pixel| {
                    pixel[3] > 128
                        && i16::from(pixel[channels[0]]) - i16::from(pixel[channels[1]]) > 40
                }),
                "the image must retain its attention color"
            );
        }
    }
}
