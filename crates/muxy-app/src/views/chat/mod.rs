//! Fork: a pane's Claude Code conversation as chat: markdown rows, turns
//! folded behind a summary once they finish, approval cards and an input
//! docked at the bottom, and a timeline of the prompts on the right.

pub(crate) mod approval;
mod dock;
pub(crate) use dock::{copied_image, image_chip, images_fit};
mod mode_switch;
pub(crate) use mode_switch::{mode_control, mode_switch};
pub(crate) mod markdown;
pub(crate) mod rows;
mod select;
mod timeline;

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, AnyView, AppContext, Context, DispatchPhase, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ListAlignment, ListOffset, ListState, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Task, WeakEntity, Window,
    canvas, div, px,
};
use muxy_app_core::PaneId;
use muxy_protocol::{ChatImage, ChatMode};
use muxy_ui::components::IconGlyph;
use muxy_ui::controls::{self, Style};
use muxy_ui::icon::Icon;
use muxy_ui::popover::PopoverAnchor;
use muxy_ui::text_input::{InputEvent, InputStyle, TextInput};
use muxy_ui::tr;

use crate::model::AppModel;
use crate::model::chat::{ChatKey, ChatSession, SHOW_APPROVAL_AFTER, Splice, WITHDRAWN_FOR};
use timeline::Timeline;

gpui::actions!(muxy, [ToggleChat, NewClaudeChat]);

/// The widest the conversation and its input get; wider panes center them.
const READABLE_WIDTH: f32 = 820.0;

/// One pane's chat. The model owns the conversation; each pane keeps its own
/// scroll, input and card, and redraws only for its session.
pub(crate) struct ChatView {
    pub(crate) key: ChatKey,
    pane: PaneId,
    list: ListState,
    /// Draws the list; cached, so the timer, the input and the card redraw without the rows.
    rows: Entity<ChatRows>,
    /// The running turn's timer, from when it started.
    elapsed: Option<(Instant, Entity<Elapsed>)>,
    /// Scrolled up from the bottom, so the list no longer follows new rows.
    scrolled: Rc<Cell<bool>>,
    /// The pane's terminal focus. The chat tracks it so shortcuts still reach
    /// the workspace; focusing it moves focus on to the input, so keys never
    /// reach the shell.
    focus: FocusHandle,
    owner: WeakEntity<AppModel>,
    input: Entity<TextInput>,
    /// The images going with the next prompt, by file name.
    images: Vec<(SharedString, ChatImage)>,
    /// The input is empty, so typing redraws this view only when that flips.
    empty: bool,
    /// File and folder references sent with the next prompt.
    files: Vec<(String, String)>,
    /// When the input last changed, so a new card doesn't take the key being typed.
    last_edit: Option<Instant>,
    /// The session's command list the slash matches were made from: pointer and length.
    commands_seen: (usize, usize),
    /// A prompt is on its way to the server.
    sending: bool,
    card: Option<approval::Card>,
    picker_open: bool,
    picker_anchor: PopoverAnchor,
    /// The slash commands offered for the `/word` being typed, and their scroll.
    slash: dock::Slash,
    slash_scroll: gpui::ScrollHandle,
    /// Where the quick commands and more buttons were last drawn, for their menus.
    commands_anchor: PopoverAnchor,
    menu_anchor: PopoverAnchor,
    timeline: Entity<Timeline>,
    /// Redraws when a waiting approval should show or a withdrawn one fade out.
    wake: Option<(Instant, Task<()>)>,
    /// Moves focus from the terminal's handle to the input; made on first draw.
    redirect: Option<Subscription>,
    /// Fork: the text selected in the rows. Focus stays in the input; ⌘C, ⌘A
    /// and Escape reach the selection from here while it is the one in use.
    select: Rc<select::Shared>,
    _input: Subscription,
}

impl ChatView {
    pub(crate) fn seed_prompt(
        &mut self,
        text: &str,
        images: Vec<ChatImage>,
        cx: &mut Context<Self>,
    ) -> Option<Instant> {
        self.input
            .update(cx, |input, cx| input.set_text(text.to_owned(), cx));
        self.images = images
            .into_iter()
            .map(|image| ("Image.png".into(), image))
            .collect();
        self.empty = text.trim().is_empty();
        self.last_edit = Some(Instant::now());
        cx.notify();
        self.last_edit
    }

