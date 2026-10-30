use super::*;
use muxy_app_core::command_center::GridLayout;
use muxy_protocol::{ChannelId, ProjectSession, ProjectSessions, ServerPath, SessionStatus};

pub(super) fn session(project: ProjectId, id: u64) -> ProjectSession {
    ProjectSession {
        info: SessionInfo {
            project,
            id: SessionId::new(id).expect("session"),
            directory: ServerPath(b"/tmp".to_vec()),
        },
        status: SessionStatus::Live,
        owner: None,
        attached: true,
    }
}

pub(super) fn listing(sessions: Vec<ProjectSession>) -> ProjectSessions {
    ProjectSessions {
        revision: 1,
        sessions,
        next: None,
    }
}

#[gpui::test]
fn sidebar_command_center_icon_stays_below_the_titlebar_and_fits_large_icons(
    cx: &mut TestAppContext,
) {
    let (mut boot, _requests) = stub_boot(AppState::bootstrap().expect("state"));
    boot.settings.appearance.agents_panel_visible = false;
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    cx.simulate_resize(size(px(640.0), px(400.0)));
    for (expanded, icon_size) in [(false, 28), (false, 48), (true, 28), (true, 48)] {
        view.update(cx, |model, cx| {
            model.appearance.sidebar_expanded = expanded;
            model.appearance.sidebar_icon_size = icon_size;
            model.open_command_center(cx);
            cx.notify();
        });
        cx.run_until_parked();
        let button = cx
            .debug_bounds("command-center-button")
            .expect("sidebar button");
        let titlebar_height = view.read_with(cx, |model, _| model.metrics.title_bar_height());
        assert!(
            button.top() >= titlebar_height,
            "the titlebar must not cover the shortcut: {button:?}"
        );
        let icon = cx
            .debug_bounds("command-center-sidebar-icon")
            .expect("colored icon tile");
        assert!(icon.top() >= button.top() && icon.bottom() <= button.bottom());
        assert!(icon.left() >= button.left() && icon.right() <= button.right());
        assert_eq!(
            icon.size,
            size(px(f32::from(icon_size)), px(f32::from(icon_size)))
        );
        if expanded {
            let filter = cx
                .debug_bounds("sidebar-project-filter")
                .expect("project filter");
            let home = cx.debug_bounds("project-row-0").expect("Home row");
            let above = button.top() - filter.bottom();
            let below = home.top() - button.bottom();
            assert!(above > px(0.0), "the shortcut needs space below the filter");
            assert!(
                below > px(0.0) && below < above,
                "Home should sit close to Command Center"
            );
        } else {
            let selection = cx
                .debug_bounds("command-center-selection")
                .expect("selected outline");
            assert_eq!(selection, button);
            assert!(selection.left() < icon.left() && selection.right() > icon.right());
            assert!(selection.top() < icon.top() && selection.bottom() > icon.bottom());
        }
    }
}

