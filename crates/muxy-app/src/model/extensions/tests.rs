mod api;
mod pages;
mod project_icons;

use super::*;
use crate::model::tests::stub_boot;
use gpui::TestAppContext;
use muxy_app_core::{AppState, TabId};

#[gpui::test]
fn exec_consent_preflight_validates_sync_and_async_commands(cx: &mut TestAppContext) {
    let (boot, _requests) = stub_boot(AppState::bootstrap().expect("state"));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.read_with(cx, |model, _| {
        assert_eq!(model.extensions.next, 0);
        for verb in ["exec", "exec.start"] {
            let (reply, _replies) = std::sync::mpsc::sync_channel(1);
            let mut call = Call {
                owner: "files".into(),
                epoch: model.extensions.epoch_for("files"),
                generation: model.servers.local.generation,
                project: model.state.home().id,
                verb: verb.into(),
                args: Value::Null,
                reply: Reply::Script(1, reply),
                approved: false,
            };
            for args in [
                json!({"argv": ["perl", "-e", "exec @ARGV", "--", "git", "ls-files", "-z"]}),
                json!({
                    "id": "1",
                    "argv": ["sh", "-c", "grep -rnF -f - -- . | head -n 120"],
                    "stdin": "test\n",
                    "timeoutMs": 350,
                }),
                json!({"shell": "git ls-files"}),
                json!({"argv": ["git", "show", "HEAD:src/main.rs"]}),
                json!({"argv": ["git", "show", "HEAD:src/main.rs"], "cwd": "/repo/worktree"}),
            ] {
                call.args = args;
                let expected = Request::for_call(&call.owner, verb, &call.args, "")
                    .expect("exec requires consent");
                assert_eq!(
                    model.consent_request(&call),
                    Ok(Some(expected)),
                    "{verb}: {}",
                    call.args
                );
                assert!(calls::exec_request(&call, 0).is_err());
                let request = calls::exec_request(&call, 42).expect("allocated exec request");
                assert_eq!(request.job, 42);
                assert_eq!(request.project, call.project);
                assert_eq!(
                    request.cwd,
                    call.args["cwd"]
                        .as_str()
                        .map(|path| { muxy_protocol::ServerPath::from(path) })
                );
                assert_eq!(request.env["MUXY_EXTENSION_ID"], "files");
            }
            for args in [
                json!({}),
                json!({"argv": []}),
                json!({"argv": [""]}),
                json!({"argv": ["git"], "shell": "git ls-files"}),
                json!({"argv": ["git", 1]}),
                json!({"argv": ["git"], "env": {"INVALID=KEY": "value"}}),
            ] {
                call.args = args;
                assert!(
                    model.consent_request(&call).is_err(),
                    "{verb} must reject invalid arguments before consent: {}",
                    call.args
                );
            }
            let null = "exec failed to launch: arguments and environment cannot contain null bytes";
            for (args, error) in [
                (json!({"argv": ["git", "\u{0}"]}), null),
                (json!({"shell": "git\u{0}"}), null),
                (json!({"argv": ["git"], "env": {"KEY": "a\u{0}b"}}), null),
                (json!({"argv": ["git"], "env": {"K\u{0}EY": "value"}}), null),
                (
                    json!({"argv": ["git"], "cwd": "/repo\u{0}"}),
                    "exec: cwd cannot contain null bytes",
                ),
            ] {
                call.args = args;
                assert_eq!(
                    model.consent_request(&call),
                    Err(error.into()),
                    "{verb}: {}",
                    call.args
                );
            }
        }
        assert_eq!(model.extensions.next, 0);
        assert!(model.extensions.jobs.is_empty());
    });
}

#[gpui::test]
fn a_finished_script_cancels_its_commands_without_a_frame(cx: &mut TestAppContext) {
    let (boot, _requests) = stub_boot(AppState::bootstrap().expect("state"));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update_in(cx, |model, window, cx| {
        let registry = &mut model.extensions.registry;
        registry.extensions.insert(
            "files".into(),
            muxy_app_core::extensions::Extension {
                name: "files".into(),
                version: "1.0.0".into(),
                directory: std::env::temp_dir(),
                manifest: muxy_app_core::extensions::Manifest::default(),
            },
        );
        registry.set_enabled("files", true).expect("enabled");
        let (reply, _replies) = std::sync::mpsc::sync_channel(4);
        let call = Call {
            owner: "files".into(),
            epoch: model.extensions.epoch_for("files"),
            generation: model.project_generation(model.state.home().id),
            project: model.state.home().id,
            verb: "exec.start".into(),
            args: json!({"id": "1", "argv": ["sleep", "60"]}),
            reply: Reply::Script(7, reply),
            approved: true,
        };
        model.extensions.jobs.insert(
            (call.reply.key(), "1".into()),
            Job {
                id: 1,
                call: call.clone(),
            },
        );
        model.dispatch_extension(
            Call {
                verb: "script.finished".into(),
                args: Value::Null,
                ..call
            },
            window,
            cx,
        );
        assert!(model.extensions.jobs.is_empty());
    });
}

