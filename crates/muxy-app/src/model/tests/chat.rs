use super::*;
use crate::model::chat::{Applied, ChatSession, Splice};
use muxy_protocol::{
    ChannelId, ChatBatch, ChatEvent, ChatItem, ChatItemKind, ChatMode, ChatRequest, ChatSnapshot,
};

fn session_id() -> SessionId {
    SessionId::new(31).expect("session")
}

#[gpui::test]
fn card_composer_keeps_existing_terminal_creation_and_does_not_open_a_session(
    cx: &mut TestAppContext,
) {
    let (mut boot, _requests) = stub_boot(AppState::bootstrap().expect("state"));
    boot.settings
        .commands
        .push(muxy_app_core::settings::CustomCommand::new(
            "Terminal".into(),
            "echo ready".into(),
        ));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.servers.local.connection = ConnectionState::Ready;
        model.servers.local.features = vec![muxy_protocol::Feature::CHAT_PROVIDERS];
        model.prepare_card_composer(cx);
        let composer = model.card_composer().expect("card composer").clone();
        assert!(
            model.state.current_project().tabs.is_empty(),
            "preparing a draft opens nothing"
        );
        model.prepare_card_composer(cx);
        assert_eq!(
            model.card_composer(),
            Some(&composer),
            "redraw keeps the draft entity"
        );
    });
    cx.simulate_resize(size(px(1100.0), px(900.0)));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("new-session-composer").is_some(),
        "the composer renders on the card screen"
    );
    let composer = cx.debug_bounds("new-session-composer").expect("composer");
    let cards = cx.debug_bounds("new-session-cards").expect("cards");
    assert_eq!(composer.origin.x, cards.origin.x);
    assert_eq!(composer.size.width, cards.size.width);
    assert!(
        composer.top() - cards.bottom() >= px(40.0),
        "the composer sits below the launch actions"
    );
    let model_field = cx.debug_bounds("new-session-models").expect("model field");
    for selector in ["new-session-effort", "new-session-send"] {
        let control = cx.debug_bounds(selector).expect("chat control");
        assert_eq!(control.center().y, model_field.center().y, "{selector}");
    }
    view.update(cx, |model, cx| {
        model.new_tab(cx);
        assert_eq!(model.state.current_project().tabs.len(), 1);
        assert!(
            model.card_composer().is_none(),
            "normal terminal tabs never show the card composer"
        );
        assert!(model.chat.card_selections.is_empty());
        assert!(
            model.chat.sessions.is_empty(),
            "normal new-tab behavior does not start chat"
        );
    });
}

#[gpui::test]
fn card_actions_group_providers_filter_commands_and_keep_the_composer_visible(
    cx: &mut TestAppContext,
) {
    use muxy_app_core::settings::CustomCommand;
    let (mut boot, _requests) = stub_boot(AppState::bootstrap().expect("state"));
    for (id, name, command) in [
        ("claude", "CC", "cc"),
        ("codex", "CX", "cx"),
        ("resume", "CC (Resume)", "claude --resume"),
        ("clipboard", "CC (Clipboard)", "claude clipboard"),
        ("handoff", "CC (Handoff Resume)", "claude handoff"),
        ("codex-clipboard", "CX (Clipboard)", "codex clipboard"),
        ("shell", "Terminal", ""),
    ] {
        let mut command = CustomCommand::new(name.into(), command.into());
        command.id = id.into();
        boot.settings.commands.push(command);
    }
    let mut hidden = CustomCommand::new("Hidden".into(), "cc".into());
    hidden.id = "hidden".into();
    hidden.hidden = true;
    let mut foreign = CustomCommand::new("Other project".into(), "cx".into());
    foreign.id = "foreign".into();
    foreign.project = Some(ProjectId::new());
    boot.settings.commands.extend([hidden, foreign]);
    boot.settings.keymap = boot
        .settings
        .keymap
        .with_binding(
            "command.clipboard",
            Some("cmd-ctrl-alt-1".parse().expect("shortcut")),
        )
        .expect("command shortcut");
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.servers.local.connection = ConnectionState::Ready;
        model.servers.local.features = vec![muxy_protocol::Feature::CHAT_PROVIDERS];
        model.prepare_card_composer(cx);
    });
    cx.simulate_resize(size(px(1400.0), px(900.0)));
    cx.run_until_parked();
    let claude = cx
        .debug_bounds("new-session-provider-Claude Code")
        .expect("Claude");
    let codex = cx
        .debug_bounds("new-session-provider-Codex")
        .expect("Codex");
    let shell = cx
        .debug_bounds("new-session-provider-Shell")
        .expect("Shell");
    assert_eq!(claude.size.height, codex.size.height);
    assert_eq!(codex.size.height, shell.size.height);
    assert!((claude.size.width - codex.size.width).abs() <= px(1.0));
    assert!((codex.size.width - shell.size.width).abs() <= px(1.0));
    assert!(claude.right() < codex.left() && codex.right() < shell.left());
    let mut bottom = claude.top();
    for selector in [
        "empty-command-claude",
        "empty-command-resume",
        "empty-command-clipboard",
        "empty-command-handoff",
    ] {
        let row = cx.debug_bounds(selector).expect("action");
        assert!(row.left() >= claude.left() && row.right() <= claude.right());
        assert!(row.top() >= bottom && row.bottom() <= claude.bottom());
        bottom = row.bottom();
    }
    for selector in ["empty-command-hidden", "empty-command-foreign"] {
        assert!(cx.debug_bounds(selector).is_none());
    }
    for (width, height) in [(1400.0, 900.0), (900.0, 480.0), (640.0, 400.0)] {
        cx.simulate_resize(size(px(width), px(height)));
        cx.run_until_parked();
        let composer = cx.debug_bounds("new-session-composer").expect("composer");
        let footer = cx.debug_bounds("new-session-footer").expect("footer");
        assert!(composer.top() >= footer.top());
        assert!(composer.bottom() <= footer.bottom() && footer.bottom() <= px(height));
        let send = cx.debug_bounds("new-session-send").expect("send");
        assert!(send.left() >= composer.left() && send.right() <= composer.right());
        assert!(send.top() >= composer.top() && send.bottom() <= composer.bottom());
    }
    cx.simulate_resize(size(px(1400.0), px(900.0)));
    cx.run_until_parked();
    let row = cx
        .debug_bounds("empty-command-clipboard")
        .expect("clipboard");
    cx.simulate_click(row.center(), Modifiers::default());
    cx.run_until_parked();
    view.read_with(cx, |model, _| {
        assert_eq!(model.state.current_project().tabs.len(), 1);
        let pane = model.active_pane().expect("launched pane");
        assert_eq!(model.state.startup_command(pane), Some("claude clipboard"));
    });
}

#[gpui::test]
fn model_dropdown_preserves_custom_alias_and_draft_when_changing_provider(cx: &mut TestAppContext) {
    let (boot, _requests) = stub_boot(AppState::bootstrap().expect("state"));
    cx.update(|cx| crate::views::workspace::bind_keys(&boot.settings.keymap, cx));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.servers.local.connection = ConnectionState::Ready;
        model.servers.local.features = vec![muxy_protocol::Feature::CHAT_PROVIDERS];
        model.prepare_card_composer(cx);
    });
    cx.simulate_resize(size(px(1100.0), px(900.0)));
    cx.run_until_parked();
    let input = cx.debug_bounds("new-session-textbox").expect("prompt");
    cx.simulate_click(input.center(), Modifiers::default());
    cx.simulate_input("keep this draft");
    let selector = cx.debug_bounds("new-session-models").expect("models");
    cx.simulate_click(selector.center(), Modifiers::default());
    cx.run_until_parked();
    cx.simulate_input("My-Custom-Model");
    cx.run_until_parked();
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    view.read_with(cx, |model, _| {
        assert!(model.state.current_project().tabs.is_empty());
    });
    let selector = cx.debug_bounds("new-session-effort").expect("effort");
    cx.simulate_click(selector.center(), Modifiers::default());
    cx.run_until_parked();
    cx.simulate_input("high");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    view.read_with(cx, |model, _| {
        let selection = model
            .chat
            .card_selections
            .values()
            .next()
            .expect("chat submitted");
        assert_eq!(selection.provider, muxy_protocol::ChatProvider::Codex);
        assert_eq!(selection.model.as_deref(), Some("My-Custom-Model"));
        assert_eq!(selection.effort.as_deref(), Some("high"));
    });
}

