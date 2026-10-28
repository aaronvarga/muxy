//! Workspace events with main's names and payloads. Every payload value is a
//! string, as on main; optional keys are omitted rather than sent empty.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::os::unix::ffi::OsStrExt;
use std::time::Duration;

use gpui::{Context, Task};
use muxy_app_core::{PaneContent, PaneId, Project, ProjectId, Tab, TabId};
use muxy_protocol::{AgentProvider, AgentState, FilesAction, FilesRequest};
use serde_json::{Map, Value, json};

use super::AppModel;

/// How long a terminal title must hold before events report it, as on main.
const TITLE_DEBOUNCE: Duration = Duration::from_millis(500);

#[derive(Default)]
pub(super) struct Tracker {
    initialized: bool,
    root: Option<ProjectId>,
    current: Option<ProjectId>,
    projects: Value,
    tabs: BTreeMap<String, Value>,
    panes: BTreeMap<String, Value>,
    selected: BTreeMap<String, String>,
    focused: Option<(ProjectId, TabId, PaneId)>,
    /// Terminal titles as events report them.
    titles: HashMap<PaneId, String>,
    /// Newer terminal titles waiting out `TITLE_DEBOUNCE`.
    pending_titles: HashMap<PaneId, (String, Task<()>)>,
    branches: BTreeMap<ProjectId, String>,
    agents: BTreeMap<String, Value>,
    /// Watched project folders; `None` is the project's own.
    pub(super) watched: BTreeSet<WatchKey>,
    /// Folders whose watch the server refused, left alone until they change.
    pub(super) refused: BTreeSet<WatchKey>,
    pub(super) watch_pending: bool,
    watch_epoch: u64,
}

/// Folders one server connection watches, as `muxy-server` caps them.
const MAX_FILE_WATCHES: usize = 32;

pub(super) type WatchKey = (ProjectId, Option<std::path::PathBuf>);

impl Tracker {
    /// Watches end with their server's extension connection.
    pub(super) fn forget_watches(&mut self, ended: impl Fn(ProjectId) -> bool) {
        self.watched.retain(|(project, _)| !ended(*project));
        self.refused.clear();
        self.watch_pending = false;
        self.watch_epoch = self.watch_epoch.wrapping_add(1);
    }
}

/// A payload whose values are all strings, like main's `[String: String]`.
pub(super) fn strings<'a>(pairs: impl IntoIterator<Item = (&'a str, String)>) -> Value {
    Value::Object(
        pairs
            .into_iter()
            .map(|(key, value)| (key.to_owned(), Value::String(value)))
            .collect(),
    )
}

pub(super) fn provider_id(provider: AgentProvider) -> &'static str {
    match provider {
        AgentProvider::Claude => "claude",
        AgentProvider::Codex => "codex",
        AgentProvider::OpenCode => "opencode",
        AgentProvider::Cursor => "cursor",
        AgentProvider::Copilot => "copilot",
        AgentProvider::Droid => "droid",
        AgentProvider::Pi => "pi",
        AgentProvider::Grok => "grok",
        AgentProvider::Kiro => "kiro",
        AgentProvider::Xal => "xal",
        AgentProvider::Antigravity => "antigravity",
        AgentProvider::OhMyPi => "omp",
        AgentProvider::Unrecognized(_) => "other",
    }
}

fn agent_rank(state: AgentState) -> (u8, &'static str) {
    match state {
        AgentState::Working => (2, "working"),
        AgentState::Blocked => (1, "waiting"),
        AgentState::Idle | AgentState::Unknown | AgentState::Unrecognized(_) => (0, "idle"),
    }
}

/// The top-level project that owns a project or worktree.
pub(in crate::model) fn root_of(project: &Project) -> ProjectId {
    project.parent_id.unwrap_or(project.id)
}

