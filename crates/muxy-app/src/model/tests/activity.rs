use super::*;
use muxy_core::capability::{Capability, Feature};
use muxy_protocol::{ActivityEvent, ActivityKind, ActivitySnapshot, AgentProvider};
use muxy_ui::notifications::SimulatedNotifications;

fn simulate_notifications(
    model: &mut AppModel,
    cx: &mut Context<AppModel>,
) -> SimulatedNotifications {
    let (notifications, opened, simulated) = SimulatedNotifications::start();
    model
        .capabilities
        .set(Feature::Notifications, notifications.capability());
    model.notifications = notifications;
    AppModel::follow_notifications(opened, cx);
    simulated
}

fn completed(id: u64, session: SessionId, project: ProjectId) -> ActivityEvent {
    ActivityEvent {
        id,
        session,
        project,
        provider: AgentProvider::Codex,
        kind: ActivityKind::Completed,
        read: false,
        timestamp: 0,
    }
}

fn claims(requests: &std::sync::mpsc::Receiver<(u64, Work)>) -> Vec<Vec<u64>> {
    requests
        .try_iter()
        .filter_map(|(_, work)| match work {
            Work::ClaimActivity(ids) => Some(ids),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn notification_snooze_suppresses_both_delivery_paths_and_replaces_its_timer(
    cx: &mut TestAppContext,
) {
    let (boot, _) = stub_boot(AppState::bootstrap().expect("state"));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    let simulated = view.update(cx, |model, cx| {
        let simulated = simulate_notifications(model, cx);
        model.notifications.inform("before", "Title", "Body");
        model.snooze_notifications(Some(Duration::from_secs(30 * 60)), cx);
        model
            .notifications
            .deliver("muted-agent", "Title", "Body", None);
        model
            .notifications
            .inform("muted-extension", "Title", "Body");
        assert_eq!(model.notifications.capability(), Capability::Supported);
        assert_eq!(simulated.visible(), vec!["before"]);
        model.notifications.clear(&["before".into()]);
        assert!(simulated.visible().is_empty());
        simulated
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(10 * 60));
    view.update(cx, |model, cx| {
        model.snooze_notifications(Some(Duration::from_secs(60 * 60)), cx);
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(20 * 60));
    cx.run_until_parked();
    view.update(cx, |model, _| {
        assert!(model.notifications_snoozed(), "the old timer was cancelled");
    });
    cx.executor().advance_clock(Duration::from_secs(40 * 60));
    cx.run_until_parked();
    view.update(cx, |model, cx| {
        assert!(!model.notifications_snoozed());
        assert!(simulated.visible().is_empty(), "no replay after snoozing");
        model
            .notifications
            .deliver("resumed-agent", "Title", "Body", None);
        model
            .notifications
            .inform("resumed-extension", "Title", "Body");
        assert_eq!(
            simulated.visible(),
            vec!["resumed-agent", "resumed-extension"]
        );
        model.snooze_notifications(Some(Duration::from_secs(30 * 60)), cx);
        model.snooze_notifications(None, cx);
        assert!(!model.notifications_snoozed());
        model.notifications.inform("manual-resume", "Title", "Body");
        assert_eq!(simulated.visible().last().unwrap(), "manual-resume");
    });
}

#[gpui::test]
fn notification_snooze_toolbar_menu_snoozes_and_resumes(cx: &mut TestAppContext) {
    let (boot, _) = stub_boot(AppState::bootstrap().expect("state"));
    cx.update(|cx| crate::views::workspace::bind_keys(&boot.settings.keymap, cx));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds("snooze-notifications")
            .expect("snooze button")
            .left(),
        cx.debug_bounds("titlebar-actions").expect("toolbar").left(),
        "notifications lead the toolbar"
    );
    let click = |cx: &mut VisualTestContext| {
        cx.run_until_parked();
        let button = cx
            .debug_bounds("snooze-notifications")
            .expect("snooze button")
            .center();
        cx.simulate_mouse_down(button, gpui::MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(button, gpui::MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
    };
    click(cx);
    cx.simulate_keystrokes("down enter");
    view.read_with(cx, |model, _| assert!(model.notifications_snoozed()));
    click(cx);
    cx.simulate_keystrokes("down down down enter");
    view.read_with(cx, |model, _| assert!(!model.notifications_snoozed()));
}

#[gpui::test]
fn activity_notifications_identify_the_project_session_and_artwork(cx: &mut TestAppContext) {
    let mut state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    state.open_terminal_tab(home).expect("tab");
    let pane = state.window().active_pane.expect("pane");
    let session = SessionId::new(42).expect("session");
    state
        .set_pane_session(pane, Some(session))
        .expect("session");
    state
        .set_pane_title(pane, "Review the estimate")
        .expect("title");
    state
        .set_project_icon(home, Some("🚀".into()))
        .expect("icon");
    let (boot, _) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, _| {
        let event = completed(1, session, home);
        let (title, body, icon) = model.activity_notification_content(ServerId::local(), &event);
        assert!(title.contains("Codex"));
        assert!(title.contains(&model.state.home().name));
        assert!(body.starts_with("Review the estimate\n"));
        assert_eq!(body.lines().count(), 2);
        assert_eq!(icon.expect("project icon").glyph, "🚀");
        model.state.set_pane_title(pane, "").expect("empty title");
        let (_, body, _) = model.activity_notification_content(ServerId::local(), &event);
        assert!(body.starts_with("Terminal 42\n"));
    });
}

#[gpui::test]
fn alerts_are_claimed_shown_and_cleared_only_with_a_notification_service(cx: &mut TestAppContext) {
    let mut state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    state.open_terminal_tab(home).expect("tab");
    let pane = state.window().active_pane.expect("pane");
    let session = SessionId::new(42).expect("session");
    let (boot, requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    let simulated = view.update(cx, |model, cx| {
        model.servers.local.connection = ConnectionState::Ready;
        model.window_active = false;
        model.receive_attached(ServerId::local(), pane, session, attachment(), true, cx);
        model.servers.local.activity.loaded = true;
        model
            .capabilities
            .set(Feature::Notifications, Capability::Unsupported);
        let mut snapshot = ActivitySnapshot {
            events: vec![completed(1, session, home)],
            ..ActivitySnapshot::default()
        };
        model.receive_activity(ServerId::local(), Ok(snapshot.clone()), cx);
        assert!(
            claims(&requests).is_empty(),
            "an app that cannot notify leaves the alert to another client"
        );
        assert_eq!(model.servers.local.activity.snapshot.events.len(), 1);

        let simulated = simulate_notifications(model, cx);
        snapshot.events.push(completed(2, session, home));
        model.receive_activity(ServerId::local(), Ok(snapshot), cx);
        assert_eq!(claims(&requests), vec![vec![2]]);
        model.deliver_activity(ServerId::local(), Ok(vec![2]));
        assert_eq!(simulated.visible(), vec!["2".to_owned()]);
        simulated
    });
    simulated.open("2");
    cx.run_until_parked();
    view.update(cx, |model, cx| {
        assert!(
            requests
                .try_iter()
                .any(|(_, work)| matches!(work, Work::AcknowledgeActivity(ids) if ids == vec![2])),
            "opening a notification reads its alert"
        );
        model.activity_acknowledged(ServerId::local(), &[2], Ok(()), cx);
        assert!(simulated.visible().is_empty());
    });
}

#[gpui::test]
fn detached_sessions_do_not_claim_alerts_or_reopen_from_notification_clicks(
    cx: &mut TestAppContext,
) {
    let mut state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    state.open_terminal_tab(home).expect("tab");
    let pane = state.window().active_pane.expect("pane");
    let session = SessionId::new(42).expect("session");
    let sibling = state.split_pane(pane, Direction::Right).expect("split");
    let (boot, requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        simulate_notifications(model, cx);
        model.servers.local.connection = ConnectionState::Ready;
        model.window_active = false;
        assert!(model.servers.local.activity.pane_sessions.is_empty());
        model.receive_attached(ServerId::local(), pane, session, attachment(), true, cx);
        model.receive_attached(ServerId::local(), sibling, session, attachment(), false, cx);
        model.servers.local.activity.loaded = true;
        let mut snapshot = ActivitySnapshot {
            events: [42, 99]
                .into_iter()
                .map(|id| ActivityEvent {
                    id,
                    session: SessionId::new(id).expect("session"),
                    project: home,
                    provider: AgentProvider::Codex,
                    kind: ActivityKind::Completed,
                    read: false,
                    timestamp: 0,
                })
                .collect(),
            ..ActivitySnapshot::default()
        };
        model.receive_activity(ServerId::local(), Ok(snapshot.clone()), cx);
        assert_eq!(
            model.servers.local.activity.pane_sessions,
            vec![session],
            "track ownership before delivering alerts"
        );
        let claims: Vec<_> = requests
            .try_iter()
            .filter_map(|(_, work)| match work {
                Work::ClaimActivity(ids) => Some(ids),
                _ => None,
            })
            .collect();
        assert_eq!(
            claims,
            vec![vec![42]],
            "only an open pane can claim an alert"
        );
        model.detach_terminal(pane, cx);
        snapshot.events[0].id = 100;
        model.receive_activity(ServerId::local(), Ok(snapshot.clone()), cx);
        assert!(
            requests
                .try_iter()
                .any(|(_, work)| matches!(work, Work::ClaimActivity(ids) if ids == vec![100])),
            "the remaining pane still owns the session's alerts"
        );
        model.detach_terminal(sibling, cx);
        assert!(model.state.home().tabs.is_empty());
        model.navigate_activity(ServerId::local(), 100, cx);
        assert!(
            model.state.home().tabs.is_empty(),
            "stale clicks cannot reopen detached panes"
        );
        snapshot.events[0].id = 101;
        model.receive_activity(ServerId::local(), Ok(snapshot), cx);
        assert!(
            !requests
                .try_iter()
                .any(|(_, work)| matches!(work, Work::ClaimActivity(_)))
        );
    });
}

#[gpui::test]
fn alerts_are_left_to_another_client_while_the_system_refuses_notifications(
    cx: &mut TestAppContext,
) {
    let mut state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    state.open_terminal_tab(home).expect("tab");
    let pane = state.window().active_pane.expect("pane");
    let session = SessionId::new(42).expect("session");
    let (boot, requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    let simulated = view.update(cx, |model, cx| {
        model.servers.local.connection = ConnectionState::Ready;
        model.window_active = false;
        model.receive_attached(ServerId::local(), pane, session, attachment(), true, cx);
        model.servers.local.activity.loaded = true;
        simulate_notifications(model, cx)
    });
    simulated.report(Capability::Unavailable("turned off".into()));
    cx.run_until_parked();
    let mut snapshot = ActivitySnapshot {
        events: vec![completed(1, session, home)],
        ..ActivitySnapshot::default()
    };
    view.update(cx, |model, cx| {
        assert_eq!(
            model.capabilities.reason(Feature::Notifications).as_deref(),
            Some("turned off")
        );
        let _ = requests.try_iter().count();
        model.receive_activity(ServerId::local(), Ok(snapshot.clone()), cx);
        assert!(
            claims(&requests).is_empty(),
            "a client that may not notify leaves the alert to another"
        );
    });
    simulated.report(Capability::Supported);
    cx.run_until_parked();
    view.update(cx, |model, cx| {
        snapshot.events.push(completed(2, session, home));
        model.receive_activity(ServerId::local(), Ok(snapshot), cx);
        assert_eq!(claims(&requests), vec![vec![2]]);
    });
}

#[gpui::test]
fn opening_an_alert_raises_the_window_only_for_a_terminal_it_shows(cx: &mut TestAppContext) {
    let mut state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    state.open_terminal_tab(home).expect("tab");
    let pane = state.window().active_pane.expect("pane");
    let session = SessionId::new(42).expect("session");
    let (boot, _requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    let simulated = view.update(cx, |model, cx| {
        model.servers.local.connection = ConnectionState::Ready;
        model.receive_attached(ServerId::local(), pane, session, attachment(), true, cx);
        model.servers.local.activity.loaded = true;
        let simulated = simulate_notifications(model, cx);
        let snapshot = ActivitySnapshot {
            events: vec![completed(1, session, home)],
            ..ActivitySnapshot::default()
        };
        model.receive_activity(ServerId::local(), Ok(snapshot), cx);
        simulated
    });
    let in_front = |cx: &mut VisualTestContext| cx.update(|window, _| window.is_window_active());
    cx.deactivate_window();
    cx.run_until_parked();
    assert!(!in_front(cx));

    simulated.open("999");
    cx.run_until_parked();
    assert!(
        !in_front(cx),
        "an alert with no terminal in this window leaves it where it is"
    );

    simulated.open("1");
    cx.run_until_parked();
    assert!(in_front(cx));
}
