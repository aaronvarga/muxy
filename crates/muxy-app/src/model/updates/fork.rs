//! Upstream releases prompt a source rebase, independently of binary installation.
#[cfg(not(test))]
use std::io::Read;
use std::io::Write;
use std::time::Duration;

use gpui::Context;
use muxy_core::release::Version;
use muxy_ui::tr;
use serde::Deserialize;

use super::super::{AppModel, OpenTerminal, Quitting};

#[cfg(test)]
pub(super) type Lookup = fn() -> Result<Option<String>, String>;

const INTERVAL: Duration = Duration::from_secs(3600);
// Fork builds keep a placeholder package version; this is their upstream base.
#[cfg(not(test))]
const BASE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../BETA_VERSION"));
#[cfg(not(test))]
const ENDPOINT: &str = "https://api.github.com/repos/muxy-app/muxy/releases/latest";
#[cfg(not(test))]
const MAX_RESPONSE: u64 = 256 * 1024;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

fn newer_release(bytes: &[u8], base: &str) -> Result<Option<String>, String> {
    let release: Release = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let version = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    let Some(version @ Version::Stable(_)) = Version::parse(version) else {
        return Ok(None);
    };
    Ok((!release.draft
        && !release.prerelease
        && Version::parse(base.trim()).is_some_and(|base| version.is_newer_than(base)))
    .then_some(release.tag_name))
}

#[cfg(not(test))]
fn latest_release() -> Result<Option<String>, String> {
    let client = crate::proxy::configure(
        reqwest::blocking::Client::builder()
            .user_agent(concat!("Muxy-Fork/", env!("CARGO_PKG_VERSION")))
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20)),
    )
    .and_then(reqwest::blocking::ClientBuilder::build)
    .map_err(|e| e.to_string())?;
    let response = client
        .get(ENDPOINT)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err("Upstream release response is too large".into());
    }
    newer_release(&bytes, BASE)
}

fn command(tag: &str) -> String {
    let prompt = format!(
        "Use $muxy-update to rebase my personal Muxy fork onto upstream release {tag} from https://github.com/muxy-app/muxy/releases/tag/{tag}. Follow AGENTS.md and the muxy-update skill. Preserve all fork customizations, commits, and dirty or untracked work. Keep the installed app, running server, and terminal sessions untouched. Validate the result and report the release, backup ref, checks, and remaining limitations."
    );
    format!("cx {}", crate::extensions::authoring::shell_quote(&prompt))
}

impl AppModel {
    fn fork_project(&self) -> Option<&muxy_app_core::Project> {
        let mut projects = self.state.projects().iter().filter(|project| {
            !project.home
                && project.server_id.is_local()
                && project.parent_id.is_none()
                && project.kind != Some(muxy_protocol::ProjectKind::FolderGroup)
                && project.name.eq_ignore_ascii_case("muxy")
        });
        let project = projects.next()?;
        projects.next().is_none().then_some(project)
    }

    pub(crate) fn fork_update_release(&self) -> Option<&str> {
        self.fork_project()?;
        self.updates.fork_release.as_deref()
    }

    pub(super) fn start_fork_release_checks(&mut self, cx: &mut Context<Self>) {
        if self.updates.fork_poll.is_some() {
            return;
        }
        self.check_fork_release(cx);
        self.updates.fork_poll = Some(cx.spawn(async move |model, cx| {
            loop {
                cx.background_executor().timer(INTERVAL).await;
                if model.update(cx, Self::check_fork_release).is_err() {
                    break;
                }
            }
        }));
    }

