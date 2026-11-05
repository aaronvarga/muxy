mod colors;
pub(crate) mod folder_entry;
pub(crate) mod icons;
pub(crate) mod logo;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement,
    ParentElement, Pixels, Point, StatefulInteractiveElement, Styled, Window, div, px, size,
};
use muxy_app_core::{DividerId, ProjectId, ProjectStatus, TabId, WorkspaceId};
use muxy_ui::text_input::{InputEvent, InputStyle, TextInput};
use muxy_ui::tr;

use super::overlays::{Overlay, clamp};
use super::server_menu::Servers;
use crate::model::AppModel;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Field {
    Name,
    Icon,
    /// One to four characters drawn in place of an icon.
    IconText,
    Color,
}

#[derive(Clone, Copy)]
enum Target {
    Project(ProjectId),
    ProjectIconText(ProjectId),
    ProjectColor(ProjectId),
    Tab(TabId),
    Workspace(WorkspaceId),
    NewWorkspace(Option<ProjectId>),
    NewFolderGroup,
    FolderGroupRoot(ProjectId, bool),
    /// A new remote server group for these servers.
    NewServerGroup(Servers),
    /// The remote server group at an index of `Settings::server_groups`.
    ServerGroup(usize),
    Divider(DividerId),
}

pub(crate) struct Editor {
    target: Target,
    input: Entity<TextInput>,
    position: Point<Pixels>,
    error: Option<String>,
    color_presets: Vec<(String, String)>,
}

impl AppModel {
    pub(crate) fn open_folder_group_root_editor(
        &mut self,
        id: ProjectId,
        auto_subfolders: bool,
        position: Point<Pixels>,
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
        self.open_metadata_editor(
            Target::FolderGroupRoot(id, auto_subfolders),
            project.directory.to_string_lossy().into_owned(),
            position,
            window,
            cx,
        );
    }

    pub(crate) fn open_new_folder_group_editor(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connect(cx);
        self.open_metadata_editor(Target::NewFolderGroup, String::new(), position, window, cx);
    }

    pub(crate) fn open_project_editor(
        &mut self,
        id: ProjectId,
        field: Field,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self
            .state
            .project(id)
            .filter(|project| project.status() == ProjectStatus::Available)
        else {
            return;
        };
        if matches!(field, Field::Icon) {
            if muxy_ui::symbols::PROJECT_ICONS {
                self.open_project_icons(id, window, cx);
            }
            return;
        }
        if matches!(field, Field::IconText) {
            let text = project
                .icon
                .clone()
                .filter(|icon| muxy_protocol::icon_text_length(icon).is_some())
                .unwrap_or_default();
            self.open_metadata_editor(Target::ProjectIconText(id), text, position, window, cx);
            return;
        }
        if matches!(field, Field::Color) {
            let text = project.color.to_string();
            self.open_metadata_editor(Target::ProjectColor(id), text, position, window, cx);
            return;
        }
        let text = project.name.clone();
        self.open_metadata_editor(Target::Project(id), text, position, window, cx);
    }

    pub(crate) fn open_tab_editor(
        &mut self,
        id: TabId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        let text = tab.title(self.state.shown_pane(tab)).to_owned();
        self.open_metadata_editor(Target::Tab(id), text, position, window, cx);
    }

    pub(crate) fn open_workspace_editor(
        &mut self,
        id: WorkspaceId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(workspace) = self.state.workspace(id) {
            let text = workspace.name.clone();
            self.open_metadata_editor(Target::Workspace(id), text, position, window, cx);
        }
    }

    /// Creates a workspace, adding `project` when it starts from a project menu.
    pub(crate) fn open_new_workspace_editor(
        &mut self,
        project: Option<ProjectId>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = Target::NewWorkspace(project);
        self.open_metadata_editor(target, String::new(), position, window, cx);
    }

    pub(crate) fn open_server_group_editor(
        &mut self,
        servers: Servers,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = Target::NewServerGroup(servers);
        self.open_metadata_editor(target, String::new(), position, window, cx);
    }

