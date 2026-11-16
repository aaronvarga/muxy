//! Fork: assistant markdown as GPUI elements. A row parses its text once per
//! change into a flat [`Document`]; drawing builds elements from it each frame.
//! Nothing is executed: HTML stays text, images show their alt text, and only
//! web links, bare `http(s)://` URLs and code naming a file that exists can be
//! clicked.
//!
//! `{{short|long}}` stretches are resolved in the source before parsing, so a
//! long form can hold whole paragraphs and lists: a collapsed one becomes a
//! link to [`STRETCH_SCHEME`] that expands it, an expanded one its long form.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, AppContext, ElementId, FontStyle, FontWeight, HighlightStyle, InteractiveElement,
    InteractiveText, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    StrikethroughStyle, Styled, StyledText, UnderlineStyle, div, px, relative,
};
use muxy_app_core::opener::Target;
use muxy_ui::components::{IconButton, Tooltip};
use muxy_ui::icon::Icon;
use muxy_ui::tr;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, TextMergeStream};

use super::rows::Scope;
use super::rows::files::path_candidate;
use crate::model::chat::{ChatFile, Lookup};

const BOLD: u8 = 1;
const ITALIC: u8 = 2;
const STRIKE: u8 = 4;
const CODE: u8 = 8;
const LINK: u8 = 16;
/// A collapsed stretch's short form, which expands when clicked.
const STRETCH: u8 = 32;
/// Code naming a file that exists, which opens it when clicked.
const PATH: u8 = 64;

/// What is known of whether code names a file: a cache read, never the disk.
pub(crate) type Resolve<'a> = &'a dyn Fn(&str) -> Lookup;
/// Path candidates looked up per row; later ones stay plain.
pub(crate) const MAX_PATHS: usize = 64;
/// What a link gets after its text, so it reads as leaving the app.
pub(crate) const WEB_ARROW: &str = "\u{2009}\u{2197}";

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Document {
    pub(crate) blocks: Vec<Block>,
    /// A path candidate's lookup hadn't answered, so answers landing re-parse it.
    pub(crate) pending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Paragraph,
    Heading(u8),
    /// Fenced or indented code.
    Code,
    /// A table: a grid of cells, or aligned monospace lines past the grid's caps.
    Table,
    Rule,
}

/// One block; lists and quotes are flattened into its `depth`, `quote` and `marker`.
#[derive(Debug, PartialEq)]
pub(crate) struct Block {
    pub(crate) kind: Kind,
    pub(crate) text: SharedString,
    /// Sorted, non-overlapping styled ranges of `text`.
    pub(crate) spans: Vec<(Range<usize>, u8)>,
    /// The web links in `text`; other link targets are styled but inert.
    pub(crate) links: Vec<(Range<usize>, SharedString)>,
    pub(crate) depth: u8,
    pub(crate) quote: u8,
    /// The list item's bullet, number or checkbox, on its first block.
    pub(crate) marker: Option<SharedString>,
    /// A table's cells, when it is small enough to lay out as a grid.
    pub(crate) grid: Option<Box<Grid>>,
    /// Collapsed stretches in `text`, by their id; a click expands one.
    pub(crate) stretches: Vec<(Range<usize>, SharedString)>,
    /// Code in `text` naming a file that exists; a click opens it.
    pub(crate) paths: Vec<(Range<usize>, Arc<ChatFile>)>,
}

/// A table drawn as cells.
#[derive(Debug, PartialEq)]
pub(crate) struct Grid {
    pub(crate) rows: Vec<Vec<SharedString>>,
    /// The first row is the header.
    pub(crate) head: bool,
    /// Each column's widest cell in chars, up to [`MAX_COLUMN`].
    pub(crate) widths: Vec<u16>,
}

/// The link target a collapsed stretch's short form gets, followed by its id.
const STRETCH_SCHEME: &str = "muxy-stretch:";

/// The ids of a row's expanded stretches.
pub(crate) type Open = BTreeSet<SharedString>;

/// `markdown` with the stretches in `open` expanded, parsed into a [`Document`];
/// `resolve` says which code spans that look like paths name a file.
pub(crate) fn parse(markdown: &str, open: &Open, resolve: Resolve<'_>) -> Document {
    let source = expand(markdown, open);
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut builder = Builder::default();
    // Merged, so a URL split at an unmatched `_` or `*` arrives whole.
    for event in TextMergeStream::new(Parser::new_ext(&source, options)) {
        builder.event(event, resolve);
    }
    builder.flush();
    Document {
        blocks: builder.blocks,
        pending: builder.pending,
    }
}

/// A parse kept until the text or its open stretches change, or until path
/// answers land (`epoch`) for a parse still waiting on one, so drawing costs
/// a set comparison rather than a parse.
#[derive(Default)]
pub(crate) struct Parsed(RefCell<Option<(Open, u64, Arc<Document>)>>);

impl Parsed {
    pub(crate) fn get(
        &self,
        markdown: &str,
        open: Option<&Open>,
        epoch: u64,
        resolve: Resolve<'_>,
    ) -> Arc<Document> {
        static NONE: Open = BTreeSet::new();
        let open = open.unwrap_or(&NONE);
        let mut parsed = self.0.borrow_mut();
        if let Some((was, at, document)) = &*parsed
            && was == open
            && (*at == epoch || !document.pending)
        {
            return document.clone();
        }
        let document = Arc::new(parse(markdown, open, resolve));
        *parsed = Some((open.clone(), epoch, document.clone()));
        document
    }
}

/// Stretches nest at most this deep; a deeper `{{` is text.
const MAX_STRETCH_DEPTH: usize = 8;
/// Stretches kept per text; later ones stay text.
const MAX_STRETCHES: usize = 256;

/// `{{short|long}}` in the source, by byte offsets of `{{`, `|` and `}}`.
#[derive(Debug, Default)]
struct Stretch {
    open: usize,
    bar: usize,
    close: usize,
    /// The stretches inside, in source order.
    inner: Vec<Stretch>,
}

/// The source to parse: each stretch outside code becomes its long form if
/// `open` has its id, else a link holding its short form. A stretch without
/// `|`, with a multi-line short form, or a `{{` or `}}` without its partner
/// stays text.
fn expand(markdown: &str, open: &Open) -> String {
    if !markdown.contains("{{") {
        return markdown.to_owned();
    }
    let stretches = find(markdown);
    let mut out = String::with_capacity(markdown.len());
    let mut id = String::new();
    emit(
        markdown,
        0..markdown.len(),
        &stretches,
        open,
        &mut id,
        &mut out,
    );
    out
}

/// Byte ranges of code spans and code blocks, where braces are text.
fn code_ranges(markdown: &str) -> Vec<Range<usize>> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    Parser::new_ext(markdown, options)
        .into_offset_iter()
        .filter_map(|(event, range)| {
            matches!(event, Event::Code(_) | Event::Start(Tag::CodeBlock(_))).then_some(range)
        })
        .collect()
}

