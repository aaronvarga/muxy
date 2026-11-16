use gpui::Context;
use muxy_app_core::{
    Direction, PaneId, Project, ProjectStatus, Tab,
    extensions::FileOpener,
    opener::{OpenContext, OpenRequest, Target},
};

use muxy_ui::tr;

use super::AppModel;
use crate::views::{
    menu::{Command, Item},
    terminal::pane::PaneState,
};

impl AppModel {
    pub(super) fn opener_context(&self, pane: PaneId) -> Option<OpenContext> {
        let project = match self.command_center_project(pane) {
            Some(project) => self.state.project(project)?,
            None => self.state.pane_project(pane)?,
        };
        Some(OpenContext {
            project: project.id,
            pane,
            server: project.server_id,
            directory: project.directory.clone(),
            project_directory: project.directory.clone(),
        })
    }

    pub(super) fn open_terminal_link(
        &mut self,
        pane: PaneId,
        target: Target,
        cx: &mut Context<Self>,
    ) {
        let Some(context) = self
            .terminal(&pane)
            .and_then(|pane| pane.view.read(cx).link_context())
        else {
            return;
        };
        if let Target::Url(url) = &target
            && self.browser_enabled()
            && self.settings.browser.open_terminal_links
        {
            if self.command_center.active {
                self.select_project(context.project, cx);
            }
            self.open_url_in_browser(url.clone(), cx);
            return;
        }
        if let Target::File(file) = &target
            && let Some((project, relative)) = self.file_opener_target(&file.path, &context)
            && let Some((owner, opener)) = self.chosen_file_opener(&relative)
        {
            let project = if let Some(project) = project {
                project
            } else {
                let Some(directory) = file.path.parent() else {
                    return;
                };
                let Some(project) =
                    self.add_project_on(context.server, directory.to_owned(), false, cx)
                else {
                    return;
                };
                project
            };
            let mut data = serde_json::json!({
                "filePath": relative,
                "source": "terminal",
                "replaceable": false,
            });
            if let Some(line) = file.line {
                data["line"] = line.into();
            }
            if let Some(column) = file.column {
                data["column"] = column.into();
            }
            let descriptor = muxy_app_core::webview::WebviewDescriptor {
                owner,
                kind: opener.tab_type,
                data,
            };
            if let Err(error) = self.open_webview_tab_in(project, descriptor, opener.singleton, cx)
            {
                self.fail(tr!("Could not open file: %@", &error).to_string(), cx);
            }
            return;
        }
        if let Target::File(file) = &target
            && !context.server.is_local()
        {
            self.remote_link_menu(pane, context.server, file, cx);
            return;
        }
        let settings = self.settings.openers.clone();
        let result = crate::opener::submit(move || {
            crate::opener::open(&OpenRequest { target, context }, &settings)
        });
        match result {
            Ok(result) => cx
                .spawn(async move |model, cx| {
                    if let Ok(Err(error)) = result.recv().await {
                        let _ = model.update(cx, |model, cx| {
                            model.fail(
                                tr!("Could not open link: %@", error.to_string()).to_string(),
                                cx,
                            );
                        });
                    }
                })
                .detach(),
            Err(error) => self.fail(
                tr!("Could not open link: %@", error.to_string()).to_string(),
                cx,
            ),
        }
    }

    /// Use the source project first, then a containing project on the same server.
    /// Otherwise the caller adds the containing folder after choosing an extension handler.
    fn file_opener_target(
        &self,
        path: &std::path::Path,
        context: &OpenContext,
    ) -> Option<(Option<muxy_app_core::ProjectId>, String)> {
        if let Ok(relative) = path.strip_prefix(&context.project_directory) {
            return Some((Some(context.project), relative.to_str()?.to_owned()));
        }
        let project = self
            .state
            .projects()
            .iter()
            .filter(|project| {
                project.server_id == context.server
                    && project.kind != Some(muxy_protocol::ProjectKind::FolderGroup)
                    && path.starts_with(&project.directory)
            })
            .max_by_key(|project| project.directory.components().count());
        let relative = match project {
            Some(project) => path.strip_prefix(&project.directory).ok()?,
            None => std::path::Path::new(path.file_name()?),
        };
        Some((
            project.map(|project| project.id),
            relative.to_str()?.to_owned(),
        ))
    }