    pub(crate) fn clear_seeded_prompt(
        &mut self,
        text: &str,
        revision: Option<Instant>,
        cx: &mut Context<Self>,
    ) {
        if self.last_edit == revision && self.input.read(cx).text() == text {
            self.input.update(cx, |input, cx| input.set_text("", cx));
            self.images.clear();
            self.empty = true;
            cx.notify();
        }
    }

    pub(crate) fn seeded_prompt_unchanged(&self, revision: Option<Instant>) -> bool {
        self.last_edit == revision
    }

    pub(crate) fn new(
        pane: PaneId,
        key: ChatKey,
        session: &ChatSession,
        focus: FocusHandle,
        owner: WeakEntity<AppModel>,
        style: InputStyle,
        cx: &mut Context<Self>,
    ) -> Self {
        let picker_open = session
            .local
            .as_ref()
            .is_some_and(|draft| draft.choose_thread);
        let rows = session.visible.len();
        let list = ListState::new(rows, ListAlignment::Bottom, px(512.0));
        let scrolled = Rc::new(Cell::new(false));
        let select = Rc::new(select::Shared::default());
        let view = cx.weak_entity();
        let rows = cx.new(|_| ChatRows {
            key,
            pane,
            list: list.clone(),
            owner: owner.clone(),
            select: select.clone(),
            scrolled: scrolled.clone(),
            view: view.clone(),
        });
        let timeline =
            cx.new(|_| Timeline::new(list.clone(), AnyView::from(rows.clone()), owner.clone()));
        let (flag, ticks) = (scrolled.clone(), timeline.downgrade());
        list.set_scroll_handler(move |event, _, cx| {
            if flag.replace(event.is_scrolled) != event.is_scrolled {
                let _ = view.update(cx, |_, cx| cx.notify());
            }
            let _ = ticks.update(cx, |ticks, cx| {
                ticks.scrolled(event.visible_range.start, cx);
            });
        });
        let pasted = cx.weak_entity();
        let input = cx.new(|cx| {
            TextInput::new(style, cx)
                .multiline()
                .with_placeholder(tr!("Ask your agent to build, fix or explain…"))
                .with_paste_delegate(move |_, cx| {
                    let content = muxy_ui::pasteboard::read_content(cx);
                    if matches!(content, Ok(muxy_ui::pasteboard::Content::Text)) {
                        return false;
                    }
                    let _ = pasted.update(cx, |view, cx| view.paste(content, cx));
                    true
                })
        });
        let changes = cx.subscribe(&input, |view: &mut Self, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Changed) {
                view.last_edit = Some(Instant::now());
                // Typing puts the input back in use, so ⌘C and ⌘A are its own again.
                view.clear_selection(cx);
                let empty = input.read(cx).text().trim().is_empty();
                if view.empty != empty {
                    view.empty = empty;
                    cx.notify();
                }
                view.refilter_slash(cx);
            }
        });
        Self {
            key,
            pane,
            list,
            rows,
            elapsed: None,
            scrolled,
            focus,
            owner,
            input,
            images: Vec::new(),
            empty: true,
            files: Vec::new(),
            last_edit: None,
            commands_seen: (0, 0),
            sending: false,
            card: None,
            picker_open,
            picker_anchor: PopoverAnchor::default(),
            slash: dock::Slash::default(),
            slash_scroll: gpui::ScrollHandle::default(),
            commands_anchor: PopoverAnchor::default(),
            menu_anchor: PopoverAnchor::default(),
            timeline,
            wake: None,
            redirect: None,
            select,
            _input: changes,
        }
    }

    /// Clears the rows' selection; true when there was one.
    fn clear_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let cleared = self.select.clear();
        if cleared {
            self.rows.update(cx, |_, cx| cx.notify());
        }
        cleared
    }

    /// ⌘C with text selected in the rows: copies it instead of the input's.
    fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(model) = self.owner.upgrade() else {
            return false;
        };
        let text = {
            let model = model.read(cx);
            let context = model.chat_link_context(self.pane, cx);
            model
                .chat
                .sessions
                .get(&self.key)
                .and_then(|session| self.select.copied(session, context.as_ref()))
        };
        let Some(text) = text else {
            return false;
        };
        model.update(cx, |model, cx| model.chat_copy_text(text, cx));
        true
    }

    /// ⌘A after a click in the rows: selects the whole conversation.
    fn select_all(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.select.active() {
            return false;
        }
        if let Some(model) = self.owner.upgrade()
            && let Some(session) = model.read(cx).chat.sessions.get(&self.key)
        {
            self.select.select_all(session);
        }
        self.rows.update(cx, |_, cx| cx.notify());
        true
    }

    /// Re-measures only the rows that changed; `rows` is how many the list shows now.
    pub(crate) fn apply(&mut self, splices: &[Splice], rows: usize, cx: &mut Context<Self>) {
        for splice in splices {
            let count = self.list.item_count();
            match splice {
                Splice::Replace(at) if *at < count => self.list.splice(*at..*at + 1, 1),
                Splice::Insert(at) if *at <= count => self.list.splice(*at..*at, 1),
                Splice::Range(range, new) if range.end <= count => {
                    self.list.splice(range.clone(), *new);
                }
                // A reset follows the bottom again; no scroll event says so.
                Splice::Reset(rows) => {
                    self.list.reset(*rows);
                    self.scrolled.set(false);
                }
                // Unreachable while every change reaches every view; recover by resetting.
                Splice::Replace(_) | Splice::Insert(_) | Splice::Range(..) => {
                    self.list.reset(rows);
                    self.scrolled.set(false);
                    break;
                }
            }
        }
        self.rows.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    pub(super) fn focus_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.read(cx).focus_handle(cx).focus(window);
    }

    /// Starts, keeps or drops the turn timer to match the session.
    fn sync_elapsed(&mut self, since: Option<Instant>, cx: &mut Context<Self>) {
        match since {
            Some(since) if self.elapsed.as_ref().is_some_and(|(at, _)| *at == since) => {}
            Some(since) => self.elapsed = Some((since, cx.new(|cx| Elapsed::new(since, cx)))),
            None => self.elapsed = None,
        }
    }

    /// Schedules a redraw for when an approval has waited long enough to show,
    /// or a withdrawn one has faded.
    fn sync_wake(&mut self, at: Option<Instant>, cx: &mut Context<Self>) {
        if self.wake.as_ref().map(|(when, _)| *when) == at {
            return;
        }
        self.wake = at.map(|at| {
            let (owner, key) = (self.owner.clone(), self.key);
            let task = cx.spawn(async move |view, cx| {
                cx.background_executor()
                    .timer(at.saturating_duration_since(Instant::now()))
                    .await;
                let _ = owner.update(cx, |model, cx| model.chat_forget_withdrawn(key, cx));
                let _ = view.update(cx, |view, cx| {
                    view.wake = None;
                    cx.notify();
                });
            });
            (at, task)
        });
    }

    /// Lays the prompt timeline out again when the rows changed.
    fn sync_timeline(&mut self, model: &Entity<AppModel>, cx: &mut Context<Self>) {
        let Some(session) = model.read(cx).chat.sessions.get(&self.key) else {
            return;
        };
        let count = (session.visible.len(), session.prompts);
        if !self.timeline.read(cx).stale(count) {
            return;
        }
        let ticks = timeline::ticks(session);
        self.timeline
            .update(cx, |timeline, cx| timeline.place(count, ticks, cx));
    }
}

