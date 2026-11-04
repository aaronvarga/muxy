/// Whether projects can take a symbol as their icon. Only macOS draws every
/// symbol, so elsewhere a project shows a logo or its initial.
pub const PROJECT_ICONS: bool = cfg!(target_os = "macos");

#[derive(Debug)]
pub struct Symbol {
    pub name: &'static str,
    pub symbol: &'static str,
}

pub const SYMBOLS: [Symbol; 192] = [
    Symbol {
        name: crate::l10n::tr_key!("Folder"),
        symbol: "folder",
    },
    Symbol {
        name: crate::l10n::tr_key!("Folder Fill"),
        symbol: "folder.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Terminal"),
        symbol: "terminal",
    },
    Symbol {
        name: crate::l10n::tr_key!("Terminal Fill"),
        symbol: "terminal.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Code"),
        symbol: "chevron.left.forwardslash.chevron.right",
    },
    Symbol {
        name: crate::l10n::tr_key!("Document"),
        symbol: "doc",
    },
    Symbol {
        name: crate::l10n::tr_key!("Document Fill"),
        symbol: "doc.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Document Text"),
        symbol: "doc.text",
    },
    Symbol {
        name: crate::l10n::tr_key!("Document Text Fill"),
        symbol: "doc.text.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Document on Clipboard"),
        symbol: "doc.on.clipboard",
    },
    Symbol {
        name: crate::l10n::tr_key!("List"),
        symbol: "list.bullet",
    },
    Symbol {
        name: crate::l10n::tr_key!("List Rectangle"),
        symbol: "list.bullet.rectangle",
    },
    Symbol {
        name: crate::l10n::tr_key!("Pencil"),
        symbol: "pencil",
    },
    Symbol {
        name: crate::l10n::tr_key!("Pencil and Ruler"),
        symbol: "pencil.and.ruler",
    },
    Symbol {
        name: crate::l10n::tr_key!("Ruler"),
        symbol: "ruler",
    },
    Symbol {
        name: crate::l10n::tr_key!("Swatchpalette"),
        symbol: "swatchpalette.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Paintpalette"),
        symbol: "paintpalette.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Sparkle"),
        symbol: "sparkle",
    },
    Symbol {
        name: crate::l10n::tr_key!("Star"),
        symbol: "star",
    },
    Symbol {
        name: crate::l10n::tr_key!("Star Fill"),
        symbol: "star.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Gear"),
        symbol: "gearshape",
    },
    Symbol {
        name: crate::l10n::tr_key!("Gear Fill"),
        symbol: "gearshape.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Wrench"),
        symbol: "wrench",
    },
    Symbol {
        name: crate::l10n::tr_key!("Wrench Fill"),
        symbol: "wrench.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Hammer"),
        symbol: "hammer",
    },
    Symbol {
        name: crate::l10n::tr_key!("Hammer Fill"),
        symbol: "hammer.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Screwdriver"),
        symbol: "screwdriver",
    },
    Symbol {
        name: crate::l10n::tr_key!("Screwdriver Fill"),
        symbol: "screwdriver.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Wand and Stars"),
        symbol: "wand.and.stars",
    },
    Symbol {
        name: crate::l10n::tr_key!("Globe"),
        symbol: "globe",
    },
    Symbol {
        name: crate::l10n::tr_key!("Globe Americas"),
        symbol: "globe.americas",
    },
    Symbol {
        name: crate::l10n::tr_key!("Network"),
        symbol: "network",
    },
    Symbol {
        name: crate::l10n::tr_key!("Server Rack"),
        symbol: "server.rack",
    },
    Symbol {
        name: crate::l10n::tr_key!("External Drive"),
        symbol: "externaldrive",
    },
    Symbol {
        name: crate::l10n::tr_key!("External Drive Fill"),
        symbol: "externaldrive.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Optical Disc"),
        symbol: "opticaldisc",
    },
    Symbol {
        name: crate::l10n::tr_key!("Optical Disc Fill"),
        symbol: "opticaldisc.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Cloud"),
        symbol: "cloud",
    },
    Symbol {
        name: crate::l10n::tr_key!("Cloud Fill"),
        symbol: "cloud.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Square Grid 2x2"),
        symbol: "square.grid.2x2",
    },
    Symbol {
        name: crate::l10n::tr_key!("Square Grid 2x2 Fill"),
        symbol: "square.grid.2x2.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Square Grid 3x2"),
        symbol: "square.grid.3x2",
    },
    Symbol {
        name: crate::l10n::tr_key!("Square Grid 3x2 Fill"),
        symbol: "square.grid.3x2.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Rectangle Grid 1x2"),
        symbol: "rectangle.grid.1x2",
    },
    Symbol {
        name: crate::l10n::tr_key!("Rectangle Grid 1x2 Fill"),
        symbol: "rectangle.grid.1x2.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Rectangle 3 Group"),
        symbol: "rectangle.3.group",
    },
    Symbol {
        name: crate::l10n::tr_key!("Rectangle 3 Group Fill"),
        symbol: "rectangle.3.group.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Square Stack"),
        symbol: "square.stack",
    },
    Symbol {
        name: crate::l10n::tr_key!("Square Stack Fill"),
        symbol: "square.stack.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Circle"),
        symbol: "circle",
    },
    Symbol {
        name: crate::l10n::tr_key!("Circle Fill"),
        symbol: "circle.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Diamond"),
        symbol: "diamond",
    },
    Symbol {
        name: crate::l10n::tr_key!("Diamond Fill"),
        symbol: "diamond.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Hexagon"),
        symbol: "hexagon",
    },
    Symbol {
        name: crate::l10n::tr_key!("Hexagon Fill"),
        symbol: "hexagon.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("App"),
        symbol: "app",
    },
    Symbol {
        name: crate::l10n::tr_key!("App Fill"),
        symbol: "app.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("App Badge"),
        symbol: "app.badge",
    },
    Symbol {
        name: crate::l10n::tr_key!("App Badge Fill"),
        symbol: "app.badge.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("App Window"),
        symbol: "macwindow",
    },
    Symbol {
        name: crate::l10n::tr_key!("Play"),
        symbol: "play",
    },
    Symbol {
        name: crate::l10n::tr_key!("Play Fill"),
        symbol: "play.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Play Rectangle"),
        symbol: "play.rectangle",
    },
    Symbol {
        name: crate::l10n::tr_key!("Play Rectangle Fill"),
        symbol: "play.rectangle.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Music Note"),
        symbol: "music.note",
    },
    Symbol {
        name: crate::l10n::tr_key!("Music Note List"),
        symbol: "music.note.list",
    },
    Symbol {
        name: crate::l10n::tr_key!("Film"),
        symbol: "film",
    },
    Symbol {
        name: crate::l10n::tr_key!("Film Fill"),
        symbol: "film.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Photo"),
        symbol: "photo",
    },
    Symbol {
        name: crate::l10n::tr_key!("Photo Fill"),
        symbol: "photo.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Video"),
        symbol: "video",
    },
    Symbol {
        name: crate::l10n::tr_key!("Video Fill"),
        symbol: "video.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Message"),
        symbol: "message",
    },
    Symbol {
        name: crate::l10n::tr_key!("Message Fill"),
        symbol: "message.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Bubble Left"),
        symbol: "bubble.left",
    },
    Symbol {
        name: crate::l10n::tr_key!("Bubble Left Fill"),
        symbol: "bubble.left.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Envelope"),
        symbol: "envelope",
    },
    Symbol {
        name: crate::l10n::tr_key!("Envelope Fill"),
        symbol: "envelope.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Calendar"),
        symbol: "calendar",
    },
    Symbol {
        name: crate::l10n::tr_key!("Clock"),
        symbol: "clock",
    },
    Symbol {
        name: crate::l10n::tr_key!("Clock Fill"),
        symbol: "clock.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Timer"),
        symbol: "timer",
    },
    Symbol {
        name: crate::l10n::tr_key!("Bell"),
        symbol: "bell",
    },
    Symbol {
        name: crate::l10n::tr_key!("Bell Fill"),
        symbol: "bell.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Bell Badge"),
        symbol: "bell.badge",
    },
    Symbol {
        name: crate::l10n::tr_key!("Bell Badge Fill"),
        symbol: "bell.badge.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Bookmark"),
        symbol: "bookmark",
    },
    Symbol {
        name: crate::l10n::tr_key!("Bookmark Fill"),
        symbol: "bookmark.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Flag"),
        symbol: "flag",
    },
    Symbol {
        name: crate::l10n::tr_key!("Flag Fill"),
        symbol: "flag.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Tag"),
        symbol: "tag",
    },
    Symbol {
        name: crate::l10n::tr_key!("Tag Fill"),
        symbol: "tag.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Book"),
        symbol: "book",
    },
    Symbol {
        name: crate::l10n::tr_key!("Book Fill"),
        symbol: "book.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Books Vertical"),
        symbol: "books.vertical",
    },
    Symbol {
        name: crate::l10n::tr_key!("Books Vertical Fill"),
        symbol: "books.vertical.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Magnifying Glass"),
        symbol: "magnifyingglass",
    },
    Symbol {
        name: crate::l10n::tr_key!("Lightbulb"),
        symbol: "lightbulb",
    },
    Symbol {
        name: crate::l10n::tr_key!("Lightbulb Fill"),
        symbol: "lightbulb.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Cube"),
        symbol: "cube",
    },
    Symbol {
        name: crate::l10n::tr_key!("Cube Fill"),
        symbol: "cube.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Cylinder"),
        symbol: "cylinder",
    },
    Symbol {
        name: crate::l10n::tr_key!("Cylinder Fill"),
        symbol: "cylinder.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Capsule"),
        symbol: "capsule",
    },
    Symbol {
        name: crate::l10n::tr_key!("Capsule Fill"),
        symbol: "capsule.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Pyramid"),
        symbol: "pyramid",
    },
    Symbol {
        name: crate::l10n::tr_key!("Pyramid Fill"),
        symbol: "pyramid.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Cone"),
        symbol: "cone",
    },
    Symbol {
        name: crate::l10n::tr_key!("Cone Fill"),
        symbol: "cone.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Key"),
        symbol: "key",
    },
    Symbol {
        name: crate::l10n::tr_key!("Key Fill"),
        symbol: "key.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Lock"),
        symbol: "lock",
    },
    Symbol {
        name: crate::l10n::tr_key!("Lock Fill"),
        symbol: "lock.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Lock Shield"),
        symbol: "lock.shield",
    },
    Symbol {
        name: crate::l10n::tr_key!("Lock Shield Fill"),
        symbol: "lock.shield.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Shield"),
        symbol: "shield",
    },
    Symbol {
        name: crate::l10n::tr_key!("Shield Fill"),
        symbol: "shield.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Bolt"),
        symbol: "bolt",
    },
    Symbol {
        name: crate::l10n::tr_key!("Bolt Fill"),
        symbol: "bolt.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Bolt Shield"),
        symbol: "bolt.shield",
    },
    Symbol {
        name: crate::l10n::tr_key!("Bolt Shield Fill"),
        symbol: "bolt.shield.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Waveform"),
        symbol: "waveform",
    },
    Symbol {
        name: crate::l10n::tr_key!("Waveform Path"),
        symbol: "waveform.path",
    },
    Symbol {
        name: crate::l10n::tr_key!("Chart Pie"),
        symbol: "chart.pie",
    },
    Symbol {
        name: crate::l10n::tr_key!("Chart Pie Fill"),
        symbol: "chart.pie.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Chart Bar"),
        symbol: "chart.bar",
    },
    Symbol {
        name: crate::l10n::tr_key!("Chart Bar Fill"),
        symbol: "chart.bar.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Chart Line"),
        symbol: "chart.line.uptrend.xyaxis",
    },
    Symbol {
        name: crate::l10n::tr_key!("Gauge"),
        symbol: "gauge",
    },
    Symbol {
        name: crate::l10n::tr_key!("Speedometer"),
        symbol: "speedometer",
    },
    Symbol {
        name: crate::l10n::tr_key!("Viewfinder"),
        symbol: "viewfinder",
    },
    Symbol {
        name: crate::l10n::tr_key!("Viewfinder Circle"),
        symbol: "viewfinder.circle",
    },
    Symbol {
        name: crate::l10n::tr_key!("Camera"),
        symbol: "camera",
    },
    Symbol {
        name: crate::l10n::tr_key!("Camera Fill"),
        symbol: "camera.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Eye"),
        symbol: "eye",
    },
    Symbol {
        name: crate::l10n::tr_key!("Eye Fill"),
        symbol: "eye.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Eye Slash"),
        symbol: "eye.slash",
    },
    Symbol {
        name: crate::l10n::tr_key!("Eye Slash Fill"),
        symbol: "eye.slash.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Person"),
        symbol: "person",
    },
    Symbol {
        name: crate::l10n::tr_key!("Person Fill"),
        symbol: "person.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Person Circle"),
        symbol: "person.circle",
    },
    Symbol {
        name: crate::l10n::tr_key!("Person Circle Fill"),
        symbol: "person.circle.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Person 2"),
        symbol: "person.2",
    },
    Symbol {
        name: crate::l10n::tr_key!("Person 2 Fill"),
        symbol: "person.2.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Person 3"),
        symbol: "person.3",
    },
    Symbol {
        name: crate::l10n::tr_key!("Person 3 Fill"),
        symbol: "person.3.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Backpack"),
        symbol: "backpack",
    },
    Symbol {
        name: crate::l10n::tr_key!("Backpack Fill"),
        symbol: "backpack.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("House"),
        symbol: "house",
    },
    Symbol {
        name: crate::l10n::tr_key!("House Fill"),
        symbol: "house.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Building"),
        symbol: "building",
    },
    Symbol {
        name: crate::l10n::tr_key!("Building Fill"),
        symbol: "building.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Building Columns"),
        symbol: "building.columns",
    },
    Symbol {
        name: crate::l10n::tr_key!("Building Columns Fill"),
        symbol: "building.columns.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Cart"),
        symbol: "cart",
    },
    Symbol {
        name: crate::l10n::tr_key!("Cart Fill"),
        symbol: "cart.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Bag"),
        symbol: "bag",
    },
    Symbol {
        name: crate::l10n::tr_key!("Bag Fill"),
        symbol: "bag.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Creditcard"),
        symbol: "creditcard",
    },
    Symbol {
        name: crate::l10n::tr_key!("Creditcard Fill"),
        symbol: "creditcard.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Link"),
        symbol: "link",
    },
    Symbol {
        name: crate::l10n::tr_key!("Link Circle"),
        symbol: "link.circle",
    },
    Symbol {
        name: crate::l10n::tr_key!("Link Circle Fill"),
        symbol: "link.circle.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Square and Arrow"),
        symbol: "square.and.arrow.up",
    },
    Symbol {
        name: crate::l10n::tr_key!("Square and Arrow Fill"),
        symbol: "square.and.arrow.up.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Square on Square"),
        symbol: "square.on.square",
    },
    Symbol {
        name: crate::l10n::tr_key!("Square on Square Fill"),
        symbol: "square.on.square.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrowshape"),
        symbol: "arrowshape.turn.up.forward",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrowshape Fill"),
        symbol: "arrowshape.turn.up.forward.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrow Up"),
        symbol: "arrow.up",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrow Down"),
        symbol: "arrow.down",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrow Left"),
        symbol: "arrow.left",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrow Right"),
        symbol: "arrow.right",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrow Up Circle"),
        symbol: "arrow.up.circle",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrow Up Circle Fill"),
        symbol: "arrow.up.circle.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrow Down Circle"),
        symbol: "arrow.down.circle",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrow Down Circle Fill"),
        symbol: "arrow.down.circle.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrow Left Circle"),
        symbol: "arrow.left.circle",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrow Left Circle Fill"),
        symbol: "arrow.left.circle.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrow Right Circle"),
        symbol: "arrow.right.circle",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrow Right Circle Fill"),
        symbol: "arrow.right.circle.fill",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrow Left Right"),
        symbol: "arrow.left.and.right",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrow Up Down"),
        symbol: "arrow.up.and.down",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrows"),
        symbol: "arrow.triangle.branch",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrows Merge"),
        symbol: "arrow.triangle.merge",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrows Swap"),
        symbol: "arrow.triangle.swap",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrowshape Backward"),
        symbol: "arrowshape.backward",
    },
    Symbol {
        name: crate::l10n::tr_key!("Arrowshape Forward"),
        symbol: "arrowshape.forward",
    },
    Symbol {
        name: crate::l10n::tr_key!("Go Backward"),
        symbol: "gobackward",
    },
    Symbol {
        name: crate::l10n::tr_key!("Go Forward"),
        symbol: "goforward",
    },
    Symbol {
        name: crate::l10n::tr_key!("Forward"),
        symbol: "forward",
    },
    Symbol {
        name: crate::l10n::tr_key!("Forward Fill"),
        symbol: "forward.fill",
    },
];

