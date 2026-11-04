use super::*;
use muxy_core::capability::{Capability, Feature};
use muxy_core::shortcuts::ShortcutId;

const TERMINAL: [ShortcutId; 10] = [
    ShortcutId::Find,
    ShortcutId::FindNext,
    ShortcutId::FindPrevious,
    ShortcutId::PreviousPrompt,
    ShortcutId::NextPrompt,
    ShortcutId::SelectCommandOutput,
    ShortcutId::ScrollToBottom,
    ShortcutId::IncreaseFontSize,
    ShortcutId::DecreaseFontSize,
    ShortcutId::DetachTerminal,
];
const PANE_FOCUS: [ShortcutId; 4] = [
    ShortcutId::FocusPaneLeft,
    ShortcutId::FocusPaneRight,
    ShortcutId::FocusPaneUp,
    ShortcutId::FocusPaneDown,
];
const APP: [ShortcutId; 6] = [
    ShortcutId::ExistingTerminals,
    ShortcutId::ToggleFullScreen,
    ShortcutId::Minimize,
    ShortcutId::Zoom,
    ShortcutId::Quit,
    ShortcutId::EndAllSessionsAndQuit,
];

/// Whether the palette lists each command, and lists it disabled.
fn listed(
    model: &mut AppModel,
    ids: &[ShortcutId],
    cx: &mut Context<AppModel>,
) -> Vec<Option<bool>> {
    let registry = model.command_registry(true, cx);
    ids.iter()
        .map(|id| {
            registry
                .command(id.name())
                .map(muxy_ui::command_palette::Command::is_disabled)
        })
        .collect()
}

#[gpui::test]
fn the_palette_lists_the_commands_only_a_menu_bar_offered(cx: &mut TestAppContext) {
    let (boot, _requests) = stub_boot(AppState::bootstrap().expect("state"));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        assert_eq!(listed(model, &APP, cx), [Some(false); 6]);
        assert_eq!(listed(model, &TERMINAL, cx), [Some(true); 10]);
        assert_eq!(listed(model, &PANE_FOCUS, cx), [Some(true); 4]);

        model.new_tab(cx);
        assert_eq!(listed(model, &TERMINAL, cx), [Some(false); 10]);
        assert_eq!(listed(model, &PANE_FOCUS, cx), [Some(true); 4]);

        model.split_pane(Direction::Right, cx);
        assert_eq!(listed(model, &PANE_FOCUS, cx), [Some(false); 4]);
    });
}

#[gpui::test]
fn the_palette_leaves_out_commands_this_build_cannot_run(cx: &mut TestAppContext) {
    let (boot, _requests) = stub_boot(AppState::bootstrap().expect("state"));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    let install = ShortcutId::InstallCommandLineTool.name();
    view.update(cx, |model, cx| {
        for feature in [Feature::Updates, Feature::CommandLineTool] {
            model.capabilities.set(feature, Capability::Supported);
        }
        let registry = model.command_registry(true, cx);
        assert!(registry.command("check_for_updates").is_some());
        assert!(registry.command(install).is_some());

        for feature in [Feature::Updates, Feature::CommandLineTool] {
            model.capabilities.set(feature, Capability::Unsupported);
        }
        let registry = model.command_registry(true, cx);
        assert!(registry.command("check_for_updates").is_none());
        assert!(registry.command(install).is_none());
    });
}

#[gpui::test]
fn palette_queries_still_run_the_commands_they_found_first(cx: &mut TestAppContext) {
    let (boot, _requests) = stub_boot(AppState::bootstrap().expect("state"));
    cx.update(|cx| crate::views::workspace::bind_keys(&boot.settings.keymap, cx));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.new_tab(cx);
        model.split_pane(Direction::Right, cx);
    });
    cx.simulate_keystrokes(ShortcutId::ToggleCommandPalette.definition().keys[0]);
    cx.simulate_input("zoom");
    cx.simulate_keystrokes("enter");
    view.read_with(cx, |model, _| {
        let tab = model.active_tab().and_then(|tab| model.tab(tab));
        assert!(tab.expect("tab").zoomed.is_some());
    });
}