/// When a waiting approval shows or the withdrawn card goes.
fn next_wake(session: &ChatSession, now: Instant) -> Option<Instant> {
    let showing = session.shown_approval(now).is_some();
    let waiting = session
        .pending
        .iter()
        .filter_map(|prompt| session.arrived.get(&prompt.request_id))
        .map(|at| *at + SHOW_APPROVAL_AFTER)
        .filter(|at| *at > now && !showing);
    let fading = session
        .withdrawn
        .as_ref()
        .map(|(_, at)| *at + WITHDRAWN_FOR);
    waiting.chain(fading).min()
}

impl Render for ChatView {
    #[allow(
        clippy::too_many_lines,
        reason = "The chat's list, card, timer and input in order"
    )]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(owner) = self.owner.upgrade() else {
            return div().into_any_element();
        };
        if self.redirect.is_none() {
            self.redirect = Some(cx.on_focus(&self.focus, window, |view, window, cx| {
                view.focus_input(window, cx);
            }));
        }
        // The pane may have been focused before this view first drew.
        if self.focus.is_focused(window) {
            self.focus_input(window, cx);
        }
        let now = Instant::now();
        let (since, wake) =
            owner
                .read(cx)
                .chat
                .sessions
                .get(&self.key)
                .map_or((None, None), |session| {
                    let busy = session.state.busy && session.mode == ChatMode::Chat;
                    (session.busy_since.filter(|_| busy), next_wake(session, now))
                });
        let commands = owner
            .read(cx)
            .chat
            .sessions
            .get(&self.key)
            .map_or((0, 0), |session| {
                let commands = &session.capabilities.commands;
                (commands.as_ptr() as usize, commands.len())
            });
        // Capabilities arrived or were replaced: the matches were made from the old list.
        if commands != self.commands_seen {
            self.commands_seen = commands;
            self.refilter_slash(cx);
        }
        self.sync_elapsed(since, cx);
        self.sync_wake(wake, cx);
        self.sync_timeline(&owner, cx);
        let style = {
            let model = owner.read(cx);
            InputStyle::field(&model.theme, &model.metrics)
        };
        let session = owner.read(cx).chat.sessions.get(&self.key);
        let shown = session.and_then(|session| session.shown_approval(now));
        // Cloned only when the card changes, not every frame.
        if shown.map(|prompt| prompt.request_id.as_str())
            != self.card.as_ref().map(|card| card.request_id.as_str())
        {
            let prompt = shown.cloned();
            let codex = session
                .is_some_and(|session| session.provider() == muxy_protocol::ChatProvider::Codex);
            self.sync_card(prompt, style, codex, window, cx);
        }
        let model = owner.read(cx);
        let m = model.metrics;
        let theme = &model.theme;
        let drop_background = theme.surface;
        let root = div()
            .debug_selector(|| "chat-view".into())
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.bg)
            .drag_over::<gpui::ExternalPaths>(move |style, _, _, _| style.bg(drop_background))
            .on_drop(cx.listener(|_, paths: &gpui::ExternalPaths, _, cx| {
                cx.stop_propagation();
                Self::attach_paths(paths.paths().to_vec(), cx);
            }));
        let Some(session) = model.chat.sessions.get(&self.key) else {
            return root.into_any_element();
        };
        let (key, pane) = (self.key, self.pane);
        let window = &*window;
        let starting = session.starting && session.visible.is_empty();
        let list = div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .when(starting, |list| list.child(starting_state(pane, model)))
            .child(AnyView::from(self.rows.clone()).cached(div().size_full().style().clone()))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(m.spacing2())
                    .flex()
                    .items_center()
                    // Presses here belong to the ticks, not the rows under them.
                    .occlude()
                    .child(self.timeline.clone()),
            )
            .when(self.scrolled.get(), |list| {
                list.child(scroll_to_bottom(
                    &self.list,
                    &self.scrolled,
                    cx.entity().downgrade(),
                    model,
                ))
            });
        let scope = rows::Scope {
            pane,
            key,
            session,
            owner: &self.owner,
            model,
            window,
            // The card's plan text names no files to open.
            context: None,
            marks: None,
        };
        let card = self.approval(session, &scope, now, window, cx);
        let elapsed = self.elapsed.as_ref().map(|(_, elapsed)| {
            div()
                .flex()
                .items_center()
                .gap(m.spacing3())
                // In line with the rows' text above.
                .px(m.spacing8())
                .text_size(m.font_footnote())
                .text_color(theme.fg_muted)
                .child(super::tab_activity::spinner(
                    format!("chat-spinner-{pane}"),
                    m.icon_sm(),
                    model,
                ))
                .child(elapsed.clone())
        });
        let dock = self.dock(session, model, window, cx);
        let (focus, view) = (self.owner.clone(), cx.entity().downgrade());
        root.on_mouse_down(MouseButton::Left, move |_, window, cx| {
            let _ = focus.update(cx, |model, cx| model.focus_pane(pane, cx));
            if !window.default_prevented() {
                let _ = view.update(cx, |view, cx| view.focus_input(window, cx));
            }
        })
        // Fork: the rows' selection, while it is the one in use, before the input sees these.
        .capture_action(cx.listener(|view, _: &muxy_ui::text_input::Copy, _, cx| {
            if view.copy_selection(cx) {
                cx.stop_propagation();
            }
        }))
        .capture_action(
            cx.listener(|view, _: &muxy_ui::text_input::SelectAll, _, cx| {
                if view.select_all(cx) {
                    cx.stop_propagation();
                }
            }),
        )
        .capture_action(cx.listener(|view, _: &muxy_ui::text_input::Cancel, _, cx| {
            if view.clear_selection(cx) {
                cx.stop_propagation();
            }
        }))
        .capture_action(cx.listener(|view, _: &super::composer::Close, _, cx| {
            if view.clear_selection(cx) {
                cx.stop_propagation();
            }
        }))
        .when_some(banner(pane, session, &self.owner, model), |root, banner| {
            root.child(banner)
        })
        .child(list)
        .child(
            div()
                .key_context("Composer")
                .flex()
                .flex_col()
                .gap(m.spacing3())
                .w_full()
                .max_w(m.scaled(READABLE_WIDTH) + m.spacing6() * 2.0)
                .mx_auto()
                .px(m.spacing6())
                .pb(m.spacing5())
                // A click here puts the input, not the rows, in use.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|view, _, _, cx| {
                        view.clear_selection(cx);
                    }),
                )
                .children(card)
                .children(elapsed)
                .child(dock)
                // Enter sends, as in the composer, or takes the highlighted slash command;
                // Shift-Enter is its newline.
                .capture_action(cx.listener(
                    |view, _: &muxy_ui::text_input::InsertNewline, window, cx| {
                        if view.input.read(cx).focus_handle(cx).is_focused(window) {
                            cx.stop_propagation();
                            if view.slash_typed_exactly(cx) {
                                view.slash = dock::Slash::default();
                            }
                            if !view.accept_slash(None, cx) {
                                view.send(window, cx);
                            }
                        }
                    },
                ))
                .capture_action(cx.listener(|view, _: &muxy_ui::text_input::Up, window, cx| {
                    let focused = view.input.read(cx).focus_handle(cx).is_focused(window);
                    if focused && view.move_slash(-1, cx) {
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(|view, _: &muxy_ui::text_input::Down, window, cx| {
                    let focused = view.input.read(cx).focus_handle(cx).is_focused(window);
                    if focused && view.move_slash(1, cx) {
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(|view, _: &muxy_ui::text_input::Cancel, _, cx| {
                    if view.dismiss_slash(cx) {
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(|view, _: &super::composer::Close, _, cx| {
                    if view.dismiss_slash(cx) {
                        cx.stop_propagation();
                    }
                }))
                .on_action(cx.listener(|view, _: &super::composer::Newline, window, cx| {
                    if view.input.read(cx).focus_handle(cx).is_focused(window) {
                        view.input
                            .update(cx, |input, cx| input.insert_at_selection("\n", cx));
                    } else {
                        cx.propagate();
                    }
                }))
                .on_action(cx.listener(|view, _: &super::composer::Submit, window, cx| {
                    view.send(window, cx);
                }))
                // Tab takes the highlighted slash command, else moves between the input and the card.
                .on_action(cx.listener(|view, _: &super::composer::Complete, window, cx| {
                    if view.accept_slash(None, cx) {
                        return;
                    }
                    match view.card.as_ref().map(|card| card.focus.clone()) {
                        Some(card) if !card.is_focused(window) => card.focus(window),
                        _ => view.focus_input(window, cx),
                    }
                })),
        )
        .into_any_element()
    }
}

/// "Starting chat…" over the whole area, as a new chat starts.
fn starting_state(pane: PaneId, model: &AppModel) -> gpui::Div {
    let m = model.metrics;
    div()
        .debug_selector(|| "chat-starting".into())
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .gap(m.spacing3())
        .text_size(m.font_body())
        .text_color(model.theme.fg_muted)
        .child(super::tab_activity::spinner(
            format!("chat-starting-{pane}"),
            m.icon_sm(),
            model,
        ))
        .child(tr!("Starting chat…"))
}

/// A pane waiting to start a new chat: its terminal stays laid out under
/// "Starting chat…", since its size is what attaches it.
pub(crate) fn waiting(pane: PaneId, terminal: AnyElement, model: &AppModel) -> AnyElement {
    div()
        .relative()
        .size_full()
        .child(terminal)
        .child(starting_state(pane, model).bg(model.theme.bg).occlude())
        .into_any_element()
}

/// The conversation's rows, drawn as a list in their own view so redrawing
/// the timer or approvals doesn't lay them out again.
struct ChatRows {
    key: ChatKey,
    pane: PaneId,
    list: ListState,
    owner: WeakEntity<AppModel>,
    /// Fork: the selection, and the texts drawn for hit-testing it.
    select: Rc<select::Shared>,
    /// The chat's "scrolled up" flag, set when a drag scrolls up.
    scrolled: Rc<Cell<bool>>,
    view: WeakEntity<ChatView>,
}

/// How close to the list's top or bottom a drag scrolls it, and the most per move.
const DRAG_EDGE: f32 = 24.0;

impl ChatRows {
    /// The plain text of the block at `pos`, as selection and copying see it.
    fn block_text(&self, pos: &select::Pos, cx: &gpui::App) -> SharedString {
        self.with_row(pos, cx, |session, row, context| {
            select::parts(session, row, context)
                .into_iter()
                .nth(pos.block)
                .map(|part| part.text)
        })
        .flatten()
        .unwrap_or_default()
    }

    fn with_row<R>(
        &self,
        pos: &select::Pos,
        cx: &gpui::App,
        f: impl FnOnce(
            &ChatSession,
            &crate::model::chat::Row,
            Option<&muxy_app_core::opener::OpenContext>,
        ) -> R,
    ) -> Option<R> {
        let model = self.owner.upgrade()?;
        let model = model.read(cx);
        let session = model.chat.sessions.get(&self.key)?;
        let row = session.rows.get(session.row_index(&pos.row)?)?;
        let context = model.chat_link_context(self.pane, cx);
        Some(f(session, row, context.as_ref()))
    }

    /// A press starts a selection: a click places it, a double click takes
    /// the word and a triple click the block; Shift extends the one there is.
    fn press(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(pos) = self.select.hit(event.position) else {
            if self.select.press_empty() {
                cx.notify();
            }
            return;
        };
        if event.modifiers.shift && self.select.extend_to(event.position, pos.clone()) {
            cx.notify();
            return;
        }
        let range = match event.click_count {
            0 | 1 => pos.offset..pos.offset,
            2 => select::word(&self.block_text(&pos, cx), pos.offset),
            _ => 0..self.block_text(&pos, cx).len(),
        };
        let at = |offset| select::Pos {
            offset,
            ..pos.clone()
        };
        self.select.press(
            event.position,
            at(range.start),
            at(range.end),
            event.click_count >= 2,
        );
        cx.notify();
    }

    /// The pointer moved with the button down: extends the selection, and
    /// scrolls when it is near the list's top or bottom edge.
    fn drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(model) = self.owner.upgrade() else {
            return;
        };
        let changed = match model.read(cx).chat.sessions.get(&self.key) {
            Some(session) => self.select.drag(session, position),
            None => return,
        };
        let viewport = self.list.viewport_bounds();
        let edge = px(DRAG_EDGE);
        let above = position.y - (viewport.top() + edge);
        let below = position.y - (viewport.bottom() - edge);
        // Scrolling down only while scrolled up: at the bottom the list follows new rows.
        let delta = if above < px(0.0) {
            above.max(-edge)
        } else if below > px(0.0) && self.scrolled.get() {
            below.min(edge)
        } else {
            px(0.0)
        };
        if delta != px(0.0) && self.select.moved() {
            self.list.scroll_by(delta);
            // As the list's own scroll handler does: `scroll_by` always leaves it scrolled.
            let top = self.list.logical_scroll_top().item_ix;
            let first = !self.scrolled.replace(true);
            let _ = self.view.update(cx, |view, cx| {
                view.timeline
                    .update(cx, |ticks, cx| ticks.scrolled(top, cx));
                if first {
                    cx.notify();
                }
            });
            cx.notify();
        } else if changed {
            cx.notify();
        }
    }

    /// Right-click: Copy, Copy Message and, on a link or path, Copy Link or Copy Path.
    fn menu(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let Some(pos) = self.select.hit(event.position) else {
            return;
        };
        let Some((copy, id, target)) = self.with_row(&pos, cx, |session, row, context| {
            (
                self.select.span(session).is_some(),
                row.item.id.clone(),
                select::target(session, row, context, &pos),
            )
        }) else {
            return;
        };
        let mut items = vec![
            muxy_ui::native_menu::Item::new(tr!("Copy").to_string()),
            muxy_ui::native_menu::Item::new(tr!("Copy Message").to_string()),
        ];
        items[0].enabled = copy;
        if let Some((link, _)) = &target {
            let mut item = muxy_ui::native_menu::Item::new(
                if *link {
                    tr!("Copy Link")
                } else {
                    tr!("Copy Path")
                }
                .to_string(),
            );
            item.separator = true;
            items.push(item);
        }
        let Some(receiver) = muxy_ui::native_menu::show(items, event.position, window, cx) else {
            return;
        };
        let (owner, key, pane, select) =
            (self.owner.clone(), self.key, self.pane, self.select.clone());
        cx.spawn(async move |_, cx| {
            let Some(choice) = receiver.recv().await.ok().flatten() else {
                return;
            };
            let _ = owner.update(cx, |model, cx| {
                let text = match choice {
                    // Assembled now, only for the item chosen.
                    0 => {
                        let context = model.chat_link_context(pane, cx);
                        model
                            .chat
                            .sessions
                            .get(&key)
                            .and_then(|session| select.copied(session, context.as_ref()))
                    }
                    1 => return model.chat_copy(key, &id, cx),
                    _ => target.map(|(_, text)| text),
                };
                if let Some(text) = text {
                    model.chat_copy_text(text, cx);
                }
            });
        })
        .detach();
    }

    /// Follows a drag past the rows, and its end anywhere, while the button is down.
    fn drag_listeners(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (select, rows) = (self.select.clone(), cx.entity().downgrade());
        canvas(
            |_, _, _| {},
            move |_, (), window, _| {
                let moving = select.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Bubble || !moving.dragging() {
                        return;
                    }
                    // Released outside the window, where no mouse up arrives.
                    if event.pressed_button != Some(MouseButton::Left) {
                        moving.end_drag();
                        return;
                    }
                    let _ = rows.update(cx, |rows, cx| rows.drag(event.position, cx));
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, _| {
                    if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                        select.end_drag();
                    }
                });
            },
        )
        .absolute()
        .size_full()
    }
}

impl Render for ChatRows {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (key, pane) = (self.key, self.pane);
        let owner = self.owner.clone();
        // The rows draw again and record where their texts land.
        self.select.begin_frame();
        let (span, color) =
            self.owner
                .upgrade()
                .map_or((None, gpui::transparent_black()), |model| {
                    let model = model.read(cx);
                    let span = model
                        .chat
                        .sessions
                        .get(&key)
                        .and_then(|session| self.select.span(session));
                    (span, model.theme.accent_soft)
                });
        let select = self.select.clone();
        // Room under the last row for the composer docked over it.
        let bottom = self
            .owner
            .upgrade()
            .map_or(px(32.0), |model| model.read(cx).metrics.spacing10());
        // Read once per draw; rows only read the session's path answers.
        let context = self
            .owner
            .upgrade()
            .and_then(|model| model.read(cx).chat_link_context(pane, cx));
        let list = gpui::list(self.list.clone(), move |index, window, cx| {
            let mut flush = false;
            let element = owner
                .upgrade()
                .and_then(|model| {
                    let model = model.read(cx);
                    let session = model.chat.sessions.get(&key)?;
                    let at = *session.visible.get(index)?;
                    let row = session.rows.get(at)?;
                    let scope = rows::Scope {
                        pane,
                        key,
                        session,
                        owner: &owner,
                        model,
                        window,
                        context: context.as_ref(),
                        marks: Some(select::Marks {
                            shared: &select,
                            span,
                            at,
                            color,
                        }),
                    };
                    // Centered at a readable width; narrower panes use all of it.
                    let element = div()
                        .w_full()
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .max_w(model.metrics.scaled(READABLE_WIDTH))
                                .child(rows::row(&scope, row)),
                        )
                        .into_any_element();
                    flush = session.files.borrow_mut().take_schedule();
                    Some(element)
                })
                .unwrap_or_else(|| div().into_any_element());
            // Paths this row asked about go to the worker after the frame, never during it.
            if flush {
                let owner = owner.clone();
                cx.defer(move |cx| {
                    let _ = owner.update(cx, |model, cx| model.chat_resolve_files(key, cx));
                });
            }
            element
        })
        .size_full()
        .pt(bottom / 2.0)
        .pb(bottom);
        div()
            .relative()
            .size_full()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::press))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::menu))
            .child(list)
            .child(self.drag_listeners(cx))
    }
}