pub fn matching(query: &str) -> Vec<&'static Symbol> {
    let query = query.trim().to_lowercase();
    SYMBOLS
        .iter()
        .filter(|symbol| {
            query.is_empty()
                || symbol.name.to_lowercase().contains(&query)
                || muxy_core::l10n::translate(symbol.name)
                    .to_lowercase()
                    .contains(&query)
                || symbol.symbol.to_lowercase().contains(&query)
        })
        .collect()
}

/// One symbol of the full catalog.
#[derive(Debug)]
pub struct Entry {
    pub symbol: String,
    /// Lowercase words to search: the name's parts, Apple's keywords and our
    /// name for it.
    search: String,
    categories: Vec<String>,
}

/// Every SF Symbol this Mac offers, in Apple's order after our own picks, and
/// the categories Apple files them under.
#[derive(Debug, Default)]
pub struct Catalog {
    pub entries: Vec<Entry>,
    /// `(key, icon symbol)` for each category, `all` excluded.
    pub categories: Vec<(String, String)>,
}

static CATALOG: std::sync::OnceLock<Catalog> = std::sync::OnceLock::new();

/// The catalog once something has read it.
pub fn loaded() -> Option<&'static Catalog> {
    CATALOG.get()
}

/// The catalog, read from the system on first use. Without it (an older or
/// newer macOS that moved the files, or another platform) it is `SYMBOLS`.
/// Reading it takes a moment, so call it off the main thread first.
pub fn catalog() -> &'static Catalog {
    CATALOG.get_or_init(|| {
        let mut catalog = Catalog::default();
        let mut seen = std::collections::HashSet::new();
        for symbol in &SYMBOLS {
            if seen.insert(symbol.symbol.to_owned()) {
                catalog.entries.push(Entry {
                    symbol: symbol.symbol.to_owned(),
                    search: format!(
                        "{} {} {}",
                        symbol.name.to_lowercase(),
                        muxy_core::l10n::translate(symbol.name).to_lowercase(),
                        symbol.symbol.replace('.', " ")
                    ),
                    categories: Vec::new(),
                });
            }
        }
        #[cfg(target_os = "macos")]
        system::extend(&mut catalog, &mut seen);
        catalog
    })
}

