use gpui::{
    AnyElement, Context, Focusable, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div,
};
use muxy_ui::l10n::tr_key;
use muxy_ui::{controls, form, tr};

use super::{Category, PickerKind, SettingsEvent, SettingsView, Snapshot};
use crate::model::preferences::{BrowserChange, Change};

pub(super) const FIELD: &str = "file-type-extension";

pub(super) struct Editor {
    previous: Option<String>,
    pub(super) handler: String,
}

pub(super) fn handler_label(snapshot: &Snapshot, value: &str) -> String {
    super::appearance::FILE_OPENERS
        .iter()
        .find(|(id, _)| *id == value)
        .map(|(_, label)| muxy_ui::l10n::translate(label).to_string())
        .or_else(|| {
            snapshot
                .file_openers
                .iter()
                .find(|(id, _)| id == value)
                .map(|(_, label)| label.clone())
        })
        .unwrap_or_else(|| super::appearance::unavailable_file_opener(value))
}

#[allow(
    clippy::too_many_lines,
    reason = "Declarative file mapping settings rows"
)]
pub(super) fn rows(pane: &SettingsView, cx: &mut Context<SettingsView>) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    if pane.matches(Category::General, tr_key!("Web links")) {
        rows.push(
            pane.row(
                "web-link-handler",
                tr_key!("Web links"),
                controls::segmented(
                    pane.style(),
                    "web-link-handler",
                    &tr!("Web links"),
                    &[
                        controls::Choice::new("default", tr!("Default Browser")),
                        controls::Choice::new("muxy", tr!("Muxy Browser")),
                    ],
                    if pane.snapshot.settings.browser.open_terminal_links {
                        "muxy"
                    } else {
                        "default"
                    },
                    cx.listener(|_, choice: &gpui::SharedString, _, cx| {
                        cx.emit(SettingsEvent::Change(Change::Browser(
                            BrowserChange::TerminalLinks(choice.as_ref() == "muxy"),
                        )));
                    }),
                )
                .into_any_element(),
            ),
        );
    }
    if !pane.matches(Category::General, tr_key!("File type mappings")) {
        return rows;
    }
    let entries = pane
        .snapshot
        .settings
        .openers
        .file_types
        .iter()
        .map(|(extension, handler)| {
            let edit = extension.clone();
            let removed = extension.clone();
            let id = format!("file-type-{extension}");
            let selector = format!("settings-file-type-edit-{extension}");
            div()
                .flex()
                .items_center()
                .gap(pane.metrics.spacing3())
                .child(div().flex_1().child(format!(".{extension}")))
                .child(
                    controls::button(
                        pane.style(),
                        &format!("{id}-edit"),
                        controls::Button::secondary(&handler_label(&pane.snapshot, handler)),
                        cx.listener(move |pane, _, window, cx| {
                            pane.edit_file_type(Some(edit.clone()), window, cx);
                        }),
                    )
                    .aria_label(format!(
                        ".{extension}: {}",
                        handler_label(&pane.snapshot, handler)
                    ))
                    .debug_selector(move || selector.clone()),
                )
                .child(
                    controls::button(
                        pane.style(),
                        &format!("{id}-remove"),
                        controls::Button::secondary(&tr!("Remove")),
                        cx.listener(move |_, _, _, cx| {
                            cx.emit(SettingsEvent::Change(Change::FileType {
                                previous: None,
                                extension: removed.clone(),
                                handler: None,
                            }));
                        }),
                    )
                    .aria_label(tr!("Remove %@", &format!(".{extension}"))),
                )
        })
        .collect::<Vec<_>>();
    let mut content = div()
        .flex()
        .flex_col()
        .gap(pane.metrics.spacing3())
        .children(entries);
    if let Some(editor) = &pane.file_type_editor {
        content = content
            .child(form::row(
                pane.layout_style(),
                &tr!("File extension"),
                None,
                pane.field(FIELD),
                pane.compact,
            ))
            .child(form::row(
                pane.layout_style(),
                &tr!("Open with"),
                None,
                pane.picker(
                    PickerKind::FileType,
                    &handler_label(&pane.snapshot, &editor.handler),
                    cx,
                ),
                pane.compact,
            ))
            .child(
                div()
                    .flex()
                    .gap(pane.metrics.spacing3())
                    .child(
                        controls::button(
                            pane.style(),
                            "save-file-type",
                            controls::Button::secondary(&tr!("Save")),
                            cx.listener(|pane, _, _, cx| pane.save_file_type(cx)),
                        )
                        .debug_selector(|| "settings-save-file-type".into()),
                    )
                    .child(controls::button(
                        pane.style(),
                        "cancel-file-type",
                        controls::Button::secondary(&tr!("Cancel")),
                        cx.listener(|pane, _, _, cx| pane.cancel_file_type(cx)),
                    )),
            );
    }
    let add = controls::button(
        pane.style(),
        "add-file-type",
        controls::Button::secondary(&tr!("Add mapping")),
        cx.listener(|pane, _, window, cx| pane.edit_file_type(None, window, cx)),
    )
    .debug_selector(|| "settings-add-file-type".into())
    .into_any_element();
    rows.push(pane.list_row(
        "file-types",
        tr_key!("File type mappings"),
        add,
        Some(content.into_any_element()),
    ));
    rows
}

