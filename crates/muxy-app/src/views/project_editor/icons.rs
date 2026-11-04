use crate::{model::AppModel, views::overlays::Overlay};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement,
    ParentElement, Pixels, ScrollStrategy, SharedString, StatefulInteractiveElement, Styled, Task,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use muxy_app_core::ProjectId;
use muxy_ui::{
    components::{ButtonInteraction, SymbolGlyph, Tooltip},
    l10n::tr_key,
    text_input::{InputEvent, InputStyle, TextInput},
    tr,
};
use std::ops::Range;

pub(crate) struct Icons {
    project: ProjectId,
    input: Entity<TextInput>,
    /// The category shown, or every symbol.
    category: Option<String>,
    /// Indexes into the catalog's entries.
    pub(crate) matches: Vec<usize>,
    scroll: UniformListScrollHandle,
    error: Option<String>,
    _loading: Option<Task<()>>,
}

impl Icons {
    fn refresh(&mut self, cx: &gpui::App) {
        self.matches = muxy_ui::symbols::loaded().map_or_else(Vec::new, |catalog| {
            catalog.matching(self.input.read(cx).text(), self.category.as_deref())
        });
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }
}

impl AppModel {
    pub(crate) fn open_project_icons(
        &mut self,
        project: ProjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            TextInput::new(InputStyle::compact(&self.theme, &self.metrics), cx)
                .with_placeholder_key(tr_key!("Search symbols…"))
        });
        input.focus_handle(cx).focus(window);
        self.overlay_subscription =
            Some(cx.subscribe(&input, |model, input, event, cx| match event {
                InputEvent::Cancelled => model.dismiss_overlay(cx),
                InputEvent::Changed => {
                    if let Some(Overlay::ProjectIcons(picker)) = &mut model.overlay {
                        picker.refresh(cx);
                        picker.error = None;
                    }
                    cx.notify();
                }
                InputEvent::Submitted => {
                    // Enter picks the first symbol, or the typed text when none match.
                    if let Some(Overlay::ProjectIcons(picker)) = &model.overlay {
                        let first = picker.matches.first().and_then(|index| {
                            muxy_ui::symbols::loaded().map(|catalog| &catalog.entries[*index])
                        });
                        if let Some(entry) = first {
                            model.choose_project_icon(Some(format!("sf:{}", entry.symbol)), cx);
                        } else if let Some(text) = text_icon(input.read(cx).text()) {
                            model.choose_project_icon(Some(text), cx);
                        }
                    }
                }
            }));
        // The system catalog takes a moment to read the first time.
        let loading = muxy_ui::symbols::loaded().is_none().then(|| {
            cx.spawn(async move |model, cx| {
                cx.background_executor()
                    .spawn(async {
                        let _ = muxy_ui::symbols::catalog();
                    })
                    .await;
                let _ = model.update(cx, |model, cx| {
                    if let Some(Overlay::ProjectIcons(picker)) = &mut model.overlay {
                        picker.refresh(cx);
                        cx.notify();
                    }
                });
            })
        });
        let mut picker = Icons {
            project,
            input,
            category: None,
            matches: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            error: None,
            _loading: loading,
        };
        picker.refresh(cx);
        self.overlay = Some(Overlay::ProjectIcons(picker));
        cx.notify();
    }

    fn choose_symbol_category(
        &mut self,
        category: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(Overlay::ProjectIcons(picker)) = &mut self.overlay {
            picker.category = category;
            picker.refresh(cx);
            // Typing keeps searching within the chosen category.
            picker.input.focus_handle(cx).focus(window);
            cx.notify();
        }
    }

    /// Servers older than text icons accept only one character; says so in `error`.
    pub(super) fn icon_needs_newer_server(&mut self, project: ProjectId, icon: &str) -> bool {
        let server = self.state.project(project).map(|project| project.server_id);
        let blocked = muxy_protocol::icon_text_length(icon) > Some(1)
            && !server.is_some_and(|server| {
                self.server_supports(server, muxy_protocol::Feature::TEXT_ICONS)
            });
        if blocked {
            self.error = Some(
                tr!("Restart the server to use icon text longer than one character.").to_string(),
            );
        }
        blocked
    }

    fn choose_project_icon(&mut self, icon: Option<String>, cx: &mut Context<Self>) {
        let Some(Overlay::ProjectIcons(picker)) = &self.overlay else {
            return;
        };
        let project = picker.project;
        if !icon
            .as_deref()
            .is_some_and(|icon| self.icon_needs_newer_server(project, icon))
            && self.edit_project(|state| state.set_project_icon(project, icon), cx)
        {
            self.dismiss_overlay(cx);
        } else if let Some(Overlay::ProjectIcons(picker)) = &mut self.overlay {
            picker.error.clone_from(&self.error);
        }
    }
}