impl Catalog {
    /// Indexes of the entries in `category` (every one when `None`) whose
    /// search words contain each word of `query`.
    pub fn matching(&self, query: &str, category: Option<&str>) -> Vec<usize> {
        let query = query.trim().to_lowercase();
        let words: Vec<_> = query.split_whitespace().collect();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                category.is_none_or(|category| entry.categories.iter().any(|key| key == category))
                    && words.iter().all(|word| entry.search.contains(word))
            })
            .map(|(index, _)| index)
            .collect()
    }
}

#[cfg(target_os = "macos")]
mod system {
    use std::collections::{HashMap, HashSet};
    use std::path::Path;

    const RESOURCES: &str = "/System/Library/CoreServices/CoreGlyphs.bundle/Contents/Resources";
    /// Script variants SF Symbols shows as localizations of one symbol.
    const LOCALES: [&str; 27] = [
        "ar", "bn", "el", "gu", "he", "hi", "ja", "km", "kn", "ko", "ml", "mni", "mr", "my", "or",
        "pa", "rtl", "ru", "sat", "si", "sr", "ta", "te", "th", "uk", "vi", "zh",
    ];

    #[derive(serde::Deserialize)]
    struct Category {
        key: String,
        icon: String,
    }

    fn read<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
        plist::from_file(Path::new(RESOURCES).join(name)).ok()
    }

    pub(super) fn extend(catalog: &mut super::Catalog, seen: &mut HashSet<String>) {
        let Some(order) = read::<Vec<String>>("symbol_order.plist") else {
            return;
        };
        let restricted: HashMap<String, String> =
            read("symbol_restrictions.strings").unwrap_or_default();
        let mut keywords: HashMap<String, Vec<String>> =
            read("symbol_search.plist").unwrap_or_default();
        let mut categories: HashMap<String, Vec<String>> =
            read("symbol_categories.plist").unwrap_or_default();
        let listed: HashSet<&str> = order.iter().map(String::as_str).collect();
        for entry in &mut catalog.entries {
            entry.categories = categories.remove(&entry.symbol).unwrap_or_default();
            if let Some(words) = keywords.remove(&entry.symbol) {
                entry.search = format!("{} {}", entry.search, words.join(" ").to_lowercase());
            }
        }
        for symbol in &order {
            let localized = symbol
                .rsplit_once('.')
                .is_some_and(|(base, suffix)| LOCALES.contains(&suffix) && listed.contains(base));
            if localized || restricted.contains_key(symbol) || !seen.insert(symbol.clone()) {
                continue;
            }
            let words = keywords.remove(symbol).unwrap_or_default();
            catalog.entries.push(super::Entry {
                search: format!(
                    "{} {}",
                    symbol.replace('.', " "),
                    words.join(" ").to_lowercase()
                ),
                categories: categories.remove(symbol).unwrap_or_default(),
                symbol: symbol.clone(),
            });
        }
        catalog.categories = read::<Vec<Category>>("categories.plist")
            .unwrap_or_default()
            .into_iter()
            .filter(|category| category.key != "all")
            .map(|category| (category.key, category.icon))
            .collect();
    }
}

