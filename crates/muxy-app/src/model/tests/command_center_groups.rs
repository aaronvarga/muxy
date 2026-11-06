use super::command_center::{listing, session};
use super::*;
use muxy_app_core::command_center::GridLayout;
use muxy_protocol::ChannelId;

#[gpui::test]
#[allow(
    clippy::too_many_lines,
    reason = "Project grouping, collapse safety and restored terminal state"
)]
fn project_sections_group_interleaved_tiles_and_collapse_without_ending_sessions(
    cx: &mut TestAppContext,
) {
    let (state, first, second, _, _) = projects::two_projects();
    let (mut boot, requests) = stub_boot(state);
    boot.settings.appearance.command_center_layout = GridLayout::TwoByTwo;
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    cx.simulate_resize(size(px(1400.0), px(1000.0)));
    let panes = view.update(cx, |model, cx| {
        model.receive(
            local(
                1,
                Update::Connected(vec![
                    session(first, 1).info,
                    session(second, 2).info,
                    session(first, 3).info,
                ]),
            ),
            cx,
        );
        acknowledge_catalog(model, cx);
        model.open_command_center(cx);
        model.receive_session_page(
            first,
            Ok(listing(vec![session(first, 1), session(first, 3)])),
            cx,
        );
        model.receive_session_page(second, Ok(listing(vec![session(second, 2)])), cx);
        model.appearance.command_center_order = vec![
            (ServerId::local(), SessionId::new(1).expect("session")),
            (ServerId::local(), SessionId::new(2).expect("session")),
            (ServerId::local(), SessionId::new(3).expect("session")),
        ];
        model.sync_command_center(cx);
        model
            .command_center
            .tiles
            .iter()
            .map(|tile| tile.pane)
            .collect::<Vec<_>>()
    });
    cx.run_until_parked();
    preferences::click_preference(cx, "command-center-layout");
    preferences::click_preference(cx, "menu-label-Group by Project");
    let group_selector = |project, collapsed| -> &'static str {
        format!("command-center-group-{project}-{collapsed}").leak()
    };
    let header = cx
        .debug_bounds(group_selector(first, false))
        .expect("first section");
    let second_header = cx
        .debug_bounds(group_selector(second, false))
        .expect("second section");
    let first_tile = cx
        .debug_bounds("command-center-drag-0")
        .expect("first tile");
    let third_tile = cx
        .debug_bounds("command-center-drag-2")
        .expect("same project tile");
    assert_eq!(first_tile.top(), third_tile.top());
    assert!(third_tile.left() > first_tile.left());
    assert!(header.bottom() < first_tile.top());
    assert!(second_header.top() > first_tile.bottom());
    let (before, order) = view.update(cx, |model, cx| {
        assert!(
            muxy_app_core::settings::Appearance::load(&model.path.with_file_name("settings.toml"))
                .expect("saved appearance")
                .command_center_group_by_project
        );
        model.start_attach(panes[0], Size { cols: 60, rows: 20 }, cx);
        let mut attached = attachment();
        attached.channel = ChannelId(88);
        model.receive(
            local(
                1,
                Update::Attached {
                    pane: panes[0],
                    session: SessionId::new(1).expect("session"),
                    attachment: attached,
                    created: false,
                },
            ),
            cx,
        );
        model.toggle_command_center_terminal(panes[2], cx);
        (
            model.state.clone(),
            model.appearance.command_center_order.clone(),
        )
    });
    cx.run_until_parked();
    requests.try_iter().for_each(drop);
    preferences::click_preference(cx, group_selector(first, false));
    view.read_with(cx, |model, _| {
        assert!(
            model
                .command_center
                .collapsed_projects
                .contains(&(ServerId::local(), first)),
            "header click must collapse its project"
        );
    });
    assert!(cx.debug_bounds(group_selector(first, true)).is_some());
    assert!(cx.debug_bounds("command-center-drag-1").is_some());
    view.read_with(cx, |model, _| {
        assert_eq!(model.command_center.tiles.len(), 3);
        assert!(!model.visible_panes().contains(&panes[0]));
        assert!(!model.grids.contains_key(&panes[0]));
        assert_eq!(model.active_pane(), Some(panes[1]));
        assert_eq!(model.state, before);
    });
    let work: Vec<_> = requests.try_iter().map(|(_, work)| work).collect();
    assert!(
        work.iter()
            .any(|work| matches!(work, Work::Detach(ChannelId(88))))
    );
    assert!(!work.iter().any(|work| matches!(
        work,
        Work::Discard(..) | Work::EndAll(_) | Work::StopServer { .. }
    )));
    preferences::click_preference(cx, group_selector(second, false));
    view.read_with(cx, |model, _| {
        assert!(model.visible_panes().is_empty());
        assert!(model.active_pane().is_none());
        assert!(model.grids.is_empty());
    });
    preferences::click_preference(cx, group_selector(first, true));
    assert!(cx.debug_bounds("command-center-terminal-0").is_some());
    assert!(cx.debug_bounds("command-center-collapse-2-true").is_some());
    view.update(cx, |model, cx| {
        model.focus_pane(panes[0], cx);
        model.focus_direction(Direction::Down, cx);
        assert_ne!(model.command_center.focused, Some(panes[1]));
        model.select_project(model.state.current_project().id, cx);
        model.open_command_center(cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds(group_selector(second, true)).is_some());
    preferences::click_preference(cx, "command-center-layout");
    preferences::click_preference(cx, "menu-label-Group by Project");
    assert!(cx.debug_bounds("command-center-drag-1").is_some());

    view.read_with(cx, |model, _| {
        assert!(!model.appearance.command_center_group_by_project);
        assert_eq!(model.appearance.command_center_order, order);
        assert_eq!(model.state, before);
    });
}

#[gpui::test]
fn project_command_center_shows_only_the_current_project_and_toggles_back(cx: &mut TestAppContext) {
    let (state, first, second, _, _) = projects::two_projects();
    let (boot, _requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.receive(
            local(
                1,
                Update::Connected(vec![session(first, 1).info, session(second, 2).info]),
            ),
            cx,
        );
        acknowledge_catalog(model, cx);
        assert_eq!(model.state.current_project().id, second);
        model.toggle_project_command_center(cx);
        model.receive_session_page(first, Ok(listing(vec![session(first, 1)])), cx);
        model.receive_session_page(second, Ok(listing(vec![session(second, 2)])), cx);
        let projects = |model: &AppModel| {
            model
                .command_center
                .tiles
                .iter()
                .map(|tile| tile.session.info.project)
                .collect::<Vec<_>>()
        };
        assert_eq!(projects(model), vec![second]);

        // The global command center still lists every project.
        model.open_command_center(cx);
        assert_eq!(model.command_center.scope, None);
        assert_eq!(projects(model).len(), 2);

        model.toggle_project_command_center(cx);
        assert_eq!(projects(model), vec![second]);
        model.toggle_project_command_center(cx);
        assert!(!model.command_center.active);
        assert_eq!(model.state.current_project().id, second);
    });
}