impl AppModel {
    fn tab_context(&self, project: &Project, tab: &Tab, cx: &gpui::App) -> Value {
        let pane = tab
            .displayed_pane(self.state.shown_pane(tab))
            .or_else(|| tab.panes.first());
        let mut payload = Map::new();
        let mut put = |key: &str, value: String| {
            payload.insert(key.into(), Value::String(value));
        };
        put("tabID", tab.id.to_string());
        put("projectID", root_of(project).to_string());
        put("worktreeID", project.id.to_string());
        put("areaID", project.id.to_string());
        put("title", self.event_title(tab, cx));
        put("projectPath", project.directory.display().to_string());
        match pane.map(|pane| (pane.id, &pane.content)) {
            Some((id, PaneContent::Webview(descriptor))) => {
                put("kind", "extensionWebView".into());
                put("extensionID", descriptor.owner.clone());
                put("tabTypeID", descriptor.kind.clone());
                put("tabInstanceID", id.to_string());
                if !descriptor.data.is_null() {
                    put("data", descriptor.data.to_string());
                }
            }
            Some((id, PaneContent::Browser(browser))) => {
                put("kind", "browser".into());
                put("paneID", id.to_string());
                let url = self
                    .browser_view(id)
                    .map(|view| view.read(cx).page.url.clone())
                    .or_else(|| browser.url.clone())
                    .unwrap_or_default();
                put("url", url);
            }
            Some((id, PaneContent::Servers)) => {
                put("kind", "remoteServers".into());
                put("paneID", id.to_string());
            }
            Some((id, _)) => {
                put("kind", "terminal".into());
                put("paneID", id.to_string());
                if let Some(directory) = self
                    .terminal(&id)
                    .and_then(|view| view.view.read(cx).directory())
                {
                    put("cwd", directory.display().to_string());
                }
            }
            None => put("kind", "terminal".into()),
        }
        Value::Object(payload)
    }

    /// A tab's title as events report it: a terminal's own title only once it
    /// has settled.
    fn event_title(&self, tab: &Tab, cx: &gpui::App) -> String {
        tab.displayed_pane(self.state.shown_pane(tab))
            .filter(|pane| {
                tab.custom_title.is_none() && matches!(pane.content, PaneContent::Terminal { .. })
            })
            .and_then(|pane| self.extensions.events.titles.get(&pane.id))
            .map_or_else(|| self.webview_title(tab, cx).to_owned(), Clone::clone)
    }

    /// Terminal titles reach events once they have held for
    /// `TITLE_DEBOUNCE`, so a title that keeps changing doesn't flood
    /// `tab.updated`. New panes and the first sync take their titles at once.
    fn debounce_titles(&mut self, cx: &mut Context<Self>) {
        let live: HashMap<PaneId, String> = self
            .state
            .projects()
            .iter()
            .flat_map(|project| &project.tabs)
            .flat_map(|tab| &tab.panes)
            .filter(|pane| matches!(pane.content, PaneContent::Terminal { .. }))
            .map(|pane| (pane.id, pane.title.clone()))
            .collect();
        let events = &mut self.extensions.events;
        events.titles.retain(|pane, _| live.contains_key(pane));
        events
            .pending_titles
            .retain(|pane, _| live.contains_key(pane));
        for (pane, title) in live {
            match events.titles.get(&pane) {
                Some(reported) if *reported == title => {
                    events.pending_titles.remove(&pane);
                }
                Some(_) if events.initialized => {
                    if events
                        .pending_titles
                        .get(&pane)
                        .is_some_and(|(waiting, _)| *waiting == title)
                    {
                        continue;
                    }
                    let settle = cx.spawn(async move |model, cx| {
                        cx.background_executor().timer(TITLE_DEBOUNCE).await;
                        let _ = model.update(cx, |model, cx| {
                            let events = &mut model.extensions.events;
                            if let Some((title, _)) = events.pending_titles.remove(&pane) {
                                events.titles.insert(pane, title);
                                model.sync_extension_events(cx);
                            }
                        });
                    });
                    events.pending_titles.insert(pane, (title, settle));
                }
                _ => {
                    events.titles.insert(pane, title);
                }
            }
        }
    }

    /// Delivers a workspace event to every enabled extension allowed to
    /// subscribe to it; `owner` restricts delivery to one extension.
    pub(in crate::model) fn emit_extension_event(
        &self,
        owner: Option<&str>,
        event: &str,
        payload: &Value,
        cx: &gpui::App,
    ) {
        let permitted = |name: &str| {
            owner.is_none_or(|owner| name == owner)
                && self
                    .extensions
                    .registry
                    .enabled(name)
                    .is_some_and(|extension| {
                        extension.allows_event(event) || self.runtime_command_event(name, event)
                    })
        };
        let source = format!("globalThis.__muxyEvent?.({}, {payload});", json!(event));
        for view in self.extension_views() {
            if permitted(&view.read(cx).source.owner) {
                view.read(cx).native.evaluate(&source);
            }
        }
        for running in self.extensions.scripts.values() {
            if permitted(&running.owner) {
                running.script.evaluate(source.clone());
            }
        }
    }