#[cfg(all(test, target_os = "macos"))]
mod catalog_tests {
    use super::*;

    #[test]
    fn the_catalog_lists_the_systems_symbols_without_script_variants() {
        let started = std::time::Instant::now();
        let catalog = catalog();
        eprintln!(
            "catalog: {} symbols in {:?}",
            catalog.entries.len(),
            started.elapsed()
        );
        assert!(catalog.entries.len() > 5000, "{}", catalog.entries.len());
        assert_eq!(
            catalog.entries[0].symbol, SYMBOLS[0].symbol,
            "our picks lead"
        );
        let names: Vec<_> = catalog
            .entries
            .iter()
            .map(|entry| entry.symbol.as_str())
            .collect();
        assert!(names.contains(&"0.circle") && !names.contains(&"0.circle.ar"));
        assert!(!catalog.categories.is_empty());
        let weather = catalog.matching("sun", Some("weather"));
        assert!(
            weather
                .iter()
                .any(|index| catalog.entries[*index].symbol == "sun.max")
        );
        // Apple's keywords find symbols whose names don't say it.
        assert!(!catalog.matching("move", None).is_empty());
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use muxy_core::l10n::{Language, PluralCategory, set_language};

    fn matches(query: &str) -> Vec<&'static str> {
        super::matching(query)
            .iter()
            .map(|symbol| symbol.symbol)
            .collect()
    }