    /// Mappings take precedence; unavailable or unsupported extension handlers use the default.
    pub(super) fn chosen_file_opener(&self, relative: &str) -> Option<(String, FileOpener)> {
        let settings = &self.settings.openers;
        let preferred = settings.file_handler(std::path::Path::new(relative));
        for handler in [preferred, settings.file.as_str()] {
            if matches!(
                handler,
                "system.application" | "system.editor" | "system.finder"
            ) {
                return None;
            }
            let Some((owner, id)) = handler.split_once(':') else {
                continue;
            };
            if let Some(opener) = self
                .extensions
                .registry
                .enabled(owner)
                .and_then(|extension| {
                    extension
                        .manifest
                        .file_openers
                        .iter()
                        .find(|opener| opener.id == id && opener.matches(relative))
                })
            {
                return Some((owner.to_owned(), opener.clone()));
            }
        }
        None
    }

    /// Enabled extensions' file openers as `(setting value, label)`.
    pub(crate) fn extension_file_openers(&self) -> Vec<(String, String)> {
        self.extensions
            .registry
            .active()
            .flat_map(|extension| {
                extension.manifest.file_openers.iter().map(|opener| {
                    (
                        format!("{}:{}", extension.name, opener.id),
                        opener.title.as_ref().map_or_else(
                            || extension.name.clone(),
                            |title| format!("{} ({title})", extension.name),
                        ),
                    )
                })
            })
            .collect()
    }

    /// Menu items that move pane `id` out of `tab`: into a new tab, a new
    /// window, or another project.
    fn pane_move_items(&self, id: PaneId, project: &Project, tab: &Tab) -> Vec<Item> {
        let missing = project.status() == ProjectStatus::Missing;
        let alone = tab.panes.len() < 2;
        let mut to_tab = Item::action(
            tr!("Move to New Tab"),
            Command::MovePaneToTab(id, project.id),
        )
        .separated();
        let mut to_window = Item::action(tr!("Move to New Window"), Command::MovePaneToWindow(id));
        if missing || (alone && !tab.detached()) {
            to_tab = to_tab.disabled();
        }
        if missing || (alone && tab.detached() && !tab.floating()) {
            to_window = to_window.disabled();
        }
        // Fork: swap the pane with a neighbor.
        let mut items: Vec<Item> = [
            (tr!("Move Left"), Direction::Left),
            (tr!("Move Right"), Direction::Right),
            (tr!("Move Up"), Direction::Up),
            (tr!("Move Down"), Direction::Down),
        ]
        .into_iter()
        .filter(|(_, direction)| self.state.neighbor(id, *direction).is_some())
        .map(|(label, direction)| Item::action(label, Command::MovePane(id, direction)))
        .collect();
        if let Some(first) = items.first_mut() {
            *first = first.clone().separated();
        }
        if tab.detached() {
            let label = if tab.floating() {
                tr!("Dock as Tab")
            } else {
                tr!("Move to Main Window")
            };
            items.push(
                Item::action(
                    label,
                    Command::Tab(tab.id, crate::views::tab_menu::Action::ReturnToMainWindow),
                )
                .separated(),
            );
        }
        items.extend([to_tab, to_window]);
        items.extend(self.float_items(id, project, tab));
        if !missing {
            items.extend(crate::views::tab_menu::move_to_project(
                &self.move_destinations(project.id),
                |project| Command::MovePaneToTab(id, project),
            ));
        }
        items
    }

