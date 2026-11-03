//! A floating stack of ticks on the pane's right edge, one per prompt, that
//! jumps the terminal to the prompt clicked.

use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::{
    Context, Hsla, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, Task, div, px, rgb,
};
use muxy_client::ClientError;
use muxy_protocol::{ErrorCode, PromptMark, PromptMarks};

use super::pane::{PaneEvent, PaneState, TerminalPane};

/// Space between ticks. The stack keeps the newest prompts that fit the pane.
const SPACING: f32 = 12.0;
/// The least time between requests while prompts keep moving.
const THROTTLE: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
pub(crate) struct Rail {
    marks: Vec<PromptMark>,
    /// The row of the tick under the pointer.
    hover: Option<u64>,
    throttle: Option<Task<()>>,
    /// Prompts changed while a request was throttled.
    stale: bool,
    /// The server predates prompt marks; ask again after the next attach.
    unsupported: bool,
    /// The tick last clicked, to find again in the next reply in case
    /// scrollback moved it.
    jump: Option<PromptMark>,
}

impl TerminalPane {
    /// Asks the server for the prompt rows, at most once per `THROTTLE`, for
    /// the focused pane; others catch up when focused.
    pub(crate) fn refresh_prompt_marks(&mut self, cx: &mut Context<Self>) {
        if !self.wants_prompt_marks() || !self.focused {
            return;
        }
        if self.rail.throttle.is_some() {
            self.rail.stale = true;
            return;
        }
        self.rail.stale = false;
        cx.emit(PaneEvent::PromptMarks);
        let executor = cx.background_executor().clone();
        self.rail.throttle = Some(cx.spawn(async move |pane, cx| {
            executor.timer(THROTTLE).await;
            let _ = pane.update(cx, |pane, cx| {
                pane.rail.throttle = None;
                if pane.rail.stale {
                    pane.refresh_prompt_marks(cx);
                }
            });
        }));
    }

    /// Forgets the last session's marks when the pane attaches to another.
    pub(super) fn reset_prompt_marks(&mut self) {
        self.rail = Rail::default();
    }

    fn wants_prompt_marks(&self) -> bool {
        self.terminal.options.prompt_rail && self.state == PaneState::Live && !self.rail.unsupported
    }

    pub(crate) fn receive_prompt_marks(
        &mut self,
        result: Result<PromptMarks, ClientError>,
        cx: &mut Context<Self>,
    ) {
        let jump = self.rail.jump.take();
        let marks = match result {
            Ok(marks) => marks.marks,
            Err(ClientError::Server(error)) if error.code == ErrorCode::Unsupported => {
                self.rail.unsupported = true;
                Vec::new()
            }
            // A passing failure keeps the last marks.
            Err(_) => return,
        };
        if let Some(jump) = jump
            && let Some(row) = moved(&jump, &marks)
        {
            self.jump_to_row(row, cx);
        }
        if marks != self.rail.marks {
            self.rail.marks = marks;
            cx.notify();
        }
    }

    /// Jumps at once, then again if the fresh marks show scrollback moved it.
    fn jump_to_mark(&mut self, mark: PromptMark, cx: &mut Context<Self>) {
        self.jump_to_row(mark.row, cx);
        if self.wants_prompt_marks() {
            self.rail.jump = Some(mark);
            cx.emit(PaneEvent::PromptMarks);
        }
    }

    fn jump_to_row(&mut self, row: u64, cx: &mut Context<Self>) {
        let Some(grid) = &self.grid else {
            return;
        };
        #[allow(
            clippy::cast_precision_loss,
            reason = "History rows stay far below f64's exact range"
        )]
        let rows = grid.history_total.saturating_sub(row) as f64;
        self.scroll_to(rows, cx);
        self.reveal_scrollbar(cx);
    }

    fn hover_mark(&mut self, row: u64, hovered: bool, cx: &mut Context<Self>) {
        let next = if hovered {
            Some(row)
        } else if self.rail.hover == Some(row) {
            None
        } else {
            self.rail.hover
        };
        if self.rail.hover != next {
            self.rail.hover = next;
            cx.notify();
        }
    }
}

/// The row `jump` sits at in `marks`, when that isn't where it was: the
/// nearest mark with its text.
fn moved(jump: &PromptMark, marks: &[PromptMark]) -> Option<u64> {
    let row = marks
        .iter()
        .filter(|mark| mark.text == jump.text)
        .map(|mark| mark.row)
        .min_by_key(|row| row.abs_diff(jump.row))?;
    (row != jump.row).then_some(row)
}