    #[test]
    fn searches_names_and_symbol_identifiers() {
        assert_eq!(super::matching("  ").len(), super::SYMBOLS.len());
        assert_eq!(matches(" FOLDER "), ["folder", "folder.fill"]);
        assert_eq!(matches(" Folder Fill "), ["folder.fill"]);
        assert_eq!(matches(" FOLDER.FILL "), ["folder.fill"]);
        assert!(matches("no such symbol").is_empty());
    }

    #[test]
    fn searches_translated_names_without_losing_english_names_or_identifiers() {
        set_language(Some(Arc::new(Language::new(
            HashMap::from([
                ("Folder".into(), "Dossier".into()),
                ("Folder Fill".into(), "Dossier rempli".into()),
            ]),
            HashMap::new(),
            |_| PluralCategory::Other,
        ))));

        assert_eq!(matches(" DOSSIER "), ["folder", "folder.fill"]);
        assert_eq!(matches("dossier rempli"), ["folder.fill"]);
        assert_eq!(matches("Folder Fill"), ["folder.fill"]);
        assert_eq!(matches("folder.fill"), ["folder.fill"]);
        assert_eq!(matches("Star Fill"), ["star.fill"]);
        assert!(matches("folder d").is_empty());
        assert!(matches("fill dossier").is_empty());

        set_language(None);
        assert!(matches("dossier").is_empty());
    }
}