#[gpui::test]
fn command_center_color_menu_saves_and_resets_without_changing_projects(cx: &mut TestAppContext) {
    let (mut boot, _requests) = stub_boot(AppState::bootstrap().expect("state"));
    boot.settings.appearance.agents_panel_visible = false;
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    cx.simulate_resize(size(px(1000.0), px(900.0)));
    cx.run_until_parked();
    let before = view.read_with(cx, |model, _| model.state.clone());
    for (label, expected, checked) in [
        (
            "menu-label-Violet",
            muxy_app_core::PROJECT_COLORS[10].1,
            "✓Default",
        ),
        ("menu-label-Default", "", "✓Violet"),
    ] {
        let at = cx
            .debug_bounds("command-center-button")
            .expect("shortcut")
            .center();
        cx.simulate_mouse_move(at, None, Modifiers::default());
        cx.simulate_mouse_down(at, gpui::MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(at, gpui::MouseButton::Right, Modifiers::default());
        cx.run_until_parked();
        preferences::click_preference(cx, "menu-label-Color");
        view.read_with(cx, |model, _| {
            assert!(
                model.menu_outline()[1]
                    .iter()
                    .any(|row| row.trim_start_matches('>') == checked)
            );
        });
        preferences::click_preference(cx, label);
        view.read_with(cx, |model, _| {
            assert!(
                !model.command_center.active,
                "a color choice must not navigate"
            );
            assert_eq!(model.state, before);
            assert_eq!(model.appearance.command_center_color, expected);
            assert_eq!(
                muxy_app_core::settings::Appearance::load(
                    &model.path.with_file_name("settings.toml")
                )
                .expect("saved appearance")
                .command_center_color,
                expected
            );
        });
    }
}

#[gpui::test]
fn dashboard_attaches_existing_sessions_without_editing_projects_or_closing_processes(
    cx: &mut TestAppContext,
) {
    let (mut state, first, second, a, b) = projects::two_projects();
    state
        .set_pane_session(a, Some(SessionId::new(41).expect("session")))
        .expect("session");
    state
        .set_pane_session(b, Some(SessionId::new(42).expect("session")))
        .expect("session");
    state.select_project(first).expect("project");
    let (boot, requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.receive(
            local(
                1,
                Update::Connected(vec![
                    session(first, 41).info,
                    session(second, 42).info,
                    session(second, 43).info,
                ]),
            ),
            cx,
        );
        acknowledge_catalog(model, cx);
        let before = model.state.clone();
        requests.try_iter().for_each(drop);
        model.open_command_center(cx);
        model.receive_session_page(first, Ok(listing(vec![session(first, 41)])), cx);
        model.receive_session_page(
            second,
            Ok(listing(vec![session(second, 42), session(second, 43)])),
            cx,
        );
        assert_eq!(
            model.command_center.tiles.len(),
            3,
            "includes a detached session"
        );
        let tiles: Vec<_> = model
            .command_center
            .tiles
            .iter()
            .map(|tile| tile.pane)
            .collect();
        model.command_center.visible = tiles.clone();
        model.sync_visible(cx);
        for (index, pane) in tiles.iter().enumerate() {
            model.start_attach(*pane, Size { cols: 60, rows: 20 }, cx);
            let mut attached = attachment();
            attached.channel = ChannelId(u32::try_from(index + 10).expect("channel"));
            let session = model.pane_session(*pane).expect("session");
            model.receive(
                local(
                    1,
                    Update::Attached {
                        pane: *pane,
                        session,
                        attachment: attached,
                        created: false,
                    },
                ),
                cx,
            );
        }
        model.focus_pane(tiles[1], cx);
        model.set_command_center_layout(GridLayout::ThreeByOne, cx);
        assert_eq!(model.active_pane(), Some(tiles[1]));
        assert_eq!(model.state, before);
        assert_eq!(store::load(&model.path).expect("saved state"), before);
        assert_eq!(model.grids.len(), 3);
        let work: Vec<_> = requests.try_iter().map(|(_, work)| work).collect();
        assert!(
            work.iter()
                .filter(|work| matches!(work, Work::Attach { .. }))
                .all(|work| matches!(
                    work,
                    Work::Attach {
                        session: Some(_),
                        ..
                    }
                ))
        );
        assert!(!work.iter().any(|work| matches!(
            work,
            Work::Discard(..) | Work::StopServer { .. } | Work::EndAll(_)
        )));
        model.close_command_center(cx);
        assert!(!model.command_center.active);
        assert_eq!(model.state, before);
        let work: Vec<_> = requests.try_iter().map(|(_, work)| work).collect();
        assert_eq!(
            work.iter()
                .filter(|work| matches!(work, Work::Detach(_)))
                .count(),
            3
        );
        assert!(!work.iter().any(|work| matches!(work, Work::Discard(..))));
    });
}

#[gpui::test]
fn dashboard_ignores_a_late_attachment_after_returning_to_a_project(cx: &mut TestAppContext) {
    let (state, first, _, _, _) = projects::two_projects();
    let (boot, requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.receive(
            local(1, Update::Connected(vec![session(first, 41).info])),
            cx,
        );
        acknowledge_catalog(model, cx);
        model.open_command_center(cx);
        model.receive_session_page(first, Ok(listing(vec![session(first, 41)])), cx);
        let pane = model.command_center.tiles[0].pane;
        model.command_center.visible = vec![pane];
        model.sync_visible(cx);
        model.start_attach(pane, Size { cols: 60, rows: 20 }, cx);
        model.close_command_center(cx);
        let before = model.state.clone();
        requests.try_iter().for_each(drop);
        model.receive(
            local(
                1,
                Update::Attached {
                    pane,
                    session: SessionId::new(41).expect("session"),
                    attachment: attachment(),
                    created: false,
                },
            ),
            cx,
        );
        assert_eq!(model.state, before);
        assert!(model.terminal(&pane).is_none());
        let work: Vec<_> = requests.try_iter().map(|(_, work)| work).collect();
        assert!(work.iter().any(|work| matches!(work, Work::Detach(_))));
        assert!(!work.iter().any(|work| matches!(work, Work::Discard(..))));
    });
}

