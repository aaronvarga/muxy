use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use gpui::{
    Context, Div, ParentElement, Stateful, StatefulInteractiveElement, Styled, Window, canvas,
    point, px,
};
use muxy_app_core::ProjectId;
use muxy_protocol::ServerPath;
use muxy_ui::popover::PopoverAnchor;
use muxy_ui::tr;

use super::{folder_group_items, folder_key};
use crate::model::AppModel;
use crate::views::menu::{Command, Item, Menu};
use crate::views::overlays::Overlay;

fn local_subfolders(directory: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut folders = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if entry.path().is_dir() {
            folders.push(entry.path());
            if folders.len() == muxy_protocol::MAX_FILE_ENTRIES {
                break;
            }
        }
    }
    folders.sort_by_cached_key(|path| (path.to_string_lossy().to_lowercase(), path.clone()));
    Ok(folders)
}

/// Opens the folder group's menu when `row` is hovered. The row stays usable under
/// the menu, so it can still be dragged or right-clicked.
pub(crate) fn hover_opens_menu(
    row: Stateful<Div>,
    id: ProjectId,
    cx: &mut Context<AppModel>,
) -> Stateful<Div> {
    let anchor = PopoverAnchor::default();
    let probe = anchor.clone();
    row.child(
        canvas(
            move |bounds, _, _| probe.set(Some(bounds)),
            |_, (), _, _| {},
        )
        .absolute()
        .inset_0(),
    )
    .on_hover(cx.listener(move |model, hovered: &bool, window, cx| {
        if *hovered && !cx.has_active_drag() {
            model.open_folder_group_menu(id, Some(anchor.clone()), window, cx);
        }
    }))
}

/// Lists `directory`'s folders off the main thread, on its server when remote.
fn list_subfolders(
    local: bool,
    client: Option<muxy_client::Client>,
    directory: PathBuf,
    cx: &Context<AppModel>,
) -> gpui::Task<Result<Vec<PathBuf>, String>> {
    cx.background_executor().spawn(async move {
        if local {
            local_subfolders(&directory).map_err(|error| error.to_string())
        } else {
            let client =
                client.ok_or_else(|| tr!("Connect to the server to list folders.").to_string())?;
            client
                .list_folders(ServerPath(directory.as_os_str().as_bytes().to_vec()))
                .map(|names| {
                    names
                        .into_iter()
                        .map(|name| directory.join(std::ffi::OsString::from_vec(name.0)))
                        .collect()
                })
                .map_err(|error| error.to_string())
        }
    })
}

/// A listing as folder rows, each opening on click and listing its own
/// folders on hover. Hidden folders stay out unless `show_hidden`.
fn subfolder_rows(
    id: ProjectId,
    result: Result<Vec<PathBuf>, String>,
    show_hidden: bool,
) -> (Vec<Item>, Vec<PathBuf>) {
    let mut folders = match result {
        Ok(folders) => folders,
        Err(error) => {
            return (
                vec![Item::heading(tr!("Could not list folders: %@", error))],
                Vec::new(),
            );
        }
    };
    if !show_hidden {
        folders.retain(|folder| {
            !folder
                .file_name()
                .is_some_and(|name| name.as_bytes().starts_with(b"."))
        });
    }
    if folders.is_empty() {
        return (vec![Item::heading(tr!("No subfolders found"))], folders);
    }
    let rows = folders
        .iter()
        .map(|folder| {
            Item::folder(
                folder
                    .file_name()
                    .unwrap_or(folder.as_os_str())
                    .to_string_lossy()
                    .into_owned(),
                Command::OpenSubfolder(id, folder_key(folder)),
            )
        })
        .collect();
    (rows, folders)
}

impl AppModel {
    pub(crate) fn open_folder_group_menu(
        &mut self,
        id: ProjectId,
        row: Option<PopoverAnchor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlay.is_some() {
            return;
        }
        let Some(project) = self
            .state
            .project(id)
            .filter(|project| project.is_folder_group())
        else {
            return;
        };
        let automatic = project.auto_subfolders;
        let hidden_toggle = self.hidden_subfolders_item(project);
        let directory = project.directory.clone();
        let local = project.server_id.is_local();
        let client = self.extensions.client(project.server_id);
        let mut items = Vec::new();
        if automatic {
            items.push(Item::heading(crate::views::project_picker::display_path(
                &directory,
            )));
            items.push(Item::heading(tr!("Loading folders…")));
        }
        items.extend(folder_group_items(project));
        items.extend(hidden_toggle);
        items.push(Item::submenu(tr!("Project"), self.project_menu(project)).separated());
        let position = window.mouse_position() + point(self.metrics.spacing5(), px(0.0));
        self.open_menu(items, position, window, cx);
        if let Some(Overlay::Menu(menu)) = &mut self.overlay {
            menu.passes_through = row;
        }
        if !automatic {
            return;
        }
        if let Some(Overlay::Menu(menu)) = &mut self.overlay {
            menu.folder_group = Some((id, directory.clone()));
        }
        let listing = list_subfolders(local, client, directory.clone(), cx);
        let task = cx.spawn(async move |model, cx| {
            let folders = listing.await;
            let _ = model.update(cx, |model, cx| {
                model.receive_folder_group_listing(id, &directory, folders, cx);
            });
        });
        if let Some(Overlay::Menu(menu)) = &mut self.overlay {
            menu.folder_listing = Some(task);
        }
    }