#[gpui::test]
fn reordering_a_project_command_center_keeps_other_projects_places(cx: &mut TestAppContext) {
    let (state, first, second, _, _) = projects::two_projects();
    let (boot, _requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        let sessions = [session(first, 1), session(second, 2), session(second, 3)];
        model.receive(
            local(
                1,
                Update::Connected(
                    sessions
                        .iter()
                        .map(|session| session.info.clone())
                        .collect(),
                ),
            ),
            cx,
        );
        acknowledge_catalog(model, cx);
        let key = |id| (ServerId::local(), SessionId::new(id).expect("session"));
        model.appearance.command_center_order = vec![key(2), key(1), key(3)];
        model.toggle_project_command_center(cx);
        model.receive_session_page(first, Ok(listing(vec![session(first, 1)])), cx);
        model.receive_session_page(
            second,
            Ok(listing(vec![session(second, 2), session(second, 3)])),
            cx,
        );
        let panes: Vec<_> = model
            .command_center
            .tiles
            .iter()
            .map(|tile| tile.pane)
            .collect();
        assert_eq!(panes.len(), 2);
        model.reorder_command_center(panes[1], panes[0], cx);
        assert_eq!(
            model.appearance.command_center_order,
            vec![key(3), key(1), key(2)]
        );
    });
}

