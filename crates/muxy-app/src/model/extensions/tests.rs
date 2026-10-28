use super::*;
use crate::model::tests::stub_boot;
use gpui::TestAppContext;
use muxy_app_core::AppState;

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
                        .map(|path| { muxy_protocol::ServerPath(path.as_bytes().to_vec()) })
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