#[gpui::test]
fn commands_are_typed_into_the_active_terminal_without_submitting(cx: &mut TestAppContext) {
    let (boot, requests) = stub_boot(AppState::bootstrap().expect("state"));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.receive((ServerId::local(), 1, Update::Connected(vec![])), cx);
        acknowledge_catalog(model, cx);
        model.new_tab(cx);
        let pane = model.active_pane().expect("pane");
        model.receive(
            (
                ServerId::local(),
                1,
                Update::Attached {
                    pane,
                    session: SessionId::new(42).expect("ID"),
                    attachment: attachment(),
                    created: true,
                },
            ),
            cx,
        );
    });
    cx.run_until_parked();
    requests.try_iter().for_each(drop);
    view.update(cx, |model, cx| {
        let command =
            muxy_app_core::settings::CustomCommand::new("Tests".into(), "cargo test".into());
        let id = command.id.clone();
        model.settings.commands.push(command);
        let tabs = model.state.current_project().tabs.len();
        let pane = model.active_pane();
        model.run_custom_command_in_pane(&id, cx);
        assert_eq!(model.state.current_project().tabs.len(), tabs);
        assert_eq!(model.active_pane(), pane);
    });
    cx.run_until_parked();
    let channel = attachment().channel;
    assert!(requests.try_iter().any(|(_, work)| matches!(
        work,
        Work::Input(actual, bytes) if actual == channel && bytes == b"\x15cargo test"
    )));
}

#[gpui::test]
fn project_commands_are_filtered_and_cannot_run_from_another_project(cx: &mut TestAppContext) {
    let (boot, _requests) = stub_boot(AppState::bootstrap().expect("state"));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        use muxy_app_core::settings::CustomCommand;
        let global = CustomCommand::new("Global".into(), "echo global".into());
        let mut local = CustomCommand::new("Local".into(), "echo local".into());
        local.project = Some(model.state.current_project().id);
        let mut foreign = CustomCommand::new("Other".into(), "echo other".into());
        foreign.project = Some(ProjectId::new());
        let id = foreign.id.clone();
        let shortcut = foreign.shortcut_id();
        model.settings.commands = vec![global, local, foreign];
        assert_eq!(
            model
                .available_commands()
                .map(|(index, _)| index)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert!(
            model
                .command_registry(true, cx)
                .command(&shortcut)
                .is_none()
        );
        let before = model.state.clone();
        model.run_custom_command(&id, cx);
        model.run_custom_command_in_split(&id, cx);
        model.run_custom_command_in_pane(&id, cx);
        assert_eq!(model.state, before);
    });
}

#[gpui::test]
fn quick_commands_follow_the_focused_command_center_project(cx: &mut TestAppContext) {
    use super::command_center::{listing, session};
    use muxy_app_core::settings::CustomCommand;
    let (mut state, home, project, first, second) = projects::two_projects();
    state
        .set_pane_session(first, Some(SessionId::new(731).expect("session")))
        .expect("session");
    state
        .set_pane_session(second, Some(SessionId::new(732).expect("session")))
        .expect("session");
    state.select_project(home).expect("first project");
    let (boot, _requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.receive(
            local(
                1,
                Update::Connected(vec![session(home, 731).info, session(project, 732).info]),
            ),
            cx,
        );
        acknowledge_catalog(model, cx);
        model.open_command_center(cx);
        model.receive_session_page(home, Ok(listing(vec![session(home, 731)])), cx);
        model.receive_session_page(project, Ok(listing(vec![session(project, 732)])), cx);
        let pane = model
            .command_center
            .tiles
            .iter()
            .find(|tile| tile.session.info.project == project)
            .expect("project tile")
            .pane;
        model.command_center.visible = model
            .command_center
            .tiles
            .iter()
            .map(|tile| tile.pane)
            .collect();
        model.command_center.focused = Some(pane);
        assert_eq!(model.state.current_project().id, home);
        assert_eq!(model.active_pane(), Some(pane));
        let mut home_command = CustomCommand::new("Home".into(), "echo home".into());
        home_command.project = Some(home);
        let mut project_command = CustomCommand::new("Project".into(), "echo project".into());
        project_command.project = Some(project);
        model.settings.commands = vec![home_command, project_command];
        assert_eq!(
            model
                .available_commands()
                .map(|(index, _)| index)
                .collect::<Vec<_>>(),
            [1]
        );
    });
}