#[gpui::test]
fn card_cli_launch_passes_claude_options_and_waits_for_worktree_selection(cx: &mut TestAppContext) {
    card_cli_launch(cx, muxy_protocol::ChatProvider::Claude);
}

#[gpui::test]
fn card_cli_launch_passes_codex_options_and_waits_for_worktree_selection(cx: &mut TestAppContext) {
    card_cli_launch(cx, muxy_protocol::ChatProvider::Codex);
}

#[allow(
    clippy::too_many_lines,
    reason = "One launch-composer-to-terminal delivery scenario"
)]
fn card_cli_launch(cx: &mut TestAppContext, provider: muxy_protocol::ChatProvider) {
    let (mut state, _, [project, ..], directory) = worktrees::pruning_fixture();
    let worktree = state.project(project).expect("worktree").directory.clone();
    state.select_project(project).expect("select worktree");
    let (mut boot, requests) = stub_boot(state);
    boot.settings
        .composer
        .chat_models
        .push(muxy_app_core::settings::ChatChoice {
            server: ServerId::local(),
            project,
            provider,
            model: "selected-model".into(),
            effort: "high".into(),
        });
    cx.update(|cx| crate::views::workspace::bind_keys(&boot.settings.keymap, cx));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.servers.local.connection = ConnectionState::Ready;
        model.servers.local.features = vec![muxy_protocol::Feature::CHAT_PROVIDERS];
        model.prepare_card_composer(cx);
    });
    cx.simulate_resize(size(px(1400.0), px(900.0)));
    cx.run_until_parked();
    let input = cx.debug_bounds("new-session-textbox").expect("prompt");
    cx.simulate_click(input.center(), Modifiers::default());
    cx.simulate_input("first 'quoted' line\nsecond $literal line");
    let cli = cx
        .debug_bounds("chat-show-console-new-session")
        .expect("CLI switch");
    cx.simulate_click(cli.center(), Modifiers::default());
    cx.run_until_parked();
    view.update(cx, |model, cx| {
        model.git.select_after_catalog = Some((ServerId::local(), project, 1));
        cx.notify();
    });
    cx.simulate_keystrokes("enter");
    view.update(cx, |model, cx| {
        assert!(
            model.state.current_project().tabs.is_empty(),
            "never launch before worktree selection finishes"
        );
        model.git.select_after_catalog = None;
        cx.notify();
    });
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let (pane, command) = view.update(cx, |model, cx| {
        assert_eq!(model.state.current_project().id, project);
        assert_eq!(model.state.current_project().tabs.len(), 1);
        assert!(model.chat.card_selections.is_empty());
        assert!(model.chat.sessions.is_empty());
        let pane = model.active_pane().expect("CLI pane");
        assert_eq!(
            model.state.prepare_creation(pane, directory.path()),
            muxy_protocol::ServerPath::from(worktree.as_path())
        );
        let command = model
            .state
            .startup_command(pane)
            .expect("durable startup command")
            .to_owned();
        assert!(
            command.contains(if provider == muxy_protocol::ChatProvider::Claude {
                "claude"
            } else {
                "codex"
            })
        );
        assert!(command.contains("--model selected-model"));
        assert!(
            command.contains(if provider == muxy_protocol::ChatProvider::Claude {
                "--effort high"
            } else {
                "model_reasoning_effort="
            })
        );
        model.receive(
            (
                ServerId::local(),
                1,
                Update::Attached {
                    pane,
                    session: session_id(),
                    attachment: attachment_on(1),
                    created: false,
                },
            ),
            cx,
        );
        (pane, command)
    });
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    let expected = format!("{command}\r").into_bytes();
    assert_eq!(
        servers::work(&requests)
            .iter()
            .filter(|work| matches!(work, Work::Input(_, bytes) if bytes == &expected))
            .count(),
        1,
        "the launch command is delivered and submitted exactly once"
    );
    view.read_with(cx, |model, _| {
        assert!(model.state.startup_command(pane).is_none());
        assert!(model.chat.card_selections.is_empty());
    });
}

#[gpui::test]
fn card_claude_prompt_preserves_selected_options(cx: &mut TestAppContext) {
    card_chat_prompt(cx, muxy_protocol::ChatProvider::Claude);
}

#[gpui::test]
fn card_prompt_shift_enter_adds_a_line_and_enter_opens_one_chat(cx: &mut TestAppContext) {
    card_chat_prompt(cx, muxy_protocol::ChatProvider::Codex);
}

#[allow(
    clippy::too_many_lines,
    reason = "One card-to-provider submission scenario"
)]
fn card_chat_prompt(cx: &mut TestAppContext, provider: muxy_protocol::ChatProvider) {
    use muxy_protocol::{
        ChatCapabilities, ChatModel, ChatReply, ChatSubmission, OperationId, SubmissionOutcome,
    };
    let expected_provider = provider;
    let (mut state, _, [project, ..], directory) = worktrees::pruning_fixture();
    let worktree = state.project(project).expect("worktree").directory.clone();
    state.select_project(project).expect("select worktree");
    let (mut boot, requests) = stub_boot(state);
    boot.settings
        .composer
        .chat_models
        .push(muxy_app_core::settings::ChatChoice {
            server: ServerId::local(),
            project: boot.state.current_project().id,
            provider,
            model: "selected-model".into(),
            effort: "high".into(),
        });
    cx.update(|cx| crate::views::workspace::bind_keys(&boot.settings.keymap, cx));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.servers.local.connection = ConnectionState::Ready;
        model.servers.local.features = vec![
            muxy_protocol::Feature::CHAT_PROVIDERS,
            muxy_protocol::Feature::AGENT_CHAT,
            muxy_protocol::Feature::CHAT_OPERATIONS,
        ];
        model.prepare_card_composer(cx);
    });
    cx.simulate_resize(size(px(1100.0), px(900.0)));
    cx.run_until_parked();
    let input = cx.debug_bounds("new-session-textbox").expect("input");
    cx.simulate_click(input.center(), Modifiers::default());
    cx.simulate_input("first line");
    cx.simulate_keystrokes("shift-enter");
    view.read_with(cx, |model, _| {
        assert!(model.state.current_project().tabs.is_empty());
    });
    cx.simulate_input("second line");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    view.read_with(cx, |model, _| {
        assert_eq!(model.state.current_project().tabs.len(), 1);
        assert_eq!(
            model.chat.card_selections.len(),
            1,
            "one chat launch queued"
        );
    });
    view.update(cx, |model, cx| {
        let pane = model.active_pane().expect("new pane");
        assert_eq!(model.state.current_project().id, project);
        assert_eq!(
            model.state.prepare_creation(pane, directory.path()),
            muxy_protocol::ServerPath::from(worktree.as_path())
        );
        model
            .state
            .set_pane_session(pane, Some(session_id()))
            .expect("session");
        model.receive(
            (
                ServerId::local(),
                1,
                Update::Attached {
                    pane,
                    session: session_id(),
                    attachment: attachment_on(1),
                    created: false,
                },
            ),
            cx,
        );
        model.grids[&pane].view.update(cx, |terminal, _| {
            terminal.process = Some(muxy_protocol::ForegroundProcess {
                name: "zsh".into(),
                is_shell: true,
            });
        });
        assert!(model.chat_start_if_idle(pane, cx));
    });
    let mut submissions = 0;
    for _ in 0..8 {
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        for work in servers::work(&requests) {
            let Work::Chat { request, reply } = work else {
                continue;
            };
            let response = match request {
                ChatRequest::Discover { provider, .. } => {
                    assert_eq!(provider, expected_provider);
                    ChatReply::Discovered {
                        host_epoch: 1,
                        provider,
                        capabilities: ChatCapabilities {
                            models: vec![ChatModel {
                                alias: "selected-model".into(),
                                label: "Selected".into(),
                                efforts: vec!["high".into()],
                                default_effort: Some("high".into()),
                                images: None,
                            }],
                            ..ChatCapabilities::default()
                        },
                        model: Some("selected-model".into()),
                        effort: Some("high".into()),
                        threads: Vec::new(),
                        launch_context: Some(7),
                    }
                }
                ChatRequest::PrepareSubmission { .. } => ChatReply::Prepared {
                    host_epoch: 1,
                    operation: OperationId::new(),
                },
                ChatRequest::Begin {
                    operation,
                    selection,
                    text,
                    ..
                } => {
                    submissions += 1;
                    assert_eq!(text, "first line\nsecond line");
                    assert_eq!(selection.provider, expected_provider);
                    assert_eq!(selection.model.as_deref(), Some("selected-model"));
                    assert_eq!(selection.effort.as_deref(), Some("high"));
                    assert_eq!(selection.launch_context, Some(7));
                    ChatReply::Submission(ChatSubmission {
                        operation,
                        generation: 1,
                        outcome: SubmissionOutcome::Accepted,
                        detail: None,
                    })
                }
                _ => continue,
            };
            reply.try_send(Ok(response)).expect("reply");
        }
    }
    assert_eq!(submissions, 1, "the first prompt starts one native chat");
}