/// The running turn's elapsed time, redrawn each second on its own.
struct Elapsed {
    since: Instant,
    _tick: Task<()>,
}

impl Elapsed {
    fn new(since: Instant, cx: &mut Context<Self>) -> Self {
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self { since, _tick: tick }
    }
}

impl Render for Elapsed {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let ms = u64::try_from(self.since.elapsed().as_millis()).unwrap_or(u64::MAX);
        div()
            .font_family(muxy_ui::font::monospace())
            .child(rows::duration(ms))
    }
}

/// The pill that jumps back to the newest row and follows it again.
fn scroll_to_bottom(
    list: &ListState,
    scrolled: &Rc<Cell<bool>>,
    view: WeakEntity<ChatView>,
    model: &AppModel,
) -> AnyElement {
    let m = model.metrics;
    let (list, scrolled) = (list.clone(), scrolled.clone());
    // Over the left edge of the rows' readable column, not the pane's.
    div()
        .absolute()
        .bottom(m.spacing6())
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            div()
                .w_full()
                .max_w(m.scaled(READABLE_WIDTH))
                .px(m.spacing8())
                .child(scroll_pill(list, scrolled, view, model)),
        )
        .into_any_element()
}

fn scroll_pill(
    list: ListState,
    scrolled: Rc<Cell<bool>>,
    view: WeakEntity<ChatView>,
    model: &AppModel,
) -> impl IntoElement {
    let m = model.metrics;
    let theme = &model.theme;
    div().flex().child(
        div()
            .id("chat-scroll-bottom")
            .debug_selector(|| "chat-scroll-bottom".into())
            // A press on the pill must not start a selection in the rows under it.
            .occlude()
            .flex()
            .items_center()
            .gap(m.spacing2())
            .px(m.spacing5())
            .py(m.spacing2())
            .rounded(m.radius_xl())
            .border_1()
            .border_color(theme.border)
            .bg(theme.raised())
            .text_size(m.font_footnote())
            .text_color(theme.fg_muted)
            .cursor_pointer()
            .hover(|pill| {
                pill.bg(theme.raised().blend(theme.hover))
                    .text_color(theme.fg)
            })
            .child(IconGlyph::new(
                Icon::ChevronDown,
                m.icon_xs(),
                theme.fg_muted,
            ))
            .child(tr!("Scroll to bottom"))
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                // Past the last row: the list clamps to the bottom and follows it again.
                list.scroll_to(ListOffset {
                    item_ix: list.item_count(),
                    offset_in_item: px(0.0),
                });
                scrolled.set(false);
                let _ = view.update(cx, |view, cx| {
                    view.rows.update(cx, |_, cx| cx.notify());
                    cx.notify();
                });
            }),
    )
}