    /// Menu items that float pane `id`, or choose which projects the
    /// floating tab it is in shows over.
    fn float_items(&self, id: PaneId, project: &Project, tab: &Tab) -> Vec<Item> {
        let Some(window) = tab.window.as_ref().filter(|window| window.is_floating()) else {
            let mut float = Item::action(tr!("Float Pane"), Command::FloatPane(id));
            if project.status() == ProjectStatus::Missing {
                float = float.disabled();
            }
            return vec![float];
        };
        let (label, scope) = match self.float_scope(window.id) {
            Some(muxy_app_core::FloatScope::AllProjects) => (
                tr!("Show Only in %@", self.project_title(project).as_str()),
                muxy_app_core::FloatScope::Project,
            ),
            _ => (
                tr!("Show in All Projects"),
                muxy_app_core::FloatScope::AllProjects,
            ),
        };
        vec![Item::action(label, Command::FloatScope(window.id, scope))]
    }

    pub(super) fn terminal_menu(
        &self,
        id: PaneId,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.terminal(&id) else {
            return;
        };
        let Some(project) = self.state.pane_project(id) else {
            return;
        };
        let Some(tab) = project
            .tabs
            .iter()
            .find(|tab| tab.panes.iter().any(|pane| pane.id == id))
        else {
            return;
        };
        let pane = pane.view.read(cx);
        let mut copy = Item::action(tr!("Copy"), Command::TerminalCopy(id));
        if pane.selection.is_none() {
            copy = copy.disabled();
        }
        let mut paste = Item::action(tr!("Paste"), Command::TerminalPaste(id));
        if pane.state != PaneState::Live {
            paste = paste.disabled();
        }
        let mut all = Item::action(tr!("Select All"), Command::TerminalSelectAll(id));
        if pane.displayed_grid().is_none() {
            all = all.disabled();
        }
        let mut output = Item::action(
            tr!("Select Command Output"),
            Command::TerminalSelectCommandOutput(id),
        );
        if pane.state != PaneState::Live {
            output = output.disabled();
        }
        let mut detach = Item::action(tr!("Detach Terminal"), Command::DetachTerminal(id));
        if !self.can_detach_terminal(id) {
            detach = detach.disabled();
        }
        let mut split_right =
            Item::action(tr!("Split Right"), Command::SplitPane(id, Direction::Right)).separated();
        let mut split_down =
            Item::action(tr!("Split Down"), Command::SplitPane(id, Direction::Down));
        let mut zoom = Item::action(
            if tab.zoomed == Some(id) {
                tr!("Restore Pane")
            } else {
                tr!("Maximize Pane")
            },
            Command::ToggleZoomPane(id),
        );
        let mut equalize = Item::action(tr!("Equalize Splits"), Command::EqualizeSplits(id));
        let mut close = Item::action(tr!("Close Pane"), Command::ClosePane(id));
        if project.status() == ProjectStatus::Missing {
            split_right = split_right.disabled();
            split_down = split_down.disabled();
            equalize = equalize.disabled();
            close = close.disabled();
        }
        if tab.panes.len() < 2 && tab.zoomed.is_none() {
            zoom = zoom.disabled();
        }
        if tab.panes.len() < 2 {
            equalize = equalize.disabled();
        }
        if tab.pinned && tab.panes.len() == 1 {
            close = close.disabled();
        }
        let mut items = vec![
            copy,
            paste,
            all,
            output,
            split_right,
            split_down,
            zoom,
            equalize,
        ];
        items.extend(self.pane_move_items(id, project, tab));
        items.extend([self.proxy_menu_item(id).separated(), detach, close]);
        let model = cx.entity().downgrade();
        let window = self.window_for_pane(id);
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                let _ = model.update(cx, |model, cx| {
                    if model.grids.contains_key(&id) {
                        model.open_menu(items, position, window, cx);
                    }
                });
            });
        });
    }
}

