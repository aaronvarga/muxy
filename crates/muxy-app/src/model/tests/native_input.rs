use super::*;
use gpui::{App, Bounds, InputHandler, IntoElement, ParentElement, Pixels, Render, Styled, Window};
use muxy_app_core::settings::TerminalSettings;
use std::{cell::Cell, ops::Range};

struct NativeInputGrid {
    panes: Vec<PaneView>,
    focus: FocusHandle,
    commits: Rc<Cell<usize>>,
}

impl Render for NativeInputGrid {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let focus = self.focus.clone();
        let commits = self.commits.clone();
        gpui::div()
            .size_full()
            .flex()
            .child(gpui::canvas(
                |_, _, _| (),
                move |_, (), window, cx| {
                    window.handle_input(&focus, NativeRedraw(commits), cx);
                },
            ))
            .children(self.panes.iter().map(PaneView::element))
    }
}

struct NativeRedraw(Rc<Cell<usize>>);

impl InputHandler for NativeRedraw {
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut App,
    ) -> Option<gpui::UTF16Selection> {
        None
    }
    fn marked_text_range(&mut self, _: &mut Window, _: &mut App) -> Option<Range<usize>> {
        None
    }
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<String> {
        None
    }
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        _: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0.set(self.0.get() + 1);
        // macOS can flush a redraw before returning the borrowed native input handler.
        window.draw(cx).clear();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        _: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        _: &mut App,
    ) {
    }
    fn unmark_text(&mut self, _: &mut Window, _: &mut App) {}
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<Bounds<Pixels>> {
        None
    }
    fn character_index_for_point(
        &mut self,
        _: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<usize> {
        None
    }
}

#[gpui::test]
fn native_redraw_keeps_later_terminal_tiles_safe(cx: &mut TestAppContext) {
    let commits = Rc::new(Cell::new(0));
    let observed = commits.clone();
    let (_, cx) = cx.add_window_view(|window, cx| {
        let focus = cx.focus_handle();
        focus.focus(window);
        let panes = (0..3)
            .map(|_| {
                let view = cx.new(|cx| {
                    TerminalPane::new(Palette::new(true), TerminalSettings::default(), cx)
                });
                let subscription = cx.observe(&view, |_, _, cx| cx.notify());
                PaneView {
                    view,
                    _subscription: subscription,
                }
            })
            .collect();
        NativeInputGrid {
            panes,
            focus,
            commits,
        }
    });
    cx.run_until_parked();
    cx.simulate_input("abc");
    assert_eq!(observed.get(), 3);
}