fn find(markdown: &str) -> Vec<Stretch> {
    let bytes = markdown.as_bytes();
    let code = code_ranges(markdown);
    let mut code = code.iter().peekable();
    // Open `{{`s: where, their `|`, and the stretches found inside so far.
    let mut stack: Vec<(usize, Option<usize>, Vec<Stretch>)> = Vec::new();
    let mut roots = Vec::new();
    // `{{` past the depth cap, so their `}}` close nothing.
    let mut skipped = 0_usize;
    let mut count = 0_usize;
    let mut at = 0;
    while at < bytes.len() {
        while code.peek().is_some_and(|range| range.end <= at) {
            code.next();
        }
        if let Some(range) = code.peek()
            && range.start <= at
        {
            at = range.end;
            continue;
        }
        let pair = |byte: u8| bytes[at] == byte && bytes.get(at + 1) == Some(&byte);
        if pair(b'{') {
            if stack.len() < MAX_STRETCH_DEPTH {
                stack.push((at, None, Vec::new()));
            } else {
                skipped += 1;
            }
            at += 2;
        } else if pair(b'}') {
            if skipped > 0 {
                skipped -= 1;
            } else if let Some((open, bar, inner)) = stack.pop() {
                let parent = stack.last_mut().map_or(&mut roots, |(_, _, inner)| inner);
                match bar {
                    Some(bar) if count < MAX_STRETCHES && !markdown[open..bar].contains('\n') => {
                        count += 1;
                        parent.push(Stretch {
                            open,
                            bar,
                            close: at,
                            inner,
                        });
                    }
                    // Not a stretch; whole ones inside it still are.
                    _ => parent.extend(inner),
                }
            }
            at += 2;
        } else {
            if bytes[at] == b'|'
                && let Some((_, bar @ None, _)) = stack.last_mut()
            {
                *bar = Some(at);
            }
            at += 1;
        }
    }
    // Unclosed `{{`s are text; whole stretches inside them still count.
    for (_, _, inner) in stack {
        roots.extend(inner);
    }
    roots.sort_by_key(|stretch| stretch.open);
    roots
}

/// Writes `range` of `markdown` with its `stretches` resolved; `id` is the
/// enclosing stretch's id, empty at the root.
fn emit(
    markdown: &str,
    range: Range<usize>,
    stretches: &[Stretch],
    open: &Open,
    id: &mut String,
    out: &mut String,
) {
    let mut at = range.start;
    for (index, stretch) in stretches.iter().enumerate() {
        out.push_str(&markdown[at..stretch.open]);
        let len = id.len();
        if !id.is_empty() {
            id.push('.');
        }
        id.push_str(&index.to_string());
        let inner_start = stretch
            .inner
            .partition_point(|inner| inner.open < stretch.bar);
        if open.contains(id.as_str()) {
            emit(
                markdown,
                stretch.bar + 1..stretch.close,
                &stretch.inner[inner_start..],
                open,
                id,
                out,
            );
        } else {
            out.push('[');
            if stretch.bar == stretch.open + 2 {
                // `{{|long}}`: an empty link can't be clicked.
                out.push('…');
            } else {
                short(
                    markdown,
                    stretch.open + 2..stretch.bar,
                    &stretch.inner[..inner_start],
                    out,
                );
            }
            out.push_str("](");
            out.push_str(STRETCH_SCHEME);
            out.push_str(id);
            out.push(')');
        }
        id.truncate(len);
        at = stretch.close + 2;
    }
    out.push_str(&markdown[at..range.end]);
}

/// A short form as link text: stretches inside show their own short form,
/// and brackets are escaped so the link holds.
fn short(markdown: &str, range: Range<usize>, stretches: &[Stretch], out: &mut String) {
    let mut at = range.start;
    let text = |text: &str, out: &mut String| {
        for c in text.chars() {
            if matches!(c, '[' | ']') {
                out.push('\\');
            }
            out.push(c);
        }
    };
    for stretch in stretches {
        text(&markdown[at..stretch.open], out);
        let inner = stretch
            .inner
            .partition_point(|inner| inner.open < stretch.bar);
        short(
            markdown,
            stretch.open + 2..stretch.bar,
            &stretch.inner[..inner],
            out,
        );
        at = stretch.close + 2;
    }
    text(&markdown[at..range.end], out);
}

#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    text: String,
    spans: Vec<(Range<usize>, u8)>,
    links: Vec<(Range<usize>, SharedString)>,
    stretches: Vec<(Range<usize>, SharedString)>,
    paths: Vec<(Range<usize>, Arc<ChatFile>)>,
    /// Code spans looked up as paths so far, up to [`MAX_PATHS`].
    candidates: usize,
    /// A lookup hadn't answered.
    pending: bool,
    /// The inline styles open now.
    styles: Vec<u8>,
    /// The open link's start in `text` and its target.
    link: Option<(usize, String)>,
    /// Each open list's next number; `None` for bullets.
    lists: Vec<Option<u64>>,
    quote: u8,
    marker: Option<SharedString>,
    heading: Option<u8>,
    code: Option<String>,
    table: Option<Table>,
    /// Table cells kept so far in the whole document, up to [`MAX_TABLE_CELLS`].
    cells: usize,
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
    head: bool,
    /// Cells past the document's budget were dropped.
    truncated: bool,
}