#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "Declarative picker layout; the column count is small and positive"
)]
pub(crate) fn render(
    picker: &Icons,
    model: &AppModel,
    window: &Window,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let m = model.metrics;
    let theme = &model.theme;
    let viewport = window.viewport_size();
    let selected = model
        .state
        .project(picker.project)
        .and_then(|project| project.icon.as_deref());
    let width = m
        .scaled(680.0)
        .min((viewport.width - px(32.0)).max(px(0.0)));
    let height = m
        .scaled(540.0)
        .min((viewport.height - px(32.0)).max(px(0.0)));
    let sidebar = m.scaled(172.0);
    // Whole cells that fit beside the categories, after the panel's padding.
    let (cell, gap) = (m.control_large(), m.spacing4());
    let grid = width - sidebar - m.spacing8() * 3.0;
    let columns = (f32::from(grid + gap) / f32::from(cell + gap))
        .floor()
        .max(1.0) as usize;
    let loaded = muxy_ui::symbols::loaded();
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            muxy_ui::popover::surface(theme, m)
                .shadow(muxy_ui::theme::Elevation::Modal.shadow(theme.bg))
                .debug_selector(|| "project-symbol-picker".into())
                .w(width)
                .h(height)
                .on_key_down(cx.listener(|model, event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        model.dismiss_overlay(cx);
                    }
                }))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(model.webview_occlusion())
                .child(muxy_ui::popover::header(theme, m).child(tr!("Icon")))
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_h(px(0.0))
                        .child(categories(picker, sidebar, model, cx))
                        .child(
                            muxy_ui::popover::body(m)
                                .flex_1()
                                .min_w(px(0.0))
                                .min_h(px(0.0))
                                .child(muxy_ui::controls::text_field(
                                    muxy_ui::controls::Style { theme, metrics: &m },
                                    "project-symbol-search",
                                    &picker.input,
                                    None,
                                ))
                                .children(text_icon_row(picker, model, cx))
                                .child(symbol_grid(picker, columns, model, cx))
                                .when(loaded.is_none(), |body| {
                                    body.child(
                                        div()
                                            .text_color(theme.fg_muted)
                                            .child(tr!("Loading symbols…")),
                                    )
                                })
                                .when(loaded.is_some() && picker.matches.is_empty(), |body| {
                                    body.child(
                                        div()
                                            .text_color(theme.fg_muted)
                                            .child(tr!("No symbols found")),
                                    )
                                })
                                .children(picker.error.as_ref().map(|error| {
                                    div().text_color(theme.danger).child(error.clone())
                                })),
                        ),
                )
                .child(muxy_ui::popover::divider(theme, m))
                .child(
                    muxy_ui::popover::row(
                        theme,
                        m,
                        "remove-project-icon",
                        selected.is_some(),
                        false,
                    )
                    .debug_selector(|| "remove-project-icon".into())
                    .when(selected.is_some(), |button| {
                        button.button_interaction(
                            cx.listener(|model, _, _, cx| model.choose_project_icon(None, cx)),
                        )
                    })
                    .child(SymbolGlyph::new(
                        "xmark.circle",
                        m.font_caption(),
                        theme.fg_muted,
                    ))
                    .child(tr!("Remove Icon")),
                ),
        )
        .into_any_element()
}