#[gpui::test]
fn closing_dashboard_terminals_confirms_and_only_ends_the_selected_session(
    cx: &mut TestAppContext,
) {
    let (mut state, first, second, a, b) = projects::two_projects();
    state
        .set_pane_session(a, SessionId::new(1))
        .expect("session");
    state
        .set_pane_session(b, SessionId::new(2))
        .expect("session");
    let (boot, requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    cx.simulate_resize(size(px(1400.0), px(900.0)));
    view.update(cx, |model, cx| {
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
        requests.try_iter().for_each(drop);
    });
    cx.run_until_parked();
    let before = view.read_with(cx, |model, _| model.state.clone());
    preferences::click_preference(cx, "command-center-close-0");
    assert!(cx.has_pending_prompt());
    assert!(
        !requests
            .try_iter()
            .any(|(_, work)| matches!(work, Work::Discard(..)))
    );
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    view.read_with(cx, |model, _| {
        assert_eq!(model.state, before);
        assert!(model.command_center.active);
        assert_eq!(model.command_center.tiles.len(), 3);
    });
    for closing in [1, 3] {
        preferences::click_preference(cx, "command-center-close-0");
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Close");
        cx.run_until_parked();
        view.read_with(cx, |model, _| {
            assert!(model.command_center.active);
            assert!(model.pane_tab(a).is_none());
            assert!(model.pane_tab(b).is_some());
            assert!(
                !model
                    .command_center
                    .tiles
                    .iter()
                    .any(|tile| tile.session.info.id.get() == closing)
            );
            assert_eq!(store::load(&model.path).expect("saved"), model.state);
        });
        let discarded: Vec<_> = requests
            .try_iter()
            .filter_map(|(_, work)| match work {
                Work::Discard(session, _) => Some(session.get()),
                Work::EndAll(_) | Work::StopServer { .. } => {
                    panic!("only the selected terminal may end")
                }
                _ => None,
            })
            .collect();
        assert_eq!(discarded, [closing]);
    }
}

#[gpui::test]
fn custom_layout_hover_selection_keyboard_and_persistence(cx: &mut TestAppContext) {
    let state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    let (boot, _requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    cx.simulate_resize(size(px(1400.0), px(900.0)));
    view.update(cx, |model, cx| {
        model.receive(
            local(
                1,
                Update::Connected((1..=4).map(|id| session(home, id).info).collect()),
            ),
            cx,
        );
        acknowledge_catalog(model, cx);
        model.receive_session_page(
            home,
            Ok(listing((1..=4).map(|id| session(home, id)).collect())),
            cx,
        );
        model.open_command_center(cx);
        model.set_command_center_layout(GridLayout::TwoByTwo, cx);
    });
    cx.run_until_parked();
    assert_even_gutters(&view, cx);
    preferences::click_preference(cx, "command-center-layout");
    preferences::click_preference(cx, "menu-label-Custom…");
    let at = cx
        .debug_bounds("layout-cell-4-3-false")
        .expect("custom cell")
        .center();
    cx.simulate_mouse_move(at, None, Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("layout-cell-4-3-true").is_some());
    assert!(cx.debug_bounds("layout-cell-5-3-false").is_some());
    view.read_with(cx, |model, _| {
        assert_eq!(model.appearance.command_center_layout, GridLayout::TwoByTwo);
    });
    preferences::click_preference(cx, "layout-cell-4-3-true");
    view.read_with(cx, |model, _| {
        assert!(model.overlay.is_none());
        assert_eq!(
            model.appearance.command_center_layout,
            GridLayout::Custom([4, 3])
        );
        assert_eq!(
            muxy_app_core::settings::Appearance::load(&model.path.with_file_name("settings.toml"))
                .expect("saved custom layout")
                .command_center_layout,
            GridLayout::Custom([4, 3])
        );
    });
    preferences::click_preference(cx, "command-center-layout");
    preferences::click_preference(cx, "menu-label-Custom…");
    cx.simulate_keystrokes("right up enter");
    cx.run_until_parked();
    view.read_with(cx, |model, _| {
        assert_eq!(
            model.appearance.command_center_layout,
            GridLayout::Custom([5, 2])
        );
        assert!(model.overlay.is_none());
    });
    preferences::click_preference(cx, "command-center-layout");
    preferences::click_preference(cx, "menu-label-Custom…");
    cx.simulate_keystrokes("left escape");
    cx.run_until_parked();
    view.read_with(cx, |model, _| {
        assert_eq!(
            model.appearance.command_center_layout,
            GridLayout::Custom([5, 2])
        );
        assert!(model.overlay.is_none());
    });
}

#[gpui::test]
fn larger_layout_presets_can_be_selected_and_saved_with_fewer_terminals(cx: &mut TestAppContext) {
    let state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    let (boot, _requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    cx.simulate_resize(size(px(1400.0), px(900.0)));
    view.update(cx, |model, cx| {
        model.receive(
            local(
                1,
                Update::Connected((1..=4).map(|id| session(home, id).info).collect()),
            ),
            cx,
        );
        acknowledge_catalog(model, cx);
        model.receive_session_page(
            home,
            Ok(listing((1..=4).map(|id| session(home, id)).collect())),
            cx,
        );
        model.open_command_center(cx);
    });
    cx.run_until_parked();
    for (layout, label) in [
        (GridLayout::FourByTwo, "menu-label-4 × 2"),
        (GridLayout::FiveByTwo, "menu-label-5 × 2"),
        (GridLayout::ThreeByThree, "menu-label-3 × 3"),
    ] {
        preferences::click_preference(cx, "command-center-layout");
        preferences::click_preference(cx, label);
        view.read_with(cx, |model, _| {
            assert_eq!(model.command_center.tiles.len(), 4);
            assert_eq!(model.appearance.command_center_layout, layout);
            assert_eq!(
                model.command_center.grid.expect("grid").0,
                layout.dimensions().expect("manual grid")
            );
            assert_eq!(
                muxy_app_core::settings::Appearance::load(
                    &model.path.with_file_name("settings.toml")
                )
                .expect("appearance")
                .command_center_layout,
                layout
            );
        });
    }
    view.update(cx, |model, cx| {
        model.set_command_center_layout(GridLayout::FourByTwo, cx);
    });
    for (count, rows) in [(8, 2), (9, 3), (8, 2)] {
        view.update(cx, |model, cx| {
            model.receive_session_page(
                home,
                Ok(listing((1..=count).map(|id| session(home, id)).collect())),
                cx,
            );
        });
        cx.run_until_parked();
        view.read_with(cx, |model, _| {
            assert_eq!(model.command_center.grid.expect("grid").0, [4, rows]);
            assert_eq!(
                model.appearance.command_center_layout,
                GridLayout::FourByTwo
            );
        });
        if count == 9 {
            let ninth = cx
                .debug_bounds("command-center-drag-8")
                .expect("ninth terminal");
            let first = cx
                .debug_bounds("command-center-drag-0")
                .expect("first terminal");
            let background = cx
                .debug_bounds("command-center-background")
                .expect("grid viewport");
            assert!(ninth.top() > first.bottom() && ninth.bottom() <= background.bottom());
        }
    }
}

#[gpui::test]
fn dashboard_close_does_not_end_a_process_when_saving_its_close_fails(cx: &mut TestAppContext) {
    let state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    let (boot, requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.receive(local(1, Update::Connected(vec![session(home, 1).info])), cx);
        acknowledge_catalog(model, cx);
        model.open_command_center(cx);
        model.receive_session_page(home, Ok(listing(vec![session(home, 1)])), cx);
    });
    cx.run_until_parked();
    let (before, path) = view.update(cx, |model, cx| {
        let before = model.state.clone();
        let path = model.path.clone();
        model.path = path.with_file_name("blocked-state.json");
        std::fs::create_dir(&model.path).expect("block state replacement");
        requests.try_iter().for_each(drop);
        model.close_pane(model.command_center.tiles[0].pane, cx);
        (before, path)
    });
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Close");
    cx.run_until_parked();
    view.update(cx, |model, _| {
        model.path = path;
        assert_eq!(model.state, before);
        assert_eq!(model.command_center.tiles.len(), 1);
        assert!(model.command_center.active);
    });
    assert!(
        !requests
            .try_iter()
            .any(|(_, work)| matches!(work, Work::Discard(..)))
    );
}

#[gpui::test]
#[allow(
    clippy::too_many_lines,
    reason = "Toolbar geometry, layout selection and virtualized scrolling"
)]
fn dashboard_layout_menu_and_scrolling_only_attach_visible_tiles(cx: &mut TestAppContext) {
    let mut state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    let logo = include_bytes!("../../../../muxy-protocol/tests/fixtures/project-logo.png");
    state
        .set_project_logo(home, Some(logo.as_slice().into()))
        .expect("logo");
    let (mut boot, requests) = stub_boot(state);
    boot.settings.appearance.agents_panel_visible = false;
    boot.settings.appearance.sidebar_expanded = true;
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
        model.receive_session_page(
            home,
            Ok(listing((1..=20).map(|id| session(home, id)).collect())),
            cx,
        );
    });
    preferences::click_preference(cx, "command-center-button");
    let titlebar = cx
        .debug_bounds("command-center-titlebar")
        .expect("titlebar");
    let background = cx
        .debug_bounds("command-center-background")
        .expect("background");
    let layout = cx
        .debug_bounds("command-center-layout")
        .expect("layout button");
    assert!(layout.top() >= titlebar.top() && layout.bottom() <= titlebar.bottom());
    assert!(
        background.top() >= titlebar.bottom(),
        "the grid must start below the titlebar"
    );
    preferences::click_preference(cx, "command-center-layout");
    assert!(
        cx.debug_bounds("context-menu").expect("layout menu").top() > layout.bottom(),
        "the layout menu must not cover its toolbar"
    );
    preferences::click_preference(cx, "menu-label-2 × 2");
    let first = view.read_with(cx, |model, _| {
        assert_eq!(model.appearance.command_center_layout, GridLayout::TwoByTwo);
        assert!(model.command_center.active);
        assert_eq!(model.command_center.tiles.len(), 20);
        assert!(model.project_logos.contains_key(&home));
        assert!(!model.command_center.visible.is_empty());
        assert!(
            model.grids.len() <= 12,
            "hidden terminals must not allocate renderers"
        );
        assert_eq!(
            muxy_app_core::settings::Appearance::load(&model.path.with_file_name("settings.toml"))
                .expect("appearance")
                .command_center_layout,
            GridLayout::TwoByTwo
        );
        model.command_center.tiles[0].pane
    });
    let artwork = cx
        .debug_bounds("command-center-artwork-0")
        .expect("project artwork");
    let name = cx
        .debug_bounds("command-center-project-name")
        .expect("project name");
    let header = cx.debug_bounds("command-center-drag-0").expect("header");
    let collapse = cx
        .debug_bounds("command-center-collapse-0-false")
        .expect("collapse button");
    let text = cx
        .debug_bounds("command-center-text-0")
        .expect("header text");
    let action = cx
        .debug_bounds(format!("command-center-project-{first}").leak())
        .expect("project action");
    let close = cx
        .debug_bounds("command-center-close-0")
        .expect("close action");
    assert!(text.right() <= collapse.left());
    assert!(collapse.right() <= action.left());
    assert!(action.right() <= close.left());
    for content in [artwork, collapse, text, action, close] {
        assert!(
            (content.center().y - header.center().y).abs() <= px(0.5),
            "header content must share one vertical center: {content:?} in {header:?}"
        );
    }
    view.read_with(cx, |model, _| {
        assert_eq!(artwork.size.width, model.metrics.scaled(26.0));
        assert!(name.size.width > px(0.0));
    });
    view.update(cx, |model, cx| {
        model.command_center.scroll.scroll_to(gpui::ListOffset {
            item_ix: 4,
            offset_in_item: px(0.0),
        });
        cx.notify();
    });
    cx.run_until_parked();
    view.read_with(cx, |model, _| {
        assert!(model.command_center.active);
        assert!(!model.command_center.visible.contains(&first));
        assert!(!model.grids.contains_key(&first));
        assert!(
            model
                .command_center
                .focused
                .is_some_and(|pane| model.command_center.visible.contains(&pane))
        );
        assert!(model.command_center.visible.iter().all(|pane| {
            model
                .command_center
                .tiles
                .iter()
                .position(|tile| tile.pane == *pane)
                .expect("tile")
                >= 8
        }));
        assert!(model.grids.len() <= 12);
    });
    let work: Vec<_> = requests.try_iter().map(|(_, work)| work).collect();
    assert!(!work.iter().any(|work| matches!(
        work,
        Work::Discard(..) | Work::EndAll(_) | Work::StopServer { .. }
    )));
}