#[gpui::test]
fn hidden_terminals_leave_the_command_center_but_keep_running(cx: &mut TestAppContext) {
    let (state, first, second, _, _) = projects::two_projects();
    let (boot, requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.receive(
            local(
                1,
                Update::Connected(vec![session(first, 1).info, session(second, 2).info]),
            ),
            cx,
        );
        acknowledge_catalog(model, cx);
        model.open_command_center(cx);
        model.receive_session_page(first, Ok(listing(vec![session(first, 1)])), cx);
        model.receive_session_page(second, Ok(listing(vec![session(second, 2)])), cx);
        let ids = |model: &AppModel| {
            model
                .command_center
                .tiles
                .iter()
                .map(|tile| tile.session.info.id.get())
                .collect::<Vec<_>>()
        };
        let one = SessionId::new(1).expect("session");
        model.toggle_command_center_session(ServerId::local(), one, cx);
        assert_eq!(ids(model), vec![2]);
        // A project's own command center still shows it.
        model.toggle_project_command_center(cx);
        model.select_project(first, cx);
        model.toggle_project_command_center(cx);
        assert_eq!(ids(model), vec![1]);

        // New terminals show by default in the global one.
        model.open_command_center(cx);
        model.receive_session_page(
            second,
            Ok(listing(vec![session(second, 2), session(second, 3)])),
            cx,
        );
        assert_eq!(ids(model), vec![2, 3]);

        model.toggle_command_center_session(ServerId::local(), one, cx);
        assert_eq!(ids(model).len(), 3);
    });
    assert!(
        !requests
            .try_iter()
            .any(|(_, request)| matches!(request, Work::Discard(..))),
        "hiding a terminal must not end it"
    );
}

#[gpui::test]
fn quick_terminal_is_not_offered_in_the_command_center_menu(cx: &mut TestAppContext) {
    let (state, first, second, _, _) = projects::two_projects();
    let (boot, _requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.receive(
            local(
                1,
                Update::Connected(vec![session(first, 1).info, session(second, 2).info]),
            ),
            cx,
        );
        acknowledge_catalog(model, cx);
        model.receive_session_page(first, Ok(listing(vec![session(first, 1)])), cx);
        model.receive_session_page(second, Ok(listing(vec![session(second, 2)])), cx);
        let quick = model.state.ensure_quick_terminal();
        let one = SessionId::new(1).expect("session");
        model
            .state
            .set_pane_session(quick, Some(one))
            .expect("session");
        // The title, then one project's heading and terminal: the Quick Terminal never gets a tile.
        assert_eq!(model.command_center_session_items().len(), 3);
    });
}

#[gpui::test]
fn project_session_counts_leave_out_the_quick_terminal_and_ended_terminals(
    cx: &mut TestAppContext,
) {
    let (state, _, _, _, _) = projects::two_projects();
    let (boot, _requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.receive(local(1, Update::Connected(vec![])), cx);
        acknowledge_catalog(model, cx);
        let home = model.state.home().id;
        let mut ended = session(home, 3);
        ended.status = muxy_protocol::SessionStatus::Ended;
        model.receive_session_page(
            home,
            Ok(listing(vec![session(home, 1), session(home, 2), ended])),
            cx,
        );
        let quick = model.state.ensure_quick_terminal();
        let one = SessionId::new(1).expect("session");
        model
            .state
            .set_pane_session(quick, Some(one))
            .expect("session");
        let home = model.state.home().clone();
        assert_eq!(model.running_session_count(&home), 1);
    });
}