    fn check_fork_release(&mut self, cx: &mut Context<Self>) {
        if self.quitting != Quitting::Idle
            || self.fork_project().is_none()
            || self.updates.fork_check.is_some()
        {
            return;
        }
        #[cfg(not(test))]
        let lookup = latest_release;
        #[cfg(test)]
        let lookup = self.updates.fork_lookup.unwrap_or(|| Ok(None));
        self.updates.fork_check = Some(cx.spawn(async move |model, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { lookup() })
                .await;
            let _ = model.update(cx, |model, cx| {
                model.updates.fork_check = None;
                match result {
                    Ok(release) => {
                        model.updates.fork_release = release;
                        cx.notify();
                    }
                    Err(error) => {
                        let _ = writeln!(
                            std::io::stderr(),
                            "Could not check upstream Muxy releases: {error}"
                        );
                    }
                }
            });
        }));
    }

    pub(crate) fn rebase_fork_update(&mut self, cx: &mut Context<Self>) {
        if self.quitting != Quitting::Idle {
            return;
        }
        let Some(tag) = self.fork_update_release().map(str::to_owned) else {
            return;
        };
        let Some(project) = self.fork_project() else {
            return;
        };
        let id = project.id;
        let directory = project.directory.clone();
        if let Some(pane) = self
            .updates
            .fork_session
            .filter(|pane| self.state.pane_project(*pane).is_some())
        {
            self.select_project(id, cx);
            if let Some(tab) = self.state.pane_tab(pane) {
                self.select_tab(tab, cx);
            }
            self.focus_pane(pane, cx);
            return;
        }
        let Some(project) = self.state.project(id) else {
            return;
        };
        if project.status() == muxy_app_core::ProjectStatus::Missing {
            self.show_notice(
                tr!("Choose an existing folder for the muxy project before updating the fork.")
                    .to_string(),
                cx,
            );
            return;
        }
        let title = format!("Update Muxy to {tag}");
        match self.open_terminal(
            OpenTerminal {
                project: id,
                title: Some(&title),
                command: Some(&command(&tag)),
                directory: Some(&directory),
            },
            cx,
        ) {
            Ok(Some((_, pane))) => self.updates.fork_session = Some(pane),
            Ok(None) => {}
            Err(error) => self.show_notice(error, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::stub_boot;
    use gpui::TestAppContext;
    use muxy_app_core::{AppState, ServerId};

    #[test]
    fn only_newer_published_stable_tags_can_enter_the_command() {
        for (tag, draft, prerelease, expected) in [
            ("v2.6.0", false, false, true),
            ("2.10.0", false, false, true),
            ("v2.5.0", false, false, false),
            ("v2.4.9", false, false, false),
            ("v2.6.0-beta.1", false, true, false),
            ("v2.6.0", true, false, false),
            ("v2.6.0", false, true, false),
            ("v2.6.0'; touch /tmp/injected", false, false, false),
        ] {
            let bytes = serde_json::to_vec(
                &serde_json::json!({"tag_name":tag,"draft":draft,"prerelease":prerelease}),
            )
            .unwrap();
            assert_eq!(
                newer_release(&bytes, "2.5.0").unwrap(),
                expected.then(|| tag.to_owned())
            );
        }
        assert!(newer_release(b"not JSON", "2.5.0").is_err());
        let command = command("v2.6.0");
        assert!(command.starts_with("cx "));
        assert!(command.contains("$muxy-update"));
        assert!(command.contains("onto upstream release v2.6.0"));
        assert!(!command.chars().any(char::is_control));
    }

    #[gpui::test]
    fn a_remote_muxy_project_never_redirects_the_update_into_an_unrelated_local_project(
        cx: &mut TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let mut remote_state = AppState::bootstrap().unwrap();
        let remote = remote_state
            .add_project(ServerId::local(), root.path().to_owned())
            .unwrap();
        remote_state.rename_project(remote, "muxy").unwrap();
        let mut state = AppState::bootstrap().unwrap();
        state
            .apply_catalog(
                ServerId::new(),
                &muxy_protocol::CatalogPage {
                    server: muxy_protocol::ServerIdentity::from_u128(2),
                    home: remote_state.home().id,
                    revision: 1,
                    projects: remote_state
                        .projects()
                        .iter()
                        .map(muxy_app_core::Project::descriptor)
                        .collect(),
                    next: None,
                    legacy_home: None,
                },
            )
            .unwrap();
        let (boot, _requests) = stub_boot(state);
        let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
        view.update(cx, |model, cx| {
            model.updates.fork_release = Some("v2.6.0".into());
            assert!(model.fork_update_release().is_none());
            model.rebase_fork_update(cx);
            assert!(
                model
                    .state
                    .projects()
                    .iter()
                    .all(|project| project.tabs.is_empty())
            );
        });
    }

    #[gpui::test]
    fn hourly_checks_offer_an_update_without_launching_and_clicks_reuse_the_tab(
        cx: &mut TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let mut state = AppState::bootstrap().unwrap();
        let project = state
            .add_project(ServerId::local(), root.path().to_owned())
            .unwrap();
        state.rename_project(project, "muxy").unwrap();
        let home = state.home().id;
        state.select_project(home).unwrap();
        let (boot, _requests) = stub_boot(state);
        let (view, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
        cx.run_until_parked();
        view.update(cx, |model, _| {
            model.updates.fork_lookup = Some(|| Ok(Some("v2.6.0".into())));
        });
        cx.executor().advance_clock(INTERVAL);
        cx.run_until_parked();
        view.read_with(cx, |model, _| {
            assert_eq!(model.fork_update_release(), Some("v2.6.0"));
            assert!(
                model.state.project(project).unwrap().tabs.is_empty(),
                "polling never launches Codex"
            );
        });
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        let button = cx.debug_bounds("fork-update-button").expect("update icon");
        cx.simulate_click(button.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        view.update(cx, |model, cx| {
            assert_eq!(model.state.current_project().id, project);
            let pane = model.updates.fork_session.unwrap();
            assert_eq!(
                model.state.startup_command(pane),
                Some(command("v2.6.0").as_str())
            );
            model.select_project(home, cx);
            model.rebase_fork_update(cx);
            assert_eq!(model.state.current_project().id, project);
            assert_eq!(
                model.state.project(project).unwrap().tabs.len(),
                1,
                "avoid concurrent rebases"
            );
            model.updates.fork_lookup = Some(|| Err("offline".into()));
        });
        cx.executor().advance_clock(INTERVAL);
        cx.run_until_parked();
        view.read_with(cx, |model, _| {
            assert_eq!(model.fork_update_release(), Some("v2.6.0"));
        });
    }
}