#[gpui::test]
fn dashboard_routes_identical_session_and_channel_ids_to_their_own_servers(
    cx: &mut TestAppContext,
) {
    let remote = ServerId::new();
    let state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    let (boot, local_work, remotes) = remote_boot(state, vec![servers::entry(remote, "box")]);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    let remote_work = view.update(cx, |model, cx| {
        model.receive(local(1, Update::Connected(vec![session(home, 1).info])), cx);
        acknowledge_catalog(model, cx);
        let remote_home = servers::connect_remote(model, remote, vec![], &[], cx);
        model.open_command_center(cx);
        model.receive_session_page(home, Ok(listing(vec![session(home, 1)])), cx);
        model.receive_session_page(remote_home, Ok(listing(vec![session(remote_home, 1)])), cx);
        assert_eq!(model.command_center.tiles.len(), 2);
        let tiles = model.command_center.tiles.clone();
        model.command_center.visible = tiles.iter().map(|tile| tile.pane).collect();
        model.sync_visible(cx);
        for tile in &tiles {
            model.start_attach(tile.pane, Size { cols: 60, rows: 20 }, cx);
            model.receive(
                (
                    tile.server,
                    1,
                    Update::Attached {
                        pane: tile.pane,
                        session: tile.session.info.id,
                        attachment: attachment(),
                        created: false,
                    },
                ),
                cx,
            );
        }
        model.reorder_command_center(tiles[1].pane, tiles[0].pane, cx);
        let saved =
            muxy_app_core::settings::Appearance::load(&model.path.with_file_name("settings.toml"))
                .expect("appearance");
        assert_eq!(
            saved.command_center_order,
            [
                (tiles[1].server, tiles[1].session.info.id),
                (tiles[0].server, tiles[0].session.info.id)
            ]
        );
        model.sync_command_center(cx);
        assert_eq!(model.command_center.tiles[0].server, tiles[1].server);
        local_work.try_iter().for_each(drop);
        let remote_work = remotes.borrow_mut().remove(&remote).expect("worker");
        remote_work.try_iter().for_each(drop);
        let tile = tiles
            .iter()
            .find(|tile| tile.server == remote)
            .expect("remote");
        model.grids[&tile.pane].view.update(cx, |_, cx| {
            cx.emit(PaneEvent::Input(ChannelId(1), b"remote only".to_vec()));
        });
        remote_work
    });
    let remote_sent = servers::work(&remote_work);
    assert!(
        remote_sent.iter().any(
            |work| matches!(work, Work::Input(ChannelId(1), bytes) if bytes == b"remote only")
        )
    );
    assert!(
        !servers::work(&local_work)
            .iter()
            .any(|work| matches!(work, Work::Input(..)))
    );
    view.update(cx, |model, cx| {
        let pane = model
            .command_center
            .tiles
            .iter()
            .find(|tile| tile.server == remote)
            .expect("remote tile")
            .pane;
        model.close_pane(pane, cx);
    });
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Close");
    cx.run_until_parked();
    assert!(
        servers::work(&remote_work)
            .iter()
            .any(|work| matches!(work, Work::Discard(session, _) if session.get() == 1))
    );
    assert!(
        !servers::work(&local_work)
            .iter()
            .any(|work| matches!(work, Work::Discard(..)))
    );
    view.read_with(cx, |model, _| {
        assert!(model.command_center.active);
        assert_eq!(model.command_center.tiles.len(), 1);
        assert!(model.command_center.tiles[0].server.is_local());
    });
}