#[gpui::test]
fn rendering_sends_no_extension_events(cx: &mut TestAppContext) {
    use gpui::Render;
    let (boot, _requests) = stub_boot(AppState::bootstrap().expect("state"));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update_in(cx, |model, window, cx| {
        model.flush_extension_events(window, cx);
        let home = model.state.home().id;
        model.state.open_terminal_tab(home).expect("tab");
        model.extensions.events.sent.borrow_mut().clear();
        let _ = model.render(window, cx);
        let _ = model.render(window, cx);
        assert!(model.extensions.events.sent.borrow().is_empty());
        model.flush_extension_events(window, cx);
        let sent = model.extensions.events.sent.take();
        assert_eq!(
            sent.iter().filter(|event| *event == "tab.created").count(),
            1
        );
        model.flush_extension_events(window, cx);
        assert!(model.extensions.events.sent.borrow().is_empty());
    });
}

#[gpui::test]
fn a_terminal_tab_that_cannot_open_leaves_nothing_behind(cx: &mut TestAppContext) {
    let (boot, _requests) = stub_boot(AppState::bootstrap().expect("state"));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    let directory = tempfile::tempdir().expect("directory");
    view.update(cx, |model, cx| {
        let before = model.state.clone();
        let args = json!({"kind": "terminal", "command": "echo hi"});

        let outside = json!({"kind": "terminal", "directory": "../../../../../../../../nope"});
        assert!(model.extension_open_tab(&outside, cx).is_err());
        assert_eq!(model.state, before);

        let path = std::mem::replace(&mut model.path, directory.path().to_path_buf());
        assert_eq!(
            model.extension_open_tab(&args, cx),
            Err("Could not save tabs".to_owned())
        );
        assert_eq!(model.state, before);

        model.path = path;
        let tab = model.extension_open_tab(&args, cx).expect("opened");
        assert_eq!(
            model.active_tab().map(|tab| json!(tab.to_string())),
            Some(tab)
        );
    });
}

#[gpui::test]
fn extensions_split_tabs_into_groups(cx: &mut TestAppContext) {
    let mut state = AppState::bootstrap().expect("state");
    let home = state.home().id;
    let [a, b, c] = [0, 1, 2].map(|index| {
        let tab = state.open_terminal_tab(home).expect("tab");
        let pane = state.window().active_pane.expect("pane");
        state
            .set_pane_session(pane, muxy_protocol::SessionId::new(300 + index))
            .expect("session");
        tab
    });
    state.select_tab(home, a).expect("select");
    let (boot, _requests) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    let split = |tab: TabId, beside: TabId, edge: &str| json!({"tabId": tab.to_string(), "besideId": beside.to_string(), "edge": edge});
    let groups = |model: &AppModel| {
        model.state.home().groups().map_or_else(Vec::new, |groups| {
            groups
                .layout()
                .leaves()
                .into_iter()
                .filter_map(|id| groups.group(id))
                .map(|group| group.tabs().to_vec())
                .collect::<Vec<_>>()
        })
    };
    view.update(cx, |model, cx| {
        assert_eq!(
            model.extension_split_tab(&split(c, a, "right"), cx),
            Ok(Value::Null)
        );
        assert_eq!(groups(model), [vec![a, b], vec![c]]);
        assert_eq!(model.active_tab(), Some(c));

        assert_eq!(
            model.extension_split_tab(&split(b, c, "down"), cx),
            Ok(Value::Null)
        );
        assert_eq!(groups(model), [vec![a], vec![c], vec![b]]);

        let before = model.state.clone();
        assert_eq!(
            model.extension_split_tab(&split(a, c, "diagonal"), cx),
            Err("invalid edge 'diagonal'".into())
        );
        assert_eq!(
            model.extension_split_tab(&json!({"tabId": "nope", "besideId": a.to_string()}), cx),
            Err("tab not found nope".into())
        );
        assert!(model.extension_split_tab(&split(a, a, "left"), cx).is_err());
        assert_eq!(model.state, before);
        assert!(model.error.is_none(), "{:?}", model.error);
    });
}

#[gpui::test]
fn a_call_that_waits_for_the_user_says_so(cx: &mut TestAppContext) {
    let (boot, _requests) = stub_boot(AppState::bootstrap().expect("state"));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update_in(cx, |model, window, cx| {
        let (reply, _replies) = std::sync::mpsc::sync_channel(1);
        let call = Call {
            owner: "tester".into(),
            epoch: model.extensions.epoch_for("tester"),
            generation: model.servers.local.generation,
            project: model.state.home().id,
            verb: "exec".into(),
            args: json!({"argv": ["git", "status"]}),
            reply: Reply::Script(1, reply),
            approved: false,
        };
        let request = model
            .consent_request(&call)
            .expect("valid")
            .expect("exec asks");
        assert_eq!(model.waiting_prompts("tester"), 0);
        model.gate(call, request, window, cx);
        let log: Vec<_> = model.extensions.logs.tail("tester").collect();
        assert!(
            log.iter()
                .any(|line| line.contains("exec is waiting for the user to allow it")),
            "{log:?}"
        );
    });
}