    /// `extension.*` events stay inside one extension: pages talk to the
    /// background script, and the background script talks to its pages.
    pub(super) fn emit_local_event(
        &self,
        owner: &str,
        from_background: bool,
        event: &str,
        payload: &Value,
        cx: &gpui::App,
    ) -> Result<(), String> {
        if !muxy_app_core::extensions::local_event(event) {
            return Err("extension events must start with extension.".into());
        }
        if !serde_json::to_vec(payload).is_ok_and(|bytes| bytes.len() <= 65_536) {
            return Err("event payload must be JSON-serializable and at most 65536 bytes".into());
        }
        let source = format!("globalThis.__muxyEvent?.({}, {payload});", json!(event));
        if from_background {
            for view in self.extension_views() {
                if view.read(cx).source.owner == owner {
                    view.read(cx).native.evaluate(&source);
                }
            }
            return Ok(());
        }
        let background = self
            .extensions
            .backgrounds
            .get(owner)
            .and_then(|background| self.extensions.scripts.get(&background.script))
            .ok_or("background script unavailable")?;
        background.script.evaluate(source);
        Ok(())
    }

    pub(crate) fn sync_extension_events(&mut self, cx: &mut Context<Self>) {
        self.switch_panel_session(cx);
        self.debounce_titles(cx);
        let silent = !self.extensions.events.initialized;
        let mut emit = |model: &Self, name: &str, payload: Value| {
            if !silent {
                model.emit_extension_event(None, name, &payload, cx);
            }
        };
        let mut tabs = BTreeMap::new();
        let mut panes = BTreeMap::new();
        let mut selected = BTreeMap::new();
        for project in self.state.projects() {
            if let Some(tab) = self.state.window().selected_tab.get(&project.id) {
                selected.insert(project.id.to_string(), tab.to_string());
            }
            for tab in &project.tabs {
                let context = self.tab_context(project, tab, cx);
                for pane in &tab.panes {
                    if matches!(pane.content, PaneContent::Terminal { .. }) {
                        let mut pane_context = context.clone();
                        pane_context["paneID"] = json!(pane.id.to_string());
                        panes.insert(pane.id.to_string(), pane_context);
                    }
                }
                tabs.insert(tab.id.to_string(), context);
            }
        }
        let previous = std::mem::take(&mut self.extensions.events.panes);
        for (id, context) in &panes {
            if !previous.contains_key(id) {
                emit(self, "pane.created", context.clone());
            }
        }
        for (id, context) in &previous {
            if !panes.contains_key(id) {
                emit(self, "pane.closed", context.clone());
            }
        }
        let previous = std::mem::take(&mut self.extensions.events.tabs);
        for (id, context) in &tabs {
            if !previous.contains_key(id) {
                emit(self, "tab.created", context.clone());
            }
        }
        for (id, context) in &previous {
            if !tabs.contains_key(id) {
                emit(self, "tab.closed", context.clone());
            }
        }
        let visible = |context: &Value| {
            ["title", "projectPath", "cwd", "data"].map(|key| context.get(key).cloned())
        };
        for (id, context) in &tabs {
            if previous
                .get(id)
                .is_some_and(|old| visible(old) != visible(context))
            {
                emit(self, "tab.updated", context.clone());
            }
        }
        self.extensions.events.panes = panes;
        self.extensions.events.tabs = tabs;
        self.sync_project_events(&mut emit);
        let previous = std::mem::replace(&mut self.extensions.events.selected, selected);
        for (project, tab) in &self.extensions.events.selected {
            if previous.get(project) != Some(tab) {
                emit(
                    self,
                    "tab.focused",
                    strings([("areaID", project.clone()), ("tabID", tab.clone())]),
                );
            }
        }
        let project = self.state.current_project().id;
        let focused = self
            .active_tab()
            .zip(self.active_pane())
            .map(|(tab, pane)| (project, tab, pane));
        let previous = std::mem::replace(&mut self.extensions.events.focused, focused);
        // Main fires it when focus moves between split areas, so switching
        // tabs or projects alone doesn't count.
        if let (Some((project, tab, pane)), Some((was_project, was_tab, was_pane))) =
            (focused, previous)
            && (project, tab) == (was_project, was_tab)
            && pane != was_pane
        {
            let project = self.state.current_project();
            emit(
                self,
                "pane.focused",
                strings([
                    ("projectID", root_of(project).to_string()),
                    ("worktreeID", project.id.to_string()),
                    ("areaID", project.id.to_string()),
                    ("tabID", tab.to_string()),
                ]),
            );
        }
        self.sync_agent_events(&mut emit);
        self.extensions.events.initialized = true;
        self.sync_extension_file_watches(cx);
    }