impl Builder {
    fn event(&mut self, event: Event<'_>, resolve: Resolve<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Html(text) | Event::InlineHtml(text) => self.push(&text, 0),
            Event::Code(text) => self.code(&text, resolve),
            Event::InlineMath(text) | Event::DisplayMath(text) => self.push(&text, CODE),
            Event::SoftBreak => self.push(" ", 0),
            Event::HardBreak => self.push("\n", 0),
            Event::Rule => {
                self.flush();
                self.block(Kind::Rule, String::new());
            }
            Event::TaskListMarker(checked) => {
                self.marker = Some(if checked { "☑" } else { "☐" }.into());
            }
            Event::FootnoteReference(name) => self.push(&format!("[{name}]"), 0),
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph | Tag::HtmlBlock => self.flush(),
            Tag::Heading { level, .. } => {
                self.flush();
                self.heading = Some(level as u8);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_add(1);
            }
            Tag::CodeBlock(_) => {
                self.flush();
                self.code = Some(String::new());
            }
            Tag::List(start) => {
                self.flush();
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                self.marker = Some(match self.lists.last_mut() {
                    Some(Some(number)) => {
                        let marker = format!("{number}.");
                        *number = number.saturating_add(1);
                        marker.into()
                    }
                    _ => "•".into(),
                });
            }
            Tag::Table(_) => {
                self.flush();
                self.table = Some(Table::default());
            }
            Tag::TableHead => {
                if let Some(table) = &mut self.table {
                    table.head = true;
                }
            }
            // An image shows only its alt text; nothing is fetched.
            Tag::Emphasis | Tag::Image { .. } => self.styles.push(ITALIC),
            Tag::Strong => self.styles.push(BOLD),
            Tag::Strikethrough => self.styles.push(STRIKE),
            Tag::Link { dest_url, .. } => {
                self.styles.push(if dest_url.starts_with(STRETCH_SCHEME) {
                    STRETCH
                } else {
                    LINK
                });
                self.link = Some((self.text.len(), dest_url.into_string()));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::HtmlBlock => self.flush(),
            TagEnd::Heading(_) => {
                self.flush();
                self.heading = None;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                if let Some(mut code) = self.code.take() {
                    code.truncate(code.trim_end_matches('\n').len());
                    self.block(Kind::Code, code);
                }
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::Item => {
                self.flush();
                self.marker = None;
            }
            TagEnd::TableCell => {
                if let Some(table) = &mut self.table {
                    let cell = std::mem::take(&mut table.cell);
                    // The parser pads short rows to the header's width, so a
                    // few bytes of markdown can stand for many cells.
                    if self.cells < MAX_TABLE_CELLS {
                        self.cells += 1;
                        table.row.push(cell.trim().to_owned());
                    } else {
                        table.truncated = true;
                    }
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.row);
                    let head = std::mem::take(&mut table.head);
                    // Every row has a cell, so an empty one was dropped whole;
                    // kept, it would read as the header's end.
                    if !row.is_empty() {
                        table.rows.push(row);
                        if head {
                            table.rows.push(Vec::new());
                        }
                    }
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    let mut text = table.render();
                    if table.truncated {
                        text.push_str(if text.is_empty() { "…" } else { "\n…" });
                    }
                    self.block(Kind::Table, text);
                    if let Some(block) = self.blocks.last_mut() {
                        block.grid = (!table.truncated).then(|| table.grid()).flatten();
                    }
                }
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Image => {
                self.styles.pop();
            }
            TagEnd::Link => {
                // A stretch's short form expands; it never reaches the opener.
                if let Some((start, url)) = self
                    .link
                    .take_if(|(_, url)| url.starts_with(STRETCH_SCHEME))
                {
                    self.styles.pop();
                    if start < self.text.len() {
                        let id = &url[STRETCH_SCHEME.len()..];
                        self.stretches
                            .push((start..self.text.len(), id.to_owned().into()));
                    }
                    return;
                }
                let web = self.link.as_ref().is_some_and(|(start, url)| {
                    *start < self.text.len() && Target::web_url(url).is_some()
                });
                if web {
                    self.push(WEB_ARROW, 0);
                }
                self.styles.pop();
                if let Some((start, url)) = self.link.take()
                    && start < self.text.len()
                    && Target::web_url(&url).is_some()
                {
                    self.links.push((start..self.text.len(), url.into()));
                }
            }
            _ => {}
        }
    }

    /// Prose, with its bare web URLs made links.
    fn text(&mut self, text: &str) {
        // Link text, code blocks and table cells hold no links of their own.
        if self.link.is_some() || self.code.is_some() || self.table.is_some() {
            return self.push(text, 0);
        }
        let mut at = 0;
        for url in bare_urls(text) {
            self.push(&text[at..url.start], 0);
            let start = self.text.len();
            self.push(&text[url.clone()], LINK);
            self.push(WEB_ARROW, LINK);
            self.links
                .push((start..self.text.len(), text[url.clone()].to_owned().into()));
            at = url.end;
        }
        self.push(&text[at..], 0);
    }

    /// An inline code span; one naming a file that exists can be clicked.
    fn code(&mut self, text: &str, resolve: Resolve<'_>) {
        if self.link.is_none()
            && self.table.is_none()
            && self.candidates < MAX_PATHS
            && path_candidate(text)
        {
            self.candidates += 1;
            match resolve(text) {
                Lookup::Found(Some(file)) => {
                    let start = self.text.len();
                    self.push(text, CODE | PATH);
                    if start < self.text.len() {
                        self.paths.push((start..self.text.len(), file));
                    }
                    return;
                }
                // Plain until the worker answers; then this parse is redone.
                Lookup::Pending => self.pending = true,
                Lookup::Found(None) => {}
            }
        }
        self.push(text, CODE);
    }

    fn push(&mut self, text: &str, style: u8) {
        if let Some(code) = &mut self.code {
            code.push_str(text);
            return;
        }
        if let Some(table) = &mut self.table {
            table.cell.push_str(text);
            return;
        }
        if text.is_empty() {
            return;
        }
        let style = self.styles.iter().fold(style, |all, style| all | style);
        let start = self.text.len();
        self.text.push_str(text);
        if style == 0 {
            return;
        }
        match self.spans.last_mut() {
            Some((range, last)) if *last == style && range.end == start => {
                range.end = self.text.len();
            }
            _ => self.spans.push((start..self.text.len(), style)),
        }
    }

    /// Ends the text block being built, if it has any text.
    fn flush(&mut self) {
        if let Some(table) = &mut self.table {
            // A block inside a cell, which GFM doesn't produce; keep its text in the cell.
            table.cell.push_str(&std::mem::take(&mut self.text));
            return;
        }
        let len = self.text.trim_end().len();
        self.text.truncate(len);
        let mut spans = std::mem::take(&mut self.spans);
        let mut links = std::mem::take(&mut self.links);
        let mut stretches = std::mem::take(&mut self.stretches);
        let mut paths = std::mem::take(&mut self.paths);
        for (range, _) in &mut spans {
            range.end = range.end.min(len);
        }
        spans.retain(|(range, _)| !range.is_empty());
        for (range, _) in links.iter_mut().chain(&mut stretches) {
            range.end = range.end.min(len);
        }
        for (range, _) in &mut paths {
            range.end = range.end.min(len);
        }
        links.retain(|(range, _)| !range.is_empty());
        stretches.retain(|(range, _)| !range.is_empty());
        paths.retain(|(range, _)| !range.is_empty());
        if self.text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.text);
        self.blocks.push(Block {
            kind: self.heading.map_or(Kind::Paragraph, Kind::Heading),
            text: text.into(),
            spans,
            links,
            depth: u8::try_from(self.lists.len()).unwrap_or(u8::MAX),
            quote: self.quote,
            marker: self.marker.take(),
            grid: None,
            stretches,
            paths,
        });
    }

    fn block(&mut self, kind: Kind, text: String) {
        self.blocks.push(Block {
            kind,
            text: text.into(),
            spans: Vec::new(),
            links: Vec::new(),
            depth: u8::try_from(self.lists.len()).unwrap_or(u8::MAX),
            quote: self.quote,
            // A list item can start with code, a table or a rule.
            marker: self.marker.take(),
            grid: None,
            stretches: Vec::new(),
            paths: Vec::new(),
        });
    }
}