#[cfg(test)]
mod file_mapping_tests {
    use super::*;
    use gpui::TestAppContext;
    use muxy_app_core::{AppState, ServerId};

    #[gpui::test]
    fn mapped_extensions_respect_support_and_disabled_handler_fallback(cx: &mut TestAppContext) {
        let profile = tempfile::tempdir().unwrap();
        let folder = profile.path().join("extensions/preview");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("index.html"), "<html></html>").unwrap();
        std::fs::write(
            folder.join("package.json"),
            serde_json::json!({
                "name": "preview", "version": "1.0.0",
                "muxy": {
                    "tabTypes": [{"id":"view", "title":"Preview", "entry":"index.html"}],
                    "fileOpeners": [{"id":"markdown", "tabType":"view", "patterns":["*.md"]}],
                    "permissions": ["files:read"]
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            profile.path().join("extension-enabled.json"),
            "[\"preview\"]",
        )
        .unwrap();
        let registry = muxy_app_core::extensions::Registry::load(profile.path());
        assert!(registry.errors.is_empty(), "{:?}", registry.errors);
        let (boot, _) = crate::model::tests::stub_boot(AppState::bootstrap().unwrap());
        let (model, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
        cx.run_until_parked();
        model.update(cx, |model, _| {
            model.extensions.registry = registry;
            model
                .settings
                .openers
                .file_types
                .insert("md".into(), "preview:markdown".into());
            assert_eq!(
                model.chosen_file_opener("docs/README.MD").unwrap().0,
                "preview"
            );
            model
                .settings
                .openers
                .file_types
                .insert("txt".into(), "preview:markdown".into());
            assert!(model.chosen_file_opener("notes.txt").is_none());
            model.settings.openers.file = "preview:markdown".into();
            model
                .settings
                .openers
                .file_types
                .insert("md".into(), "missing:view".into());
            assert!(model.chosen_file_opener("README.md").is_some());
            model
                .settings
                .openers
                .file_types
                .insert("md".into(), "system.application".into());
            assert!(model.chosen_file_opener("README.md").is_none());
            model.settings.openers.file_types.clear();
            model
                .extensions
                .registry
                .set_enabled("preview", false)
                .unwrap();
            assert!(model.chosen_file_opener("README.md").is_none());
        });
    }

    #[gpui::test]
    fn outside_files_use_their_containing_project_without_crossing_servers(
        cx: &mut TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let notes = root.path().join("notes");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&notes).unwrap();
        let mut state = AppState::bootstrap().unwrap();
        let home = state.home().id;
        let home_directory = state.home().directory.clone();
        state.add_folder_group("Folders").unwrap();
        let source_project = state
            .add_project(ServerId::local(), source.clone())
            .unwrap();
        let notes_project = state.add_project(ServerId::local(), notes.clone()).unwrap();
        let (boot, _) = crate::model::tests::stub_boot(state);
        let (model, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
        model.update(cx, |model, _| {
            let mut context = OpenContext {
                project: source_project,
                pane: PaneId::new(),
                server: ServerId::local(),
                directory: source.clone(),
                project_directory: source.clone(),
            };
            assert_eq!(
                model.file_opener_target(&source.join("README.md"), &context),
                Some((Some(source_project), "README.md".into()))
            );
            assert_eq!(
                model.file_opener_target(&notes.join("talk.md"), &context),
                Some((Some(notes_project), "talk.md".into()))
            );
            assert_eq!(
                model.file_opener_target(&root.path().join("outside.md"), &context),
                Some((None, "outside.md".into()))
            );
            assert_eq!(
                model.file_opener_target(&home_directory.join("Desktop/notes.md"), &context),
                Some((Some(home), "Desktop/notes.md".into())),
                "a collection does not grant access to its directory"
            );
            context.server = ServerId::new();
            assert_eq!(
                model.file_opener_target(&notes.join("talk.md"), &context),
                Some((None, "talk.md".into()))
            );
        });
    }
}