    fn sync_project_events(&mut self, emit: &mut impl FnMut(&Self, &str, Value)) {
        let project = self.state.current_project();
        let (root, current) = (root_of(project), project.id);
        if self.extensions.events.root != Some(root) {
            self.extensions.events.root = Some(root);
            emit(
                self,
                "project.switched",
                strings([("projectID", root.to_string())]),
            );
        }
        if self.extensions.events.current != Some(current) {
            self.extensions.events.current = Some(current);
            emit(
                self,
                "worktree.switched",
                strings([
                    ("projectID", root.to_string()),
                    ("worktreeID", current.to_string()),
                ]),
            );
        }
        let listing = Value::Array(
            self.state
                .projects()
                .iter()
                .filter(|project| project.parent_id.is_none())
                .map(|project| {
                    json!([
                        project.id.to_string(),
                        project.name,
                        project.icon,
                        project.color.as_str(),
                        project.logo.as_ref().map(|logo| logo.len()),
                        // A folder's name need not be text, which a path as JSON must be.
                        project.directory.as_os_str().as_encoded_bytes(),
                        project
                            .folders
                            .iter()
                            .map(|folder| folder.as_os_str().as_encoded_bytes())
                            .collect::<Vec<_>>(),
                    ])
                })
                .collect(),
        );
        if self.extensions.events.projects != listing {
            self.extensions.events.projects = listing;
            emit(self, "projects.changed", json!({}));
        }
        let mut branches = BTreeMap::new();
        for project in self.state.projects() {
            let Some(branch) = self
                .git
                .projects
                .get(&project.id)
                .and_then(|repository| repository.summary.as_ref())
                .and_then(|summary| summary.branch.clone())
            else {
                continue;
            };
            if self
                .extensions
                .events
                .branches
                .get(&project.id)
                .is_some_and(|previous| *previous != branch)
            {
                emit(
                    self,
                    "worktree.headChanged",
                    strings([
                        ("projectID", root_of(project).to_string()),
                        ("worktreeID", project.id.to_string()),
                        ("branch", branch.clone()),
                        ("path", project.directory.display().to_string()),
                    ]),
                );
            }
            branches.insert(project.id, branch);
        }
        self.extensions.events.branches = branches;
    }

    /// One entry per worktree: its most active agent, as `agents.list` reports.
    pub(super) fn agent_statuses(&self) -> BTreeMap<String, Value> {
        let mut best: BTreeMap<String, (u8, Value)> = BTreeMap::new();
        let agents = self.servers.iter().flat_map(|(server, runtime)| {
            runtime
                .activity
                .snapshot
                .agents
                .iter()
                .map(move |agent| (server, agent))
        });
        for (server, agent) in agents {
            let Some((project, _, pane)) = self.session_pane(server, agent.session) else {
                continue;
            };
            let (rank, status) = agent_rank(agent.state);
            let payload = strings([
                ("worktreeID", project.id.to_string()),
                ("projectID", root_of(project).to_string()),
                ("paneID", pane.to_string()),
                ("providerID", provider_id(agent.provider).to_owned()),
                ("status", status.to_owned()),
            ]);
            let entry = best
                .entry(project.id.to_string())
                .or_insert((rank, payload.clone()));
            if rank > entry.0 {
                *entry = (rank, payload);
            }
        }
        best.into_iter()
            .map(|(worktree, (_, payload))| (worktree, payload))
            .collect()
    }

    fn sync_agent_events(&mut self, emit: &mut impl FnMut(&Self, &str, Value)) {
        let agents = self.agent_statuses();
        for (worktree, payload) in &agents {
            if self.extensions.events.agents.get(worktree) != Some(payload) {
                emit(self, "agent.status", payload.clone());
            }
        }
        for (worktree, previous) in &self.extensions.events.agents {
            if !agents.contains_key(worktree) && previous["status"] != "idle" {
                let mut idle = previous.clone();
                idle["status"] = json!("idle");
                emit(self, "agent.status", idle);
            }
        }
        self.extensions.events.agents = agents;
    }

