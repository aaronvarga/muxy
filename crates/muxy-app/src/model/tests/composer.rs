use super::*;

fn composer_text(view: &Entity<AppModel>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |model, cx| {
        model
            .composer
            .view
            .as_ref()
            .expect("composer")
            .read(cx)
            .draft
            .text
            .clone()
    })
}

fn attached_composer(
    cx: &mut TestAppContext,
) -> (
    Entity<AppModel>,
    &mut VisualTestContext,
    std::sync::mpsc::Receiver<(u64, Work)>,
) {
    let mut state = AppState::bootstrap().expect("state");
    state.open_terminal_tab(state.home().id).expect("tab");
    let pane = state.home().tabs[0].panes[0].id;
    let session = SessionId::new(12).expect("session");
    state
        .set_pane_session(pane, Some(session))
        .expect("session");
    let (mut boot, requests) = stub_boot(state);
    boot.settings.composer.clear_after_sending = true;
    cx.update(|cx| crate::views::workspace::bind_keys(&boot.settings.keymap, cx));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    cx.simulate_resize(size(px(1000.0), px(600.0)));
    view.update(cx, |model, cx| {
        model.servers.local.connection = ConnectionState::Ready;
        model.receive(
            (
                ServerId::local(),
                1,
                Update::Attached {
                    pane,
                    session,
                    attachment: attachment(),
                    created: false,
                },
            ),
            cx,
        );
    });
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-i"
    } else {
        "ctrl-shift-i"
    });
    (view, cx, requests)
}

fn delivery(
    requests: &std::sync::mpsc::Receiver<(u64, Work)>,
) -> (Vec<u8>, async_channel::Sender<Result<(), String>>) {
    requests
        .try_iter()
        .find_map(|(_, work)| match work {
            Work::WriteInput {
                bytes, completion, ..
            } => Some((bytes, completion)),
            _ => None,
        })
        .expect("acknowledged terminal input")
}

#[gpui::test]
fn composer_delivery_preserves_edits_and_failures_then_clears_confirmed_draft(
    cx: &mut TestAppContext,
) {
    let (view, cx, requests) = attached_composer(cx);
    cx.simulate_input("first");
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-enter"
    } else {
        "ctrl-enter"
    });
    cx.run_until_parked();
    let (bytes, completion) = delivery(&requests);
    assert_eq!(bytes, b"\x15first\r");
    cx.simulate_input(" edited");
    completion.try_send(Ok(())).expect("ack");
    cx.run_until_parked();
    assert_eq!(composer_text(&view, cx), "first edited");
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-shift-enter"
    } else {
        "ctrl-shift-enter"
    });
    cx.run_until_parked();
    let (bytes, completion) = delivery(&requests);
    assert_eq!(bytes, b"\x15first edited");
    completion
        .try_send(Err("writer failed".into()))
        .expect("failure");
    cx.run_until_parked();
    assert_eq!(composer_text(&view, cx), "first edited");
    view.read_with(cx, |model, cx| {
        assert!(
            model
                .composer
                .view
                .as_ref()
                .expect("composer")
                .read(cx)
                .error
                .as_ref()
                .expect("error")
                .contains("Draft preserved")
        );
    });
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-enter"
    } else {
        "ctrl-enter"
    });
    cx.run_until_parked();
    delivery(&requests).1.try_send(Ok(())).expect("ack");
    cx.run_until_parked();
    assert_eq!(composer_text(&view, cx), "");
}