    pub(crate) fn open_group_rename_editor(
        &mut self,
        index: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(group) = self.settings.server_groups().get(index) {
            let text = (*group).to_owned();
            self.open_metadata_editor(Target::ServerGroup(index), text, position, window, cx);
        }
    }

    pub(crate) fn open_divider_editor(
        &mut self,
        id: DividerId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(divider) = self
            .state
            .dividers()
            .iter()
            .find(|divider| divider.id == id)
        {
            let text = divider.label.clone().unwrap_or_default();
            self.open_metadata_editor(Target::Divider(id), text, position, window, cx);
        }
    }

    fn open_metadata_editor(
        &mut self,
        target: Target,
        text: String,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            TextInput::new(InputStyle::field(&self.theme, &self.metrics), cx)
                .with_text(text)
                .with_placeholder(if matches!(target, Target::ProjectColor(_)) {
                    "#RRGGBB"
                } else {
                    ""
                })
        });
        input.update(cx, TextInput::select_all_text);
        input.focus_handle(cx).focus(window);
        self.overlay_subscription = Some(cx.subscribe(&input, |model, _, event, cx| match event {
            InputEvent::Submitted => model.submit_project_editor(cx),
            InputEvent::Cancelled => model.dismiss_overlay(cx),
            InputEvent::Changed => {
                if let Some(Overlay::ProjectEditor(editor)) = &mut model.overlay {
                    editor.error = None;
                }
                cx.notify();
            }
        }));
        self.overlay = Some(Overlay::ProjectEditor(Editor {
            target,
            input,
            position,
            error: None,
            color_presets: if matches!(target, Target::ProjectColor(_)) {
                colors::presets()
            } else {
                Vec::new()
            },
        }));
        cx.notify();
    }

    fn submit_project_editor(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::ProjectEditor(editor)) = &self.overlay else {
            return;
        };
        let target = editor.target;
        let text = editor.input.read(cx).text().trim().to_owned();
        let saved = match target {
            Target::Tab(id) => self.edit_tab(|state| state.set_tab_title(id, Some(text)), cx),
            Target::Project(id) => self.edit_project(|state| state.rename_project(id, &text), cx),
            Target::ProjectColor(id) => {
                if let Ok(color) = text.parse::<muxy_app_core::Color>() {
                    self.edit_project(|state| state.set_project_color(id, color), cx)
                } else {
                    self.error = Some(tr!("Enter a color as #RRGGBB.").to_string());
                    false
                }
            }
            // Empty text removes the icon, restoring the default letter.
            Target::ProjectIconText(id) if text.is_empty() => {
                self.edit_project(|state| state.set_project_icon(id, None), cx)
            }
            Target::ProjectIconText(id) if muxy_protocol::icon_text_length(&text).is_some() => {
                !self.icon_needs_newer_server(id, &text)
                    && self.edit_project(|state| state.set_project_icon(id, Some(text)), cx)
            }
            Target::ProjectIconText(_) => {
                self.error = Some(tr!("Enter 1 to 4 characters.").to_string());
                false
            }
            Target::Workspace(id) => {
                self.edit_workspaces(|state| state.rename_workspace(id, &text), cx)
            }
            Target::NewWorkspace(project) => {
                let created = self.create_workspace(&text, project, cx);
                if project.is_none() && created.is_some() {
                    self.select_workspace(created, cx);
                }
                created.is_some()
            }
            Target::NewFolderGroup => {
                if self
                    .extensions
                    .client(muxy_app_core::ServerId::local())
                    .is_some_and(|client| client.supports(muxy_protocol::Feature::FOLDER_GROUPS))
                {
                    self.edit_project(|state| state.add_folder_group(&text).map(drop), cx)
                } else {
                    self.fail(
                        tr!("Folder groups require an updated, connected local server.")
                            .to_string(),
                        cx,
                    );
                    false
                }
            }
            Target::FolderGroupRoot(id, auto_subfolders) => {
                let home = self
                    .state
                    .project(id)
                    .and_then(|project| self.state.server_home(project.server_id))
                    .unwrap_or(self.state.home());
                let directory = if text == "~" {
                    home.directory.clone()
                } else if let Some(relative) = text.strip_prefix("~/") {
                    home.directory.join(relative)
                } else {
                    std::path::PathBuf::from(&text)
                };
                self.edit_project(
                    |state| state.set_folder_group_source(id, directory, auto_subfolders),
                    cx,
                )
            }
            Target::NewServerGroup(servers) => self.name_server_group(servers, &text, cx),
            Target::ServerGroup(index) => {
                self.name_server_group(Servers::Group(Some(index)), &text, cx)
            }
            Target::Divider(id) => {
                self.edit_workspaces(|state| state.set_divider_label(id, &text), cx)
            }
        };
        if saved {
            self.dismiss_overlay(cx);
        } else if let Some(Overlay::ProjectEditor(editor)) = &mut self.overlay {
            editor.error.clone_from(&self.error);
        }
        cx.notify();
    }
}

