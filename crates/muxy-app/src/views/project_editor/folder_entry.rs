use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use gpui::{
    AnyElement, AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement,
    ParentElement, Pixels, Point, Styled, Subscription, Window, div, size,
};
use muxy_app_core::ProjectId;
use muxy_protocol::{FolderEntry, FolderEntryId, ServerPath};
use muxy_ui::text_input::{InputEvent, InputStyle, TextInput};
use muxy_ui::{controls, tr};

use crate::model::AppModel;
use crate::views::overlays::{Overlay, clamp};

pub(crate) struct Editor {
    project: ProjectId,
    entry: FolderEntryId,
    editing: bool,
    fields: [Entity<TextInput>; 3],
    position: Point<Pixels>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl AppModel {
    pub(crate) fn open_folder_entry_editor(
        &mut self,
        project: ProjectId,
        entry: Option<FolderEntryId>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(record) = self
            .state
            .project(project)
            .filter(|project| project.is_folder_group())
        else {
            return;
        };
        let saved = entry.and_then(|id| record.folder_entries.iter().find(|entry| entry.id == id));
        if entry.is_some() && saved.is_none() {
            return;
        }
        let text = saved.map_or_else(
            || [String::new(), String::new(), String::new()],
            |entry| {
                [
                    entry.name.clone(),
                    String::from_utf8_lossy(&entry.directory.0).into_owned(),
                    entry.command.clone(),
                ]
            },
        );
        let fields = text.map(|text| {
            cx.new(|cx| {
                TextInput::new(InputStyle::field(&self.theme, &self.metrics), cx).with_text(text)
            })
        });
        let subscriptions = fields
            .iter()
            .map(|input| {
                cx.subscribe(input, |model, _, event, cx| match event {
                    InputEvent::Submitted => model.save_folder_entry(cx),
                    InputEvent::Cancelled => model.dismiss_overlay(cx),
                    InputEvent::Changed => {
                        if let Some(Overlay::FolderEntry(editor)) = &mut model.overlay {
                            editor.error = None;
                        }
                        cx.notify();
                    }
                })
            })
            .collect();
        fields[0].focus_handle(cx).focus(window);
        self.overlay = Some(Overlay::FolderEntry(Editor {
            project,
            entry: entry.unwrap_or_default(),
            editing: entry.is_some(),
            fields,
            position,
            error: None,
            _subscriptions: subscriptions,
        }));
        cx.notify();
    }

    fn save_folder_entry(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::FolderEntry(editor)) = &self.overlay else {
            return;
        };
        let [name, path, command] = editor
            .fields
            .each_ref()
            .map(|input| input.read(cx).text().trim().to_owned());
        let Some(project) = self.state.project(editor.project) else {
            return;
        };
        let local = project.server_id.is_local();
        let home = self
            .state
            .server_home(project.server_id)
            .unwrap_or(self.state.home());
        let directory = if path == "~" {
            home.directory.clone()
        } else if let Some(relative) = path.strip_prefix("~/") {
            home.directory.join(relative)
        } else {
            PathBuf::from(path)
        };
        let valid_folder = directory.is_absolute() && (!local || directory.is_dir());
        let entry = FolderEntry {
            id: editor.entry,
            name: if name.is_empty() {
                directory
                    .file_name()
                    .unwrap_or(directory.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            } else {
                name
            },
            directory: ServerPath(directory.as_os_str().as_bytes().to_vec()),
            command,
        };
        if !valid_folder || entry.validate().is_err() {
            if let Some(Overlay::FolderEntry(editor)) = &mut self.overlay {
                editor.error = Some(if valid_folder {
                    tr!("Use a name and a single-line startup command.").to_string()
                } else {
                    tr!("Choose an existing absolute folder path.").to_string()
                });
            }
            cx.notify();
            return;
        }
        let project = editor.project;
        let editing = editor.editing;
        let Some(mut entries) = self
            .state
            .project(project)
            .map(|project| project.folder_entries.clone())
        else {
            return;
        };
        if editing {
            let Some(saved) = entries.iter_mut().find(|saved| saved.id == entry.id) else {
                self.fail(tr!("This folder entry was removed.").to_string(), cx);
                return;
            };
            *saved = entry;
        } else {
            entries.push(entry);
        }
        if self.edit_project(|state| state.set_folder_entries(project, entries), cx) {
            self.dismiss_overlay(cx);
        } else if let Some(Overlay::FolderEntry(editor)) = &mut self.overlay {
            editor.error.clone_from(&self.error);
        }
    }

    pub(crate) fn remove_folder_entry(
        &mut self,
        project: ProjectId,
        entry: FolderEntryId,
        cx: &mut Context<Self>,
    ) {
        let Some(mut entries) = self
            .state
            .project(project)
            .map(|project| project.folder_entries.clone())
        else {
            return;
        };
        entries.retain(|saved| saved.id != entry);
        self.edit_project(|state| state.set_folder_entries(project, entries), cx);
    }

    fn choose_folder_entry_path(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::FolderEntry(editor)) = &self.overlay else {
            return;
        };
        if !self
            .state
            .project(editor.project)
            .is_some_and(|project| project.server_id.is_local())
        {
            return;
        }
        let name = editor.fields[0].clone();
        let input = editor.fields[1].clone();
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(tr!("Choose Folder")),
        });
        cx.spawn(async move |_, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.first()
            {
                let _ = input.update(cx, |input, cx| {
                    input.set_text(path.to_string_lossy().into_owned(), cx);
                });
                let _ = name.update(cx, |input, cx| {
                    if input.text().trim().is_empty() {
                        input.set_text(
                            path.file_name()
                                .unwrap_or(path.as_os_str())
                                .to_string_lossy()
                                .into_owned(),
                            cx,
                        );
                    }
                });
            }
        })
        .detach();
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
    let style = controls::Style { theme, metrics: &m };
    let origin = clamp(
        editor.position,
        size(m.scaled(420.0), m.scaled(390.0)),
        window.viewport_size(),
    );
    muxy_ui::popover::surface(theme, m)
        .absolute().left(origin.x).top(origin.y).w(m.scaled(420.0).min(window.viewport_size().width))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(model.webview_occlusion())
        .child(muxy_ui::popover::header(theme, m).child(if editor.editing { tr!("Edit Folder") } else { tr!("Add Folder") }))
        .child(muxy_ui::popover::body(m)
            .children([tr!("Name"), tr!("Folder Path"), tr!("Startup Command (optional)")].into_iter().enumerate().map(|(index, label)| {
                div().flex().flex_col().gap(m.spacing2())
                    .child(div().text_size(m.font_body()).text_color(theme.fg).child(label))
                    .child(controls::text_field(style, &format!("folder-entry-{index}"), &editor.fields[index], None))
            }))
            .child(controls::button(style, "folder-entry-browse", &tr!("Choose Folder…"), model.state.project(editor.project).is_some_and(|project| project.server_id.is_local()),
                cx.listener(|model, _, _, cx| model.choose_folder_entry_path(cx))))
            .child(div().text_size(m.font_footnote()).text_color(theme.fg_muted)
                .child(tr!("Leave the command blank for a terminal, or run a command such as claude \"Review this client\".")))
            .children(editor.error.as_ref().map(|error| div().text_size(m.font_footnote()).text_color(theme.danger).child(error.clone()))))
        .child(muxy_ui::popover::footer(theme, m)
            .child(controls::button(style, "folder-entry-cancel", &tr!("Cancel"), true, cx.listener(|model, _, _, cx| model.dismiss_overlay(cx))))
            .child(controls::button(style, "folder-entry-save", &tr!("Save"), true, cx.listener(|model, _, _, cx| model.save_folder_entry(cx)))))
        .into_any_element()
}