#[gpui::test]
fn composer_broadcast_deduplicates_sessions_and_retains_partial_failure(cx: &mut TestAppContext) {
    let mut state = AppState::bootstrap().expect("state");
    state.open_terminal_tab(state.home().id).expect("tab");
    let first = state.home().tabs[0].panes[0].id;
    let second = state.split_pane(first, Direction::Right).expect("split");
    let third = state.split_pane(second, Direction::Right).expect("split");
    for (pane, id) in [(first, 31), (second, 31), (third, 32)] {
        state
            .set_pane_session(pane, SessionId::new(id))
            .expect("session");
    }
    let (mut boot, requests) = stub_boot(state);
    boot.settings.composer.broadcast = true;
    boot.settings.composer.clear_after_sending = true;
    cx.update(|cx| crate::views::workspace::bind_keys(&boot.settings.keymap, cx));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.servers.local.connection = ConnectionState::Ready;
        for (index, pane) in [first, second, third].into_iter().enumerate() {
            let mut attachment = attachment();
            attachment.channel =
                muxy_protocol::ChannelId(u32::try_from(index + 1).expect("channel"));
            model.receive(
                (
                    ServerId::local(),
                    1,
                    Update::Attached {
                        pane,
                        session: model.pane_session(pane).expect("session"),
                        attachment,
                        created: false,
                    },
                ),
                cx,
            );
        }
    });
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-i"
    } else {
        "ctrl-shift-i"
    });
    cx.simulate_input("broadcast");
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-enter"
    } else {
        "ctrl-enter"
    });
    cx.run_until_parked();
    delivery(&requests).1.try_send(Ok(())).expect("first ack");
    cx.run_until_parked();
    delivery(&requests)
        .1
        .try_send(Err("second writer failed".into()))
        .expect("second failure");
    cx.run_until_parked();
    assert_eq!(composer_text(&view, cx), "broadcast");
    view.read_with(cx, |model, cx| {
        let panel = model.composer.view.as_ref().expect("composer").read(cx);
        assert!(
            panel
                .error
                .as_ref()
                .expect("partial result")
                .starts_with("1/2 terminals sent.")
        );
    });
    assert!(
        !requests
            .try_iter()
            .any(|(_, work)| matches!(work, Work::WriteInput { .. }))
    );
}

#[gpui::test]
fn stored_clipboard_strategy_sends_an_image_path_without_a_clipboard_backend(
    cx: &mut TestAppContext,
) {
    use muxy_app_core::composer::submission::ImageSubmissionStrategy;
    use muxy_core::capability::{Capability, Feature};

    let (view, cx, requests) = attached_composer(cx);
    let mut png = Vec::new();
    image::RgbaImage::new(1, 1)
        .write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("png");
    view.update(cx, |model, cx| {
        model
            .capabilities
            .set(Feature::ClipboardImages, Capability::Unsupported);
        let composer = model.composer.view.clone().expect("composer");
        composer.update(cx, |composer, cx| {
            composer.settings.image_strategy = ImageSubmissionStrategy::Clipboard;
            cx.emit(crate::views::composer::ComposerEvent::Image(png));
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-enter"
    } else {
        "ctrl-enter"
    });
    cx.run_until_parked();
    let (bytes, completion) = delivery(&requests);
    assert!(!bytes.contains(&0x16));
    assert!(String::from_utf8_lossy(&bytes).contains(".png"));
    completion.try_send(Ok(())).expect("ack");
    cx.run_until_parked();
}

#[gpui::test]
fn the_app_drawn_composer_menu_opens_at_its_button_and_hands_the_keyboard_back(
    cx: &mut TestAppContext,
) {
    use crate::views::overlays::Overlay;
    use gpui::Focusable;

    let (view, cx, _requests) = attached_composer(cx);
    let button = gpui::Bounds::new(gpui::point(px(120.0), px(300.0)), size(px(20.0), px(20.0)));
    let composer = view.update(cx, |model, cx| {
        let composer = model.composer.view.clone().expect("composer");
        composer.update(cx, |composer, _| composer.sending = true);
        let choices = composer.read(cx).menu_items();
        model.open_composer_menu(&choices, button, cx);
        let Some(Overlay::Menu(menu)) = &model.overlay else {
            panic!("the composer menu is not open");
        };
        assert_eq!(menu.drops_from, Some(button));
        composer
    });

    cx.simulate_keystrokes("down down");
    cx.simulate_keystrokes("enter");
    view.update_in(cx, |model, window, cx| {
        assert!(model.overlay.is_none());
        assert!(!model.settings.composer.clear_after_sending);
        assert!(composer.focus_handle(cx).is_focused(window));
    });

    view.update(cx, |model, cx| {
        let choices = composer.read(cx).menu_items();
        model.open_composer_menu(&choices, button, cx);
    });
    cx.simulate_keystrokes("escape");
    view.update_in(cx, |model, window, cx| {
        assert!(model.overlay.is_none());
        assert!(composer.focus_handle(cx).is_focused(window));
    });
}

#[gpui::test]
fn a_floating_composer_closed_by_its_menu_row_leaves_the_keyboard_to_the_terminal(
    cx: &mut TestAppContext,
) {
    use crate::views::composer::menu::Command;

    let (view, cx, _requests) = attached_composer(cx);
    let button = gpui::Bounds::new(gpui::point(px(120.0), px(300.0)), size(px(20.0), px(20.0)));
    view.update(cx, |model, cx| {
        let composer = model.composer.view.clone().expect("composer");
        composer.update(cx, |composer, cx| {
            composer.menu_action(
                Command::Presentation(muxy_app_core::settings::ComposerPresentation::Floating),
                cx,
            );
        });
    });
    cx.run_until_parked();
    view.update(cx, |model, cx| {
        let composer = model.composer.view.clone().expect("composer");
        let choices = composer.read(cx).menu_items();
        model.open_composer_menu(&choices, button, cx);
        model.dismiss_overlay(cx);
        model.dismiss_composer(false, cx);
        assert!(model.composer.closing.is_some());
    });
    cx.run_until_parked();
    cx.executor()
        .advance_clock(crate::views::composer::TRANSITION * 2);
    cx.run_until_parked();
    view.update_in(cx, |model, window, cx| {
        let pane = model.active_pane().expect("pane");
        let terminal = model.terminal(&pane).expect("terminal");
        assert!(terminal.view.read(cx).focus.is_focused(window));
    });

    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-i"
    } else {
        "ctrl-shift-i"
    });
    view.update(cx, |model, cx| {
        let composer = model.composer.view.clone().expect("composer");
        let choices = composer.read(cx).menu_items();
        model.open_composer_menu(&choices, button, cx);
        model.close_composer(cx);
        assert!(model.overlay.is_none());
    });
}