pub(crate) fn render(
    editor: &Editor,
    model: &AppModel,
    window: &Window,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let m = model.metrics;
    let theme = &model.theme;
    let color_editor = matches!(editor.target, Target::ProjectColor(_));
    let width = m.scaled(if color_editor { 416.0 } else { 320.0 });
    let origin = clamp(
        editor.position,
        size(width, m.scaled(if color_editor { 540.0 } else { 220.0 })),
        window.viewport_size(),
    );
    muxy_ui::popover::surface(theme, m)
        .absolute()
        .left(origin.x)
        .top(origin.y)
        .w(width.min((window.viewport_size().width - px(16.0)).max(px(0.0))))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(model.webview_occlusion())
        .child(
            muxy_ui::popover::header(theme, m).child(match editor.target {
                Target::Tab(_) => tr!("Rename Tab"),
                Target::Project(_) => tr!("Rename Project"),
                Target::ProjectIconText(_) => tr!("Icon Text"),
                Target::ProjectColor(_) => tr!("Project Color"),
                Target::Workspace(_) => tr!("Rename Workspace"),
                Target::NewWorkspace(_) => tr!("New Workspace"),
                Target::NewFolderGroup => tr!("New Folder Group"),
                Target::FolderGroupRoot(..) => tr!("Parent Folder"),
                Target::NewServerGroup(_) => tr!("New Group"),
                Target::ServerGroup(_) => tr!("Rename Group"),
                Target::Divider(_) => tr!("Divider Label"),
            }),
        )
        .child(
            muxy_ui::popover::body(m)
                .id("project-editor-body")
                .when(color_editor, |body| {
                    body.max_h(
                        (window.viewport_size().height - m.scaled(150.0)).max(m.scaled(100.0)),
                    )
                    .overflow_y_scroll()
                    .child(colors::grid(editor, model, cx))
                    .child(div().text_size(m.font_footnote()).child(tr!("Hex color")))
                })
                .child(muxy_ui::controls::text_field(
                    muxy_ui::controls::Style { theme, metrics: &m },
                    "project-editor",
                    &editor.input,
                    None,
                ))
                .children(editor.error.as_ref().map(|error| {
                    div()
                        .text_size(m.font_footnote())
                        .text_color(theme.danger)
                        .child(error.clone())
                })),
        )
        .child(
            muxy_ui::popover::footer(theme, m)
                .child(muxy_ui::controls::button(
                    muxy_ui::controls::Style { theme, metrics: &m },
                    "project-editor-cancel",
                    &tr!("Cancel"),
                    true,
                    cx.listener(|model, _, _, cx| model.dismiss_overlay(cx)),
                ))
                .child(
                    div()
                        .id("save-project-editor")
                        .debug_selector(|| "save-project-editor".into())
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(m.control_medium())
                        .px(m.spacing5())
                        .rounded(m.radius_md())
                        .bg(theme.accent)
                        .text_color(theme.accent_foreground)
                        .text_size(m.font_body())
                        .child(
                            if matches!(
                                editor.target,
                                Target::NewWorkspace(_)
                                    | Target::NewServerGroup(_)
                                    | Target::NewFolderGroup
                            ) {
                                tr!("Create")
                            } else {
                                tr!("Save")
                            },
                        )
                        .on_click(cx.listener(|model, _, _, cx| model.submit_project_editor(cx))),
                ),
        )
        .into_any_element()
}