/// The marks that fit the pane, newest kept, and the one the view is at.
fn shown(pane: &TerminalPane) -> Option<(Vec<&PromptMark>, Option<u64>)> {
    if !pane.terminal.options.prompt_rail || pane.state != PaneState::Live || pane.reports_wheel() {
        return None;
    }
    let grid = pane.grid.as_ref()?;
    let total = grid.history_total;
    // The prompt holding the cursor is where you type, not somewhere to go back to.
    let typing = total + u64::from(grid.cursor.row);
    let mut marks: Vec<&PromptMark> = pane
        .rail
        .marks
        .iter()
        .filter(|mark| mark.row != typing)
        .collect();
    if marks.is_empty() {
        return None;
    }
    let rows = pane.viewport().unwrap_or(grid.size).rows;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "A pane's height in ticks is small and positive"
    )]
    let fit = ((f32::from(rows) * pane.cell_height / SPACING) as usize)
        .saturating_sub(2)
        .max(1);
    marks.drain(..marks.len().saturating_sub(fit));
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "Scroll offsets are whole, non-negative rows"
    )]
    let top = pane
        .scroll
        .view
        .is_some()
        .then(|| total.saturating_sub(pane.scroll.requested_rows().round() as u64));
    let selected = top.and_then(|top| {
        marks
            .iter()
            .rev()
            .find(|mark| mark.row <= top)
            .map(|mark| mark.row)
    });
    Some((marks, selected))
}

pub(crate) fn rail(
    pane: &TerminalPane,
    cx: &mut Context<TerminalPane>,
) -> Option<gpui::AnyElement> {
    let (marks, selected) = shown(pane)?;
    let (fg, bg) = (pane.palette.foreground, pane.palette.background);
    let lower = marks.len() / 2;
    let ticks = marks.iter().enumerate().map(|(index, mark)| {
        let row = mark.row;
        let target = (*mark).clone();
        let hovered = pane.rail.hover == Some(row);
        let (width, alpha) = if hovered {
            (14.0, 0.95)
        } else if selected == Some(row) {
            (11.0, 1.0)
        } else {
            (6.0, 0.35)
        };
        div()
            .id(("prompt-mark", usize::try_from(row).unwrap_or(usize::MAX)))
            .relative()
            .h(px(SPACING))
            .w(px(20.0))
            .flex()
            .items_center()
            .justify_end()
            .cursor_pointer()
            .on_hover(cx.listener(move |pane, hovered: &bool, _, cx| {
                pane.hover_mark(row, *hovered, cx);
            }))
            .on_click(cx.listener(move |pane, _, _, cx| {
                pane.jump_to_mark(target.clone(), cx);
            }))
            .child(
                div()
                    .h(px(2.0))
                    .w(px(width))
                    .rounded(px(1.0))
                    .bg(color(fg, alpha)),
            )
            .when(hovered, |tick| {
                tick.child(card(&mark.text, index >= lower, fg, bg))
            })
    });
    Some(
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .right(px(14.0))
            .flex()
            .flex_col()
            .justify_center()
            .child(
                div()
                    .id("prompt-rail")
                    .debug_selector(|| "prompt-rail".into())
                    .flex()
                    .flex_col()
                    .py(px(6.0))
                    // Mouse-up passes so a selection dragged over the stack still ends.
                    .map(|mut stack| {
                        for button in gpui::MouseButton::all() {
                            stack = stack.on_mouse_down(button, |_, _, cx| cx.stop_propagation());
                        }
                        stack
                    })
                    .children(ticks),
            )
            .into_any_element(),
    )
}

/// The prompt's text, beside its tick; it opens upward in the stack's lower half.
fn card(text: &str, upward: bool, fg: u32, bg: u32) -> impl IntoElement {
    div()
        .absolute()
        .right(px(24.0))
        .when(upward, |card| card.bottom(px(-8.0)))
        .when(!upward, |card| card.top(px(-8.0)))
        .w(px(320.0))
        .px(px(12.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .bg(rgb(bg))
        .border_1()
        .border_color(color(fg, 0.18))
        .shadow_lg()
        .text_size(px(12.0))
        .text_color(color(fg, 0.95))
        .truncate()
        .child(text.to_owned())
}

fn color(hex: u32, alpha: f32) -> Hsla {
    let mut color: Hsla = rgb(hex).into();
    color.a = alpha;
    color
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(row: u64, text: &str) -> PromptMark {
        PromptMark {
            row,
            text: text.to_owned(),
        }
    }

    #[test]
    fn a_clicked_tick_follows_its_message_when_scrollback_shifts() {
        let clicked = mark(100, "fix it");
        // Eviction moved every row up by 30; the same text also appears later.
        let fresh = [mark(40, "ls"), mark(70, "fix it"), mark(300, "fix it")];
        assert_eq!(moved(&clicked, &fresh), Some(70));
        assert_eq!(moved(&clicked, &[mark(100, "fix it")]), None);
        assert_eq!(moved(&clicked, &[mark(100, "other")]), None);
    }
}