#[cfg(target_os = "macos")]
#[gpui::test]
fn focused_terminal_and_composer_are_text_inputs(cx: &mut TestAppContext) {
    let (view, cx, _requests) = attached_composer(cx);
    view.update_in(cx, |model, window, cx| {
        assert_eq!(
            model.focused_text_input(window, cx),
            Some("Composer".into())
        );
    });
    cx.simulate_keystrokes("cmd-i");
    cx.executor()
        .advance_clock(crate::views::composer::TRANSITION * 2);
    cx.run_until_parked();
    view.update_in(cx, |model, window, cx| {
        assert_eq!(
            model.focused_text_input(window, cx),
            Some("Terminal".into())
        );
    });
}

#[allow(
    clippy::too_many_lines,
    reason = "One docked composer session walkthrough"
)]
#[gpui::test]
fn docked_composer_keeps_a_draft_per_session_and_sends_to_the_clicked_session(
    cx: &mut TestAppContext,
) {
    let mut state = AppState::bootstrap().expect("state");
    state.open_terminal_tab(state.home().id).expect("tab");
    let first = state.home().tabs[0].panes[0].id;
    let second = state.split_pane(first, Direction::Right).expect("split");
    for (pane, id) in [(first, 41), (second, 42)] {
        state
            .set_pane_session(pane, SessionId::new(id))
            .expect("session");
    }
    state.focus_pane(first).expect("focus");
    let (mut boot, requests) = stub_boot(state);
    boot.settings.composer.presentation = muxy_app_core::settings::ComposerPresentation::Docked;
    boot.settings.composer.clear_on_close = true;
    cx.update(|cx| crate::views::workspace::bind_keys(&boot.settings.keymap, cx));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    cx.simulate_resize(size(px(1000.0), px(600.0)));
    view.update(cx, |model, cx| {
        model.servers.local.connection = ConnectionState::Ready;
        for (index, pane) in [first, second].into_iter().enumerate() {
            let mut attachment = attachment();
            attachment.channel =
                muxy_protocol::ChannelId(u32::try_from(index + 1).expect("channel"));
            model.receive(
                (
                    ServerId::local(),
                    1,
                    Update::Attached {
                        pane,
                        session: model.pane_session(pane).expect("session"),
                        attachment,
                        created: false,
                    },
                ),
                cx,
            );
        }
    });
    cx.run_until_parked();
    // The active session hosts the composer; the other shows a same-height dock.
    let editor = cx.debug_bounds("composer-editor").expect("docked editor");
    let dock = cx
        .debug_bounds(format!("composer-dock-{second}").leak())
        .expect("inactive dock");
    assert!(editor.bottom() <= dock.bottom() + px(1.0));
    view.read_with(cx, |model, _| assert_eq!(model.composer.pane, Some(first)));
    cx.simulate_click(editor.center(), Modifiers::none());
    cx.simulate_input("for first");
    cx.simulate_click(dock.center(), Modifiers::none());
    cx.run_until_parked();
    view.read_with(cx, |model, _| {
        assert_eq!(model.active_pane(), Some(second));
        assert_eq!(model.composer.pane, Some(second));
    });
    assert_eq!(composer_text(&view, cx), "");
    cx.simulate_input("for second");
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-enter"
    } else {
        "ctrl-enter"
    });
    cx.run_until_parked();
    let (channel, bytes) = requests
        .try_iter()
        .find_map(|(_, work)| match work {
            Work::WriteInput { channel, bytes, .. } => Some((channel, bytes)),
            _ => None,
        })
        .expect("delivery");
    assert_eq!(channel, muxy_protocol::ChannelId(2));
    assert_eq!(bytes, b"\x15for second\r");
    // Switching back restores the first session's draft, even with clear-on-close.
    let first_dock = cx
        .debug_bounds(format!("composer-dock-{first}").leak())
        .expect("first dock");
    cx.simulate_click(first_dock.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(composer_text(&view, cx), "for first");
    // Leaving and returning to the dock keeps session drafts despite clear-on-close.
    for presentation in [
        muxy_app_core::settings::ComposerPresentation::Panel,
        muxy_app_core::settings::ComposerPresentation::Docked,
    ] {
        view.update(cx, |model, cx| {
            let composer = model.composer.view.clone().expect("composer");
            composer.update(cx, |composer, cx| {
                composer.menu_action(
                    crate::views::composer::menu::Command::Presentation(presentation),
                    cx,
                );
            });
        });
        cx.run_until_parked();
    }
    assert_eq!(composer_text(&view, cx), "for first");
    // Switching sessions puts the keyboard in the new session's composer, and
    // Escape hands it to that session's terminal.
    let composer_focused = |view: &Entity<AppModel>, cx: &mut VisualTestContext| {
        view.update_in(cx, |model, window, cx| {
            let pane = model.active_pane().expect("pane");
            let terminal = model.terminal(&pane).expect("terminal");
            let composer = model.composer.view.clone().expect("composer");
            (
                gpui::Focusable::focus_handle(composer.read(cx), cx).contains_focused(window, cx),
                terminal.view.read(cx).focus.is_focused(window),
            )
        })
    };
    view.update(cx, |model, cx| model.focus_pane(second, cx));
    cx.run_until_parked();
    assert_eq!(composer_focused(&view, cx), (true, false));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(composer_focused(&view, cx), (false, true));
}

