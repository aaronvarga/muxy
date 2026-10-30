use super::command_center::{listing, session};
use super::*;
use gpui::MouseButton;
use muxy_app_core::command_center::GridLayout;
use muxy_protocol::{ActivitySnapshot, AgentActivity, AgentProvider, AgentState, ChannelId};

#[gpui::test]
#[allow(
    clippy::too_many_lines,
    reason = "Collapse, compact rows, detach safety and reopening the same sessions"
)]
fn collapsing_tiles_keeps_sessions_and_attention_without_live_hidden_views(
    cx: &mut TestAppContext,
) {
    let state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    let (mut boot, requests) = stub_boot(state);
    boot.settings.appearance.command_center_layout = GridLayout::TwoByTwo;
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    cx.simulate_resize(size(px(1400.0), px(900.0)));
    let panes = view.update(cx, |model, cx| {
        model.receive(
            local(
                1,
                Update::Connected((1..=4).map(|id| session(home, id).info).collect()),
            ),
            cx,
        );
        acknowledge_catalog(model, cx);
        model.open_command_center(cx);
        model.receive_session_page(
            home,
            Ok(listing((1..=4).map(|id| session(home, id)).collect())),
            cx,
        );
        let panes: Vec<_> = model
            .command_center
            .tiles
            .iter()
            .map(|tile| tile.pane)
            .collect();
        model.command_center.visible = panes.clone();
        model.sync_visible(cx);
        for (index, pane) in panes.iter().enumerate() {
            model.start_attach(*pane, Size { cols: 60, rows: 20 }, cx);
            let mut attached = attachment();
            attached.channel = ChannelId(u32::try_from(index + 10).expect("channel"));
            model.receive(
                local(
                    1,
                    Update::Attached {
                        pane: *pane,
                        session: SessionId::new(u64::try_from(index + 1).expect("session"))
                            .expect("session"),
                        attachment: attached,
                        created: false,
                    },
                ),
                cx,
            );
        }
        model.receive_activity(
            ServerId::local(),
            Ok(ActivitySnapshot {
                revision: 1,
                agents: vec![AgentActivity {
                    project: home,
                    session: SessionId::new(1).expect("session"),
                    provider: AgentProvider::Codex,
                    state: AgentState::Blocked,
                }],
                ..ActivitySnapshot::default()
            }),
            cx,
        );
        panes
    });
    cx.run_until_parked();
    let before = view.read_with(cx, |model, _| model.state.clone());
    let full_row = cx.debug_bounds("command-center-row-0").expect("row");
    requests.try_iter().for_each(drop);
    preferences::click_preference(cx, "command-center-collapse-0-false");
    view.read_with(cx, |model, _| {
        assert!(!model.grids.contains_key(&panes[0]));
        assert!(!model.visible_panes().contains(&panes[0]));
        assert_ne!(model.active_pane(), Some(panes[0]));
        assert_eq!(model.state, before);
    });
    assert!(
        cx.debug_bounds("command-center-attention-0-Blocked")
            .is_some()
    );
    assert_eq!(
        cx.debug_bounds("command-center-row-0")
            .expect("mixed row")
            .size,
        full_row.size
    );
    let collapsed = cx
        .debug_bounds("command-center-tile-0-Blocked-focused-false")
        .expect("collapsed tile");
    let header = cx
        .debug_bounds("command-center-drag-0")
        .expect("collapsed header");
    assert!((collapsed.size.height - header.size.height - px(4.0)).abs() <= px(0.5));
    let detached: Vec<_> = requests.try_iter().map(|(_, work)| work).collect();
    assert!(
        detached
            .iter()
            .any(|work| matches!(work, Work::Detach(ChannelId(10))))
    );
    assert!(!detached.iter().any(|work| matches!(
        work,
        Work::Discard(..) | Work::EndAll(_) | Work::StopServer { .. }
    )));
    preferences::click_preference(cx, "command-center-collapse-1-false");
    let compact_row = cx
        .debug_bounds("command-center-row-0")
        .expect("compact row");
    assert!(compact_row.size.height < full_row.size.height / 2.0);
    let expanded_row = cx
        .debug_bounds("command-center-row-1")
        .expect("expanded row");
    assert!(expanded_row.size.height > full_row.size.height);
    let at = cx
        .debug_bounds("command-center-drag-0")
        .expect("header")
        .center();
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("menu-label-Expand Terminal").is_some());
    view.update(cx, AppModel::dismiss_overlay);
    preferences::click_preference(cx, "command-center-collapse-2-false");
    preferences::click_preference(cx, "command-center-collapse-3-false");
    view.read_with(cx, |model, _| {
        assert!(model.grids.is_empty());
        assert!(model.visible_panes().is_empty());
        assert!(model.active_pane().is_none());
        assert_eq!(model.state, before);
    });
    view.update(cx, |model, cx| {
        model.close_command_center(cx);
        model.open_command_center(cx);
        model.sync_command_center(cx);
        model.set_command_center_layout(GridLayout::Auto, cx);
        assert!(
            model
                .command_center
                .tiles
                .iter()
                .all(|tile| model.command_center.is_collapsed(tile))
        );
    });
    cx.run_until_parked();
    preferences::click_preference(cx, "command-center-collapse-3-true");
    view.read_with(cx, |model, _| {
        assert_eq!(model.visible_panes(), [panes[3]]);
        assert_eq!(model.active_pane(), Some(panes[3]));
        assert_eq!(model.grids.len(), 1);
        assert_eq!(model.state, before);
    });
}

