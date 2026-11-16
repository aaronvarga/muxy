//! Fork: selecting chat text with the mouse and copying it. A position is
//! logical: a row's id, a block of the row's text and a byte offset into that
//! block's plain text, so a selection outlives scrolling and streaming. The
//! texts drawn each frame record where they landed in prepaint, for hit-testing.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, Pixels, Point, SharedString, StyledText, TextLayout, Window, point, px,
};
use muxy_app_core::opener::OpenContext;
use muxy_protocol::ChatItemKind;

use super::markdown::{self, Document};
use super::rows::Scope;
use crate::model::chat::{ChatSession, Row};

/// Where a drag starts selecting rather than clicking.
const DRAG_SLOP: f32 = 3.0;

/// A place in the conversation. Ordered by row, block, then offset; `at` is
/// the row's index when the place was made, `row` its id for when rows move.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Pos {
    pub(crate) at: usize,
    pub(crate) block: usize,
    pub(crate) offset: usize,
    pub(crate) row: SharedString,
}

/// A resolved place: row index, block, offset.
pub(crate) type Spot = (usize, usize, usize);

impl Pos {
    /// This place with its row's current index, or `None` once the row is gone.
    fn resolve(&self, session: &ChatSession) -> Option<Spot> {
        let at = if session
            .rows
            .get(self.at)
            .is_some_and(|row| row.key == self.row)
        {
            self.at
        } else {
            session.row_index(&self.row)?
        };
        Some((at, self.block, self.offset))
    }

    /// Brings `at` up to the row's current index, so places compare by where rows are now.
    fn refresh(&mut self, session: &ChatSession) {
        if let Some((at, ..)) = self.resolve(session) {
            self.at = at;
        }
    }

    /// The same place, whatever index the row had when each was made.
    fn same(&self, other: &Self) -> bool {
        (&self.row, self.block, self.offset) == (&other.row, other.block, other.offset)
    }
}

#[derive(Default)]
struct Selection {
    anchor: Option<Pos>,
    head: Option<Pos>,
    /// The word or block a double or triple click took, which a drag keeps.
    pivot: Option<(Pos, Pos)>,
    dragging: bool,
    down: Point<Pixels>,
    /// The last click was in the conversation and nothing was typed since, so ⌘A selects it.
    active: bool,
}

/// One text drawn this frame, where it landed.
pub(crate) struct Hit {
    at: usize,
    row: SharedString,
    block: usize,
    /// Where the drawn text starts in its block's plain text.
    base: usize,
    /// The length of the source text drawn, which the layout may show cut short with "…".
    len: usize,
    bounds: Bounds<Pixels>,
    layout: TextLayout,
}

/// A pane's selection and the texts its rows drew, shared by the rows and the chat.
#[derive(Default)]
pub(crate) struct Shared {
    selection: RefCell<Selection>,
    hits: RefCell<Vec<Hit>>,
    /// The pointer moved since the button went down, so the press selects rather than clicks.
    moved: Cell<bool>,
}

impl Shared {
    /// Forgets the texts drawn last frame; the rows draw again and record theirs.
    pub(crate) fn begin_frame(&self) {
        self.hits.borrow_mut().clear();
    }