#[gpui::test]
fn enter_sends_the_composer_and_shift_enter_adds_a_line(cx: &mut TestAppContext) {
    let (view, cx, requests) = attached_composer(cx);
    cx.simulate_input("first");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("second");
    assert_eq!(composer_text(&view, cx), "first\nsecond");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let (bytes, completion) = delivery(&requests);
    assert!(bytes.starts_with(b"\x15first"));
    assert!(bytes.ends_with(b"second\r"));
    completion.try_send(Ok(())).expect("ack");
    cx.run_until_parked();
    assert_eq!(composer_text(&view, cx), "");
}

#[gpui::test]
fn agent_skills_complete_from_a_popup_without_sending(cx: &mut TestAppContext) {
    use muxy_app_core::agent_skills::Skill;

    let (view, cx, requests) = attached_composer(cx);
    view.update(cx, |model, cx| {
        let composer = model.composer.view.clone().expect("composer");
        composer.update(cx, |composer, _| {
            composer.skills = std::sync::Arc::new(
                ["/review", "/release-notes", "/plan", "$imagegen"]
                    .map(|text| Skill {
                        text: text.into(),
                        description: String::new(),
                    })
                    .to_vec(),
            );
        });
    });
    let offered = |cx: &mut VisualTestContext| {
        view.read_with(cx, |model, cx| {
            let composer = model.composer.view.as_ref().expect("composer").read(cx);
            composer.completion(cx).map(|(_, matches)| {
                matches
                    .iter()
                    .map(|skill| skill.text.clone())
                    .collect::<Vec<_>>()
            })
        })
    };
    cx.simulate_input("/re");
    cx.run_until_parked();
    assert!(cx.debug_bounds("composer-skill-1").is_some());
    assert_eq!(
        offered(cx),
        Some(vec!["/review".into(), "/release-notes".into()])
    );
    cx.simulate_keystrokes("down enter");
    assert_eq!(composer_text(&view, cx), "/release-notes ");
    // `/` completes only at the start; `$` anywhere; Escape hides the list.
    cx.simulate_input("x /pl");
    assert_eq!(offered(cx), None);
    cx.simulate_input(" $ima");
    assert_eq!(offered(cx), Some(vec!["$imagegen".into()]));
    cx.simulate_keystrokes("escape");
    assert_eq!(offered(cx), None);
    assert!(view.read_with(cx, |model, _| model.composer.view.is_some()));
    assert!(
        !requests
            .try_iter()
            .any(|(_, work)| matches!(work, Work::WriteInput { .. }))
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        delivery(&requests).0,
        b"\x15/release-notes x /pl $ima\r".to_vec()
    );
}

