//! Fork: the prompt timeline, a strip of ticks floating on the chat's right
//! edge: one per prompt, the one in view brighter. Hovering a tick shows the
//! prompt's first line; clicking scrolls to it.

use gpui::{
    AnyView, AppContext, Context, InteractiveElement, IntoElement, ListState, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, WeakEntity, Window, div, px,
};
use muxy_protocol::ChatItemKind;
use muxy_ui::components::Tooltip;

use crate::model::AppModel;
use crate::model::chat::ChatSession;

pub(crate) struct Timeline {
    list: ListState,
    /// The list's view, redrawn after a jump since it is cached.
    rows: AnyView,
    owner: WeakEntity<AppModel>,
    /// Each prompt's list index and first line.
    ticks: Vec<(usize, SharedString)>,
    /// The list's row count and [`ChatSession::prompts`] the ticks were placed for.
    count: Option<(usize, u64)>,
    /// The first row in view.
    top: usize,
}

impl Timeline {
    pub(crate) fn new(list: ListState, rows: AnyView, owner: WeakEntity<AppModel>) -> Self {
        Self {
            list,
            rows,
            owner,
            ticks: Vec::new(),
            count: None,
            top: 0,
        }
    }

    /// The ticks were placed for another row count or other prompts.
    pub(crate) fn stale(&self, count: (usize, u64)) -> bool {
        self.count != Some(count)
    }

    pub(crate) fn place(
        &mut self,
        count: (usize, u64),
        ticks: Vec<(usize, SharedString)>,
        cx: &mut Context<Self>,
    ) {
        self.count = Some(count);
        self.ticks = ticks;
        cx.notify();
    }

    /// The list scrolled; redraws only when another prompt comes into view.
    pub(crate) fn scrolled(&mut self, top: usize, cx: &mut Context<Self>) {
        let before = self.current();
        self.top = top;
        if self.current() != before {
            cx.notify();
        }
    }

    /// The last prompt at or above the top of the view.
    fn current(&self) -> Option<usize> {
        let after = self.ticks.partition_point(|(index, _)| *index <= self.top);
        after
            .checked_sub(1)
            .or((!self.ticks.is_empty()).then_some(0))
    }
}

impl Render for Timeline {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(model) = self.owner.upgrade() else {
            return div();
        };
        // A lone prompt needs no map.
        if self.ticks.len() < 2 {
            return div();
        }
        let model = model.read(cx);
        let m = model.metrics;
        let theme = &model.theme;
        let current = self.current();
        let (raised, fg, border, bg) = (theme.raised(), theme.fg, theme.border, theme.bg);
        let (bright, dim) = (theme.fg, theme.fg_dim);
        div()
            .debug_selector(|| "chat-timeline".into())
            .flex()
            .flex_col()
            .items_end()
            .gap(px(5.0))
            .max_h_full()
            .overflow_hidden()
            .py(m.spacing3())
            // shortcut: ticks past the strip's height are clipped; scale the gap if chats get that long.
            .children(self.ticks.iter().enumerate().map(|(tick, (index, line))| {
                let (list, rows, index, line) =
                    (self.list.clone(), self.rows.clone(), *index, line.clone());
                let bright_tick = current == Some(tick);
                div()
                    .id(("chat-tick", tick))
                    .py(px(2.0))
                    .pl(m.spacing3())
                    .cursor_pointer()
                    .child(
                        div()
                            .h(px(2.0))
                            .w(if bright_tick { px(16.0) } else { px(12.0) })
                            .rounded(px(1.0))
                            .bg(if bright_tick { bright } else { dim }),
                    )
                    .tooltip(move |_, cx| {
                        cx.new(|_| Tooltip::new(line.clone(), raised, fg, border, bg))
                            .into()
                    })
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        list.scroll_to_reveal_item(index);
                        cx.notify(rows.entity_id());
                    })
            }))
    }
}

/// Each prompt's list index and first line.
pub(crate) fn ticks(session: &ChatSession) -> Vec<(usize, SharedString)> {
    session
        .visible
        .iter()
        .enumerate()
        .filter_map(|(index, &at)| {
            let row = session.rows.get(at)?;
            matches!(row.item.kind, ChatItemKind::User { .. }).then(|| (index, row.preview()))
        })
        .collect()
}