    /// The text nearest `point`: the one under it, else the closest above or below.
    pub(crate) fn hit(&self, point: Point<Pixels>) -> Option<Pos> {
        let hits = self.hits.borrow();
        let hit = hits.iter().min_by(|a, b| {
            distance(a.bounds, point)
                .partial_cmp(&distance(b.bounds, point))
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        let index = hit
            .layout
            .index_for_position(point)
            .unwrap_or_else(|index| index);
        Some(Pos {
            at: hit.at,
            block: hit.block,
            // A hit on the ellipsis is the end of the text, never past it.
            offset: hit.base + index.min(hit.len),
            row: hit.row.clone(),
        })
    }

    /// The selection's ends in order, or `None` when nothing is selected.
    pub(crate) fn span(&self, session: &ChatSession) -> Option<(Spot, Spot)> {
        let selection = self.selection.borrow();
        let anchor = selection.anchor.as_ref()?.resolve(session)?;
        let head = selection.head.as_ref()?.resolve(session)?;
        (anchor != head).then(|| (anchor.min(head), anchor.max(head)))
    }

    /// Clears the selection; true when there was a visible one to clear, not a bare click's caret.
    pub(crate) fn clear(&self) -> bool {
        let mut selection = self.selection.borrow_mut();
        selection.active = false;
        selection.dragging = false;
        selection.pivot = None;
        match (selection.anchor.take(), selection.head.take()) {
            (Some(anchor), Some(head)) => !anchor.same(&head),
            (None, None) => false,
            _ => true,
        }
    }

    pub(crate) fn active(&self) -> bool {
        self.selection.borrow().active
    }

    pub(crate) fn dragging(&self) -> bool {
        self.selection.borrow().dragging
    }

    pub(crate) fn moved(&self) -> bool {
        self.moved.get()
    }

    pub(crate) fn end_drag(&self) {
        self.selection.borrow_mut().dragging = false;
    }

    /// A press at `position` on `range` of one block: a click, word or block.
    pub(crate) fn press(&self, position: Point<Pixels>, start: Pos, end: Pos, unit: bool) {
        self.moved.set(false);
        let mut selection = self.selection.borrow_mut();
        selection.pivot = unit.then(|| (start.clone(), end.clone()));
        selection.anchor = Some(start);
        selection.head = Some(end);
        selection.dragging = true;
        selection.down = position;
        selection.active = true;
    }

    /// A press that hit no text: nothing is selected, though ⌘A still selects the conversation.
    pub(crate) fn press_empty(&self) -> bool {
        self.moved.set(false);
        let cleared = self.clear();
        self.selection.borrow_mut().active = true;
        cleared
    }

    /// Shift-click: moves the selection's head to `head`; false with no selection to extend.
    pub(crate) fn extend_to(&self, position: Point<Pixels>, head: Pos) -> bool {
        self.moved.set(false);
        let mut selection = self.selection.borrow_mut();
        if selection.anchor.is_none() {
            return false;
        }
        selection.pivot = None;
        selection.head = Some(head);
        selection.dragging = true;
        selection.down = position;
        selection.active = true;
        true
    }

    /// The pointer moved to `position` while dragging; true when the selection changed.
    pub(crate) fn drag(&self, session: &ChatSession, position: Point<Pixels>) -> bool {
        let down = self.selection.borrow().down;
        if !self.moved.get() {
            let (dx, dy) = (position.x - down.x, position.y - down.y);
            if dx.abs() < px(DRAG_SLOP) && dy.abs() < px(DRAG_SLOP) {
                return false;
            }
            self.moved.set(true);
        }
        let Some(to) = self.hit(position) else {
            return false;
        };
        let mut selection = self.selection.borrow_mut();
        // Rows may have come in above since the places were made; compare by current index.
        if let Some(anchor) = &mut selection.anchor {
            anchor.refresh(session);
        }
        if let Some((start, end)) = &mut selection.pivot {
            start.refresh(session);
            end.refresh(session);
        }
        let Some(anchor) = selection.anchor.clone() else {
            return false;
        };
        let (anchor, head) = extend(selection.pivot.as_ref(), anchor, to);
        let changed =
            selection.anchor.as_ref() != Some(&anchor) || selection.head.as_ref() != Some(&head);
        selection.anchor = Some(anchor);
        selection.head = Some(head);
        changed
    }

    /// Selects every row the list shows.
    pub(crate) fn select_all(&self, session: &ChatSession) {
        let (Some(&first), Some(&last)) = (session.visible.first(), session.visible.last()) else {
            return;
        };
        let pos = |at: usize, end: usize| {
            session.rows.get(at).map(|row| Pos {
                at,
                block: end,
                offset: end,
                row: row.key.clone(),
            })
        };
        let (Some(anchor), Some(head)) = (pos(first, 0), pos(last, usize::MAX)) else {
            return;
        };
        let mut selection = self.selection.borrow_mut();
        selection.anchor = Some(anchor);
        selection.head = Some(head);
        selection.pivot = None;
        selection.active = true;
    }

    /// The selected text, or `None` when nothing is selected.
    pub(crate) fn copied(
        &self,
        session: &ChatSession,
        context: Option<&OpenContext>,
    ) -> Option<String> {
        let (start, end) = self.span(session)?;
        let rows: Vec<(usize, Vec<Part>)> = session
            .visible
            .iter()
            .filter(|at| (start.0..=end.0).contains(at))
            .filter_map(|&at| Some((at, parts(session, session.rows.get(at)?, context))))
            .collect();
        let text = assemble(&rows, start, end);
        (!text.is_empty()).then_some(text)
    }

    fn record(&self, hit: Hit) {
        self.hits.borrow_mut().push(hit);
    }
}

/// How far `point` is from `bounds`: vertically first, then sideways.
fn distance(bounds: Bounds<Pixels>, point: Point<Pixels>) -> (Pixels, Pixels) {
    let gap = |at: Pixels, low: Pixels, high: Pixels| {
        if at < low {
            low - at
        } else if at > high {
            at - high
        } else {
            px(0.0)
        }
    };
    (
        gap(point.y, bounds.top(), bounds.bottom()),
        gap(point.x, bounds.left(), bounds.right()),
    )
}

/// A drag from `anchor` to `to`: with a pivot word or block, the selection
/// always holds it and grows from its far end.
pub(crate) fn extend<T: Ord + Clone>(pivot: Option<&(T, T)>, anchor: T, to: T) -> (T, T) {
    match pivot {
        Some((start, end)) if to < *start => (end.clone(), to),
        Some((start, end)) if to > *end => (start.clone(), to),
        Some((start, end)) => (start.clone(), end.clone()),
        None => (anchor, to),
    }
}

/// `offset` moved back onto a char boundary of `text`, and into it.
pub(crate) fn floor(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// The word, run of spaces or run of punctuation at `offset`.
pub(crate) fn word(text: &str, offset: usize) -> Range<usize> {
    fn class(c: char) -> u8 {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    }
    let at = floor(text, offset);
    let Some(kind) = text[at..]
        .chars()
        .next()
        .or_else(|| text[..at].chars().next_back())
        .map(class)
    else {
        return at..at;
    };
    let start = text[..at]
        .char_indices()
        .rev()
        .take_while(|(_, c)| class(*c) == kind)
        .last()
        .map_or(at, |(index, _)| index);
    let end = text[at..]
        .char_indices()
        .find(|(_, c)| class(*c) != kind)
        .map_or(text.len(), |(index, _)| at + index);
    start..end
}

/// The part of `[base, base + len)` of block `(at, block)` that `span` covers, from `base`.
pub(crate) fn local(
    span: (Spot, Spot),
    (at, block): (usize, usize),
    base: usize,
    len: usize,
) -> Option<Range<usize>> {
    let ((start_at, start_block, start), (end_at, end_block, end)) = span;
    let here = (at, block);
    if here < (start_at, start_block) || here > (end_at, end_block) {
        return None;
    }
    let low = if here == (start_at, start_block) {
        start
    } else {
        0
    };
    let high = if here == (end_at, end_block) {
        end
    } else {
        usize::MAX
    };
    let low = low.clamp(base, base + len) - base;
    let high = high.clamp(base, base + len) - base;
    (low < high).then_some(low..high)
}

/// One block of a row as copied: its plain text, the list marker it copies
/// with when copied from its start, and whether it follows the block before
/// it on the next line rather than after a blank one.
#[derive(Debug, PartialEq)]
pub(crate) struct Part {
    pub(crate) text: SharedString,
    pub(crate) marker: Option<String>,
    pub(crate) tight: bool,
}

impl Part {
    fn plain(text: SharedString) -> Self {
        Self {
            text,
            marker: None,
            tight: false,
        }
    }
}

/// The blocks of `row` the list draws as selectable text.
pub(crate) fn parts(session: &ChatSession, row: &Row, context: Option<&OpenContext>) -> Vec<Part> {
    if session.hidden(row) {
        return Vec::new();
    }
    match &row.item.kind {
        ChatItemKind::Assistant { .. } => {
            document_parts(&row.document(session.stretched.get(&row.key), &session.files, context))
        }
        ChatItemKind::User { .. } => {
            let text = row
                .prompt()
                .map_or_else(|| row.shared(), |split| split.text.clone());
            vec![Part::plain(text)]
        }
        ChatItemKind::Tool {
            name,
            summary,
            result: Some(_),
            is_error,
            ..
        } if *is_error || (name == "Agent" && summary == "Subagent report") => {
            let mut parts = vec![Part::plain(if *is_error {
                tool_text(row).into()
            } else {
                muxy_ui::tr!("Subagent report")
            })];
            if let Some(text) = row.tool_details(session.expanded.contains(&row.item.id)) {
                parts.push(Part::plain(text.to_owned().into()));
            }
            parts
        }
        ChatItemKind::Tool { .. } if row.text.is_empty() => vec![Part::plain(row.verb.clone())],
        ChatItemKind::Tool { .. } => vec![Part::plain(tool_text(row).into())],
        ChatItemKind::Thinking { .. } if session.expanded.contains(&row.item.id) => {
            vec![Part::plain(row.shared())]
        }
        ChatItemKind::Notice { .. } | ChatItemKind::Folded { .. } => {
            vec![Part::plain(row.shared())]
        }
        ChatItemKind::Thinking { .. } | ChatItemKind::TurnEnd { .. } => Vec::new(),
    }
}

/// A step's text: its description, then its pill at [`pill_base`].
fn tool_text(row: &Row) -> String {
    format!("{} {}", row.verb, row.text)
}

/// Where a step's pill starts in its text.
pub(crate) fn pill_base(row: &Row) -> usize {
    row.verb.len() + 1
}

/// Each block's plain text; a grid's cells joined by tabs and lines.
pub(crate) fn document_parts(document: &Document) -> Vec<Part> {
    let mut previous = 0;
    document
        .blocks
        .iter()
        .map(|block| {
            let text = block.grid.as_deref().map_or_else(
                || block.text.clone(),
                |grid| {
                    let rows: Vec<String> = grid.rows.iter().map(|row| row.join("\t")).collect();
                    rows.join("\n").into()
                },
            );
            let marker = block.marker.as_ref().map(|marker| {
                let indent = "  ".repeat(usize::from(block.depth.saturating_sub(1)));
                let marker = match marker.as_ref() {
                    "•" => "-",
                    "☑" => "- [x]",
                    "☐" => "- [ ]",
                    number => number,
                };
                format!("{indent}{marker} ")
            });
            let tight = block.depth > 0 && previous > 0;
            previous = block.depth;
            Part {
                text,
                marker,
                tight,
            }
        })
        .collect()
}

/// Where cell `index` of a grid starts in its block's plain text.
pub(crate) fn cell_bases(grid: &markdown::Grid) -> impl Iterator<Item = usize> + '_ {
    grid.rows.iter().flatten().scan(0, |base, cell| {
        let at = *base;
        *base += cell.len() + 1;
        Some(at)
    })
}

/// The text from `start` to `end` of `rows`, each with its row index: blocks
/// apart by a blank line, list items by a line, rows by a blank line.
pub(crate) fn assemble(rows: &[(usize, Vec<Part>)], start: Spot, end: Spot) -> String {
    let mut out = String::new();
    for (at, parts) in rows {
        let mut started = false;
        for (block, part) in parts.iter().enumerate() {
            let len = part.text.len();
            let Some(range) = local((start, end), (*at, block), 0, len) else {
                continue;
            };
            let text = &part.text[floor(&part.text, range.start)..floor(&part.text, range.end)];
            if text.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push_str(if started && part.tight { "\n" } else { "\n\n" });
            }
            started = true;
            if range.start == 0
                && let Some(marker) = &part.marker
            {
                out.push_str(marker);
            }
            out.push_str(&text.replace(markdown::WEB_ARROW, ""));
        }
    }
    out
}

/// The link or path at `pos`: its menu label and the text to copy.
pub(crate) fn target(
    session: &ChatSession,
    row: &Row,
    context: Option<&OpenContext>,
    pos: &Pos,
) -> Option<(bool, String)> {
    match &row.item.kind {
        ChatItemKind::Assistant { .. } => {
            let document = row.document(session.stretched.get(&row.key), &session.files, context);
            let block = document.blocks.get(pos.block)?;
            let inside = |range: &Range<usize>| range.contains(&pos.offset);
            if let Some((_, url)) = block.links.iter().find(|(range, _)| inside(range)) {
                return Some((true, url.to_string()));
            }
            let (_, file) = block.paths.iter().find(|(range, _)| inside(range))?;
            Some((false, file.location.path.to_string_lossy().into_owned()))
        }
        ChatItemKind::Tool { .. } if pos.offset >= pill_base(row) => {
            row.path.clone().map(|path| (false, path))
        }
        _ => None,
    }
}

/// What a row's texts need to take part in selection, for one frame.
pub(crate) struct Marks<'a> {
    pub(crate) shared: &'a Rc<Shared>,
    pub(crate) span: Option<(Spot, Spot)>,
    /// The row's index in the session.
    pub(crate) at: usize,
    pub(crate) color: Hsla,
}