#[gpui::test]
fn folder_calls_never_reach_servers_that_would_ignore_the_folder(cx: &mut TestAppContext) {
    let (boot, _) = stub_boot(AppState::bootstrap().expect("state"));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, _| {
        let (reply, _replies) = std::sync::mpsc::sync_channel(1);
        let call = |args| Call {
            owner: "files".into(),
            epoch: 0,
            generation: model.servers.local.generation,
            project: model.state.home().id,
            verb: "files.write".into(),
            args,
            reply: Reply::Script(1, reply.clone()),
            approved: false,
        };
        let extra = call(json!({"path": "a", "contents": "x", "root": "/srv/shared"}));
        assert!(calls::files_request(&extra, false).is_err());
        let request = calls::files_request(&extra, true).expect("request");
        assert_eq!(
            request.root,
            Some(muxy_protocol::ServerPath(b"/srv/shared".to_vec()))
        );
        let main = call(json!({"path": "a", "contents": "x", "root": null}));
        assert_eq!(
            calls::files_request(&main, false).expect("request").root,
            None
        );
    });
}

#[gpui::test]
fn the_current_project_keeps_its_watches_when_folders_fill_the_cap(cx: &mut TestAppContext) {
    let (boot, _) = stub_boot(AppState::bootstrap().expect("state"));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, _| {
        let busy = tempfile::tempdir().expect("folder");
        let current = tempfile::tempdir().expect("folder");
        let busy = model
            .state
            .add_project(ServerId::local(), busy.path().into())
            .expect("project");
        let current = model
            .state
            .add_project(ServerId::local(), current.path().into())
            .expect("project");
        for (project, parent) in [(busy, "/theirs"), (current, "/mine")] {
            let folders = (0..16)
                .map(|index| format!("{parent}/{index}").into())
                .collect();
            model
                .state
                .set_project_folders(project, folders)
                .expect("folders");
        }
        assert_eq!(model.state.current_project().id, current);
        let keys = model.watch_keys([busy, current].into());
        assert_eq!(keys.len(), 32);
        assert!(keys.contains(&(current, None)));
        assert_eq!(
            keys.iter()
                .filter(|(project, _)| *project == current)
                .count(),
            17
        );
    });
}

#[gpui::test]
fn refused_folder_watches_wait_until_the_folder_is_added_again(cx: &mut TestAppContext) {
    let (boot, _) = stub_boot(AppState::bootstrap().expect("state"));
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, _| {
        let directory = tempfile::tempdir().expect("folder");
        let project = model
            .state
            .add_project(ServerId::local(), directory.path().into())
            .expect("project");
        let folder = std::path::PathBuf::from("/srv/shared");
        let set = |model: &mut AppModel, folders: Vec<std::path::PathBuf>| {
            model
                .state
                .set_project_folders(project, folders)
                .expect("folders");
        };
        set(model, vec![folder.clone()]);
        let key = (project, Some(folder.clone()));
        model.extensions.events.refused.insert(key.clone());
        assert!(!model.wanted_watches([project].into()).contains(&key));
        assert!(!model.wanted_watches([project].into()).contains(&key));
        set(model, Vec::new());
        model.wanted_watches([project].into());
        set(model, vec![folder]);
        assert!(model.wanted_watches([project].into()).contains(&key));
    });
}

#[gpui::test]
fn a_folder_shared_by_a_project_and_its_worktree_is_watched_once(cx: &mut TestAppContext) {
    use std::os::unix::ffi::OsStrExt;
    let directory = tempfile::tempdir().expect("folder");
    let mut state = AppState::bootstrap().expect("state");
    let project = state
        .add_project(ServerId::local(), directory.path().into())
        .expect("project");
    state
        .set_project_folders(project, vec!["/srv/shared".into()])
        .expect("folders");
    let worktree = tempfile::tempdir().expect("folder");
    let mut descriptor = state.project(project).expect("parent").descriptor();
    let child = ProjectId::new();
    descriptor.id = child;
    descriptor.directory = muxy_protocol::ServerPath(worktree.path().as_os_str().as_bytes().into());
    descriptor.parent_id = Some(project);
    descriptor.kind = Some(muxy_app_core::ProjectKind::Worktree);
    descriptor.folders.clear();
    let mut projects: Vec<_> = state
        .projects()
        .iter()
        .map(muxy_app_core::Project::descriptor)
        .collect();
    projects.push(descriptor);
    state
        .apply_catalog(
            ServerId::local(),
            &muxy_protocol::CatalogPage {
                server: muxy_protocol::ServerIdentity::from_u128(1),
                home: state.home().id,
                revision: 1,
                projects,
                next: None,
                legacy_home: None,
            },
        )
        .expect("catalog");
    let (boot, _) = stub_boot(state);
    let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
    view.update(cx, |model, _| {
        let keys = model.watch_keys([project, child].into());
        let shared = keys.iter().filter(|(_, folder)| folder.is_some()).count();
        assert_eq!(shared, 1);
        assert!(keys.contains(&(project, None)) && keys.contains(&(child, None)));
    });
}