#[gpui::test]
fn focus_navigation_skips_collapsed_sessions_and_collapse_survives_reorder(
    cx: &mut TestAppContext,
) {
    let state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    let (mut boot, _requests) = stub_boot(state);
    boot.settings.appearance.command_center_layout = GridLayout::ThreeByOne;
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.receive(
            local(
                1,
                Update::Connected((1..=3).map(|id| session(home, id).info).collect()),
            ),
            cx,
        );
        acknowledge_catalog(model, cx);
        model.open_command_center(cx);
        model.receive_session_page(
            home,
            Ok(listing((1..=3).map(|id| session(home, id)).collect())),
            cx,
        );
    });
    cx.run_until_parked();
    view.update(cx, |model, cx| {
        let panes: Vec<_> = model
            .command_center
            .tiles
            .iter()
            .map(|tile| tile.pane)
            .collect();
        model.toggle_command_center_terminal(panes[1], cx);
        model.focus_pane(panes[0], cx);
        model.focus_direction(Direction::Right, cx);
        assert_eq!(model.active_pane(), Some(panes[2]));
        model.focus_direction(Direction::Left, cx);
        assert_eq!(model.active_pane(), Some(panes[0]));
        model.reorder_command_center(panes[1], panes[0], cx);
        assert!(
            model
                .command_center
                .is_collapsed(&model.command_center.tiles[0])
        );
        model.focus_pane(panes[1], cx);
        assert_ne!(model.active_pane(), Some(panes[1]));
    });
}

#[gpui::test]
fn collapse_state_is_scoped_to_the_server_and_removed_when_a_session_ends(cx: &mut TestAppContext) {
    let remote = ServerId::new();
    let state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    let (boot, _local_work, _remotes) = remote_boot(state, vec![servers::entry(remote, "box")]);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.receive(local(1, Update::Connected(vec![session(home, 1).info])), cx);
        acknowledge_catalog(model, cx);
        let remote_home = servers::connect_remote(model, remote, vec![], &[], cx);
        model.open_command_center(cx);
        model.receive_session_page(home, Ok(listing(vec![session(home, 1)])), cx);
        model.receive_session_page(remote_home, Ok(listing(vec![session(remote_home, 1)])), cx);
        let local = model.command_center.tiles[0].pane;
        model.toggle_command_center_terminal(local, cx);
        assert!(
            model
                .command_center
                .is_collapsed(&model.command_center.tiles[0])
        );
        assert!(
            !model
                .command_center
                .is_collapsed(&model.command_center.tiles[1])
        );
        model.forget_listed_session(ServerId::local(), SessionId::new(1).expect("session"));
        assert!(model.command_center.collapsed.is_empty());
    });
}