/// Records where `element`, drawing `layout`, lands as text `[base, base + len)`
/// of `row`'s `block`, and paints the selection under it.
pub(crate) fn mark(
    scope: &Scope<'_>,
    row: &SharedString,
    block: usize,
    base: usize,
    len: usize,
    layout: TextLayout,
    element: impl IntoElement,
) -> AnyElement {
    let Some(marks) = &scope.marks else {
        return element.into_any_element();
    };
    Marked {
        inner: element.into_any_element(),
        layout: layout.clone(),
        hit: Some(Hit {
            at: marks.at,
            row: row.clone(),
            block,
            base,
            len,
            bounds: Bounds::default(),
            layout,
        }),
        selected: marks
            .span
            .and_then(|span| local(span, (marks.at, block), base, len)),
        color: marks.color,
        shared: marks.shared.clone(),
    }
    .into_any_element()
}

/// `text` drawn plainly as text `[base, ..)` of `row`'s `block`.
pub(crate) fn plain(
    scope: &Scope<'_>,
    row: &SharedString,
    block: usize,
    base: usize,
    text: SharedString,
) -> AnyElement {
    let len = text.len();
    let styled = StyledText::new(text);
    let layout = styled.layout().clone();
    mark(scope, row, block, base, len, layout, styled)
}

