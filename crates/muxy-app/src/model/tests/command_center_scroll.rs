use super::command_center::{listing, session};
use super::*;
use muxy_app_core::command_center::GridLayout;

#[gpui::test]
fn first_wheel_event_scrolls_without_touching_the_scrollbar(cx: &mut TestAppContext) {
    for grouped in [false, true] {
        for delta in [
            gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-200.0))),
            gpui::ScrollDelta::Lines(gpui::point(0.0, -10.0)),
        ] {
            let state = AppState::bootstrap().expect("state");
            let home = state.home().id;
            let (mut boot, _requests) = stub_boot(state);
            boot.settings.appearance.command_center_layout = GridLayout::TwoByTwo;
            boot.settings.appearance.command_center_group_by_project = grouped;
            let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
            cx.simulate_resize(size(px(1400.0), px(900.0)));
            view.update(cx, |model, cx| {
                model.receive(
                    local(
                        1,
                        Update::Connected((1..=20).map(|id| session(home, id).info).collect()),
                    ),
                    cx,
                );
                acknowledge_catalog(model, cx);
                model.open_command_center(cx);
                model.receive_session_page(
                    home,
                    Ok(listing((1..=20).map(|id| session(home, id)).collect())),
                    cx,
                );
            });
            cx.run_until_parked();
            let header = cx.debug_bounds("command-center-drag-0").expect("header");
            cx.simulate_event(gpui::ScrollWheelEvent {
                position: header.center(),
                delta,
                ..Default::default()
            });
            cx.run_until_parked();
            view.read_with(cx, |model, _| {
                let offset = model.command_center.scroll.logical_scroll_top();
                assert!(
                    offset.item_ix > 0 || offset.offset_in_item >= px(199.0),
                    "the first wheel event must scroll its full distance: grouped={grouped}, delta={delta:?}, offset={offset:?}"
                );
                assert!(model.grids.len() <= 12, "offscreen terminals stay virtualized");
            });
        }
    }
}