    /// Each project's own folder and its extra folders, within the server's cap.
    /// The current project comes first, so it keeps its watches at the cap.
    /// A worktree shares its parent's folders, which are watched once.
    pub(super) fn watch_keys(&self, mut projects: BTreeSet<ProjectId>) -> BTreeSet<WatchKey> {
        let current = self.state.current_project().id;
        let first = projects.remove(&current).then_some(current);
        let mut folders = BTreeSet::new();
        first
            .into_iter()
            .chain(projects)
            .filter_map(|id| self.state.project(id))
            .flat_map(|project| {
                std::iter::once((project.id, None)).chain(
                    self.state
                        .project_folders(project)
                        .iter()
                        .map(|folder| (project.id, Some(folder.clone()))),
                )
            })
            .filter(|(_, folder)| {
                folder
                    .as_ref()
                    .is_none_or(|folder| folders.insert(folder.clone()))
            })
            .take(MAX_FILE_WATCHES)
            .collect()
    }

    /// The watches to hold, leaving out ones the server refused so they are
    /// not retried until their folder leaves and comes back.
    pub(super) fn wanted_watches(&mut self, projects: BTreeSet<ProjectId>) -> BTreeSet<WatchKey> {
        let mut desired = self.watch_keys(projects);
        let refused = &mut self.extensions.events.refused;
        refused.retain(|key| desired.contains(key));
        desired.retain(|key| !refused.contains(key));
        desired
    }

    fn sync_extension_file_watches(&mut self, cx: &mut Context<Self>) {
        if self.extensions.events.watch_pending {
            return;
        }
        let mut desired = BTreeSet::new();
        if self
            .extensions
            .registry
            .active()
            .any(|extension| extension.allows_event("file.changed"))
        {
            desired.insert(self.state.current_project().id);
            for project in self.state.projects() {
                if desired.len() == 32 {
                    break;
                }
                if project.tabs.iter().flat_map(|tab| &tab.panes).any(|pane| matches!(&pane.content, PaneContent::Webview(d) if self.extensions.registry.enabled(&d.owner).is_some_and(|ext| ext.allows_event("file.changed")))) {
                    desired.insert(project.id);
                }
            }
        }
        let client = |model: &Self, project: ProjectId| {
            model
                .state
                .project_server(project)
                .and_then(|server| model.extensions.clients.get(&server))
                .cloned()
        };
        desired.retain(|project| client(self, *project).is_some());
        let desired = self.wanted_watches(desired);
        if desired == self.extensions.events.watched {
            return;
        }
        let removed: Vec<_> = self
            .extensions
            .events
            .watched
            .difference(&desired)
            .cloned()
            .collect();
        let added: Vec<_> = desired
            .difference(&self.extensions.events.watched)
            .cloned()
            .collect();
        let trace = crate::diagnostics::Span::new(
            "extension.watch",
            format_args!("added={added:?} removed={removed:?}"),
        );
        let requests: Vec<_> = removed
            .into_iter()
            .map(|key| (key, FilesAction::Unwatch))
            .chain(added.into_iter().map(|key| (key, FilesAction::Watch)))
            .filter_map(|(key, action)| Some((client(self, key.0)?, key, action)))
            .collect();
        self.extensions.events.watched = desired;
        self.extensions.events.watch_pending = true;
        let epoch = self.extensions.events.watch_epoch;
        let task = cx.spawn(async move |_, _| {
            trace.stage(format_args!("phase=started"));
            let mut errors = Vec::new();
            for (client, key, action) in requests {
                let watch = action == FilesAction::Watch;
                let root = key.1.as_ref().map(|folder| {
                    muxy_protocol::ServerPath(folder.as_os_str().as_encoded_bytes().into())
                });
                if root.is_some() && !client.supports(muxy_protocol::Feature::PROJECT_FOLDERS) {
                    continue;
                }
                let request = FilesRequest {
                    project: key.0,
                    action,
                    root,
                };
                if let Err(error) = client.files_async(request).await {
                    errors.push((watch.then_some(key), error.to_string()));
                }
            }
            trace.stage(format_args!("phase=reply errors={}", errors.len()));
            errors
        });
        cx.spawn(async move |model, cx| {
            let errors = task.await;
            let _ = model.update(cx, |model, cx| {
                if epoch != model.extensions.events.watch_epoch {
                    return;
                }
                model.extensions.events.watch_pending = false;
                for (refused, error) in errors {
                    if let Some(key) = refused {
                        model.extensions.events.watched.remove(&key);
                        model.extensions.events.refused.insert(key);
                    }
                    model.extension_log("", format!("[muxy] file watcher: {error}"));
                }
                model.sync_extension_file_watches(cx);
            });
        })
        .detach();
    }