/// Whether a click is really the end of a drag, for click handlers in rows.
#[derive(Clone)]
pub(crate) struct Guard(Option<Rc<Shared>>);

impl Guard {
    pub(crate) fn new(scope: &Scope<'_>) -> Self {
        Self(scope.marks.as_ref().map(|marks| marks.shared.clone()))
    }

    pub(crate) fn dragged(&self) -> bool {
        self.0.as_ref().is_some_and(|shared| shared.moved())
    }
}

/// A text that records where it landed and paints its part of the selection.
struct Marked {
    inner: AnyElement,
    layout: TextLayout,
    /// Recorded once prepainted.
    hit: Option<Hit>,
    /// The selected bytes of this text.
    selected: Option<Range<usize>>,
    color: Hsla,
    shared: Rc<Shared>,
}

impl IntoElement for Marked {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Marked {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.inner.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.inner.prepaint(window, cx);
        // Only prepainted text is recorded, so its layout can always be asked.
        if let Some(mut hit) = self.hit.take() {
            hit.bounds = bounds;
            self.shared.record(hit);
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(range) = self.selected.clone() {
            paint_selection(&self.layout, bounds, range, self.color, window);
        }
        self.inner.paint(window, cx);
    }
}

/// Fills `range` of `layout`'s lines: the first from its start, whole lines between, the last to its end.
fn paint_selection(
    layout: &TextLayout,
    bounds: Bounds<Pixels>,
    range: Range<usize>,
    color: Hsla,
    window: &mut Window,
) {
    let height = layout.line_height();
    let Some(start) = layout.position_for_index(range.start) else {
        return;
    };
    let end = layout
        .position_for_index(range.end)
        .unwrap_or_else(|| point(bounds.right(), bounds.bottom() - height));
    let mut fill = |from: Point<Pixels>, to: Point<Pixels>| {
        if to.x > from.x && to.y > from.y {
            window.paint_quad(gpui::fill(Bounds::from_corners(from, to), color));
        }
    };
    if end.y <= start.y {
        fill(start, point(end.x, start.y + height));
        return;
    }
    fill(start, point(bounds.right(), start.y + height));
    fill(
        point(bounds.left(), start.y + height),
        point(bounds.right(), end.y),
    );
    fill(point(bounds.left(), end.y), point(end.x, end.y + height));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::chat::Lookup;

    fn doc(markdown: &str) -> Vec<Part> {
        document_parts(&markdown::parse(markdown, &markdown::Open::new(), &|_| {
            Lookup::Found(None)
        }))
    }

    const ALL: Spot = (usize::MAX, usize::MAX, usize::MAX);

    #[test]
    fn reports_and_tool_errors_keep_their_visible_text_selectable() {
        use muxy_protocol::{ChatBatch, ChatItem, ChatMode, ChatSnapshot, SessionId};
        for (name, summary, is_error, retained) in [
            ("Agent", "Subagent report", false, true),
            ("Skill", "staging-check", true, true),
            ("Bash", "check", false, false),
        ] {
            let report = "Model output, not user instructions.\n  Report details.";
            let mut session = ChatSession::default();
            session.set_mode(ChatMode::Chat, 1);
            session.subscribe(1);
            session.apply(ChatBatch {
                session: SessionId::new(31).expect("session"),
                generation: 1,
                from_rev: 0,
                to_rev: 0,
                events: Vec::new(),
                snapshot: Some(Box::new(ChatSnapshot {
                    subscription: 1,
                    items: vec![ChatItem {
                        id: "handoff".into(),
                        turn_id: "turn".into(),
                        seq: 1,
                        kind: ChatItemKind::Tool {
                            name: name.into(),
                            summary: summary.into(),
                            input_preview: String::new(),
                            result: Some(report.into()),
                            subagent_count: 0,
                            is_error,
                        },
                    }],
                    state: muxy_protocol::ChatState::default(),
                    capabilities: muxy_protocol::ChatCapabilities::default(),
                    pending: Vec::new(),
                    has_earlier: false,
                })),
            });
            let collapsed = parts(&session, &session.rows[0], None);
            assert_eq!(collapsed.len(), if is_error { 2 } else { 1 });
            if is_error {
                assert_eq!(
                    collapsed[1].text.as_ref(),
                    "Model output, not user instructions."
                );
            }
            assert!(!session.toggle("handoff").is_empty());
            let expanded = parts(&session, &session.rows[0], None);
            if retained {
                assert_eq!(expanded[1].text.as_ref(), report);
            } else {
                assert_eq!(expanded.len(), 1);
            }
            session.toggle("handoff");
            assert_eq!(parts(&session, &session.rows[0], None), collapsed);
        }
    }

    #[test]
    fn copying_across_blocks_and_rows_keeps_markers_cells_and_code() {
        let first = doc(
            "Intro **bold** [docs](https://a.dev).\n\n- one\n- two\n  1. inner\n\n\
             | a | b |\n|---|---|\n| c | d |\n\n```\nfn main() {\n    x();\n}\n```",
        );
        let second = vec![Part::plain("Thanks".into())];
        let rows = [(3, first), (5, second)];
        assert_eq!(
            assemble(&rows, (0, 0, 0), ALL),
            "Intro bold docs.\n\n- one\n- two\n  1. inner\n\na\tb\nc\td\n\n\
             fn main() {\n    x();\n}\n\nThanks"
        );
        // Mid-block ends: no marker for an item copied from its middle.
        assert_eq!(
            assemble(&rows, (3, 2, 1), (5, 0, 4)),
            "wo\n  1. inner\n\na\tb\nc\td\n\nfn main() {\n    x();\n}\n\nThan"
        );
        // Inside one block only.
        assert_eq!(assemble(&rows, (3, 0, 6), (3, 0, 10)), "bold");
    }

    #[test]
    fn a_selection_whose_rows_shrank_or_vanished_copies_what_is_left() {
        let rows = [(0, vec![Part::plain("héllo".into())]), (2, vec![])];
        // Ends past the text and inside the two-byte `é` snap back onto char boundaries.
        assert_eq!(assemble(&rows, (0, 0, 2), (0, 0, 99)), "éllo");
        assert_eq!(assemble(&rows, (0, 0, 0), (0, 0, 2)), "h");
        assert_eq!(assemble(&rows, (0, 7, 0), ALL), "");
        assert_eq!(assemble(&rows, (1, 0, 0), ALL), "");
        assert_eq!(floor("🎉x", 3), 0);
        assert_eq!(floor("🎉x", 4), 4);
        assert_eq!(local(((0, 0, 1), (0, 0, 3)), (0, 0), 0, 2), Some(1..2));
        assert_eq!(local(((0, 0, 4), (0, 1, 3)), (0, 0), 0, 2), None);
    }

    #[test]
    fn grid_cells_start_where_their_copied_text_does() {
        let document = markdown::parse(
            "| é | bb |\n|---|---|\n| c | |\n",
            &markdown::Open::new(),
            &|_| Lookup::Found(None),
        );
        let block = &document.blocks[0];
        let grid = block.grid.as_deref().expect("a grid");
        let text = &document_parts(&document)[0].text;
        let cells: Vec<_> = grid.rows.iter().flatten().collect();
        for (cell, base) in cells.iter().zip(cell_bases(grid)) {
            assert_eq!(&text[base..base + cell.len()], cell.as_ref());
        }
    }

    #[test]
    fn words_and_runs_select_whole_on_char_boundaries() {
        let text = "let café_1 = (x, y);";
        assert_eq!(&text[word(text, 6)], "café_1");
        assert_eq!(&text[word(text, 8)], "café_1", "inside the é");
        assert_eq!(&text[word(text, 3)], " ");
        assert_eq!(&text[word(text, 14)], "(");
        assert_eq!(&text[word(text, text.len())], ");");
        assert_eq!(word("", 5), 0..0);
    }

    #[test]
    fn dragging_upward_orders_the_ends_and_keeps_a_clicked_word() {
        // Plain: the anchor stays put and the ends come out in order.
        let (anchor, head) = extend(None, (4, 0, 2), (1, 3, 0));
        assert_eq!((anchor.min(head), anchor.max(head)), ((1, 3, 0), (4, 0, 2)));
        // A double-clicked word at 10..15 stays selected whichever way the drag goes.
        let word = ((2, 0, 10), (2, 0, 15));
        assert_eq!(
            extend(Some(&word), word.0, (1, 0, 0)),
            ((2, 0, 15), (1, 0, 0))
        );
        assert_eq!(
            extend(Some(&word), word.0, (2, 0, 20)),
            ((2, 0, 10), (2, 0, 20))
        );
        assert_eq!(extend(Some(&word), word.0, (2, 0, 12)), word);
    }

    fn notice(id: &str, seq: u64) -> muxy_protocol::ChatItem {
        muxy_protocol::ChatItem {
            id: id.into(),
            turn_id: "t".into(),
            seq,
            kind: ChatItemKind::Notice { text: id.into() },
        }
    }

    fn batch(
        from_rev: u64,
        to_rev: u64,
        events: Vec<muxy_protocol::ChatEvent>,
        snapshot: Option<Vec<muxy_protocol::ChatItem>>,
    ) -> muxy_protocol::ChatBatch {
        muxy_protocol::ChatBatch {
            session: muxy_protocol::SessionId::new(1).expect("session"),
            generation: 1,
            from_rev,
            to_rev,
            events,
            snapshot: snapshot.map(|items| {
                Box::new(muxy_protocol::ChatSnapshot {
                    subscription: 1,
                    items,
                    state: muxy_protocol::ChatState::default(),
                    capabilities: muxy_protocol::ChatCapabilities::default(),
                    pending: Vec::new(),
                    has_earlier: false,
                })
            }),
        }
    }

    #[test]
    fn a_drag_keeps_its_pivot_side_after_a_row_comes_in_above() {
        let mut session = ChatSession::default();
        session.set_mode(muxy_protocol::ChatMode::Chat, 1);
        session.subscribe(1);
        session.apply(batch(
            5,
            5,
            Vec::new(),
            Some(vec![notice("y", 2), notice("x", 3)]),
        ));
        let pos = |at, row: &str| Pos {
            at,
            block: 0,
            offset: 1,
            row: row.to_owned().into(),
        };
        let mut pivot = (pos(1, "x"), pos(1, "x"));
        // A row arrives above: "x" moves from index 1 to 2.
        session.apply(batch(
            5,
            6,
            vec![muxy_protocol::ChatEvent::Upsert(notice("a", 1))],
            None,
        ));
        assert_eq!(session.row_index("x"), Some(2));
        // Dragging to "y" (now index 1, made fresh) is before the pivot, so it holds the pivot's end.
        let to = pos(1, "y");
        assert!(to > pivot.0, "stale indices order y after x");
        pivot.0.refresh(&session);
        pivot.1.refresh(&session);
        let (anchor, head) = extend(Some(&pivot), pivot.0.clone(), to.clone());
        assert_eq!((anchor, head), (pivot.1.clone(), to));
    }

    #[test]
    fn a_bare_click_leaves_nothing_for_escape_to_clear() {
        let shared = Shared::default();
        let at = |offset| Pos {
            at: 0,
            block: 0,
            offset,
            row: "r".into(),
        };
        shared.press(point(px(0.0), px(0.0)), at(2), at(2), false);
        assert!(!shared.clear(), "a collapsed caret is not a selection");
        shared.press(point(px(0.0), px(0.0)), at(2), at(2), false);
        shared.selection.borrow_mut().head = Some(at(5));
        assert!(shared.clear());
        assert!(!shared.clear());
    }
}
