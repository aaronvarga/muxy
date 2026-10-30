use std::collections::HashMap;

use async_channel::{Receiver, Sender};
use block2::RcBlock;
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained, sel,
};
use objc2_app_kit::{
    NSAccessibility, NSBezierPath, NSColor, NSCompositingOperation, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSImage, NSImageSymbolConfiguration, NSMenu, NSMenuItem,
    NSSquareStatusItemLength, NSStatusBar, NSStatusItem, NSStringDrawing,
};
use objc2_foundation::{
    NSData, NSDictionary, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use super::{Attention, Entry, ProjectIcon};
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
    icons: HashMap<ProjectIcon, Retained<NSImage>>,
    mtm: MainThreadMarker,
}

impl StatusMenu {
    pub fn new() -> Option<(Self, Receiver<String>)> {
        let mtm = MainThreadMarker::new()?;
        let normal = app_icon(None);
        let red = app_icon(Some(NSColor::systemRedColor()));
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
            icons: HashMap::new(),
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
        let mut icons = HashMap::new();
        for entry in &entries {
            let row = self.row(&entry.id, &entry.label);
            let icon = icons.entry(entry.icon.clone()).or_insert_with(|| {
                self.icons
                    .remove(&entry.icon)
                    .unwrap_or_else(|| project_icon(&entry.icon))
            });
            let dot = match entry.attention {
                Some(Attention::Input) => Some(self.yellow.clone()),
                Some(Attention::Unread) => Some(self.blue.clone()),
                None => None,
            };
            row.setImage(Some(&session_icon(icon.clone(), dot)));
            menu.addItem(&row);
        }
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        menu.addItem(&self.row("command-center", &tr!("Command Center")));
        self.item.setMenu(Some(&menu));
        self.icons = icons;
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
        // macOS 27 hides images in automatic mode; older systems lack this selector.
        if item.respondsToSelector(sel!(setPreferredImageVisibility:)) {
            // SAFETY: Availability is checked; NSMenuItemImageVisibilityVisible is NSInteger 1.
            unsafe {
                let _: () = msg_send![&*item, setPreferredImageVisibility: 1_isize];
            }
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

fn draw_image(image: &NSImage, rect: NSRect) {
    image.drawInRect_fromRect_operation_fraction(
        rect,
        NSRect::ZERO,
        NSCompositingOperation::SourceOver,
        1.0,
    );
}

fn session_icon(icon: Retained<NSImage>, dot: Option<Retained<NSImage>>) -> Retained<NSImage> {
    let draw = RcBlock::new(move |_| {
        draw_image(&icon, NSRect::new(NSPoint::ZERO, NSSize::new(18.0, 18.0)));
        if let Some(dot) = &dot {
            draw_image(
                dot,
                NSRect::new(NSPoint::new(22.0, 4.5), NSSize::new(9.0, 9.0)),
            );
        }
        true.into()
    });
    let image = NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(32.0, 18.0), true, &draw);
    image.setTemplate(false);
    image
}

fn project_icon(icon: &ProjectIcon) -> Retained<NSImage> {
    if let Some(bytes) = &icon.logo
        && let Some(image) = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(bytes))
    {
        image.setSize(NSSize::new(18.0, 18.0));
        image.setTemplate(false);
        return image;
    }
    let rgb = u32::from_str_radix(icon.color.trim_start_matches('#'), 16).unwrap_or(0x0080_8080);
    let channel = |shift: u32| f64::from((rgb >> shift) & 255_u32) / 255.0;
    let (red, green, blue) = (channel(16), channel(8), channel(0));
    let background = NSColor::colorWithSRGBRed_green_blue_alpha(red, green, blue, 1.0);
    let foreground = if 0.2126 * red + 0.7152 * green + 0.0722 * blue > 0.5 {
        NSColor::blackColor()
    } else {
        NSColor::whiteColor()
    };
    let symbol = icon
        .glyph
        .strip_prefix("sf:")
        .and_then(|name| symbol(name, 13.0, Some(&foreground)));
    let text = NSString::from_str(&icon.glyph);
    let font = NSFont::systemFontOfSize(13.0);
    // SAFETY: AppKit's attribute keys have their documented font and color value types.
    let attributes = unsafe {
        NSDictionary::from_slices(
            &[NSFontAttributeName, NSForegroundColorAttributeName],
            &[
                font.as_ref() as &objc2::runtime::AnyObject,
                foreground.as_ref(),
            ],
        )
    };
    let draw = RcBlock::new(move |_| {
        background.setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            NSRect::new(NSPoint::ZERO, NSSize::new(18.0, 18.0)),
            4.0,
            4.0,
        )
        .fill();
        if let Some(symbol) = &symbol {
            draw_image(
                symbol,
                NSRect::new(NSPoint::new(2.0, 2.0), NSSize::new(14.0, 14.0)),
            );
        } else {
            // SAFETY: The dictionary contains valid NSString drawing attributes.
            unsafe {
                let size = text.sizeWithAttributes(Some(&attributes));
                text.drawInRect_withAttributes(
                    NSRect::new(
                        NSPoint::new((18.0 - size.width) / 2.0, (18.0 - size.height) / 2.0),
                        size,
                    ),
                    Some(&attributes),
                );
            }
        }
        true.into()
    });
    let image = NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(18.0, 18.0), true, &draw);
    image.setTemplate(false);
    image
}