/// Every symbol, then each of Apple's categories.
fn categories(
    picker: &Icons,
    width: Pixels,
    model: &AppModel,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let m = model.metrics;
    let theme = &model.theme;
    let listed = muxy_ui::symbols::loaded().map_or(&[][..], |catalog| &catalog.categories);
    let row = |key: Option<&str>, icon: &str, label: SharedString, cx: &mut Context<AppModel>| {
        let active = picker.category.as_deref() == key;
        let key = key.map(str::to_owned);
        div()
            .id(SharedString::from(format!(
                "symbol-category-{}",
                key.as_deref().unwrap_or("all")
            )))
            .debug_selector({
                let key = key.clone();
                move || format!("symbol-category-{}", key.as_deref().unwrap_or("all"))
            })
            .flex()
            .flex_none()
            .items_center()
            .gap(m.spacing3())
            .px(m.spacing3())
            .py(m.spacing2())
            .rounded(m.radius_sm())
            .cursor_pointer()
            .text_size(m.font_caption())
            .when(active, |row| {
                row.bg(theme.accent_soft).text_color(theme.accent)
            })
            .when(!active, |row| row.hover(|style| style.bg(theme.hover)))
            // A fixed slot, since symbols differ in width, so labels line up.
            .child(
                div()
                    .flex()
                    .flex_none()
                    .justify_center()
                    .w(m.scaled(18.0))
                    .child(SymbolGlyph::new(
                        icon.to_owned(),
                        m.font_caption(),
                        if active { theme.accent } else { theme.fg_muted },
                    )),
            )
            .child(div().min_w(px(0.0)).truncate().child(label))
            .button_interaction(cx.listener(move |model, _, window, cx| {
                model.choose_symbol_category(key.clone(), window, cx);
            }))
            .into_any_element()
    };
    let mut rows = vec![row(None, "square.grid.2x2", tr!("All Symbols"), cx)];
    for (key, icon) in listed {
        rows.push(row(Some(key), icon, category_label(key), cx));
    }
    div()
        .id("symbol-categories")
        .flex()
        .flex_col()
        .flex_none()
        .w(width)
        .p(m.spacing4())
        .gap(m.spacing1())
        .overflow_y_scroll()
        .border_r_1()
        .border_color(theme.border)
        .children(rows)
        .into_any_element()
}

/// Only the rows on screen are built, as a category can hold thousands.
fn symbol_grid(
    picker: &Icons,
    columns: usize,
    model: &AppModel,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let gap = model.metrics.spacing4();
    let rows = picker.matches.len().div_ceil(columns);
    uniform_list(
        "project-symbols-scroll",
        rows,
        cx.processor(move |model: &mut AppModel, range: Range<usize>, _, cx| {
            let (Some(Overlay::ProjectIcons(picker)), Some(catalog)) =
                (&model.overlay, muxy_ui::symbols::loaded())
            else {
                return Vec::new();
            };
            let selected = model
                .state
                .project(picker.project)
                .and_then(|project| project.icon.clone());
            let rows: Vec<Vec<&'static str>> = range
                .map(|row| {
                    let cells = row * columns..((row + 1) * columns).min(picker.matches.len());
                    picker.matches[cells]
                        .iter()
                        .map(|index| catalog.entries[*index].symbol.as_str())
                        .collect()
                })
                .collect();
            rows.into_iter()
                .map(|row| {
                    div().flex().gap(gap).pb(gap).children(
                        row.into_iter()
                            .map(|name| symbol_button(name, selected.as_deref(), model, cx)),
                    )
                })
                .collect()
        }),
    )
    .track_scroll(picker.scroll.clone())
    .flex_1()
    .min_h(px(0.0))
    .into_any_element()
}