impl SettingsView {
    fn edit_file_type(
        &mut self,
        extension: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let handler = extension
            .as_ref()
            .and_then(|extension| self.snapshot.settings.openers.file_types.get(extension))
            .unwrap_or(&self.snapshot.settings.openers.file)
            .clone();
        self.fields[FIELD].update(cx, |input, cx| {
            input.set_text(extension.clone().unwrap_or_default(), cx);
        });
        self.file_type_editor = Some(Editor {
            previous: extension,
            handler,
        });
        self.errors.remove("file-types");
        self.results.dirty = true;
        self.fields[FIELD].focus_handle(cx).focus(window);
        cx.notify();
    }

    pub(super) fn save_file_type(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.file_type_editor else {
            return;
        };
        cx.emit(SettingsEvent::Change(Change::FileType {
            previous: editor.previous.clone(),
            extension: self.fields[FIELD].read(cx).text().to_owned(),
            handler: Some(editor.handler.clone()),
        }));
    }

    pub(super) fn cancel_file_type(&mut self, cx: &mut Context<Self>) {
        self.file_type_editor = None;
        self.errors.remove("file-types");
        self.results.dirty = true;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AppModel, tests::stub_boot};
    use gpui::TestAppContext;
    use muxy_app_core::AppState;

    #[gpui::test]
    fn mapping_editor_saves_valid_input_and_keeps_invalid_edits(cx: &mut TestAppContext) {
        let (boot, _) = stub_boot(AppState::bootstrap().unwrap());
        let (model, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
        cx.update(|window, cx| model.update(cx, |model, cx| model.open_settings(window, cx)));
        cx.run_until_parked();
        let view = model.read_with(cx, |model, _| {
            model.settings_tab.as_ref().unwrap().view.clone()
        });
        cx.update(|window, cx| {
            view.update(cx, |pane, cx| {
                pane.edit_file_type(None, window, cx);
                pane.fields[FIELD].update(cx, |input, cx| input.set_text(" .MD ", cx));
                pane.file_type_editor.as_mut().unwrap().handler =
                    "markdown-preview:markdown".into();
                pane.save_file_type(cx);
            });
        });
        cx.run_until_parked();
        model.read_with(cx, |model, cx| {
            assert_eq!(
                model
                    .settings
                    .openers
                    .file_handler(std::path::Path::new("/tmp/notes.md")),
                "markdown-preview:markdown"
            );
            assert!(view.read(cx).file_type_editor.is_none());
            let saved =
                muxy_app_core::settings::Settings::load(&model.configuration_path("settings.toml"))
                    .unwrap();
            assert_eq!(saved.openers, model.settings.openers);
        });
        cx.update(|window, cx| {
            view.update(cx, |pane, cx| {
                pane.edit_file_type(Some("md".into()), window, cx);
                pane.fields[FIELD].update(cx, |input, cx| input.set_text("../md", cx));
                pane.save_file_type(cx);
            });
        });
        cx.run_until_parked();
        view.read_with(cx, |pane, _| {
            assert!(pane.file_type_editor.is_some());
            assert!(pane.errors.contains_key("file-types"));
        });
        model.update(cx, |model, cx| {
            model.change_preference(
                Change::FileType {
                    previous: None,
                    extension: "md".into(),
                    handler: None,
                },
                cx,
            );
        });
        assert!(model.read_with(cx, |model, _| model.settings.openers.file_types.is_empty()));
    }
}