#[gpui::test]
fn card_launch_discovers_the_selected_provider_without_launching_the_default(
    cx: &mut TestAppContext,
) {
    let (view, cx, requests, [pane, ..]) = two_panes_on_one_session(cx, false);
    view.update(cx, |model, cx| {
        let terminal = model.grids[&pane].view.clone();
        terminal.update(cx, |terminal, _| {
            terminal.process = Some(muxy_protocol::ForegroundProcess {
                name: "zsh".into(),
                is_shell: true,
            });
        });
        model.chat.card_selections.insert(
            pane,
            muxy_protocol::ChatSelection {
                provider: muxy_protocol::ChatProvider::Codex,
                model: Some("selected-model".into()),
                effort: Some("high".into()),
                target: None,
                launch_context: None,
            },
        );
        model.chat_start_when_idle(pane, cx);
        assert!(model.chat_start_if_idle(pane, cx));
        let draft = model
            .chat_session(pane)
            .expect("session")
            .local
            .as_ref()
            .expect("draft");
        assert_eq!(draft.selection.provider, muxy_protocol::ChatProvider::Codex);
        assert_eq!(draft.selection.model.as_deref(), Some("selected-model"));
        assert_eq!(draft.selection.effort.as_deref(), Some("high"));
        assert!(model.chat.card_selections.is_empty());
    });
    cx.run_until_parked();
    let work = chat_work(&requests, cx);
    let requests = chat_requests(&work);
    assert!(requests.iter().any(|request| matches!(
        request,
        ChatRequest::Discover {
            provider: muxy_protocol::ChatProvider::Codex,
            ..
        }
    )));
    assert!(!requests.iter().any(|request| matches!(
        request,
        ChatRequest::Discover {
            provider: muxy_protocol::ChatProvider::Claude,
            ..
        } | ChatRequest::Start { .. }
            | ChatRequest::Begin { .. }
    )));
}

fn batch(generation: u64, from_rev: u64, to_rev: u64, events: Vec<ChatEvent>) -> ChatBatch {
    ChatBatch {
        session: session_id(),
        generation,
        from_rev,
        to_rev,
        events,
        snapshot: None,
    }
}

fn snapshot(generation: u64, to_rev: u64, subscription: u32, items: Vec<ChatItem>) -> ChatBatch {
    ChatBatch {
        snapshot: Some(Box::new(ChatSnapshot {
            subscription,
            items,
            state: muxy_protocol::ChatState::default(),
            capabilities: muxy_protocol::ChatCapabilities::default(),
            pending: Vec::new(),
            has_earlier: false,
        })),
        ..batch(generation, to_rev, to_rev, Vec::new())
    }
}

fn assistant(id: &str, seq: u64, text: &str) -> ChatItem {
    ChatItem {
        id: id.into(),
        turn_id: "turn".into(),
        seq,
        kind: ChatItemKind::Assistant {
            markdown: text.into(),
            final_in_turn: false,
        },
    }
}

fn delta(id: &str, text: &str) -> ChatEvent {
    ChatEvent::Delta {
        item_id: id.into(),
        text: text.into(),
    }
}

fn texts(session: &ChatSession) -> Vec<&str> {
    session.rows.iter().map(|row| row.text.as_ref()).collect()
}

/// A Chat session subscribed with `token` and synced at revision 5.
fn synced(token: u32) -> ChatSession {
    let mut session = ChatSession::default();
    session.set_mode(ChatMode::Chat, 1);
    session.subscribe(token);
    session.apply(snapshot(1, 5, token, vec![assistant("a", 1, "Hi")]));
    session
}

#[test]
fn only_the_latest_subscriptions_snapshot_applies_even_before_its_reply() {
    let mut session = ChatSession::default();
    session.set_mode(ChatMode::Chat, 1);
    assert!(session.wants_subscription());
    session.subscribe(1);
    assert!(!session.wants_subscription());
    assert_eq!(
        session.apply(batch(
            1,
            0,
            1,
            vec![ChatEvent::Upsert(assistant("x", 0, "early"))]
        )),
        Applied::default(),
        "batches before the snapshot are dropped"
    );
    session.subscribe(2);
    let stale = session.apply(snapshot(1, 3, 1, vec![assistant("old", 1, "old")]));
    assert!(stale.splices.is_empty() && session.rows.is_empty());
    let fresh = session.apply(snapshot(1, 4, 2, vec![assistant("a", 1, "Hi")]));
    assert_eq!(fresh.splices, [Splice::Reset(1)]);
    assert_eq!(texts(&session), ["Hi"]);
}

#[test]
fn a_revision_gap_or_resync_resubscribes_and_drops_batches_until_the_snapshot() {
    let mut session = synced(1);
    let gap = session.apply(batch(1, 6, 7, vec![delta("a", " lost")]));
    assert!(gap.resubscribe && gap.splices.is_empty());
    let after = session.apply(batch(1, 7, 8, vec![delta("a", " later")]));
    assert!(!after.resubscribe && after.splices.is_empty());
    assert_eq!(texts(&session), ["Hi"]);

    let mut session = synced(1);
    assert!(
        session
            .apply(batch(1, 5, 6, vec![ChatEvent::Resync]))
            .resubscribe
    );
}

#[test]
fn deltas_splice_their_own_row_and_unknown_ids_are_ignored_without_a_gap() {
    let mut session = synced(1);
    let applied = session.apply(batch(
        1,
        5,
        6,
        vec![delta("gone", "x"), delta("a", " there")],
    ));
    assert_eq!(applied.splices, [Splice::Replace(0)]);
    assert!(!applied.resubscribe);
    assert_eq!(texts(&session), ["Hi there"]);
    assert_eq!(session.rows[0].shared().as_ref(), "Hi there");
    let next = session.apply(batch(
        1,
        6,
        7,
        vec![ChatEvent::Upsert(assistant("b", 0, "first"))],
    ));
    assert_eq!(
        next.splices.first(),
        Some(&Splice::Range(0..1, 2)),
        "items keep their seq order; the new first step heads the turn, so all of it re-measures"
    );
    assert!(session.apply(batch(1, 7, 8, vec![delta("a", "!")])).splices == [Splice::Replace(1)]);
    assert_eq!(texts(&session), ["first", "Hi there!"]);
    assert_eq!(
        session.rows[1].shared().as_ref(),
        "Hi there!",
        "a delta drops the drawn copy"
    );
}

#[test]
fn an_exit_reported_after_the_console_reset_still_toasts_its_notices() {
    let mut session = synced(1);
    session.switching = true;
    session.set_mode(ChatMode::Console, 0);
    assert!(
        session.switching,
        "the switch waits for the Enter or Leave reply"
    );
    let notice = ChatItem {
        kind: ChatItemKind::Notice {
            text: "claude --resume u".into(),
        },
        ..assistant("n", 9, "")
    };
    let applied = session.apply(batch(
        1,
        5,
        6,
        vec![
            ChatEvent::Exited {
                reason: "Claude Code exited".into(),
            },
            ChatEvent::Upsert(notice.clone()),
        ],
    ));
    assert_eq!(applied.toasts, ["Claude Code exited", "claude --resume u"]);
    let later = session.apply(batch(1, 6, 7, vec![ChatEvent::Upsert(notice)]));
    assert_eq!(later.toasts, ["claude --resume u"]);
}