    pub(crate) fn receive_folder_group_listing(
        &mut self,
        id: ProjectId,
        directory: &Path,
        result: Result<Vec<PathBuf>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(Overlay::Menu(menu)) = &self.overlay else {
            return;
        };
        if menu
            .folder_group
            .as_ref()
            .is_none_or(|(project, root)| *project != id || root != directory)
        {
            return;
        }
        let position = menu.position;
        let row = menu.passes_through.clone();
        let Some(project) = self
            .state
            .project(id)
            .filter(|project| project.auto_subfolders && project.directory == directory)
        else {
            return;
        };
        let mut items = vec![Item::heading(crate::views::project_picker::display_path(
            directory,
        ))];
        let show_hidden = self.appearance.hidden_subfolders_shown.contains(&id);
        let (rows, folders) = subfolder_rows(id, result, show_hidden);
        items.extend(rows);
        items.extend(folder_group_items(project));
        items.extend(self.hidden_subfolders_item(project));
        items.push(Item::submenu(tr!("Project"), self.project_menu(project)).separated());
        let mut menu = Menu::new(items, position);
        menu.passes_through = row;
        menu.folder_group = Some((id, directory.to_owned()));
        menu.folder_paths = folders;
        self.overlay = Some(Overlay::Menu(menu));
        cx.notify();
    }

    /// Starts listing the folders of each folder row the menu has highlighted,
    /// so hovering a folder drills into it.
    pub(crate) fn list_highlighted_subfolders(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::Menu(menu)) = &self.overlay else {
            return;
        };
        let Some((id, _)) = menu.folder_group.clone() else {
            return;
        };
        let Some(project) = self.state.project(id) else {
            return;
        };
        let local = project.server_id.is_local();
        let client = self.extensions.client(project.server_id);
        let unlisted: Vec<_> = menu
            .highlighted_folders()
            .into_iter()
            .filter(|key| !menu.subfolder_listings.contains_key(key))
            .filter_map(|key| {
                menu.folder_paths
                    .iter()
                    .find(|path| folder_key(path) == key)
                    .map(|path| (key, path.clone()))
            })
            .collect();
        for (key, directory) in unlisted {
            let listing = list_subfolders(local, client.clone(), directory, cx);
            let task = cx.spawn(async move |model, cx| {
                let result = listing.await;
                let _ = model.update(cx, |model, cx| {
                    model.receive_subfolders(id, key, result, cx);
                });
            });
            if let Some(Overlay::Menu(menu)) = &mut self.overlay {
                menu.subfolder_listings.insert(key, task);
            }
        }
    }

    fn receive_subfolders(
        &mut self,
        id: ProjectId,
        key: u64,
        result: Result<Vec<PathBuf>, String>,
        cx: &mut Context<Self>,
    ) {
        let show_hidden = self.appearance.hidden_subfolders_shown.contains(&id);
        let Some(Overlay::Menu(menu)) = &mut self.overlay else {
            return;
        };
        if menu
            .folder_group
            .as_ref()
            .is_none_or(|(group, _)| *group != id)
        {
            return;
        }
        let (rows, folders) = subfolder_rows(id, result, show_hidden);
        if menu.set_subfolders(key, rows) {
            menu.folder_paths.extend(folders);
            cx.notify();
        }
    }

    fn hidden_subfolders_item(&self, project: &muxy_app_core::Project) -> Option<Item> {
        project.auto_subfolders.then(|| {
            Item::action(
                tr!("Show Hidden Folders"),
                Command::ToggleHiddenSubfolders(project.id),
            )
            .checked_if(
                self.appearance
                    .hidden_subfolders_shown
                    .contains(&project.id),
            )
        })
    }

    pub(crate) fn toggle_auto_subfolders(
        &mut self,
        id: ProjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self
            .state
            .project(id)
            .filter(|project| project.is_folder_group())
        else {
            return;
        };
        let directory = project.directory.clone();
        if !project.auto_subfolders
            && self
                .state
                .server_home(project.server_id)
                .is_some_and(|home| home.directory == directory)
        {
            self.choose_folder_group_parent(id, Some(true), window, cx);
        } else {
            let enabled = !project.auto_subfolders;
            self.edit_project(
                |state| state.set_folder_group_source(id, directory, enabled),
                cx,
            );
        }
    }

    pub(crate) fn choose_folder_group_parent(
        &mut self,
        id: ProjectId,
        enabled: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self
            .state
            .project(id)
            .filter(|project| project.is_folder_group())
        else {
            return;
        };
        let enabled = enabled.unwrap_or(project.auto_subfolders);
        if !project.server_id.is_local() {
            self.open_folder_group_root_editor(id, enabled, window.mouse_position(), window, cx);
            return;
        }
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(tr!("Choose Parent Folder")),
        });
        cx.spawn(async move |model, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(directory) = paths.into_iter().next()
            {
                let _ = model.update(cx, |model, cx| {
                    model.edit_project(
                        |state| state.set_folder_group_source(id, directory, enabled),
                        cx,
                    );
                });
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_lists_only_immediate_folders_and_directory_links() {
        let root = tempfile::tempdir().expect("root");
        let client = root.path().join("Client");
        std::fs::create_dir(&client).expect("client");
        std::fs::create_dir(client.join("Nested")).expect("nested");
        std::fs::write(root.path().join("notes.txt"), "notes").expect("file");
        let alias = root.path().join("Alias");
        std::os::unix::fs::symlink(&client, &alias).expect("folder link");
        assert_eq!(
            local_subfolders(root.path()).expect("folders"),
            [alias, client]
        );
        assert!(local_subfolders(&root.path().join("missing")).is_err());
    }
}
