use super::command_center::{listing, session};
use super::*;
use muxy_protocol::{
    ActivityEvent, ActivityKind, ActivitySnapshot, AgentActivity, AgentProvider, AgentState,
};
use muxy_ui::status_menu::Attention;

#[gpui::test]
fn menu_lists_background_and_detached_sessions_without_attaching_them(cx: &mut TestAppContext) {
    let (mut state, first, second, a, b) = projects::two_projects();
    state
        .set_pane_session(a, SessionId::new(41))
        .expect("session");
    state
        .set_pane_session(b, SessionId::new(42))
        .expect("session");
    state
        .set_pane_title(a, "Review the estimate")
        .expect("title");
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
        model.receive_session_page(first, Ok(listing(vec![session(first, 41)])), cx);
        let mut detached = session(second, 43);
        detached.attached = false;
        model.receive_session_page(second, Ok(listing(vec![session(second, 42), detached])), cx);
        let before = model.state.clone();
        requests.try_iter().for_each(drop);
        let mut snapshot = ActivitySnapshot {
            revision: 1,
            agents: vec![AgentActivity {
                session: SessionId::new(42).expect("session"),
                project: second,
                provider: AgentProvider::Codex,
                state: AgentState::Blocked,
            }],
            events: vec![ActivityEvent {
                id: 1,
                session: SessionId::new(43).expect("session"),
                project: second,
                provider: AgentProvider::Codex,
                timestamp: 0,
                kind: ActivityKind::Completed,
                read: false,
            }],
        };
        model.receive_activity(ServerId::local(), Ok(snapshot.clone()), cx);
        let rows = model.status_menu_entries();
        assert_eq!(rows.len(), 3);
        assert!(rows[0].label.contains("Review the estimate"));
        assert_eq!(rows[0].attention, None);
        assert_eq!(rows[1].attention, Some(Attention::Input));
        assert_eq!(rows[2].attention, Some(Attention::Unread));
        assert_eq!(model.state, before);
        assert!(!model.command_center.active);
        assert!(!requests.try_iter().any(|(_, work)| matches!(
            work,
            Work::Attach { .. } | Work::Detach(_) | Work::Resize(..) | Work::Discard(..)
        )));
        model.activity_acknowledged(ServerId::local(), &[1], Ok(()), cx);
        let rows = model.status_menu_entries();
        assert_eq!(
            rows[1].attention,
            Some(Attention::Input),
            "reading must not clear blocked agents"
        );
        assert_eq!(rows[2].attention, None);
        snapshot.revision = 2;
        snapshot.agents[0].state = AgentState::Working;
        snapshot.events.clear();
        model.receive_activity(ServerId::local(), Ok(snapshot), cx);
        assert!(
            model
                .status_menu_entries()
                .iter()
                .all(|row| row.attention.is_none())
        );
    });
}

#[gpui::test]
fn menu_selection_reuses_panes_and_rejects_ended_sessions(cx: &mut TestAppContext) {
    let (mut state, first, second, a, b) = projects::two_projects();
    state
        .set_pane_session(a, SessionId::new(41))
        .expect("session");
    state
        .set_pane_session(b, SessionId::new(42))
        .expect("session");
    state.select_project(first).expect("project");
    let (boot, requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.receive(
            local(
                1,
                Update::Connected(vec![session(first, 41).info, session(second, 42).info]),
            ),
            cx,
        );
        acknowledge_catalog(model, cx);
        model.receive_session_page(first, Ok(listing(vec![session(first, 41)])), cx);
        model.receive_session_page(second, Ok(listing(vec![session(second, 42)])), cx);
        let selected = model.status_menu_entries()[1].id.clone();
        let tabs = model.state.project(second).expect("project").tabs.len();
        model.choose_status_session(&selected, cx);
        assert_eq!(model.state.current_project().id, second);
        assert_eq!(model.active_pane(), Some(b));
        assert_eq!(
            model.state.project(second).expect("project").tabs.len(),
            tabs
        );
        model.select_project(first, cx);
        model.forget_listed_session(ServerId::local(), SessionId::new(42).expect("session"));
        requests.try_iter().for_each(drop);
        let before = model.state.clone();
        model.choose_status_session(&selected, cx);
        model.choose_status_session("bad-session-id", cx);
        assert_eq!(model.state, before);
        assert!(
            !requests
                .try_iter()
                .any(|(_, work)| matches!(work, Work::Attach { .. } | Work::Discard(..)))
        );
        let mut detached = session(second, 43);
        detached.attached = false;
        model.receive_session_page(second, Ok(listing(vec![detached])), cx);
        let detached_id = model.status_menu_entries()[1].id.clone();
        model.choose_status_session(&detached_id, cx);
        assert_eq!(model.state.current_project().id, second);
        assert_eq!(
            model.pane_session(model.active_pane().expect("pane")),
            SessionId::new(43)
        );
        assert_eq!(
            model.state.project(second).expect("project").tabs.len(),
            tabs + 1
        );
    });
}

#[gpui::test]
fn menu_keeps_remote_session_ids_separate_and_drops_disconnected_servers(cx: &mut TestAppContext) {
    let remote = ServerId::new();
    let state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    let (boot, _local_work, _remotes) = remote_boot(state, vec![servers::entry(remote, "box")]);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, cx| {
        model.receive(local(1, Update::Connected(vec![session(home, 1).info])), cx);
        acknowledge_catalog(model, cx);
        let remote_home = servers::connect_remote(model, remote, vec![], &[], cx);
        model.receive_session_page(home, Ok(listing(vec![session(home, 1)])), cx);
        model.receive_session_page(remote_home, Ok(listing(vec![session(remote_home, 1)])), cx);
        model.receive_activity(
            remote,
            Ok(ActivitySnapshot {
                revision: 1,
                agents: vec![AgentActivity {
                    session: SessionId::new(1).expect("session"),
                    project: remote_home,
                    provider: AgentProvider::Codex,
                    state: AgentState::Blocked,
                }],
                ..ActivitySnapshot::default()
            }),
            cx,
        );
        let rows = model.status_menu_entries();
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].id, rows[1].id);
        assert_eq!(rows[0].attention, None);
        assert_eq!(rows[1].attention, Some(Attention::Input));
        assert!(rows[1].label.contains("box"));
        model.receive((remote, 1, Update::Event(ClientEvent::Disconnected)), cx);
        assert_eq!(model.status_menu_entries().len(), 1);
        let before = model.state.clone();
        model.choose_status_session(&rows[1].id, cx);
        assert_eq!(model.state, before);
    });
}