#[gpui::test]
fn ended_dashboard_sessions_disappear_without_leaving_the_dashboard(cx: &mut TestAppContext) {
    let state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    let (boot, _requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
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
        model.close_ended_session(ServerId::local(), SessionId::new(1).expect("session"), cx);
        assert!(model.command_center.active);
        assert_eq!(model.command_center.tiles.len(), 1);
        model.sync_command_center(cx);
        assert_eq!(
            model.command_center.tiles.len(),
            1,
            "the cached inventory must not resurrect an ended session"
        );
    });
}

#[gpui::test]
fn open_in_project_reuses_the_original_pane_and_only_adds_a_tab_for_a_detached_session(
    cx: &mut TestAppContext,
) {
    let (mut state, first, second, a, b) = projects::two_projects();
    state
        .set_pane_session(a, SessionId::new(41))
        .expect("session");
    state
        .set_pane_session(b, SessionId::new(42))
        .expect("session");
    let (boot, _requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.receive(
            local(
                1,
                Update::Connected(vec![
                    session(first, 41).info,
                    session(second, 42).info,
                    session(first, 43).info,
                ]),
            ),
            cx,
        );
        acknowledge_catalog(model, cx);
        let projects = model.state.projects().to_vec();
        model.open_command_center(cx);
        model.receive_session_page(
            first,
            Ok(listing(vec![session(first, 41), session(first, 43)])),
            cx,
        );
        model.receive_session_page(second, Ok(listing(vec![session(second, 42)])), cx);
        let pane = model
            .command_center
            .tiles
            .iter()
            .find(|tile| tile.session.info.id.get() == 42)
            .expect("tile")
            .pane;
        model.open_command_center_project(pane, cx);
        assert!(!model.command_center.active);
        assert_eq!(model.state.current_project().id, second);
        assert_eq!(model.active_pane(), Some(b));
        assert_eq!(model.state.projects(), projects);
        model.open_command_center(cx);
        let pane = model
            .command_center
            .tiles
            .iter()
            .find(|tile| tile.session.info.id.get() == 43)
            .expect("detached")
            .pane;
        model.open_command_center_project(pane, cx);
        assert!(!model.command_center.active);
        assert_eq!(model.state.current_project().id, first);
        assert_eq!(model.state.current_project().tabs.len(), 2);
        assert_eq!(
            model.pane_session(model.active_pane().expect("opened pane")),
            SessionId::new(43)
        );
    });
}