#[test]
fn a_newer_generation_clears_the_chat_and_resubscribes_while_older_ones_are_ignored() {
    let mut session = synced(1);
    assert_eq!(
        session.apply(batch(0, 5, 6, vec![delta("a", "!")])),
        Applied::default()
    );
    let newer = session.apply(batch(2, 0, 1, vec![delta("a", "!")]));
    assert!(newer.resubscribe);
    assert_eq!(newer.splices, [Splice::Reset(0)]);
    assert!(session.rows.is_empty());
    assert_eq!(session.generation, 2);
    assert_eq!(session.mode, ChatMode::Chat);
}

/// The model, its window, the work it sent, and panes on sessions 31, 31 and 32.
type Fixture<'a> = (
    Entity<AppModel>,
    &'a mut VisualTestContext,
    std::sync::mpsc::Receiver<(u64, Work)>,
    [PaneId; 3],
);

fn two_panes_on_one_session(cx: &mut TestAppContext, broadcast: bool) -> Fixture<'_> {
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
    boot.settings.composer.broadcast = broadcast;
    cx.update(|cx| crate::views::workspace::bind_keys(&boot.settings.keymap, cx));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.servers.local.connection = ConnectionState::Ready;
        model.servers.local.features = vec![
            muxy_protocol::Feature::AGENT_CHAT,
            muxy_protocol::Feature::CHAT_PROVIDERS,
            muxy_protocol::Feature::CHAT_OPERATIONS,
            muxy_protocol::Feature::CHAT_NATIVE_APPROVALS,
        ];
        for (index, pane) in [first, second, third].into_iter().enumerate() {
            let attachment = attachment_on(u32::try_from(index + 1).expect("channel"));
            model.receive(
                (
                    ServerId::local(),
                    1,
                    Update::Attached {
                        pane,
                        session: model.pane_session(pane).expect("session").1,
                        attachment,
                        created: false,
                    },
                ),
                cx,
            );
        }
    });
    let _ = chat_work(&requests, cx);
    (view, cx, requests, [first, second, third])
}

fn chat_work(
    requests: &std::sync::mpsc::Receiver<(u64, Work)>,
    cx: &mut VisualTestContext,
) -> Vec<Work> {
    use muxy_protocol::{ChatReply, ChatSubmission, OperationId, SubmissionOutcome};
    let mut work = Vec::new();
    for _ in 0..8 {
        let pending = servers::work(requests);
        if pending.is_empty() {
            break;
        }
        for item in &pending {
            if let Work::Chat { request, reply } = item {
                let response = match request {
                    ChatRequest::PrepareSubmission { .. } => Some(ChatReply::Prepared {
                        host_epoch: 1,
                        operation: OperationId::new(),
                    }),
                    ChatRequest::Submit {
                        operation,
                        generation,
                        ..
                    } => Some(ChatReply::Submission(ChatSubmission {
                        operation: *operation,
                        generation: *generation,
                        outcome: SubmissionOutcome::Accepted,
                        detail: None,
                    })),
                    _ => None,
                };
                if let Some(response) = response {
                    reply.try_send(Ok(response)).expect("test reply");
                }
            }
        }
        work.extend(pending);
        cx.run_until_parked();
    }
    work
}

fn chat_requests(work: &[Work]) -> Vec<&ChatRequest> {
    work.iter()
        .filter_map(|work| match work {
            Work::Chat { request, .. } => Some(request),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn duplicate_panes_share_one_subscription_until_the_last_one_detaches(cx: &mut TestAppContext) {
    let (view, cx, requests, [first, second, _]) = two_panes_on_one_session(cx, false);
    view.update(cx, |model, cx| {
        // The server reports the mode on each attachment of the session.
        for channel in [1, 2] {
            model.chat_mode(ServerId::local(), ChannelId(channel), ChatMode::Chat, 1, cx);
        }
    });
    cx.run_until_parked();
    let work = chat_work(&requests, cx);
    let sent = chat_requests(&work);
    assert_eq!(sent.len(), 1, "one Subscribe for both panes: {sent:?}");
    assert!(matches!(sent[0], ChatRequest::Subscribe { session, .. } if *session == session_id()));
    view.read_with(cx, |model, _| {
        assert!(model.chat_view(first).is_some() && model.chat_view(second).is_some());
    });

    view.update(cx, |model, cx| model.detach_terminal(second, cx));
    cx.run_until_parked();
    let work = chat_work(&requests, cx);
    assert!(
        chat_requests(&work).is_empty(),
        "a pane still shows the chat"
    );

    view.update(cx, |model, cx| model.detach_terminal(first, cx));
    cx.run_until_parked();
    let work = chat_work(&requests, cx);
    let sent = chat_requests(&work);
    assert_eq!(
        sent.len(),
        1,
        "{:?}",
        work.iter().map(Work::name).collect::<Vec<_>>()
    );
    assert!(matches!(sent[0], ChatRequest::Unsubscribe { .. }));
}

fn submit(cx: &mut VisualTestContext, text: &str) {
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-i"
    } else {
        "ctrl-shift-i"
    });
    cx.simulate_input(text);
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-enter"
    } else {
        "ctrl-enter"
    });
    cx.run_until_parked();
}

#[gpui::test]
fn the_composer_sends_chat_to_one_session_and_never_types_into_its_terminal(
    cx: &mut TestAppContext,
) {
    let (view, cx, requests, [first, ..]) = two_panes_on_one_session(cx, true);
    view.update(cx, |model, cx| {
        model.chat_mode(ServerId::local(), ChannelId(1), ChatMode::Chat, 1, cx);
        model.focus_pane(first, cx);
    });
    cx.run_until_parked();
    let _ = chat_work(&requests, cx);
    submit(cx, "hello");
    let work = chat_work(&requests, cx);
    let sends: Vec<_> = chat_requests(&work)
        .into_iter()
        .filter(|request| matches!(request, ChatRequest::Submit { .. }))
        .collect();
    let composer = view.read_with(cx, |model, cx| {
        model.composer.view.as_ref().map(|view| {
            let view = view.read(cx);
            (view.draft.text.clone(), view.error.clone())
        })
    });
    assert_eq!(
        sends.len(),
        1,
        "{composer:?} {:?}",
        work.iter().map(Work::name).collect::<Vec<_>>()
    );
    assert!(
        matches!(sends[0], ChatRequest::Submit { session, text, .. } if *session == session_id() && text == "hello")
    );
    assert!(
        !work
            .iter()
            .any(|work| matches!(work, Work::WriteInput { .. })),
        "broadcast never reaches a terminal from a chat pane"
    );
}

