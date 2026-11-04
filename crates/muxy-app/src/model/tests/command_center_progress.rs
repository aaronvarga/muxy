use super::command_center::{listing, session};
use super::*;
use muxy_app_core::command_center::GridLayout;
use muxy_protocol::{
    ActivitySnapshot, AgentActivity, AgentProvider, AgentState, ProgressState, SessionProgress,
    TerminalProgress,
};

#[gpui::test]
#[allow(
    clippy::too_many_lines,
    reason = "Progress transitions, stable viewports and collapsed activity"
)]
fn progress_border_matches_session_activity_without_resizing_and_survives_collapse(
    cx: &mut TestAppContext,
) {
    let state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    let (mut boot, requests) = stub_boot(state);
    boot.settings.appearance.command_center_layout = GridLayout::TwoByOne;
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    cx.simulate_resize(size(px(1200.0), px(700.0)));
    let update = |model: &mut AppModel,
                  id: u64,
                  state: Option<ProgressState>,
                  cx: &mut Context<AppModel>| {
        model.receive_progress(
            ServerId::local(),
            SessionId::new(id).expect("session"),
            SessionProgress {
                progress: state.map(|state| TerminalProgress {
                    state,
                    percent: Some(30),
                }),
                completed: 0,
            },
            cx,
        );
    };
    view.update(cx, |model, cx| {
        model.receive(
            local(
                1,
                Update::Connected(vec![session(home, 1).info, session(home, 2).info]),
            ),
            cx,
        );
        acknowledge_catalog(model, cx);
        model.open_command_center(cx);
        model.receive_session_page(
            home,
            Ok(listing(vec![session(home, 1), session(home, 2)])),
            cx,
        );
        update(model, 1, Some(ProgressState::Running), cx);
        update(model, 2, Some(ProgressState::Paused), cx);
    });
    cx.run_until_parked();
    let bar = cx
        .debug_bounds("command-center-progress-0-Running")
        .expect("working border");
    let session = cx
        .debug_bounds("command-center-terminal-0")
        .expect("session");
    assert!((bar.top() - session.top()).abs() <= px(0.5));
    assert_eq!(bar.left(), session.left());
    assert_eq!(bar.right(), session.right());
    assert_eq!(bar.size.height, px(4.0));
    assert!(
        cx.debug_bounds("command-center-progress-1-Paused")
            .is_some()
    );
    let geometry = view.read_with(cx, |model, cx| {
        model
            .command_center
            .tiles
            .iter()
            .map(|tile| {
                model.grids[&tile.pane]
                    .view
                    .read(cx)
                    .geometry
                    .expect("geometry")
                    .0
            })
            .collect::<Vec<_>>()
    });
    requests.try_iter().for_each(drop);
    view.update(cx, |model, cx| {
        update(model, 1, None, cx);
        update(model, 2, Some(ProgressState::Error), cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("command-center-progress-1-Error").is_some());
    view.read_with(cx, |model, cx| {
        assert!(
            model
                .servers
                .get(ServerId::local())
                .expect("local")
                .progress[&SessionId::new(1).expect("session")]
                .progress
                .is_none()
        );
        assert_eq!(
            model
                .command_center
                .tiles
                .iter()
                .map(|tile| model.grids[&tile.pane]
                    .view
                    .read(cx)
                    .geometry
                    .expect("geometry")
                    .0)
                .collect::<Vec<_>>(),
            geometry
        );
    });
    assert!(
        !requests
            .try_iter()
            .any(|(_, work)| matches!(work, Work::Resize(..)))
    );
    view.update(cx, |model, cx| {
        model.receive_activity(
            ServerId::local(),
            Ok(ActivitySnapshot {
                revision: 1,
                agents: vec![AgentActivity {
                    project: home,
                    session: SessionId::new(1).expect("session"),
                    provider: AgentProvider::Codex,
                    state: AgentState::Working,
                }],
                ..ActivitySnapshot::default()
            }),
            cx,
        );
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("command-center-progress-0-Indeterminate")
            .is_some()
    );
    preferences::click_preference(cx, "command-center-collapse-0-false");
    let bar = cx
        .debug_bounds("command-center-progress-0-Indeterminate")
        .expect("collapsed activity");
    let header = cx
        .debug_bounds("command-center-drag-0")
        .expect("collapsed header");
    assert!(bar.top() >= header.top() && bar.bottom() <= header.bottom());
    view.read_with(cx, |model, _| {
        assert!(
            !model
                .grids
                .contains_key(&model.command_center.tiles[0].pane)
        );
        assert_eq!(
            model
                .servers
                .get(ServerId::local())
                .expect("local")
                .progress[&SessionId::new(2).expect("session")]
                .progress
                .expect("error progress")
                .state,
            ProgressState::Error
        );
    });
}

#[gpui::test]
fn clicking_another_command_center_session_focuses_its_docked_composer(cx: &mut TestAppContext) {
    let state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    let (mut boot, _requests) = stub_boot(state);
    boot.settings.appearance.command_center_layout = GridLayout::TwoByOne;
    boot.settings.composer.presentation = muxy_app_core::settings::ComposerPresentation::Docked;
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    cx.simulate_resize(size(px(1200.0), px(700.0)));
    view.update(cx, |model, cx| {
        model.receive(
            local(
                1,
                Update::Connected(vec![session(home, 1).info, session(home, 2).info]),
            ),
            cx,
        );
        acknowledge_catalog(model, cx);
        model.open_command_center(cx);
        model.receive_session_page(
            home,
            Ok(listing(vec![session(home, 1), session(home, 2)])),
            cx,
        );
    });
    cx.run_until_parked();
    let focus = |view: &Entity<AppModel>, cx: &mut VisualTestContext| {
        view.update_in(cx, |model, window, cx| {
            let pane = model.active_pane().expect("pane");
            let composer = model.composer.view.clone().expect("composer");
            (
                pane,
                gpui::Focusable::focus_handle(composer.read(cx), cx).contains_focused(window, cx),
                model
                    .terminal(&pane)
                    .expect("terminal")
                    .view
                    .read(cx)
                    .focus
                    .is_focused(window),
            )
        })
    };
    let second = view.read_with(cx, |model, _| model.command_center.tiles[1].pane);
    let tile = cx
        .debug_bounds("command-center-terminal-1")
        .expect("second session");
    let terminal = gpui::point(tile.center().x, tile.top() + px(40.0));
    cx.simulate_click(terminal, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(focus(&view, cx), (second, true, false));
    // Another click in the now-active terminal keeps the keyboard there.
    cx.simulate_click(terminal, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(focus(&view, cx), (second, false, true));
}