fn drag_tile(
    cx: &mut VisualTestContext,
    from: gpui::Point<gpui::Pixels>,
    to: gpui::Point<gpui::Pixels>,
) {
    cx.simulate_event(gpui::MouseDownEvent {
        position: from,
        button: gpui::MouseButton::Left,
        click_count: 1,
        ..Default::default()
    });
    cx.run_until_parked();
    for position in [from + gpui::point(px(12.0), px(0.0)), to] {
        cx.simulate_event(gpui::MouseMoveEvent {
            position,
            pressed_button: Some(gpui::MouseButton::Left),
            ..Default::default()
        });
        cx.run_until_parked();
        cx.update(|_, cx| assert!(cx.has_active_drag(), "header must start a native drag"));
        let preview = cx
            .debug_bounds("command-center-drag-preview")
            .expect("visible dragged tile");
        assert!(
            preview.left() > position.x && preview.top() > position.y,
            "the drag preview must follow beside the pointer: {preview:?}, pointer: {position:?}"
        );
    }
    cx.simulate_event(gpui::MouseUpEvent {
        position: to,
        button: gpui::MouseButton::Left,
        click_count: 1,
        ..Default::default()
    });
    cx.run_until_parked();
}

#[gpui::test]
#[allow(
    clippy::too_many_lines,
    reason = "One drag, refresh and restart regression scenario"
)]
fn dragging_dashboard_headers_reorders_only_the_dashboard_and_survives_refresh_and_restart(
    cx: &mut TestAppContext,
) {
    let (mut state, first, second, a, b) = projects::two_projects();
    state
        .set_pane_session(a, SessionId::new(u64::MAX - 3))
        .expect("session");
    state
        .set_pane_session(b, SessionId::new(u64::MAX - 2))
        .expect("session");
    let (mut boot, requests) = stub_boot(state);
    boot.settings.appearance.agents_panel_visible = false;
    boot.settings.appearance.command_center_layout = GridLayout::ThreeByOne;
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    cx.simulate_resize(size(px(1400.0), px(900.0)));
    view.update(cx, |model, cx| {
        model.receive(
            local(
                1,
                Update::Connected(vec![
                    session(first, u64::MAX - 3).info,
                    session(second, u64::MAX - 2).info,
                    session(first, u64::MAX - 1).info,
                ]),
            ),
            cx,
        );
        acknowledge_catalog(model, cx);
        model.open_command_center(cx);
        model.receive_session_page(
            first,
            Ok(listing(vec![
                session(first, u64::MAX - 3),
                session(first, u64::MAX - 1),
            ])),
            cx,
        );
        model.receive_session_page(second, Ok(listing(vec![session(second, u64::MAX - 2)])), cx);
    });
    cx.run_until_parked();
    let (before, panes, target) = view.read_with(cx, |model, cx| {
        let panes: Vec<_> = model
            .command_center
            .tiles
            .iter()
            .map(|tile| tile.pane)
            .collect();
        let target = model.grids[&panes[2]]
            .view
            .read(cx)
            .geometry
            .expect("target bounds")
            .0
            .center();
        (model.state.clone(), panes, target)
    });
    requests.try_iter().for_each(drop);
    let source = cx
        .debug_bounds("command-center-drag-0")
        .expect("draggable header")
        .center();
    drag_tile(cx, source, target);
    view.read_with(cx, |model, _| {
        assert_eq!(
            model
                .command_center
                .tiles
                .iter()
                .map(|tile| tile.pane)
                .collect::<Vec<_>>(),
            [panes[1], panes[2], panes[0]]
        );
        assert_eq!(model.state, before);
        assert_eq!(store::load(&model.path).expect("saved state"), before);
    });
    let work: Vec<_> = requests.try_iter().map(|(_, work)| work).collect();
    assert!(!work.iter().any(|work| matches!(
        work,
        Work::Attach { .. }
            | Work::Detach(_)
            | Work::Discard(..)
            | Work::EndAll(_)
            | Work::StopServer { .. }
    )));
    view.update(cx, |model, cx| {
        model.receive_session_page(
            first,
            Ok(listing(vec![
                session(first, u64::MAX - 3),
                session(first, u64::MAX - 1),
                session(first, u64::MAX),
            ])),
            cx,
        );
        assert_eq!(
            model
                .command_center
                .tiles
                .iter()
                .map(|tile| tile.session.info.id.get())
                .collect::<Vec<_>>(),
            [u64::MAX - 1, u64::MAX - 2, u64::MAX - 3, u64::MAX]
        );
        let saved =
            muxy_app_core::settings::Appearance::load(&model.path.with_file_name("settings.toml"))
                .expect("saved order");
        model.close_command_center(cx);
        model.command_center.tiles.clear();
        model.appearance.command_center_order = saved.command_center_order;
        model.open_command_center(cx);
        assert_eq!(
            model
                .command_center
                .tiles
                .iter()
                .map(|tile| tile.session.info.id.get())
                .collect::<Vec<_>>(),
            [u64::MAX - 1, u64::MAX - 2, u64::MAX - 3, u64::MAX]
        );
        assert_eq!(model.state, before);
    });
}