#[gpui::test]
fn chat_hides_the_docked_composer_and_restores_its_console_draft(cx: &mut TestAppContext) {
    let (view, cx, _requests, [first, ..]) = two_panes_on_one_session(cx, false);
    view.update(cx, |model, cx| {
        model.settings.composer.presentation =
            muxy_app_core::settings::ComposerPresentation::Docked;
        model.focus_pane(first, cx);
        model.sync_docked_composer(cx);
    });
    cx.run_until_parked();
    let editor = cx
        .debug_bounds("composer-editor")
        .expect("console composer");
    cx.simulate_click(editor.center(), Modifiers::none());
    cx.simulate_input("console draft");
    view.update(cx, |model, cx| {
        model.chat_mode(ServerId::local(), ChannelId(1), ChatMode::Chat, 1, cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |model, _| {
        assert!(model.chat_view(first).is_some());
        assert!(model.composer.view.is_none());
    });
    cx.simulate_input("chat draft");
    assert_eq!(chat_input(&view, cx, first), "chat draft");
    view.update(cx, |model, cx| {
        model.chat_mode(ServerId::local(), ChannelId(1), ChatMode::Console, 1, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("composer-editor").is_some());
    view.read_with(cx, |model, cx| {
        assert_eq!(
            model
                .composer
                .view
                .as_ref()
                .expect("composer")
                .read(cx)
                .draft
                .text,
            "console draft"
        );
    });
}

/// Types into the chat input of `pane`, which the chat focuses with its pane.
fn chat_input(view: &Entity<AppModel>, cx: &mut VisualTestContext, pane: PaneId) -> String {
    view.read_with(cx, |model, cx| {
        model
            .chat_view(pane)
            .expect("chat view")
            .read(cx)
            .input_text(cx)
    })
}

fn sent_texts(
    requests: &std::sync::mpsc::Receiver<(u64, Work)>,
    cx: &mut VisualTestContext,
) -> Vec<String> {
    chat_work(requests, cx)
        .iter()
        .filter_map(|work| match work {
            Work::Chat {
                request: ChatRequest::Submit { text, .. },
                ..
            } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn enter_takes_the_highlighted_slash_command_and_tui_commands_stay_local(cx: &mut TestAppContext) {
    let (view, cx, requests, [first, ..]) = two_panes_on_one_session(cx, false);
    view.update(cx, |model, cx| {
        model.chat_mode(ServerId::local(), ChannelId(1), ChatMode::Chat, 1, cx);
        model.focus_pane(first, cx);
    });
    cx.run_until_parked();
    let set_commands = |view: &Entity<AppModel>, cx: &mut VisualTestContext, names: &[&str]| {
        view.update(cx, |model, _| {
            let key = model.chat_key(first).expect("chat key");
            let session = model.chat.sessions.get_mut(&key).expect("session");
            session.capabilities.commands = names
                .iter()
                .map(|name| muxy_protocol::ChatCommand {
                    name: (*name).into(),
                    description: String::new(),
                    skill: false,
                })
                .collect();
        });
    };
    set_commands(&view, cx, &["compact", "context"]);
    let _ = chat_work(&requests, cx);

    cx.simulate_input("/comp");
    cx.run_until_parked();
    let shown = view.read_with(cx, |model, cx| {
        let chat = model.chat_view(first).expect("chat view").read(cx);
        chat.slash_names()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });
    assert_eq!(shown, ["compact"]);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(chat_input(&view, cx, first), "/compact ");
    assert!(
        sent_texts(&requests, cx).is_empty(),
        "Enter took the command instead of sending"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(sent_texts(&requests, cx), ["/compact"]);

    // A command typed out in full is sent, not completed.
    view.update(cx, |model, cx| {
        let chat = model.chat_view(first).expect("chat view").clone();
        chat.update(cx, |chat, cx| chat.set_input_text("/compact", cx));
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(sent_texts(&requests, cx), ["/compact"]);

    // A TUI-only command the list leaves out stays in the input instead of being sent.
    view.update(cx, |model, cx| {
        let chat = model.chat_view(first).expect("chat view").clone();
        chat.update(cx, |chat, cx| chat.set_input_text("/login", cx));
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(sent_texts(&requests, cx).is_empty());
    assert_eq!(chat_input(&view, cx, first), "/login");
    assert!(view.read_with(cx, |model, _| {
        model
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("/login"))
    }));

    // Before Claude Code lists its commands, it goes as typed.
    set_commands(&view, cx, &[]);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(sent_texts(&requests, cx), ["/login"]);
}

#[gpui::test]
fn a_console_broadcast_skips_sessions_in_chat(cx: &mut TestAppContext) {
    let (view, cx, requests, [_, _, third]) = two_panes_on_one_session(cx, true);
    view.update(cx, |model, cx| {
        model.chat_mode(ServerId::local(), ChannelId(1), ChatMode::Chat, 1, cx);
        model.focus_pane(third, cx);
    });
    cx.run_until_parked();
    let _ = chat_work(&requests, cx);
    submit(cx, "ls");
    let work = chat_work(&requests, cx);
    let written: Vec<_> = work
        .iter()
        .filter_map(|work| match work {
            Work::WriteInput { channel, .. } => Some(*channel),
            _ => None,
        })
        .collect();
    assert_eq!(
        written,
        [ChannelId(3)],
        "only the Console session gets input"
    );
    assert!(chat_requests(&work).is_empty());
}

fn item(id: &str, turn: &str, seq: u64, kind: ChatItemKind) -> ChatItem {
    ChatItem {
        id: id.into(),
        turn_id: turn.into(),
        seq,
        kind,
    }
}

fn tool(id: &str, seq: u64) -> ChatItem {
    item(
        id,
        "t1",
        seq,
        ChatItemKind::Tool {
            name: "Bash".into(),
            summary: String::new(),
            input_preview: String::new(),
            result: Some(String::new()),
            subagent_count: 0,
            is_error: false,
        },
    )
}

#[gpui::test]
fn failed_skills_keep_their_error_and_open_it_from_the_chat(cx: &mut TestAppContext) {
    let (view, cx, _requests, [first, ..]) = two_panes_on_one_session(cx, false);
    let error = "Skill not found: staging-check\nAvailable skills: estimator, aaron";
    view.update(cx, |model, cx| {
        model.chat_mode(ServerId::local(), ChannelId(1), ChatMode::Chat, 1, cx);
        let key = model.chat_key(first).expect("chat");
        model
            .chat
            .sessions
            .get_mut(&key)
            .expect("session")
            .subscribe(100);
        let failed = item(
            "failed-skill",
            "turn",
            1,
            ChatItemKind::Tool {
                name: "Skill".into(),
                summary: "staging-check".into(),
                input_preview: "{}".into(),
                result: Some(error.into()),
                subagent_count: 0,
                is_error: true,
            },
        );
        model.receive_chat(ServerId::local(), snapshot(1, 5, 100, vec![failed]), cx);
        model.focus_pane(first, cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |model, _| {
        let row = &model.chat_session(first).expect("chat").rows[0];
        assert_eq!(
            row.tool_details(false),
            Some("Skill not found: staging-check")
        );
    });
    let details = cx
        .debug_bounds("chat-details-failed-skill")
        .expect("error disclosure");
    cx.simulate_click(details.center(), Modifiers::none());
    cx.run_until_parked();
    view.read_with(cx, |model, _| {
        let session = model.chat_session(first).expect("chat");
        assert!(session.expanded.contains("failed-skill"));
        assert_eq!(session.rows[0].tool_details(true), Some(error));
    });
}

#[test]
fn live_tool_errors_survive_turn_folding_but_successful_outputs_are_discarded() {
    let mut session = synced(1);
    let failed = item(
        "skill",
        "t1",
        2,
        ChatItemKind::Tool {
            name: "Skill".into(),
            summary: "staging-check".into(),
            input_preview: "{}".into(),
            result: Some("Permission denied\nDetails".into()),
            subagent_count: 0,
            is_error: true,
        },
    );
    let end = item(
        "end",
        "t1",
        3,
        ChatItemKind::TurnEnd {
            duration_ms: 10,
            is_error: false,
            changed: Vec::new(),
        },
    );
    session.apply(batch(
        1,
        5,
        6,
        vec![ChatEvent::Upsert(failed.clone()), ChatEvent::Upsert(end)],
    ));
    assert!(
        !session.hidden(&session.rows[1]),
        "errors stay visible after the turn finishes"
    );
    assert_eq!(
        session.rows[1].tool_details(true),
        Some("Permission denied\nDetails")
    );
    let mut recovered = failed.clone();
    if let ChatItemKind::Tool { is_error, .. } = &mut recovered.kind {
        *is_error = false;
    }
    session.apply(batch(1, 6, 7, vec![ChatEvent::Upsert(recovered)]));
    assert_eq!(session.rows[1].tool_details(true), None);
    assert!(session.hidden(&session.rows[1]));
    let mut empty = failed;
    if let ChatItemKind::Tool { result, .. } = &mut empty.kind {
        *result = Some(" \n".into());
    }
    session.apply(batch(1, 7, 8, vec![ChatEvent::Upsert(empty)]));
    assert_eq!(
        session.rows[1].tool_details(false),
        Some("The tool did not provide error details.")
    );
}

fn hidden(session: &ChatSession) -> Vec<bool> {
    session.rows.iter().map(|row| session.hidden(row)).collect()
}

/// Applies `splices` to a mirror of a pane's list, as the view does, and
/// checks every row they didn't touch is still the row the list shows there.
fn mirror(list: &mut Vec<Option<String>>, session: &ChatSession, splices: &[Splice]) {
    for splice in splices {
        match splice {
            Splice::Reset(rows) => *list = vec![None; *rows],
            Splice::Replace(at) => list[*at] = None,
            Splice::Insert(at) => list.insert(*at, None),
            Splice::Range(range, rows) => {
                list.splice(range.clone(), std::iter::repeat_n(None, *rows));
            }
        }
    }
    let shown: Vec<&str> = session
        .visible
        .iter()
        .map(|&at| session.rows[at].item.id.as_str())
        .collect();
    assert_eq!(list.len(), shown.len(), "{splices:?}");
    for (row, id) in list.iter_mut().zip(shown) {
        if let Some(row) = row {
            assert_eq!(row, id, "a row moved without a splice: {splices:?}");
        }
        *row = Some(id.to_owned());
    }
}

fn shown(session: &ChatSession) -> Vec<&str> {
    session
        .visible
        .iter()
        .map(|&at| session.rows[at].item.id.as_str())
        .collect()
}

#[test]
fn a_finished_turn_leaves_its_steps_out_of_the_list_and_reopens_on_toggle() {
    let mut session = synced(1);
    let mut list = Vec::new();
    mirror(&mut list, &session, &[Splice::Reset(session.visible.len())]);
    let user = item("u", "t1", 2, ChatItemKind::User { text: "go".into() });
    let early = item(
        "m1",
        "t1",
        4,
        ChatItemKind::Assistant {
            markdown: "looking".into(),
            final_in_turn: false,
        },
    );
    let closing = item(
        "m2",
        "t1",
        6,
        ChatItemKind::Assistant {
            markdown: "done".into(),
            final_in_turn: false,
        },
    );
    // Out of order, so one lands between rows already shown.
    let events = [user, tool("x2", 5), early, tool("x1", 3), closing.clone()]
        .into_iter()
        .map(ChatEvent::Upsert)
        .collect();
    let applied = session.apply(batch(1, 5, 6, events));
    mirror(&mut list, &session, &applied.splices);
    assert_eq!(
        shown(&session),
        ["a", "u", "x1", "m1", "x2", "m2"],
        "a running turn keeps every step open"
    );
    assert_eq!(session.turns["t1"].tools, 2);
    assert_eq!(session.turns["t1"].head.as_deref(), Some("x1"));

    let end = item(
        "end",
        "t1",
        7,
        ChatItemKind::TurnEnd {
            duration_ms: 1500,
            is_error: false,
            changed: Vec::new(),
        },
    );
    let finished = ChatItem {
        kind: ChatItemKind::Assistant {
            markdown: "done".into(),
            final_in_turn: true,
        },
        ..closing
    };
    let applied = session.apply(batch(
        1,
        6,
        7,
        vec![ChatEvent::Upsert(end), ChatEvent::Upsert(finished)],
    ));
    mirror(&mut list, &session, &applied.splices);
    assert_eq!(
        shown(&session),
        ["a", "u", "x1", "m2", "end"],
        "the head stays for the summary; the other steps leave the list"
    );
    assert!(
        hidden(&session) == [false, false, true, true, true, false, false],
        "the head draws only the summary"
    );
    let turn = &session.turns["t1"];
    assert_eq!((turn.tools, turn.messages, turn.done), (2, 1, true));
    assert!(turn.has_final);
    assert_eq!(turn.summary.as_ref(), "2 tool calls, 1 message");
    assert_eq!(session.copy_text("end"), Some("done"));

    let hidden_delta = session.apply(batch(1, 7, 8, vec![delta("m1", " more")]));
    assert!(
        hidden_delta.splices.is_empty(),
        "a row out of the list isn't re-measured"
    );
    let shown_delta = session.apply(batch(1, 8, 9, vec![delta("m2", "!")]));
    assert_eq!(shown_delta.splices, [Splice::Replace(3)]);
    mirror(&mut list, &session, &shown_delta.splices);

    let opened = session.toggle("t1");
    mirror(&mut list, &session, &opened);
    assert_eq!(shown(&session).len(), 7);
    assert_eq!(hidden(&session), [false; 7]);
    let closed = session.toggle("t1");
    mirror(&mut list, &session, &closed);
    assert_eq!(shown(&session), ["a", "u", "x1", "m2", "end"]);

    // A row arriving after the end of an open turn still lands in place.
    let opened = session.toggle("t1");
    mirror(&mut list, &session, &opened);
    let notice = item(
        "n",
        "",
        8,
        ChatItemKind::Notice {
            text: "later".into(),
        },
    );
    let applied = session.apply(batch(1, 9, 10, vec![ChatEvent::Upsert(notice)]));
    mirror(&mut list, &session, &applied.splices);
}

#[test]
fn a_snapshots_folded_steps_count_in_the_summary() {
    let mut session = ChatSession::default();
    session.set_mode(ChatMode::Chat, 1);
    session.subscribe(1);
    let items = vec![
        item("u", "t1", 1, ChatItemKind::User { text: "go".into() }),
        tool("x1", 2),
        item("f", "t1", 3, ChatItemKind::Folded { count: 40 }),
        tool("x2", 4),
        item(
            "end",
            "t1",
            5,
            ChatItemKind::TurnEnd {
                duration_ms: 1,
                is_error: false,
                changed: Vec::new(),
            },
        ),
    ];
    let applied = session.apply(snapshot(1, 5, 1, items));
    assert_eq!(applied.splices, [Splice::Reset(3)]);
    assert_eq!(shown(&session), ["u", "x1", "end"]);
    assert_eq!(session.turns["t1"].summary.as_ref(), "42 tool calls");
}

#[test]
fn a_delta_reparses_only_its_own_row_and_a_same_text_upsert_keeps_the_parse() {
    let mut session = synced(1);
    session.apply(batch(
        1,
        5,
        6,
        vec![ChatEvent::Upsert(assistant("b", 2, "**one**"))],
    ));
    let other = session.rows[0]
        .document(None, &session.files, None)
        .blocks
        .as_ptr();
    assert_eq!(
        session.rows[1].document(None, &session.files, None).blocks[0]
            .text
            .as_ref(),
        "one"
    );
    session.apply(batch(1, 6, 7, vec![delta("b", " two")]));
    assert_eq!(
        session.rows[1].document(None, &session.files, None).blocks[0]
            .text
            .as_ref(),
        "one two"
    );
    assert!(
        std::ptr::eq(
            other,
            session.rows[0]
                .document(None, &session.files, None)
                .blocks
                .as_ptr()
        ),
        "the other row keeps its parse"
    );
    let kept = session.rows[1]
        .document(None, &session.files, None)
        .blocks
        .as_ptr();
    session.apply(batch(
        1,
        7,
        8,
        vec![ChatEvent::Upsert(assistant("b", 2, "**one** two"))],
    ));
    assert!(std::ptr::eq(
        kept,
        session.rows[1]
            .document(None, &session.files, None)
            .blocks
            .as_ptr()
    ));
}

#[test]
fn an_exit_in_chat_adds_a_row_after_the_conversation_and_still_toasts() {
    let mut session = synced(1);
    let applied = session.apply(batch(
        1,
        5,
        6,
        vec![ChatEvent::Exited {
            reason: "Claude Code exited".into(),
        }],
    ));
    assert_eq!(applied.toasts, ["Claude Code exited"]);
    assert_eq!(applied.splices, [Splice::Insert(1)]);
    assert_eq!(texts(&session), ["Hi", "Claude Code exited"]);
    assert_eq!(session.rows[1].item.id, crate::model::chat::EXITED_ROW);
}

#[gpui::test]
fn typing_in_a_chat_sends_enter_to_its_one_session_and_never_to_the_terminal(
    cx: &mut TestAppContext,
) {
    let (view, cx, requests, [first, ..]) = two_panes_on_one_session(cx, false);
    view.update(cx, |model, cx| {
        model.chat_mode(ServerId::local(), ChannelId(1), ChatMode::Chat, 1, cx);
        model.focus_pane(first, cx);
    });
    cx.run_until_parked();
    let _ = chat_work(&requests, cx);
    // Focusing the pane lands in the chat's input, not its terminal.
    cx.simulate_input("fix 1");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("bug");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let work = chat_work(&requests, cx);
    let sends: Vec<_> = chat_requests(&work)
        .into_iter()
        .filter(|request| matches!(request, ChatRequest::Submit { .. }))
        .collect();
    assert_eq!(
        sends.len(),
        1,
        "{:?}",
        work.iter().map(Work::name).collect::<Vec<_>>()
    );
    assert!(
        matches!(sends[0], ChatRequest::Submit { session, text, .. } if *session == session_id() && text == "fix 1\nbug")
    );
    assert!(
        !work
            .iter()
            .any(|work| matches!(work, Work::WriteInput { .. })),
        "chat keys never reach the terminal"
    );
}

#[gpui::test]
fn attached_files_are_removable_pills_and_can_be_sent_without_text(cx: &mut TestAppContext) {
    let (view, cx, requests, [first, ..]) = two_panes_on_one_session(cx, false);
    let dir = tempfile::tempdir().unwrap();
    let removed = dir.path().join("removed.txt");
    let kept = dir.path().join("my notes.txt");
    std::fs::write(&removed, "remove me").unwrap();
    std::fs::write(&kept, "keep me").unwrap();
    view.update(cx, |model, cx| {
        model.chat_mode(ServerId::local(), ChannelId(1), ChatMode::Chat, 1, cx);
        model.focus_pane(first, cx);
        model.chat_view(first).unwrap().clone().update(cx, |_, cx| {
            crate::views::chat::ChatView::test_attach_paths(vec![removed, kept.clone()], cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(chat_input(&view, cx, first), "");
    let _ = chat_work(&requests, cx);
    let chip = cx
        .debug_bounds(format!("chat-file-{first}-0").leak())
        .expect("file chip");
    cx.simulate_click(chip.center(), Modifiers::none());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        chat_requests(&chat_work(&requests, cx))
            .iter()
            .all(|request| !matches!(request, ChatRequest::Submit { .. })),
        "selecting a file keeps it attached and never sends the draft"
    );
    cx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
    let remove = cx
        .debug_bounds(format!("chat-file-{first}-0-remove").leak())
        .expect("remove button");
    cx.simulate_click(remove.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |model, cx| model
            .chat_view(first)
            .unwrap()
            .read(cx)
            .test_file_names()),
        ["my notes.txt"],
        "only the chosen file is removed"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let work = chat_work(&requests, cx);
    let sends = chat_requests(&work);
    let text = sends
        .iter()
        .find_map(|request| match request {
            ChatRequest::Submit { text, .. } => Some(text),
            _ => None,
        })
        .expect("file-only prompt is sent");
    let split = crate::views::chat::rows::files::prompt(text).unwrap();
    assert_eq!(split.attachments, [kept.to_str().unwrap()]);
    assert!(!text.contains("removed.txt"));
    assert!(
        !work
            .iter()
            .any(|work| matches!(work, Work::WriteInput { .. }))
    );
}

#[gpui::test]
fn pasted_tiff_image_is_attached_as_png(cx: &mut TestAppContext) {
    use muxy_protocol::ChatImageType;
    use std::io::Cursor;
    let (view, cx, _requests, [first, ..]) = two_panes_on_one_session(cx, false);
    let mut tiff = Vec::new();
    image::RgbaImage::new(2, 2)
        .write_to(&mut Cursor::new(&mut tiff), image::ImageFormat::Tiff)
        .unwrap();
    let chat = view.update(cx, |model, cx| {
        model.chat_mode(ServerId::local(), ChannelId(1), ChatMode::Chat, 1, cx);
        let chat = model.chat_view(first).unwrap().clone();
        chat.update(cx, |chat, cx| chat.test_paste_image(tiff, cx));
        chat
    });
    cx.run_until_parked();
    let images = chat.update(cx, |chat, _| chat.test_image(None));
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].mime, ChatImageType::Png);
    assert!(ChatImageType::Png.matches(&images[0].bytes));
    chat.update(cx, |chat, cx| {
        let revision = chat.seed_prompt("first prompt", images.clone(), cx);
        assert_eq!(chat.test_image(None), images);
        chat.clear_seeded_prompt("first prompt", revision, cx);
        assert!(chat.test_image(None).is_empty());
        let revision = chat.seed_prompt("retry", images.clone(), cx);
        chat.seed_prompt("edited while starting", images.clone(), cx);
        chat.clear_seeded_prompt("retry", revision, cx);
        assert_eq!(
            chat.test_image(None),
            images,
            "edits keep their attachments"
        );
    });
}

#[gpui::test]
fn a_new_chat_starts_only_once_its_shell_is_idle(cx: &mut TestAppContext) {
    let (view, cx, requests, [first, ..]) = two_panes_on_one_session(cx, false);
    let foreground = |model: &mut AppModel, is_shell: bool, cx: &mut Context<AppModel>| {
        let terminal = model.grids.get(&first).expect("terminal").view.clone();
        terminal.update(cx, |terminal, _| {
            terminal.process = Some(muxy_protocol::ForegroundProcess {
                name: if is_shell { "zsh" } else { "vim" }.into(),
                is_shell,
            });
        });
    };
    view.update(cx, |model, cx| {
        foreground(model, false, cx);
        model.chat_start_when_idle(first, cx);
        assert!(
            !model.chat_start_if_idle(first, cx),
            "vim is in the foreground"
        );
        assert!(
            model
                .chat_session(first)
                .is_some_and(|session| session.starting)
        );
    });
    cx.run_until_parked();
    let work = chat_work(&requests, cx);
    assert!(
        !chat_requests(&work)
            .iter()
            .any(|request| matches!(request, ChatRequest::Start { .. })),
        "never started over a running program"
    );

    view.update(cx, |model, cx| {
        foreground(model, true, cx);
        assert!(model.chat_start_if_idle(first, cx));
        // It no longer waits, so another check sends nothing.
        assert!(model.chat_start_if_idle(first, cx));
    });
    cx.run_until_parked();
    let work = chat_work(&requests, cx);
    assert!(
        !chat_requests(&work).iter().any(|request| matches!(
            request,
            ChatRequest::Start { .. } | ChatRequest::Begin { .. }
        )),
        "a draft does not launch a conversation"
    );
    assert!(view.read_with(cx, |model, _| {
        model
            .chat_session(first)
            .is_some_and(|session| session.local.is_some())
    }));
}

#[gpui::test]
fn console_while_a_new_chat_waits_to_start_cancels_it(cx: &mut TestAppContext) {
    let (view, cx, requests, [first, ..]) = two_panes_on_one_session(cx, false);
    view.update(cx, |model, cx| {
        let terminal = model.grids.get(&first).expect("terminal").view.clone();
        terminal.update(cx, |terminal, _| {
            terminal.process = Some(muxy_protocol::ForegroundProcess {
                name: "vim".into(),
                is_shell: false,
            });
        });
        model.chat_start_when_idle(first, cx);
        assert!(!model.chat_start_if_idle(first, cx));
        model.toggle_chat(first, cx);
        assert!(
            !model
                .chat_session(first)
                .is_some_and(|session| session.starting),
            "no longer starting"
        );
        // The wait is gone, so an idle shell later starts nothing.
        assert!(model.chat_start_if_idle(first, cx));
    });
    cx.run_until_parked();
    let work = chat_work(&requests, cx);
    assert!(
        chat_requests(&work).is_empty(),
        "{:?}",
        chat_requests(&work)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn chat_never_launches_bundles_scripts_or_folders_it_names() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let at = |name: &str| dir.path().join(name);
    std::fs::write(at("notes.md"), "x").expect("write");
    std::fs::write(at("run.COMMAND"), "x").expect("write");
    std::fs::write(at("tool"), "x").expect("write");
    std::fs::set_permissions(at("tool"), std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::create_dir(at("Evil.app")).expect("mkdir");
    std::os::unix::fs::symlink(at("run.COMMAND"), at("innocent.md")).expect("symlink");

    assert!(!crate::model::chat::runnable(&at("notes.md")));
    for name in ["run.COMMAND", "tool", "Evil.app", "innocent.md", "gone.txt"] {
        assert!(crate::model::chat::runnable(&at(name)), "{name}");
    }
}

#[test]
fn drawing_queues_unknown_paths_once_and_never_for_remote_sessions() {
    use crate::model::chat::{Files, Lookup};
    use muxy_app_core::opener::OpenContext;
    let local = OpenContext {
        project: ProjectId::new(),
        pane: PaneId::new(),
        server: ServerId::local(),
        directory: "/work".into(),
        project_directory: "/work".into(),
    };
    let mut files = Files::default();
    assert_eq!(files.lookup("src/a.rs", Some(&local)), Lookup::Pending);
    assert!(files.take_schedule());
    assert_eq!(files.lookup("src/a.rs", Some(&local)), Lookup::Pending);
    assert!(!files.take_schedule(), "asked once");
    assert_eq!(files.lookup("src/a.rs", None), Lookup::Found(None));
    let remote = OpenContext {
        server: ServerId::new(),
        ..local
    };
    assert_eq!(files.lookup("src/b.rs", Some(&remote)), Lookup::Found(None));
    assert!(!files.take_schedule(), "remote paths aren't on this disk");
}

/// The composer's Chat|Console switch shows both modes as two buttons, and
/// Console there asks first while Claude's session isn't quiet.
#[gpui::test]
fn the_composer_switch_shows_both_modes_and_console_asks_before_leaving(cx: &mut TestAppContext) {
    let (view, cx, requests, [first, ..]) = two_panes_on_one_session(cx, false);
    view.update(cx, |model, cx| {
        model.chat_mode(ServerId::local(), ChannelId(1), ChatMode::Chat, 1, cx);
    });
    cx.run_until_parked();
    let _ = chat_work(&requests, cx);
    let selector = |name: &str| -> &'static str { format!("chat-{name}-{first}").leak() };
    assert!(cx.debug_bounds(selector("show-chat")).is_some());
    let console = cx
        .debug_bounds(selector("show-console"))
        .expect("console button");
    // CLI comes first, and the selected Chat button does nothing.
    let chat_button = cx.debug_bounds(selector("show-chat")).expect("chat button");
    assert!(console.right() <= chat_button.left());
    let chat = cx.debug_bounds(selector("show-chat")).expect("chat button");
    let title = cx
        .debug_bounds(format!("pane-handle-{first}").leak())
        .expect("pane title");
    assert!(
        console.top() >= title.bottom(),
        "mode switch belongs to the composer below the header"
    );
    cx.simulate_click(chat.center(), Modifiers::none());
    cx.run_until_parked();
    assert!(!view.read_with(cx, |model, _| model.dialogs.is_open()));
    cx.simulate_click(console.center(), Modifiers::none());
    cx.run_until_parked();
    assert!(view.read_with(cx, |model, _| model.dialogs.is_open()));
    assert!(chat_requests(&chat_work(&requests, cx)).is_empty());
}

#[gpui::test]
fn changing_the_draft_provider_keeps_text_images_and_excludes_broadcast(cx: &mut TestAppContext) {
    use muxy_protocol::{ChatImage, ChatImageType, ChatProvider};
    let (view, cx, requests, [first, ..]) = two_panes_on_one_session(cx, true);
    let image = ChatImage {
        mime: ChatImageType::Png,
        bytes: b"\x89PNG\r\n\x1a\n".to_vec(),
    };
    let chat = view.update(cx, |model, cx| {
        model.chat_make_draft(first, Some(ChatProvider::Claude), false, cx);
        let chat = model.chat_view(first).unwrap().clone();
        chat.update(cx, |chat, cx| {
            chat.set_input_text("keep this draft", cx);
            chat.test_image(Some(image.clone()));
        });
        chat
    });
    view.update(cx, |model, cx| {
        model.chat_change_provider(first, ChatProvider::Codex, cx);
        assert_eq!(
            model.chat_session(first).unwrap().provider(),
            ChatProvider::Codex
        );
        assert_eq!(model.chat_view(first), Some(&chat));
    });
    cx.run_until_parked();
    assert_eq!(chat_input(&view, cx, first), "keep this draft");
    assert_eq!(
        chat.update(cx, |chat, _| chat.test_image(None)),
        vec![image]
    );
    let work = chat_work(&requests, cx);
    assert!(!chat_requests(&work).iter().any(|request| matches!(
        request,
        ChatRequest::Start { .. } | ChatRequest::Begin { .. } | ChatRequest::Submit { .. }
    )));
    assert!(
        !work
            .iter()
            .any(|work| matches!(work, Work::WriteInput { .. }))
    );
}

#[gpui::test]
fn provider_acknowledgment_preserves_edits_made_while_delivery_is_pending(cx: &mut TestAppContext) {
    use muxy_protocol::{ChatReply, OperationId};
    let (view, cx, requests, [first, ..]) = two_panes_on_one_session(cx, false);
    view.update(cx, |model, cx| {
        model.chat_mode(ServerId::local(), ChannelId(1), ChatMode::Chat, 1, cx);
        model.focus_pane(first, cx);
    });
    cx.run_until_parked();
    let _ = chat_work(&requests, cx);
    cx.simulate_input("frozen prompt");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let preparing = servers::work(&requests);
    let reply = preparing
        .iter()
        .find_map(|work| match work {
            Work::Chat {
                request: ChatRequest::PrepareSubmission { .. },
                reply,
            } => Some(reply),
            _ => None,
        })
        .expect("server-issued operation ticket");
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    });
    cx.simulate_input("edited while pending");
    cx.run_until_parked();
    reply
        .try_send(Ok(ChatReply::Prepared {
            host_epoch: 1,
            operation: OperationId::new(),
        }))
        .expect("prepared reply");
    cx.run_until_parked();
    let sent = chat_work(&requests, cx);
    assert!(
        chat_requests(&sent).iter().any(
            |request| matches!(request,ChatRequest::Submit {text,..} if text=="frozen prompt")
        )
    );
    assert_eq!(chat_input(&view, cx, first), "edited while pending");
}

#[gpui::test]
fn a_rejected_first_prompt_keeps_its_text_but_retries_and_leaves_the_started_provider(
    cx: &mut TestAppContext,
) {
    use muxy_protocol::{ChatProvider, ChatReply, ChatSubmission, SubmissionOutcome};
    let (view, cx, requests, [first, ..]) = two_panes_on_one_session(cx, false);
    view.update(cx, |model, cx| {
        model.chat_make_draft(first, Some(ChatProvider::Codex), false, cx);
        let session = model
            .chat
            .sessions
            .get_mut(&(ServerId::local(), session_id()))
            .unwrap();
        let draft = session.local.as_mut().unwrap();
        draft.discovering = false;
        draft.host_epoch = Some(1);
        model.focus_pane(first, cx);
    });
    cx.run_until_parked();
    let _ = chat_work(&requests, cx);
    cx.simulate_input("first prompt");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let starting = chat_work(&requests, cx);
    let (operation, reply) = starting
        .iter()
        .find_map(|work| match work {
            Work::Chat {
                request: ChatRequest::Begin { operation, .. },
                reply,
            } => Some((*operation, reply)),
            _ => None,
        })
        .expect("first Begin");
    view.update(cx, |model, cx| {
        model.chat_mode(ServerId::local(), ChannelId(1), ChatMode::Chat, 1, cx);
    });
    reply
        .try_send(Ok(ChatReply::Submission(ChatSubmission {
            operation,
            generation: 1,
            outcome: SubmissionOutcome::Rejected,
            detail: Some("Correct the prompt".into()),
        })))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(chat_input(&view, cx, first), "first prompt");
    view.read_with(cx, |model, _| {
        let session = model.chat_session(first).unwrap();
        assert!(session.local.is_none() && session.operation.is_none());
        assert_eq!(session.provider(), ChatProvider::Codex);
    });
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    });
    cx.simulate_input("corrected prompt");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let retry = chat_work(&requests, cx);
    assert!(chat_requests(&retry).iter().any(
        |request| matches!(request,ChatRequest::Submit {text,..} if text=="corrected prompt")
    ));
    assert!(
        !chat_requests(&retry)
            .iter()
            .any(|request| matches!(request, ChatRequest::Begin { .. }))
    );
    view.update(cx, |model, cx| {
        model
            .chat
            .sessions
            .get_mut(&(ServerId::local(), session_id()))
            .unwrap()
            .state
            .quiescent = true;
        model.toggle_chat(first, cx);
    });
    let leaving = chat_work(&requests, cx);
    assert!(chat_requests(&leaving).iter().any(|request| matches!(
        request,
        ChatRequest::LeaveProvider {
            interrupt: false,
            ..
        }
    )));
}