fn banner(
    pane: PaneId,
    session: &ChatSession,
    owner: &WeakEntity<AppModel>,
    model: &AppModel,
) -> Option<AnyElement> {
    let m = model.metrics;
    let theme = &model.theme;
    let (text, retry) = match session.mode {
        ChatMode::Blocked => (
            session.blocked.as_ref().map_or_else(
                || tr!("Terminal input stays locked until recovery is confirmed.").to_string(),
                |reason| tr!("Chat recovery is blocked: %@", reason).to_string(),
            ),
            true,
        ),
        ChatMode::Transitioning => (tr!("Switching to Console…").to_string(), false),
        ChatMode::Chat | ChatMode::Console | ChatMode::Unrecognized(_) => return None,
    };
    let owner = owner.clone();
    Some(
        div()
            .debug_selector(|| "chat-banner".into())
            .flex()
            .items_center()
            .gap(m.spacing3())
            .px(m.spacing4())
            .py(m.spacing2())
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .text_size(m.font_footnote())
            .text_color(theme.fg)
            .child(div().flex_1().min_w_0().child(text))
            .when(retry, |banner| {
                banner.child(
                    controls::button(
                        Style { theme, metrics: &m },
                        &format!("chat-retry-{pane}"),
                        controls::Button::secondary(&tr!("Try Again")).enabled(!session.switching),
                        move |_, _, cx| {
                            let _ = owner.update(cx, |model, cx| model.chat_leave(pane, cx));
                        },
                    )
                    .debug_selector(|| "chat-retry".into()),
                )
            })
            .into_any_element(),
    )
}