#[gpui::test]
#[allow(
    clippy::too_many_lines,
    reason = "Per-session attention, acknowledgement and stable geometry"
)]
fn attention_borders_follow_the_session_and_server_without_resizing_terminals(
    cx: &mut TestAppContext,
) {
    use muxy_protocol::{
        ActivityEvent, ActivityKind, ActivitySnapshot, AgentActivity, AgentProvider, AgentState,
    };
    let remote = ServerId::new();
    let state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    let (mut boot, requests, _remotes) = remote_boot(state, vec![servers::entry(remote, "box")]);
    boot.settings.appearance.agents_panel_visible = false;
    boot.settings.appearance.command_center_layout = GridLayout::ThreeByOne;
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    cx.simulate_resize(size(px(1400.0), px(900.0)));
    let snapshot = |project, states: &[(u64, AgentState)]| ActivitySnapshot {
        revision: 1,
        agents: states
            .iter()
            .map(|(id, state)| AgentActivity {
                project,
                session: SessionId::new(*id).expect("session"),
                provider: AgentProvider::Codex,
                state: *state,
            })
            .collect(),
        ..ActivitySnapshot::default()
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
        let remote_home = servers::connect_remote(model, remote, vec![], &[], cx);
        model.open_command_center(cx);
        model.receive_session_page(
            home,
            Ok(listing(vec![session(home, 1), session(home, 2)])),
            cx,
        );
        model.receive_session_page(remote_home, Ok(listing(vec![session(remote_home, 1)])), cx);
        // Start blocked: GPUI retains old debug bounds, so the normal marker must be new after resuming.
        let mut waiting = snapshot(home, &[(1, AgentState::Working), (2, AgentState::Blocked)]);
        waiting.events.push(ActivityEvent {
            id: 1,
            session: SessionId::new(2).expect("session"),
            project: home,
            provider: AgentProvider::Codex,
            timestamp: 0,
            kind: ActivityKind::Attention,
            read: false,
        });
        model.receive_activity(ServerId::local(), Ok(waiting), cx);
        model.receive_activity(
            remote,
            Ok(snapshot(remote_home, &[(1, AgentState::Blocked)])),
            cx,
        );
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("command-center-attention-0-Blocked")
            .is_none(),
        "a different session in the same project must not light up"
    );
    assert!(
        cx.debug_bounds("command-center-attention-1-Blocked")
            .is_some()
    );
    assert!(
        cx.debug_bounds("command-center-attention-2-Blocked")
            .is_some(),
        "the remote session with the same ID has its own status"
    );
    assert!(
        cx.debug_bounds("command-center-tile-1-Working-focused-true")
            .is_none()
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
                    .expect("terminal geometry")
                    .0
            })
            .collect::<Vec<_>>()
    });
    requests.try_iter().for_each(drop);
    view.update(cx, |model, cx| {
        let pane = model.command_center.tiles[1].pane;
        model.focus_pane(pane, cx);
        model.activity_acknowledged(ServerId::local(), &[1], Ok(()), cx);
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("command-center-tile-1-Blocked-focused-true")
            .is_some(),
        "focus and a still-blocked state must both remain visible"
    );
    assert!(
        cx.debug_bounds("command-center-tile-1-Working-focused-true")
            .is_none(),
        "reading an alert must not clear a still-blocked agent"
    );
    view.update(cx, |model, cx| {
        model.receive_activity(
            ServerId::local(),
            Ok(snapshot(
                home,
                &[(1, AgentState::Working), (2, AgentState::Working)],
            )),
            cx,
        );
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("command-center-tile-1-Working-focused-true")
            .is_some()
    );
    assert!(
        cx.debug_bounds("command-center-tile-2-Working-focused-false")
            .is_none()
    );
    view.update(cx, |model, cx| {
        let mut finished = snapshot(home, &[(1, AgentState::Idle), (2, AgentState::Working)]);
        finished.events.push(ActivityEvent {
            id: 2,
            session: SessionId::new(1).expect("session"),
            project: home,
            provider: AgentProvider::Codex,
            timestamp: 0,
            kind: ActivityKind::Completed,
            read: false,
        });
        model.receive_activity(ServerId::local(), Ok(finished), cx);
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("command-center-attention-0-Completed")
            .is_some()
    );
    assert!(
        cx.debug_bounds("command-center-tile-0-Idle-focused-false")
            .is_none()
    );
    view.update(cx, |model, cx| {
        model.activity_acknowledged(ServerId::local(), &[2], Ok(()), cx);
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("command-center-tile-0-Idle-focused-false")
            .is_some(),
        "acknowledging the update must clear the amber attention state"
    );
    view.read_with(cx, |model, cx| {
        let after: Vec<_> = model
            .command_center
            .tiles
            .iter()
            .map(|tile| {
                model.grids[&tile.pane]
                    .view
                    .read(cx)
                    .geometry
                    .expect("terminal geometry")
                    .0
            })
            .collect();
        assert_eq!(
            after, geometry,
            "status changes must not resize terminal viewports"
        );
    });
    assert!(
        !requests
            .try_iter()
            .any(|(_, work)| matches!(work, Work::Resize(..)))
    );
}

fn assert_even_gutters(view: &Entity<AppModel>, cx: &mut VisualTestContext) {
    let background = cx
        .debug_bounds("command-center-background")
        .expect("background");
    let tiles: Vec<_> = (0..4)
        .map(|index| {
            let selected = view.read_with(cx, |model, _| {
                model.active_pane() == Some(model.command_center.tiles[index].pane)
            });
            cx.debug_bounds(format!("command-center-tile-{index}-None-focused-{selected}").leak())
                .expect("tile")
        })
        .collect();
    let gutters = [
        tiles[0].left() - background.left(),
        tiles[0].top() - background.top(),
        background.right() - tiles[1].right(),
        background.bottom() - tiles[3].bottom(),
        tiles[1].left() - tiles[0].right(),
        tiles[2].top() - tiles[0].bottom(),
    ];
    for gutter in gutters {
        assert!(
            (gutter - gutters[0]).abs() <= px(0.5),
            "uneven grid gutters: {gutters:?}"
        );
    }
}