fn symbol_button(
    name: &'static str,
    selected: Option<&str>,
    model: &AppModel,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let m = model.metrics;
    let theme = &model.theme;
    let active = selected.and_then(|value| value.strip_prefix("sf:")) == Some(name);
    let tooltip_theme = theme.clone();
    div()
        .id(SharedString::from(format!("project-symbol-{name}")))
        .debug_selector(move || format!("project-symbol-{name}"))
        .size(m.control_large())
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(m.radius_sm())
        .cursor_pointer()
        .when(active, |button| button.bg(theme.accent_soft))
        .hover(|style| style.bg(theme.hover))
        .tooltip(move |_, cx| {
            cx.new(|_| {
                Tooltip::new(
                    name,
                    tooltip_theme.raised(),
                    tooltip_theme.fg,
                    tooltip_theme.border,
                    tooltip_theme.bg,
                )
            })
            .into()
        })
        .child(
            SymbolGlyph::new(
                name,
                m.font_title_large(),
                if active { theme.accent } else { theme.fg },
            )
            .regular(),
        )
        .button_interaction(cx.listener(move |model, _, _, cx| {
            model.choose_project_icon(Some(format!("sf:{name}")), cx);
        }))
        .into_any_element()
}

/// Apple's name for a category.
fn category_label(key: &str) -> SharedString {
    match key {
        "whatsnew" => tr!("What’s New"),
        "draw" => tr!("Draw"),
        "variable" => tr!("Variable"),
        "multicolor" => tr!("Multicolor"),
        "communication" => tr!("Communication"),
        "weather" => tr!("Weather"),
        "maps" => tr!("Maps"),
        "objectsandtools" => tr!("Objects & Tools"),
        "devices" => tr!("Devices"),
        "cameraandphotos" => tr!("Camera & Photos"),
        "gaming" => tr!("Gaming"),
        "connectivity" => tr!("Connectivity"),
        "transportation" => tr!("Transportation"),
        "automotive" => tr!("Automotive"),
        "accessibility" => tr!("Accessibility"),
        "privacyandsecurity" => tr!("Privacy & Security"),
        "human" => tr!("Human"),
        "home" => tr!("Home"),
        "fitness" => tr!("Fitness"),
        "nature" => tr!("Nature"),
        "editing" => tr!("Editing"),
        "textformatting" => tr!("Text Formatting"),
        "media" => tr!("Media"),
        "keyboard" => tr!("Keyboard"),
        "commerce" => tr!("Commerce"),
        "time" => tr!("Time"),
        "health" => tr!("Health"),
        "shapes" => tr!("Shapes"),
        "arrows" => tr!("Arrows"),
        "indices" => tr!("Indices"),
        "math" => tr!("Math"),
        other => other.to_owned().into(),
    }
}

/// Offers the typed text itself as the icon when it is short enough.
fn text_icon_row(
    picker: &Icons,
    model: &AppModel,
    cx: &mut Context<AppModel>,
) -> Option<AnyElement> {
    let m = model.metrics;
    let theme = &model.theme;
    let text = text_icon(picker.input.read(cx).text())?;
    let label = tr!("Use “%@” as Icon", &text);
    Some(
        div()
            .id("project-text-icon")
            .debug_selector(|| "project-text-icon".into())
            .flex()
            .items_center()
            .gap(m.spacing3())
            .px(m.spacing3())
            .py(m.spacing2())
            .rounded(m.radius_sm())
            .cursor_pointer()
            .hover(|style| style.bg(theme.hover))
            .child(SymbolGlyph::new(
                "textformat",
                m.font_caption(),
                theme.fg_muted,
            ))
            .child(label)
            .button_interaction(cx.listener(move |model, _, _, cx| {
                model.choose_project_icon(Some(text.clone()), cx);
            }))
            .into_any_element(),
    )
}

/// Typed text usable as a project icon: one to four characters.
fn text_icon(query: &str) -> Option<String> {
    let text = query.trim();
    muxy_protocol::icon_text_length(text).map(|_| text.to_owned())
}