    /// `root` is the extra folder that changed, or the project's own when `None`.
    fn file_changed(
        &self,
        project: &Project,
        root: Option<&std::path::Path>,
        relative: &str,
        cx: &gpui::App,
    ) {
        let directory = project.directory.display().to_string();
        let root = root.map_or_else(|| directory.clone(), |root| root.display().to_string());
        let path = if relative.is_empty() {
            root.clone()
        } else {
            format!("{}/{relative}", root.trim_end_matches('/'))
        };
        self.emit_extension_event(
            None,
            "file.changed",
            &strings([("path", path), ("projectPath", directory), ("root", root)]),
            cx,
        );
    }

    pub(in crate::model) fn extension_files_changed(
        &self,
        project: ProjectId,
        changes: muxy_protocol::FileChanges,
        cx: &gpui::App,
    ) {
        let Some(project) = self.state.project(project) else {
            return;
        };
        let root = changes
            .root
            .as_ref()
            .map(|root| std::path::Path::new(std::ffi::OsStr::from_bytes(&root.0)))
            .filter(|root| *root != project.directory);
        if changes.rescan {
            // An extra folder is watched once for every project showing it.
            let sharing = self.state.projects().iter().filter(|other| {
                other.id == project.id
                    || root.is_some_and(|root| {
                        self.state
                            .project_folders(other)
                            .iter()
                            .any(|folder| folder == root)
                    })
            });
            let mut paths = BTreeSet::new();
            for (owner, pane) in sharing
                .flat_map(|other| other.tabs.iter().map(move |tab| (other, tab)))
                .flat_map(|(other, tab)| tab.panes.iter().map(move |pane| (other, pane)))
            {
                // Only tabs showing a file from the folder that changed.
                let tab_root = pane_root(&pane.content).filter(|tab| *tab != owner.directory);
                if let PaneContent::Webview(descriptor) = &pane.content
                    && let Some(path) = descriptor.data["filePath"].as_str()
                    && tab_root == root
                    && paths.insert(path)
                {
                    self.file_changed(project, root, path, cx);
                }
            }
            self.file_changed(project, root, "", cx);
        }
        for path in changes.paths {
            if let Ok(path) = muxy_app_core::extensions::api::path_text(&path) {
                self.file_changed(project, root, path, cx);
            }
        }
    }

    /// New AI-hook notifications reach extensions as `notification.posted`,
    /// with the agent provider as their source.
    pub(in crate::model) fn activity_notifications_posted(
        &self,
        server: muxy_app_core::ServerId,
        ids: &[u64],
        cx: &gpui::App,
    ) {
        for event in self
            .activity(server)
            .into_iter()
            .flat_map(|activity| &activity.snapshot.events)
            .filter(|event| ids.contains(&event.id))
        {
            let Some((project, tab, pane)) = self.session_pane(server, event.session) else {
                continue;
            };
            let payload = strings([
                ("paneID", pane.to_string()),
                ("projectID", root_of(project).to_string()),
                ("worktreeID", project.id.to_string()),
                ("worktreePath", project.directory.display().to_string()),
                ("tabID", tab.to_string()),
                ("source", provider_id(event.provider).to_owned()),
                (
                    "title",
                    format!("{} · {}", event.provider.name(), project.name),
                ),
                ("body", event.kind.description().to_owned()),
            ]);
            self.emit_extension_event(None, "notification.posted", &payload, cx);
        }
    }

    /// Repository state lives under `.git`, which main reports as file changes.
    pub(in crate::model) fn extension_git_changed(&self, project: ProjectId, cx: &gpui::App) {
        if let Some(project) = self
            .state
            .project(project)
            .filter(|project| self.extensions.events.watched.contains(&(project.id, None)))
        {
            self.file_changed(project, None, ".git/index", cx);
        }
    }
}

/// The folder an extension tab's file lives in, when it names one.
fn pane_root(content: &PaneContent) -> Option<&std::path::Path> {
    match content {
        PaneContent::Webview(descriptor) => descriptor.data["root"]
            .as_str()
            .filter(|root| !root.is_empty())
            .map(std::path::Path::new),
        _ => None,
    }
}