/// The bare web URLs in prose, by byte range: each starts a word and ends
/// before whitespace, with trailing punctuation and an unbalanced `)` left out.
/// Only what [`Target::web_url`] accepts counts.
fn bare_urls(text: &str) -> Vec<Range<usize>> {
    let mut urls = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find("http") {
        let start = from + found;
        from = start + 4;
        if text[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || matches!(c, '/' | '_' | '-' | '.' | '@'))
        {
            continue;
        }
        let end = text[start..]
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`'))
            .map_or(text.len(), |end| start + end);
        // Longer than any URL `web_url` takes, so not worth trimming.
        if end - start > MAX_URL {
            from = end;
            continue;
        }
        let url = trim_url(&text[start..end]);
        if Target::web_url(url).is_some() {
            from = start + url.len();
            urls.push(start..from);
        }
    }
    urls
}

const MAX_URL: usize = 2048;

/// A URL without the sentence's punctuation after it: `.,;:!?'` and a `)`
/// that closes nothing in the URL. One pass from the end, counting parens once.
fn trim_url(url: &str) -> &str {
    let opens = url.matches('(').count();
    let mut closes = url.matches(')').count();
    let mut end = url.len();
    while let Some(c) = url[..end].chars().next_back() {
        match c {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' => {}
            ')' if closes > opens => closes -= 1,
            _ => break,
        }
        end -= c.len_utf8();
    }
    &url[..end]
}

/// Table cells kept per document; the rest are dropped, marked by a `…` line.
pub(crate) const MAX_TABLE_CELLS: usize = 4096;
/// The widest cell a column pads to; a longer one is left as it is.
const MAX_CELL: usize = 40;
/// A table with more cells, or longer ones, draws as text instead of a grid.
const MAX_GRID_CELLS: usize = 1024;
const MAX_GRID_CELL: usize = 2048;
/// A column is laid out at most this many chars wide; longer cells wrap.
const MAX_COLUMN: u16 = 36;

impl Table {
    /// The cells as a grid, unless the table is past the grid's caps.
    fn grid(&self) -> Option<Box<Grid>> {
        let rows: Vec<&Vec<String>> = self.rows.iter().filter(|row| !row.is_empty()).collect();
        let cells = rows.iter().map(|row| row.len()).sum::<usize>();
        if rows.is_empty()
            || cells > MAX_GRID_CELLS
            || rows
                .iter()
                .flat_map(|row| row.iter())
                .any(|cell| cell.len() > MAX_GRID_CELL)
        {
            return None;
        }
        let columns = rows.iter().map(|row| row.len()).max().unwrap_or(0);
        let widths = (0..columns)
            .map(|column| {
                rows.iter()
                    .filter_map(|row| row.get(column))
                    .map(|cell| u16::try_from(cell.chars().count()).unwrap_or(u16::MAX))
                    .max()
                    .unwrap_or(0)
                    .clamp(3, MAX_COLUMN)
            })
            .collect();
        Some(Box::new(Grid {
            rows: rows
                .iter()
                .map(|row| {
                    (0..columns)
                        .map(|column| {
                            row.get(column)
                                .map_or_else(SharedString::default, |cell| cell.clone().into())
                        })
                        .collect()
                })
                .collect(),
            // The header's end is marked by an empty row after it.
            head: self.rows.get(1).is_some_and(Vec::is_empty),
            widths,
        }))
    }

    /// Columns padded to their widest cell up to [`MAX_CELL`] chars, counted
    /// as chars rather than display width; an empty row marks the header's end.
    fn render(&self) -> String {
        let columns = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        let widths: Vec<usize> = (0..columns)
            .map(|column| {
                self.rows
                    .iter()
                    .filter_map(|row| row.get(column))
                    .map(|cell| cell.chars().count())
                    .filter(|width| *width <= MAX_CELL)
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let mut out = String::new();
        for row in &self.rows {
            if !out.is_empty() {
                out.push('\n');
            }
            if row.is_empty() {
                let rule: Vec<String> = widths.iter().map(|width| "─".repeat(*width)).collect();
                out.push_str(&rule.join("─┼─"));
                continue;
            }
            for (column, (cell, width)) in row.iter().zip(&widths).enumerate() {
                if column > 0 {
                    out.push_str(" │ ");
                }
                out.push_str(cell);
                if column + 1 < row.len() {
                    let width = width.saturating_sub(cell.chars().take(*width).count());
                    out.extend(std::iter::repeat_n(' ', width));
                }
            }
        }
        out
    }
}

/// Nesting deeper than this draws at this depth, so text keeps its width.
const MAX_INDENT: u8 = 6;

/// Draws `document`; its element ids are `row`'s, numbered from 16.
pub(crate) fn render(scope: &Scope<'_>, document: &Document, row: &SharedString) -> AnyElement {
    let m = scope.model.metrics;
    let theme = &scope.model.theme;
    div()
        .flex()
        .flex_col()
        .gap(m.spacing6())
        .w_full()
        .min_w_0()
        .children(document.blocks.iter().enumerate().map(|(at, block)| {
            let id = ElementId::from((row.clone(), 16 + 2 * at));
            let content = match block.kind {
                Kind::Rule => div()
                    .my(m.spacing3())
                    .h(px(1.0))
                    .w_full()
                    .bg(theme.border)
                    .into_any_element(),
                Kind::Table if block.grid.is_some() => block.grid.as_deref().map_or_else(
                    || div().into_any_element(),
                    |grid| table(scope, grid, id, row, at),
                ),
                Kind::Code | Kind::Table => code(
                    scope,
                    block,
                    ElementId::from((row.clone(), 17 + 2 * at)),
                    row,
                    at,
                ),
                Kind::Paragraph | Kind::Heading(_) => text(scope, block, id, row, at),
            };
            let depth = block.depth.saturating_sub(1).min(MAX_INDENT);
            // Each level starts where its parent item's text does.
            let indent = m.spacing9() * f32::from(depth)
                + m.spacing6() * f32::from(block.quote.min(MAX_INDENT));
            div()
                .flex()
                .w_full()
                .min_w_0()
                .pl(indent)
                .when(
                    matches!(block.kind, Kind::Heading(_)) && at > 0,
                    |heading| heading.pt(m.spacing4()),
                )
                .when(block.quote > 0, |quoted| {
                    quoted.border_l_2().border_color(theme.border)
                })
                .when_some(block.marker.clone(), |item, marker| {
                    item.child(
                        div()
                            .flex_none()
                            // Fixed, so "9." and "10." items keep their text in line.
                            .w(m.spacing9())
                            .text_size(m.font_headline())
                            .line_height(relative(1.6))
                            .text_color(theme.fg_muted)
                            .child(marker),
                    )
                })
                .when(block.marker.is_none() && block.depth > 0, |item| {
                    item.pl(indent + m.spacing9())
                })
                .child(div().flex_1().min_w_0().child(content))
                .into_any_element()
        }))
        .into_any_element()
}

/// A table as rows of cells: columns sized to their content up to
/// [`MAX_COLUMN`] chars, wrapping past it, scrolling sideways when too wide.
fn table(
    scope: &Scope<'_>,
    grid: &Grid,
    id: ElementId,
    row: &SharedString,
    block: usize,
) -> AnyElement {
    let m = scope.model.metrics;
    let theme = &scope.model.theme;
    let size = m.font_body();
    let pad = m.spacing5();
    // An average proportional char; columns only need to be about right.
    let widths: Vec<_> = grid
        .widths
        .iter()
        .map(|chars| size * (0.56 * f32::from(*chars)) + pad * 2.0)
        .collect();
    let last = grid.rows.len().saturating_sub(1);
    let mut bases = super::select::cell_bases(grid);
    div()
        .id(id)
        .w_full()
        .min_w_0()
        .overflow_x_scroll()
        .rounded(m.radius_lg())
        .border_1()
        .border_color(theme.border)
        .text_size(size)
        .line_height(relative(1.5))
        .text_color(theme.fg)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .children(grid.rows.iter().enumerate().map(|(index, cells)| {
                    let head = grid.head && index == 0;
                    let mut cells_row = div();
                    for (cell, width) in cells.iter().zip(&widths) {
                        let base = bases.next().unwrap_or_default();
                        cells_row =
                            cells_row.child(
                                div().flex_none().w(*width).px(pad).py(m.spacing3()).child(
                                    super::select::plain(scope, row, block, base, cell.clone()),
                                ),
                            );
                    }
                    cells_row
                        .flex()
                        .flex_none()
                        .when(index < last, |row| {
                            row.border_b_1().border_color(theme.border)
                        })
                        .when(head, |row| {
                            row.bg(theme.surface).font_weight(FontWeight::SEMIBOLD)
                        })
                })),
        )
        .into_any_element()
}

fn text(
    scope: &Scope<'_>,
    block: &Block,
    id: ElementId,
    row: &SharedString,
    at: usize,
) -> AnyElement {
    let (content, spans, links, stretches, paths) = (
        block.text.clone(),
        &block.spans,
        &block.links,
        &block.stretches,
        &block.paths,
    );
    let m = scope.model.metrics;
    let theme = &scope.model.theme;
    let (size, weight) = match block.kind {
        Kind::Heading(1) => (m.font_display(), FontWeight::SEMIBOLD),
        Kind::Heading(2) => (m.font_title_large(), FontWeight::SEMIBOLD),
        Kind::Heading(3) => (m.font_title(), FontWeight::SEMIBOLD),
        Kind::Heading(_) => (m.font_headline(), FontWeight::SEMIBOLD),
        _ => (m.font_headline(), FontWeight::NORMAL),
    };
    // Runs rather than highlights, since only a run can change the font.
    let mut base = scope.window.text_style();
    base.color = if block.quote > 0 {
        theme.fg_muted
    } else {
        theme.fg
    };
    base.font_weight = weight;
    let mono = muxy_ui::font::monospace();
    let len = content.len();
    let mut runs = Vec::with_capacity(spans.len() * 2 + 1);
    let mut start = 0;
    for (range, style) in spans {
        // The parser keeps spans sorted and inside the text; a bad one is skipped, not a panic.
        if range.start < start || range.end > len {
            continue;
        }
        if start < range.start {
            runs.push(base.to_run(range.start - start));
        }
        let mut run = base
            .clone()
            .highlight(highlight(*style, theme))
            .to_run(range.len());
        if style & CODE != 0 {
            run.font.family = mono.clone();
        }
        runs.push(run);
        start = range.end;
    }
    if start < len {
        runs.push(base.to_run(len - start));
    }
    let styled = StyledText::new(content).with_runs(runs);
    let layout = styled.layout().clone();
    let body = if links.is_empty() && stretches.is_empty() && paths.is_empty() {
        styled.into_any_element()
    } else {
        let ranges = links
            .iter()
            .map(|(range, _)| range.clone())
            .chain(stretches.iter().map(|(range, _)| range.clone()))
            .chain(paths.iter().map(|(range, _)| range.clone()))
            .collect();
        let urls: Vec<SharedString> = links.iter().map(|(_, url)| url.clone()).collect();
        let opens: Vec<SharedString> = stretches.iter().map(|(_, id)| id.clone()).collect();
        let files: Rc<[(Range<usize>, Arc<ChatFile>)]> = paths.as_slice().into();
        let (owner, pane, key, row) = (scope.owner.clone(), scope.pane, scope.key, row.clone());
        let guard = super::select::Guard::new(scope);
        let tips = files.clone();
        let (raised, fg, border, bg) = (theme.raised(), theme.fg, theme.border, theme.bg);
        let text = InteractiveText::new(id, styled).on_click(ranges, move |index, window, cx| {
            // A drag that started on a link selected text instead.
            if guard.dragged() {
                return;
            }
            if let Some(url) = urls.get(index) {
                let _ = owner.update(cx, |model, cx| model.chat_open_link(pane, url, cx));
            } else if let Some(stretch) = opens.get(index - urls.len()) {
                let stretch = (row.clone(), stretch.clone());
                let _ = owner.update(cx, |model, cx| model.chat_stretch(key, stretch, cx));
            } else if let Some((_, file)) = files.get(index - urls.len() - opens.len()) {
                let reveal = window.modifiers().platform;
                let _ = owner.update(cx, |model, cx| model.chat_open_path(pane, file, reveal, cx));
            }
        });
        if tips.is_empty() {
            text.into_any_element()
        } else {
            // The whole path, built only while hovered.
            text.tooltip(move |index, _, cx| {
                let (_, file) = tips.iter().find(|(range, _)| range.contains(&index))?;
                let path = SharedString::from(file.location.path.to_string_lossy().into_owned());
                Some(
                    cx.new(|_| Tooltip::new(path, raised, fg, border, bg))
                        .into(),
                )
            })
            .into_any_element()
        }
    };
    div()
        .w_full()
        .min_w_0()
        .text_size(size)
        .line_height(relative(1.6))
        .child(super::select::mark(scope, row, at, 0, len, layout, body))
        .into_any_element()
}

fn highlight(style: u8, theme: &muxy_ui::theme::Theme) -> HighlightStyle {
    HighlightStyle {
        font_weight: (style & BOLD != 0).then_some(FontWeight::SEMIBOLD),
        font_style: (style & ITALIC != 0).then_some(FontStyle::Italic),
        strikethrough: (style & STRIKE != 0).then_some(StrikethroughStyle {
            thickness: px(1.0),
            color: None,
        }),
        background_color: (style & CODE != 0).then_some(theme.surface),
        color: (style & LINK != 0).then_some(theme.accent),
        // A static underline, not one on hover: that would redraw the text each mouse move.
        underline: if style & STRETCH != 0 {
            Some(UnderlineStyle {
                thickness: px(1.0),
                color: Some(theme.accent.opacity(0.7)),
                wavy: false,
            })
        } else if style & PATH != 0 {
            Some(super::rows::path_underline(theme))
        } else {
            None
        },
        fade_out: None,
    }
}

fn code(
    scope: &Scope<'_>,
    block: &Block,
    copy_id: ElementId,
    row: &SharedString,
    at: usize,
) -> AnyElement {
    let m = scope.model.metrics;
    let theme = &scope.model.theme;
    let copy = (block.kind == Kind::Code).then(|| {
        let (text, owner) = (block.text.clone(), scope.owner.clone());
        let guard = super::select::Guard::new(scope);
        IconButton::new(
            copy_id,
            tr!("Copy"),
            Icon::Overlap,
            m.icon_xs(),
            m.control_small(),
            theme.fg_muted,
            theme.fg,
        )
        .tooltip(theme)
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            if !guard.dragged() {
                let _ = owner.update(cx, |model, cx| model.chat_copy_text(text.to_string(), cx));
            }
        })
    });
    div()
        .relative()
        .w_full()
        .min_w_0()
        .px(m.spacing7())
        .py(m.spacing6())
        .when(copy.is_some(), |code| {
            code.pr(m.control_small() + m.spacing7())
        })
        .rounded(m.radius_lg())
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface)
        .font_family(muxy_ui::font::monospace())
        .text_size(m.font_body())
        .line_height(relative(1.5))
        .text_color(theme.fg)
        .child(super::select::plain(scope, row, at, 0, block.text.clone()))
        .when_some(copy, |code, copy| {
            code.child(
                div()
                    .absolute()
                    .top(m.spacing3())
                    .right(m.spacing3())
                    .child(copy),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(markdown: &str) -> Document {
        super::parse(markdown, &Open::new(), &|_| Lookup::Found(None))
    }

    fn opened(markdown: &str, ids: &[&str]) -> Document {
        let open = ids
            .iter()
            .map(|id| SharedString::from(id.to_string()))
            .collect();
        super::parse(markdown, &open, &|_| Lookup::Found(None))
    }

    fn stretches(block: &Block) -> Vec<(&str, &str)> {
        block
            .stretches
            .iter()
            .map(|(range, id)| (&block.text[range.clone()], id.as_ref()))
            .collect()
    }

    const ATTENTION: &str = "Done.\n\n{{Three things still need attention.|Three things still need attention:\n\n\
        - **Your Time Machine backup is still failing** because the NAS is gone.\n\
        - **The crash dumps will build up again.** Nothing cleans them.\n\
        - **The other three agent worktrees are still there.** I left them.}}\n";

    #[test]
    fn a_stretch_whose_long_form_is_a_paragraph_and_a_list_expands_into_blocks() {
        let collapsed = parse(ATTENTION);
        assert_eq!(
            kinds(&collapsed),
            [
                (Kind::Paragraph, "Done.", 0, None),
                (
                    Kind::Paragraph,
                    "Three things still need attention.",
                    0,
                    None
                ),
            ]
        );
        let short = &collapsed.blocks[1];
        assert_eq!(
            stretches(short),
            [("Three things still need attention.", "0")]
        );
        assert_eq!(short.spans, [(0..short.text.len(), STRETCH)]);
        assert!(short.links.is_empty());

        let expanded = opened(ATTENTION, &["0"]);
        assert_eq!(
            kinds(&expanded),
            [
                (Kind::Paragraph, "Done.", 0, None),
                (
                    Kind::Paragraph,
                    "Three things still need attention:",
                    0,
                    None
                ),
                (
                    Kind::Paragraph,
                    "Your Time Machine backup is still failing because the NAS is gone.",
                    1,
                    Some("•")
                ),
                (
                    Kind::Paragraph,
                    "The crash dumps will build up again. Nothing cleans them.",
                    1,
                    Some("•")
                ),
                (
                    Kind::Paragraph,
                    "The other three agent worktrees are still there. I left them.",
                    1,
                    Some("•")
                ),
            ]
        );
        let item = &expanded.blocks[3];
        assert_eq!(item.spans, [(0..36, BOLD)], "{:?}", &item.text[..36]);
        assert!(
            expanded
                .blocks
                .iter()
                .all(|block| block.stretches.is_empty())
        );
    }

    #[test]
    fn nested_stretches_open_one_level_at_a_time_and_keep_their_styles() {
        let markdown = "Disk {{full|full: {{logs|old logs}} and caches}} now.";
        let collapsed = &parse(markdown).blocks[0];
        assert_eq!(collapsed.text.as_ref(), "Disk full now.");
        assert_eq!(stretches(collapsed), [("full", "0")]);

        let one = &opened(markdown, &["0"]).blocks[0];
        assert_eq!(one.text.as_ref(), "Disk full: logs and caches now.");
        assert_eq!(stretches(one), [("logs", "0.0")]);

        let both = &opened(markdown, &["0", "0.0"]).blocks[0];
        assert_eq!(both.text.as_ref(), "Disk full: old logs and caches now.");
        assert!(both.stretches.is_empty());

        // An inner id alone does nothing while its outer one is collapsed.
        assert_eq!(
            opened(markdown, &["0.0"]).blocks[0].text.as_ref(),
            "Disk full now."
        );

        let styled = &parse("a {{**b** [c]|**b** `c|d`}} e").blocks[0];
        assert_eq!(styled.text.as_ref(), "a b [c] e");
        assert_eq!(styled.spans, [(2..3, BOLD | STRETCH), (3..7, STRETCH)]);
        let open = &opened("a {{**b**|**b** `c|d`}} e", &["0"]).blocks[0];
        assert_eq!(open.text.as_ref(), "a b c|d e");
        assert_eq!(open.spans, [(2..3, BOLD), (4..7, CODE)]);
    }

    #[test]
    fn braces_in_code_and_malformed_stretches_stay_literal() {
        for markdown in [
            "`{{a|b}}` in code",
            "``{{a|`b`}}`` in code",
            "```\n{{a|b}}\n```",
            "~~~\n{{a|b}}\n~~~",
            "text\n\n    {{a|b}}\n",
            "- item\n\n      {{a|b}}\n",
            "no bar {{here}} at all",
            "unclosed {{a|b",
            "stray }} and {{",
            "{{two\nlines|b}}",
        ] {
            assert_eq!(expand(markdown, &Open::new()), markdown, "{markdown}");
        }
        // An unclosed outer one leaves a whole inner one working.
        let block = &parse("x {{a {{b|c}} d").blocks[0];
        assert_eq!(block.text.as_ref(), "x {{a b d");
        assert_eq!(stretches(block), [("b", "0")]);
        // Code inside a long form stays code.
        let code = &opened("{{a|a `{{x|y}}`}}", &["0"]).blocks[0];
        assert_eq!(code.text.as_ref(), "a {{x|y}}");
        assert!(code.stretches.is_empty());
    }

    #[test]
    fn stretches_past_the_depth_cap_stay_text_without_breaking_the_rest() {
        let deep = "{{a|".repeat(12) + &"}}".repeat(12);
        let ids: Vec<String> = (1..=MAX_STRETCH_DEPTH)
            .map(|depth| vec!["0"; depth].join("."))
            .collect();
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let block = &opened(&deep, &ids).blocks[0];
        assert_eq!(block.text, "{{a|".repeat(4) + &"}}".repeat(4));
        assert!(block.stretches.is_empty());
        let shallower = &opened(&deep, &ids[..MAX_STRETCH_DEPTH - 1]).blocks[0];
        assert_eq!(stretches(shallower), [("a", ids[MAX_STRETCH_DEPTH - 1])]);
    }

    #[test]
    fn stretch_links_never_reach_the_url_opener() {
        let block = &parse("[x](muxy-stretch:3) and [y](https://example.com)").blocks[0];
        assert_eq!(stretches(block), [("x", "3")]);
        let urls: Vec<_> = block.links.iter().map(|(_, url)| url.as_ref()).collect();
        assert_eq!(urls, ["https://example.com"]);
        assert!(!block.text.starts_with("x\u{2009}"), "no web arrow");
        assert!(Target::web_url("muxy-stretch:3").is_none());
    }

    #[test]
    fn a_parse_is_kept_until_the_open_stretches_change() {
        let parsed = Parsed::default();
        let first = parsed.get("{{a|b}}", None, 0, &|_| Lookup::Found(None));
        assert!(Arc::ptr_eq(
            &first,
            &parsed.get("{{a|b}}", Some(&Open::new()), 0, &|_| Lookup::Found(None))
        ));
        let open = Open::from([SharedString::from("0")]);
        let expanded = parsed.get("{{a|b}}", Some(&open), 0, &|_| Lookup::Found(None));
        assert_eq!(expanded.blocks[0].text.as_ref(), "b");
        assert!(Arc::ptr_eq(
            &expanded,
            &parsed.get("{{a|b}}", Some(&open), 0, &|_| Lookup::Found(None))
        ));
    }

    fn urls(block: &Block) -> Vec<(&str, &str)> {
        block
            .links
            .iter()
            .map(|(range, url)| (&block.text[range.clone()], url.as_ref()))
            .collect()
    }

    #[test]
    fn bare_urls_link_without_their_trailing_punctuation() {
        for (text, url) in [
            ("see https://example.com.", "https://example.com"),
            ("(https://example.com/a)", "https://example.com/a"),
            (
                "at https://en.wikipedia.org/wiki/A_(b)).",
                "https://en.wikipedia.org/wiki/A_(b)",
            ),
            (
                "x https://a.dev/x_y_z?q=1&r=2!",
                "https://a.dev/x_y_z?q=1&r=2",
            ),
            ("'http://a.dev/p',", "http://a.dev/p"),
            ("ends https://a.dev/b:", "https://a.dev/b"),
        ] {
            let block = &parse(text).blocks[0];
            let arrow = format!("{url}{WEB_ARROW}");
            assert_eq!(urls(block), [(arrow.as_str(), url)], "{text}");
            assert!(
                block
                    .spans
                    .iter()
                    .any(|(range, style)| block.text[range.clone()] == arrow && *style == LINK),
                "{text}"
            );
        }
    }

    #[test]
    fn urls_in_code_links_or_other_schemes_are_not_linked_again() {
        for text in [
            "`https://example.com`",
            "```\nhttps://example.com\n```",
            "    https://example.com",
            "javascript:alert(1) ftp://a.dev file:///etc/passwd",
            "xhttps://a.dev https:// http://",
            "| a |\n|---|\n| https://a.dev |",
        ] {
            let document = parse(text);
            assert!(
                document.blocks.iter().all(|block| block.links.is_empty()),
                "{text}"
            );
        }
        let block = &parse("[docs](https://a.dev) and <https://b.dev>").blocks[0];
        assert_eq!(
            urls(block),
            [
                ("docs\u{2009}\u{2197}", "https://a.dev"),
                ("https://b.dev\u{2009}\u{2197}", "https://b.dev"),
            ]
        );
    }

    fn main_rs() -> Arc<ChatFile> {
        Arc::new(ChatFile {
            location: muxy_app_core::opener::FileLocation {
                path: "/work/src/main.rs".into(),
                line: Some(12),
                column: Some(3),
            },
            reveal: false,
        })
    }

    #[test]
    fn only_code_naming_a_found_file_links_and_each_is_asked_once() {
        let asked = RefCell::new(Vec::new());
        let resolve = |text: &str| {
            asked.borrow_mut().push(text.to_owned());
            Lookup::Found((text == "src/main.rs:12:3").then(main_rs))
        };
        let markdown = "Fixed `src/main.rs:12:3`, not `src/gone.rs` or `foo()`.\n\n            ```\nsrc/main.rs\n```\n\n[`src/main.rs:12:3`](https://a.dev)";
        let parsed = Parsed::default();
        let document = parsed.get(markdown, None, 0, &resolve);
        let block = &document.blocks[0];
        let paths: Vec<_> = block
            .paths
            .iter()
            .map(|(range, file)| {
                (
                    &block.text[range.clone()],
                    file.location.line,
                    file.location.column,
                )
            })
            .collect();
        assert_eq!(paths, [("src/main.rs:12:3", Some(12), Some(3))]);
        assert!(!document.pending);
        assert!(
            document
                .blocks
                .iter()
                .skip(1)
                .all(|block| block.paths.is_empty())
        );
        // Code blocks, link text and non-paths are never looked up.
        assert_eq!(*asked.borrow(), ["src/main.rs:12:3", "src/gone.rs"]);
        // Drawing again reuses the parse, even after other answers land.
        parsed.get(markdown, None, 1, &resolve);
        assert_eq!(asked.borrow().len(), 2);

        let many = "`a.rs` ".repeat(MAX_PATHS * 2);
        asked.borrow_mut().clear();
        super::parse(&many, &Open::new(), &resolve);
        assert_eq!(
            asked.borrow().len(),
            MAX_PATHS,
            "lookups per document are capped"
        );
    }

    #[test]
    fn unanswered_paths_draw_plain_and_link_once_their_answer_lands() {
        // The parse sees only what the cache knows; the disk is never asked.
        let answer = RefCell::new(Lookup::Pending);
        let resolve = |_: &str| answer.borrow().clone();
        let parsed = Parsed::default();
        let waiting = parsed.get("see `src/main.rs:12:3`", None, 0, &resolve);
        assert!(waiting.pending);
        assert!(waiting.blocks[0].paths.is_empty());
        assert_eq!(
            waiting.blocks[0].spans,
            [(4..20, CODE)],
            "plain code, no PATH"
        );
        // Same epoch: nothing landed, so the parse is kept.
        assert!(Arc::ptr_eq(
            &waiting,
            &parsed.get("see `src/main.rs:12:3`", None, 0, &resolve)
        ));

        *answer.borrow_mut() = Lookup::Found(Some(main_rs()));
        let linked = parsed.get("see `src/main.rs:12:3`", None, 1, &resolve);
        assert!(!linked.pending);
        assert_eq!(linked.blocks[0].paths.len(), 1);
        assert_eq!(linked.blocks[0].spans, [(4..20, CODE | PATH)]);
    }

    #[test]
    fn a_long_run_of_closing_parens_trims_in_one_pass() {
        let url = format!("https://a.dev/x{}", ")".repeat(100_000));
        let started = std::time::Instant::now();
        assert_eq!(trim_url(&url), "https://a.dev/x");
        assert_eq!(trim_url("https://a.dev/A_(b))."), "https://a.dev/A_(b)");
        // Past `web_url`'s length, prose isn't trimmed or linked at all.
        let document = parse(&format!("see {url} now"));
        assert!(document.blocks[0].links.is_empty());
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    fn kinds(document: &Document) -> Vec<(Kind, &str, u8, Option<&str>)> {
        document
            .blocks
            .iter()
            .map(|block| {
                (
                    block.kind,
                    block.text.as_ref(),
                    block.depth,
                    block.marker.as_ref().map(AsRef::as_ref),
                )
            })
            .collect()
    }

    #[test]
    fn nested_and_task_lists_flatten_with_their_markers_and_depth() {
        let document = parse("1. one\n   - inner\n   - [x] done\n2. two\n\n3) loose\n\n   more\n");
        assert_eq!(
            kinds(&document),
            [
                (Kind::Paragraph, "one", 1, Some("1.")),
                (Kind::Paragraph, "inner", 2, Some("•")),
                (Kind::Paragraph, "done", 2, Some("☑")),
                (Kind::Paragraph, "two", 1, Some("2.")),
                (Kind::Paragraph, "loose", 1, Some("3.")),
                (Kind::Paragraph, "more", 1, None),
            ]
        );
    }

    #[test]
    fn an_unclosed_fence_keeps_the_rest_as_code() {
        let document = parse("before\n```rust\nfn main() {\n# not a heading\n");
        assert_eq!(
            kinds(&document),
            [
                (Kind::Paragraph, "before", 0, None),
                (Kind::Code, "fn main() {\n# not a heading", 0, None),
            ]
        );
    }

    #[test]
    fn html_is_text_images_are_alt_text_and_only_web_links_click() {
        let document = parse(
            "<script>alert(1)</script>\n\nhi <b>x</b> ![a cat](http://img/cat.png) \
             [web](https://example.com) [js](javascript:alert(1)) [file](file:///etc/passwd)\n",
        );
        assert_eq!(
            document.blocks[0].text.as_ref(),
            "<script>alert(1)</script>"
        );
        let text = &document.blocks[1];
        assert_eq!(
            text.text.as_ref(),
            "hi <b>x</b> a cat web\u{2009}\u{2197} js file"
        );
        let urls: Vec<_> = text.links.iter().map(|(_, url)| url.as_ref()).collect();
        assert_eq!(urls, ["https://example.com"]);
        let (range, _) = &text.links[0];
        assert_eq!(
            &text.text[range.clone()],
            "web\u{2009}\u{2197}",
            "the arrow clicks too"
        );
        let styled: Vec<_> = text
            .spans
            .iter()
            .map(|(range, style)| (&text.text[range.clone()], *style))
            .collect();
        assert_eq!(
            styled,
            [
                ("a cat", ITALIC),
                ("web\u{2009}\u{2197}", LINK),
                ("js", LINK),
                ("file", LINK)
            ]
        );
    }

    #[test]
    fn nested_styles_merge_into_non_overlapping_spans_on_char_boundaries() {
        let block = &parse("é **bold *both* 🎉** `co🎉de` plain").blocks[0];
        let styled: Vec<_> = block
            .spans
            .iter()
            .map(|(range, style)| (&block.text[range.clone()], *style))
            .collect();
        assert_eq!(
            styled,
            [
                ("bold ", BOLD),
                ("both", BOLD | ITALIC),
                (" 🎉", BOLD),
                ("co🎉de", CODE)
            ]
        );
        assert!(
            block
                .spans
                .windows(2)
                .all(|pair| pair[0].0.end <= pair[1].0.start)
        );
    }

    #[test]
    fn tables_become_aligned_monospace_and_blocks_keep_quote_depth() {
        let document = parse("> | a | bb |\n> |---|---|\n> | ccc | `d` |\n\n---\n");
        assert_eq!(
            document.blocks[0].text.as_ref(),
            "a   │ bb\n────┼───\nccc │ d"
        );
        assert_eq!(
            (document.blocks[0].kind, document.blocks[0].quote),
            (Kind::Table, 1)
        );
        assert_eq!(document.blocks[1].kind, Kind::Rule);
    }

    #[test]
    fn a_table_with_huge_cells_and_many_rows_stays_about_as_long_as_its_markdown() {
        let huge = "x".repeat(128 * 1024);
        let mut markdown = format!("| a | b |\n|---|---|\n| {huge} | y |\n");
        markdown.push_str(&"| a | b |\n".repeat(20_000));
        let table = &parse(&markdown).blocks[0];
        assert_eq!(table.kind, Kind::Table);
        assert!(
            table.text.len() < markdown.len() * 2,
            "{} bytes from {} of markdown",
            table.text.len(),
            markdown.len()
        );

        // Under the cell budget, the huge cell isn't padded against.
        let small = format!("| a | b |\n|---|---|\n| {huge} | y |\n| c | d |\n");
        let table = &parse(&small).blocks[0];
        assert!(table.text.len() < huge.len() + 64);
        assert!(
            table.text.ends_with("c │ d"),
            "{:?}",
            &table.text[table.text.len() - 8..]
        );
    }

    #[test]
    fn padded_rows_cannot_make_a_document_hold_millions_of_cells() {
        // Short rows are padded to the header's 100 columns: 200K cells from
        // 14 KB, under pulldown-cmark's own cap of 2^18 padded cells.
        let header = format!("|{}\n|{}\n", "a|".repeat(100), "-|".repeat(100));
        let table = header + &"|x\n".repeat(100);
        let markdown = vec![table; 20].join("\n");
        let document = parse(&markdown);
        let tables: Vec<_> = document
            .blocks
            .iter()
            .filter(|block| block.kind == Kind::Table)
            .collect();
        let cells: usize = tables
            .iter()
            .map(|block| {
                block
                    .text
                    .lines()
                    .filter(|line| !line.starts_with('─') && *line != "…")
                    .map(|line| line.matches(" │ ").count() + 1)
                    .sum::<usize>()
            })
            .sum();
        let bytes: usize = document.blocks.iter().map(|block| block.text.len()).sum();

        assert_eq!(tables.len(), 20, "every table still shows");
        assert!(cells <= MAX_TABLE_CELLS, "{cells} cells");
        assert!(
            bytes < 4 * markdown.len(),
            "{bytes} bytes from {}",
            markdown.len()
        );
        assert!(tables[19].text.ends_with('…'), "the cut is marked");
        assert!(tables.iter().all(|block| block.grid.is_none()));
    }

    #[test]
    fn small_tables_become_grids_and_large_ones_stay_text() {
        let grid = parse("| Item | Freed |\n|---|---|\n| npm cache | ~2 GB |\n| x |\n")
            .blocks
            .remove(0)
            .grid
            .expect("a grid");
        assert!(grid.head);
        assert_eq!(grid.rows.len(), 3);
        assert_eq!(grid.rows[2].len(), 2, "short rows are padded");
        assert_eq!(grid.widths, [9, 5]);

        let mut large = "| a | b |\n|---|---|\n".to_owned();
        large.push_str(&"| a | b |\n".repeat(MAX_GRID_CELLS));
        assert!(parse(&large).blocks[0].grid.is_none());
    }

    #[test]
    fn a_very_long_line_stays_one_block() {
        let line = "word ".repeat(40_000) + &"x".repeat(100_000);
        let document = parse(&line);
        assert_eq!(document.blocks.len(), 1);
        assert_eq!(document.blocks[0].text.len(), line.trim_end().len());
    }
}