fn app_icon(color: Option<Retained<NSColor>>) -> Retained<NSImage> {
    let template = color.is_none();
    let color = color.unwrap_or_else(NSColor::blackColor);
    let draw = RcBlock::new(move |_| {
        color.setFill();
        // The two filled chevrons from packaging/macos/AppIconBeta.svg, without the tile background.
        let scale = 14.0 / 550.0;
        let left = (18.0 - 492.89 * scale) / 2.0;
        for offset in [0.0, 194.0] {
            let path = NSBezierPath::bezierPath();
            let point = |x, y| NSPoint::new(left + (x + offset) * scale, 2.0 + y * scale);
            path.moveToPoint(point(0.0, 0.0));
            for (x, y) in [
                (298.89, 275.0),
                (0.0, 550.0),
                (0.0, 430.43),
                (167.38, 275.0),
                (0.0, 119.57),
            ] {
                path.lineToPoint(point(x, y));
            }
            path.closePath();
            path.fill();
        }
        true.into()
    });
    let image = NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(18.0, 18.0), true, &draw);
    image.setTemplate(template);
    image
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
    fn project_images_keep_attention_dots_separate_and_colored() {
        let logo = app_icon(None).TIFFRepresentation().expect("logo").to_vec();
        for (glyph, logo) in [
            ("A", Some(std::sync::Arc::from(logo))),
            ("sf:house.fill", None),
            ("🚀", None),
            ("A", Some(std::sync::Arc::from(&b"invalid"[..]))),
        ] {
            let icon = project_icon(&ProjectIcon {
                logo,
                glyph: glyph.into(),
                color: "#1188cc".into(),
            });
            for color in [
                None,
                Some(NSColor::systemYellowColor()),
                Some(NSColor::systemBlueColor()),
            ] {
                let dot = color
                    .as_ref()
                    .and_then(|color| symbol("circle.fill", 9.0, Some(color)));
                let image = session_icon(icon.clone(), dot);
                assert!(!image.isTemplate());
                let pixels = crate::bitmap::render_rgba(&image, 64, 36).expect("bitmap");
                assert!(
                    pixels
                        .chunks_exact(4)
                        .enumerate()
                        .any(|(i, pixel)| i % 64 < 36 && pixel[3] > 128)
                );
                let dots: Vec<_> = pixels
                    .chunks_exact(4)
                    .enumerate()
                    .filter(|(i, pixel)| i % 64 >= 44 && pixel[3] > 128)
                    .map(|(_, pixel)| pixel)
                    .collect();
                if let Some(color) = color {
                    assert!(
                        !dots.is_empty(),
                        "attention dot must survive the project image"
                    );
                    let blue = color == NSColor::systemBlueColor();
                    assert!(dots.iter().any(|pixel| if blue {
                        pixel[2] > pixel[0]
                    } else {
                        pixel[0] > pixel[2]
                    }));
                } else {
                    assert!(
                        dots.is_empty(),
                        "clear sessions must not show an attention dot"
                    );
                }
            }
        }
    }

    #[test]
    fn attention_symbols_render_color_instead_of_menu_templates() {
        assert!(app_icon(None).isTemplate());
        for (name, color, channels) in [
            ("muxy", NSColor::systemRedColor(), [0, 1]),
            ("circle.fill", NSColor::systemYellowColor(), [1, 2]),
            ("circle.fill", NSColor::systemBlueColor(), [2, 0]),
        ] {
            let image = if name == "muxy" {
                app_icon(Some(color.clone()))
            } else {
                symbol(name, 16.0, Some(&color)).expect("colored icon")
            };
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