#[gpui::test]
fn new_tabs_and_splits_focus_their_docked_composer_once_attached(cx: &mut TestAppContext) {
    let (mut boot, _requests) = stub_boot(AppState::bootstrap().expect("state"));
    boot.settings.composer.presentation = muxy_app_core::settings::ComposerPresentation::Docked;
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    cx.simulate_resize(size(px(1000.0), px(600.0)));
    view.update(cx, |model, cx| {
        model.receive((ServerId::local(), 1, Update::Connected(vec![])), cx);
        acknowledge_catalog(model, cx);
    });
    for (session, split) in [(42, false), (43, true)] {
        view.update(cx, |model, cx| {
            if split {
                model.split_pane(Direction::Right, cx);
            } else {
                model.new_tab(cx);
            }
        });
        // The session attaches after the new pane has already taken focus.
        cx.run_until_parked();
        view.update(cx, |model, cx| {
            let pane = model.active_pane().expect("pane");
            let mut attachment = attachment();
            attachment.channel = muxy_protocol::ChannelId(u32::try_from(session).expect("id"));
            model.receive(
                (
                    ServerId::local(),
                    1,
                    Update::Attached {
                        pane,
                        session: SessionId::new(session).expect("ID"),
                        attachment,
                        created: true,
                    },
                ),
                cx,
            );
        });
        cx.run_until_parked();
        view.update_in(cx, |model, window, cx| {
            let composer = model.composer.view.clone().expect("composer");
            assert_eq!(model.composer.pane, model.active_pane());
            assert!(
                gpui::Focusable::focus_handle(composer.read(cx), cx).contains_focused(window, cx),
                "split: {split}"
            );
        });
    }
}

#[gpui::test]
fn quick_commands_insert_into_draft_without_sending_and_reject_other_projects(
    cx: &mut TestAppContext,
) {
    let (view, cx, requests) = attached_composer(cx);
    requests.try_iter().for_each(drop);
    cx.simulate_input("before ");
    view.update(cx, |model, cx| {
        use muxy_app_core::settings::CustomCommand;
        let mut local = CustomCommand::new("Build".into(), "cargo build".into());
        local.status_bar = true;
        local.project = Some(model.state.current_project().id);
        let mut foreign = CustomCommand::new("Other".into(), "rm other".into());
        foreign.project = Some(ProjectId::new());
        model.settings.commands = vec![local, foreign];
        model.insert_quick_command(1, cx);
    });
    cx.run_until_parked();
    assert_eq!(composer_text(&view, cx), "before ");
    view.update(cx, |model, cx| model.insert_quick_command(0, cx));
    cx.run_until_parked();
    assert_eq!(composer_text(&view, cx), "before cargo build");
    assert!(
        !requests
            .try_iter()
            .any(|(_, work)| matches!(work, Work::Input(..) | Work::WriteInput { .. }))
    );
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    assert_eq!(composer_text(&view, cx), "before ");
}
