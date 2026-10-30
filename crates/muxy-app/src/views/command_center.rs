use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, AppContext, Context, FontWeight, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, Render, SharedString, StatefulInteractiveElement, Styled, Window,
    canvas, div, px,
};
use muxy_app_core::{
    activity::{ActivityIndicator, indicator},
    command_center::GridLayout,
};
use muxy_ui::{components::IconGlyph, icon::Icon, tr};

use super::menu::{Command, Item, color_items};
use crate::model::{AppModel, command_center::Tile};

struct DraggedTerminal {
    pane: muxy_app_core::PaneId,
}

struct TerminalDragPreview {
    project: String,
    title: String,
    theme: muxy_ui::theme::Theme,
    metrics: muxy_ui::theme::Metrics,
    cursor_offset: gpui::Point<Pixels>,
}

impl Render for TerminalDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let m = self.metrics;
        let card = div()
            .debug_selector(|| "command-center-drag-preview".into())
            .relative()
            .left(self.cursor_offset.x + m.spacing4())
            .top(self.cursor_offset.y + m.spacing4())
            .flex()
            .items_center()
            .gap(m.spacing3())
            .w(m.scaled(300.0))
            .p(m.spacing4())
            .rounded(m.radius_md())
            .border_2()
            .border_color(self.theme.accent)
            .bg(self.theme.bg)
            .shadow(muxy_ui::theme::Elevation::Elevated.shadow(self.theme.bg))
            .child(IconGlyph::new(Icon::Move, m.icon_md(), self.theme.accent))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .truncate()
                            .text_size(m.font_body())
                            .text_color(self.theme.fg)
                            .child(self.project.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(m.font_footnote())
                            .text_color(self.theme.fg_muted)
                            .child(self.title.clone()),
                    ),
            );
        div().child(card)
    }
}

pub(super) fn register_commands(
    registry: &mut muxy_ui::command_palette::Registry<super::command_palette::Handler>,
    model: &AppModel,
) {
    use super::command_palette::Handler;
    use muxy_ui::command_palette::Command as PaletteCommand;
    use std::rc::Rc;
    let handler: Handler = Rc::new(|model, _, cx| model.open_command_center(cx));
    registry.register(
        PaletteCommand::new("command_center", tr!("Command Center"), handler)
            .keywords("dashboard all running terminals"),
    );
    let handler: Handler = Rc::new(|model, window, cx| {
        layout_menu(model, window.mouse_position(), window, cx);
    });
    registry.register(
        PaletteCommand::new(
            "command_center_layout",
            tr!("Command Center Layout…"),
            handler,
        )
        .keywords("dashboard grid columns rows")
        .disabled(!model.command_center.active),
    );
    let handler: Handler = Rc::new(|model, _, cx| {
        if let Some(pane) = model.active_pane() {
            model.open_command_center_project(pane, cx);
        }
    });
    registry.register(
        PaletteCommand::new("command_center_project", tr!("Open in Project"), handler)
            .keywords("dashboard terminal")
            .disabled(!model.command_center.active || model.active_pane().is_none()),
    );
    for (forward, title) in [
        (false, tr!("Move Terminal Earlier")),
        (true, tr!("Move Terminal Later")),
    ] {
        let handler: Handler = Rc::new(move |model, _, cx| {
            let Some(index) = model
                .command_center
                .tiles
                .iter()
                .position(|tile| Some(tile.pane) == model.active_pane())
            else {
                return;
            };
            let target = if forward {
                index.checked_add(1)
            } else {
                index.checked_sub(1)
            };
            if let Some(target) = target.and_then(|index| model.command_center.tiles.get(index)) {
                model.reorder_command_center(
                    model.command_center.tiles[index].pane,
                    target.pane,
                    cx,
                );
            }
        });
        registry.register(
            PaletteCommand::new(
                if forward {
                    "command_center_move_later"
                } else {
                    "command_center_move_earlier"
                },
                title,
                handler,
            )
            .keywords("dashboard reorder terminal")
            .disabled(!model.command_center.active || model.active_pane().is_none()),
        );
    }
}

fn sidebar_menu(model: &AppModel) -> Vec<Item> {
    let current = model.appearance.command_center_color.as_str();
    let mut colors = vec![
        Item::action(tr!("Default"), Command::CenterColor(None))
            .checked_if(current.is_empty())
            .swatch(model.theme.accent),
    ];
    colors.extend(color_items(Some(current), |index| {
        Command::CenterColor(Some(index))
    }));
    vec![Item::submenu(tr!("Color"), colors)]
}

pub(super) fn sidebar_button(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let m = model.metrics;
    let active = model.command_center.active;
    let wide = model.appearance.sidebar_expanded;
    let icon_size = model.sidebar_icon();
    let scale = f32::from(icon_size) / f32::from(m.icon_xxl());
    let (background, foreground) =
        muxy_ui::theme::parse_hex(&model.appearance.command_center_color).map_or(
            (model.theme.accent, model.theme.accent_foreground),
            |color| {
                (
                    color.into(),
                    muxy_ui::theme::contrasting_foreground(color).into(),
                )
            },
        );
    div()
        .flex_none()
        .px(m.spacing2())
        .pt(if wide { m.spacing4() } else { px(0.0) })
        .child(
            div()
                .id("command-center-button")
                .debug_selector(|| "command-center-button".into())
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .gap(m.spacing3())
                .h(icon_size + m.spacing2() * 2.0)
                .rounded(if wide { m.radius_lg() } else { m.scaled(9.0) })
                .cursor_pointer()
                .text_size(m.font_body())
                .text_color(if active {
                    model.theme.accent
                } else {
                    model.theme.fg_muted
                })
                .when(active, |button| button.bg(model.theme.surface))
                .hover(|button| {
                    button.bg(if active {
                        model.theme.surface
                    } else {
                        model.theme.hover
                    })
                })
                .tooltip(super::status_bar::tooltip(
                    tr!("Command Center").to_string(),
                    model,
                ))
                .on_click(cx.listener(|model, _, _, cx| model.open_command_center(cx)))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|model, event: &gpui::MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        model.open_menu(sidebar_menu(model), event.position, window, cx);
                    }),
                )
                .child(
                    div()
                        .debug_selector(|| "command-center-sidebar-icon".into())
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(icon_size)
                        .rounded(m.radius_md() * scale)
                        .bg(background)
                        .child(IconGlyph::new(
                            Icon::Grid,
                            m.font_title_large() * scale,
                            foreground,
                        )),
                )
                .when(wide, |button| {
                    button
                        .justify_start()
                        .px(m.spacing3())
                        .child(tr!("Command Center"))
                })
                .when(active && !wide, |button| {
                    button.child(
                        div()
                            .debug_selector(|| "command-center-selection".into())
                            .absolute()
                            .inset_0()
                            .rounded(m.scaled(9.0))
                            .border(m.scaled(1.5))
                            .border_color(model.theme.accent),
                    )
                }),
        )
        .into_any_element()
}

#[allow(
    clippy::too_many_lines,
    reason = "One toolbar with its anchored Layout control"
)]
pub(super) fn header(
    model: &AppModel,
    sidebar_width: f32,
    actions: gpui::Div,
    window: &Window,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let m = model.metrics;
    let selected = model.appearance.command_center_layout;
    let (loading, errors) = model.command_center_listing_status();
    let navigation = super::titlebar::navigation_width(model, window);
    let layout_anchor = model.command_center.layout_anchor.clone();
    super::titlebar::background("command-center-titlebar")
        .debug_selector(|| "command-center-titlebar".into())
        .flex()
        .flex_none()
        .items_center()
        .h(m.title_bar_height())
        .pl(px((navigation - sidebar_width).max(0.0)) + m.spacing4())
        .pr(m.spacing2())
        .gap(m.spacing4())
        .border_b_1()
        .border_color(model.theme.border)
        .when(loading, |bar| {
            bar.child(
                div()
                    .text_size(m.font_footnote())
                    .text_color(model.theme.fg_muted)
                    .child(tr!("Loading…")),
            )
        })
        .when(errors > 0, |bar| {
            bar.child(
                div()
                    .text_size(m.font_footnote())
                    .text_color(model.theme.fg_muted)
                    .child(tr!("Some projects unavailable")),
            )
        })
        .child(div().flex_1())
        .child(
            div()
                .id("command-center-layout")
                .debug_selector(|| "command-center-layout".into())
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .gap(m.spacing2())
                .px(m.spacing3())
                .py(m.spacing2())
                .rounded(m.radius_md())
                .cursor_pointer()
                .hover(|button| button.bg(model.theme.hover))
                .text_size(m.font_body())
                .tooltip(super::status_bar::tooltip(
                    tr!("Layout · columns × rows").to_string(),
                    model,
                ))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(
                    cx.listener(move |model, event: &gpui::ClickEvent, window, cx| {
                        layout_menu(model, event.position(), window, cx);
                        cx.stop_propagation();
                    }),
                )
                .child(
                    canvas(
                        move |bounds, _, _| layout_anchor.set(Some(bounds)),
                        |_, (), _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
                .child(IconGlyph::new(
                    Icon::Grid,
                    m.icon_md(),
                    model.theme.fg_muted,
                ))
                .child(tr!("Layout"))
                .child(div().text_color(model.theme.fg_muted).child(
                    if selected == GridLayout::Auto {
                        tr!("Auto")
                    } else {
                        selected.label().into()
                    },
                ))
                .child(IconGlyph::new(
                    Icon::ChevronDown,
                    m.icon_sm(),
                    model.theme.fg_muted,
                )),
        )
        .child(
            super::titlebar::icon_button(
                "command-center-back",
                Icon::ChevronLeft,
                tr!("Return to Project"),
                model,
            )
            .on_click(cx.listener(|model, _, _, cx| model.close_command_center(cx))),
        )
        .child(actions)
        .into_any_element()
}

pub(super) fn layout_menu(
    model: &mut AppModel,
    position: gpui::Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<AppModel>,
) {
    let selected = model.appearance.command_center_layout;
    let mut items: Vec<_> = GridLayout::ALL
        .into_iter()
        .map(|layout| {
            Item::action(
                if layout == GridLayout::Auto {
                    tr!("Auto")
                } else {
                    layout.label().into()
                },
                Command::CenterLayout(layout),
            )
            .checked_if(layout == selected)
            .grid(layout.dimensions())
        })
        .collect();
    items.push(Item::action(tr!("Custom…"), Command::CenterCustomLayout).separated());
    model.open_menu(items, position, window, cx);
    if let Some(crate::views::overlays::Overlay::Menu(menu)) = &mut model.overlay {
        menu.drops_from = model.command_center.layout_anchor.get();
    }
}

pub(super) fn custom_layout_picker(
    selection: [u8; 2],
    model: &AppModel,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    use super::overlays::Overlay;

    let m = model.metrics;
    let mut cells = div().flex().flex_col().gap(m.spacing2());
    for row in 1..=8_u8 {
        let mut line = div().flex().gap(m.spacing2());
        for column in 1..=10_u8 {
            let highlighted = column <= selection[0] && row <= selection[1];
            line = line.child(
                div()
                    .id(SharedString::from(format!("layout-cell-{column}-{row}")))
                    .debug_selector(move || format!("layout-cell-{column}-{row}-{highlighted}"))
                    .size(m.scaled(22.0))
                    .border_1()
                    .border_color(if highlighted {
                        model.theme.accent
                    } else {
                        model.theme.border
                    })
                    .bg(if highlighted {
                        model.theme.accent.opacity(0.2)
                    } else {
                        model.theme.bg
                    })
                    .cursor_pointer()
                    .on_mouse_move(cx.listener(move |model, _, _, cx| {
                        if let Some(Overlay::CenterLayout(current)) = &mut model.overlay
                            && *current != [column, row]
                        {
                            *current = [column, row];
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |model, _, _, cx| {
                        model.set_command_center_layout(GridLayout::Custom([column, row]), cx);
                        model.dismiss_overlay(cx);
                    })),
            );
        }
        cells = cells.child(line);
    }
    let content = muxy_ui::popover::surface(&model.theme, m)
        .id("command-center-custom-layout")
        .debug_selector(|| "command-center-custom-layout".into())
        .track_focus(&model.overlay_focus)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_key_down(cx.listener(|model, event: &gpui::KeyDownEvent, _, cx| {
            let Some(Overlay::CenterLayout(mut selected)) = model.overlay else {
                return;
            };
            match event.keystroke.key.as_str() {
                "left" => selected[0] = selected[0].saturating_sub(1).max(1),
                "right" => selected[0] = (selected[0] + 1).min(10),
                "up" => selected[1] = selected[1].saturating_sub(1).max(1),
                "down" => selected[1] = (selected[1] + 1).min(8),
                "enter" => {
                    model.set_command_center_layout(GridLayout::Custom(selected), cx);
                    model.dismiss_overlay(cx);
                    cx.stop_propagation();
                    return;
                }
                "escape" => {
                    model.dismiss_overlay(cx);
                    cx.stop_propagation();
                    return;
                }
                _ => return,
            }
            model.overlay = Some(Overlay::CenterLayout(selected));
            cx.notify();
            cx.stop_propagation();
        }))
        .child(
            muxy_ui::popover::header(&model.theme, m)
                .debug_selector(|| "command-center-custom-size".into())
                .child(format!("{} × {}", selection[0], selection[1])),
        )
        .child(muxy_ui::popover::body(m).child(cells))
        .child(model.webview_occlusion())
        .into_any_element();
    let owner = cx.weak_entity();
    muxy_ui::popover::anchored_popover(
        model.command_center.layout_anchor.clone(),
        content,
        move |_, cx| {
            let _ = owner.update(cx, AppModel::dismiss_overlay);
        },
    )
}

pub(super) fn render(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let m = model.metrics;
    if model.command_center.tiles.is_empty() {
        let (loading, errors) = model.command_center_listing_status();
        return div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(m.spacing4())
            .bg(model.theme.bg)
            .child(IconGlyph::new(
                Icon::Grid,
                m.icon_xxl(),
                model.theme.fg_muted,
            ))
            .child(div().text_size(m.font_headline()).child(if loading {
                tr!("Loading terminals…")
            } else {
                tr!("No running terminals")
            }))
            .when(errors > 0, |body| {
                body.child(
                    div()
                        .text_size(m.font_body())
                        .text_color(model.theme.fg_muted)
                        .child(tr!("Connect to your servers to see their terminals.")),
                )
            })
            .into_any_element();
    }
    let owner = cx.weak_entity();
    div()
        .debug_selector(|| "command-center-background".into())
        .size_full()
        .overflow_hidden()
        .bg(model.theme.bg)
        .p(m.spacing4())
        .child(
            canvas(
                move |bounds, window, cx| {
                    let mut list = owner
                        .update(cx, |model, cx| grid(model, bounds.size, cx))
                        .unwrap_or_else(|_| div().into_any_element());
                    list.layout_as_root(bounds.size.into(), window, cx);
                    list.prepaint_at(bounds.origin, window, cx);
                    list
                },
                |_, mut list, window, cx| list.paint(window, cx),
            )
            .size_full(),
        )
        .into_any_element()
}

#[allow(
    clippy::too_many_lines,
    reason = "Packed tile geometry and its virtualized scroll viewport"
)]
fn grid(model: &mut AppModel, size: gpui::Size<Pixels>, cx: &mut Context<AppModel>) -> AnyElement {
    let m = model.metrics;
    let [columns, rows] = model.appearance.command_center_layout.grid(
        model.command_center.tiles.len(),
        f32::from(size.width) / f32::from(m.scaled(1.0)),
        f32::from(size.height) / f32::from(m.scaled(1.0)),
    );
    let gap = m.spacing4();
    let dimensions = [columns, rows];
    let columns = usize::from(columns);
    let height = ((size.height + gap) / f32::from(rows)).max(m.scaled(180.0));
    let compact_height = m.scaled(36.0) + px(4.0) + gap;
    let mut bottoms = vec![px(0.0); columns];
    let positions: Vec<_> = model
        .command_center
        .tiles
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let column = index % columns;
            let top = bottoms[column];
            let extent = if model.command_center.is_collapsed(entry) {
                compact_height
            } else {
                height
            };
            bottoms[column] += extent;
            (top, extent - gap)
        })
        .collect();
    let total = (bottoms.into_iter().max().unwrap_or_default() - gap).max(px(0.0));
    let scrolling = total > size.height + px(0.5);
    let row_count = model.command_center.tiles.len().div_ceil(columns);
    let bands: Vec<_> = (0..row_count)
        .map(|row| (total - height * f32::from(u16::try_from(row).unwrap_or(u16::MAX))).min(height))
        .take_while(|height| *height > px(0.0))
        .collect();
    let grid = (dimensions, size, row_count, total);
    model.command_center.columns = columns;
    if model.command_center.grid != Some(grid) {
        let offset = model.command_center.scroll.logical_scroll_top();
        model.command_center.scroll.reset(bands.len());
        model.command_center.scroll.scroll_to(offset);
        model.command_center.scrollbar.reset();
        model.command_center.grid = Some(grid);
    }
    if let Some(pane) = model.command_center.reveal.take()
        && let Some(index) = model
            .command_center
            .tiles
            .iter()
            .position(|tile| tile.pane == pane)
    {
        let current = model.command_center.scroll.logical_scroll_top();
        let offset = height * f32::from(u16::try_from(current.item_ix).unwrap_or(u16::MAX))
            + current.offset_in_item;
        let (top, extent) = positions[index];
        let target = if top < offset {
            top
        } else if top + extent > offset + size.height {
            top + extent - size.height
        } else {
            offset
        };
        let mut remaining = target;
        let mut item_ix = 0;
        while item_ix + 1 < bands.len() && remaining >= bands[item_ix] {
            remaining -= bands[item_ix];
            item_ix += 1;
        }
        model.command_center.scroll.scroll_to(gpui::ListOffset {
            item_ix,
            offset_in_item: remaining,
        });
    }
    let scroll = model.command_center.scroll.clone();
    let paint_scroll = scroll.clone();
    let owner = cx.weak_entity();
    // The list supplies scrolling; only intersecting packed tiles build UI.
    let list = gpui::list(scroll, move |row, _, _| {
        div().w_full().h(bands[row]).into_any_element()
    })
    .size_full();
    let tiles = canvas(
        move |bounds, window, cx| {
            let top = paint_scroll.logical_scroll_top();
            let offset = height * f32::from(u16::try_from(top.item_ix).unwrap_or(u16::MAX))
                + top.offset_in_item;
            let width = (bounds.size.width
                - gap * f32::from(u16::try_from(columns - 1).unwrap_or(u16::MAX)))
                / f32::from(dimensions[0]);
            let mut elements = Vec::new();
            let _ = owner.update(cx, |model, cx| {
                let mut visible = Vec::new();
                window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                    for (index, (top, tile_height)) in positions.iter().enumerate() {
                        let frame = gpui::Bounds::new(
                            bounds.origin
                                + gpui::point(
                                    (width + gap)
                                        * f32::from(
                                            u16::try_from(index % columns).unwrap_or(u16::MAX),
                                        ),
                                    *top - offset,
                                ),
                            gpui::size(width, *tile_height),
                        );
                        if !frame.intersects(&bounds) {
                            continue;
                        }
                        let entry = &model.command_center.tiles[index];
                        if !model.command_center.is_collapsed(entry) {
                            visible.push(entry.pane);
                        }
                        let mut element = div()
                            .size_full()
                            .child(tile(entry, index, model, cx))
                            .into_any_element();
                        element.layout_as_root(frame.size.into(), window, cx);
                        element.prepaint_at(frame.origin, window, cx);
                        elements.push(element);
                    }
                });
                model.command_center_visible(visible, window, cx);
            });
            (bounds, elements)
        },
        |_, (bounds, mut elements), window, cx| {
            window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                for element in &mut elements {
                    element.paint(window, cx);
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .bottom_0()
    .right(if scrolling { m.scaled(12.0) } else { px(0.0) });
    div()
        .relative()
        .size_full()
        .when(scrolling, |grid| grid.pr(m.scaled(12.0)))
        .child(list)
        .child(tiles)
        .when(scrolling, |grid| {
            grid.child(
                div().absolute().top_0().right_0().bottom_0().child(
                    model
                        .command_center
                        .scrollbar
                        .element(&model.command_center.scroll, model.theme.fg_dim),
                ),
            )
        })
        .into_any_element()
}

#[allow(clippy::too_many_lines, reason = "One terminal tile and its controls")]
fn tile(entry: &Tile, index: usize, model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let m = model.metrics;
    let pane = entry.pane;
    let collapsed = model.command_center.is_collapsed(entry);
    let project = model.state.project(entry.session.info.project);
    let name = project.map_or_else(
        || tr!("Project").to_string(),
        |project| {
            let name = project
                .parent_id
                .and_then(|id| model.state.project(id))
                .map_or_else(
                    || project.name.clone(),
                    |parent| format!("{} / {}", parent.name, project.name),
                );
            model
                .server_name(entry.server)
                .filter(|_| !entry.server.is_local())
                .map_or_else(|| name.clone(), |server| format!("{server} · {name}"))
        },
    );
    let selected = model.active_pane() == Some(pane);
    let status = model
        .activity(entry.server)
        .map_or(ActivityIndicator::None, |activity| {
            indicator(&activity.snapshot, |session| {
                session == entry.session.info.id
            })
        });
    let progress = match super::tab_activity::resolve(
        entry.server,
        &[entry.session.info.id],
        false,
        false,
        model,
    ) {
        super::tab_activity::Status::Progress(progress) => Some(progress),
        _ => None,
    };
    let attention = match status {
        ActivityIndicator::Blocked => Some(tr!("Needs input")),
        ActivityIndicator::Completed => Some(tr!("Unread update")),
        _ => None,
    };
    let accent = model.theme.accent;
    let drag_project = name.clone();
    let drag_title = entry.title.clone();
    let drag_theme = model.theme.clone();
    let group = SharedString::from(format!("command-center-tile-{pane}"));
    let artwork = project.map_or_else(
        || IconGlyph::new(Icon::Terminal, m.icon_md(), model.theme.fg_muted).into_any_element(),
        |project| super::sidebar::project_tile(project, model, group.clone(), m.scaled(26.0)),
    );
    div()
        .id(group.clone())
        .group(group)
        .debug_selector(move || {
            format!("command-center-tile-{index}-{status:?}-focused-{selected}")
        })
        .flex()
        .relative()
        .flex_col()
        .flex_1()
        .min_w(px(0.0))
        .h_full()
        .when(collapsed, Styled::h_auto)
        .rounded(m.radius_md())
        .border_2()
        .border_color(if selected {
            model.theme.accent
        } else if attention.is_some() {
            model.theme.warning
        } else {
            model.theme.border
        })
        .overflow_hidden()
        .drag_over::<DraggedTerminal>(move |style, _, _, _| style.border_color(accent))
        .on_drop(cx.listener(move |model, drag: &DraggedTerminal, _, cx| {
            model.reorder_command_center(drag.pane, pane, cx);
            cx.stop_propagation();
        }))
        .child(
            div()
                .id(SharedString::from(format!("command-center-drag-{pane}")))
                .debug_selector(move || format!("command-center-drag-{index}"))
                .relative()
                .flex()
                .items_center()
                .flex_none()
                .gap(m.spacing3())
                .px(m.spacing3())
                .py(m.spacing2())
                .bg(model.theme.surface)
                .cursor_grab()
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |model, event: &gpui::MouseDownEvent, _, cx| {
                        model.command_center_menu(pane, event.position, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_drag(DraggedTerminal { pane }, move |_, cursor_offset, _, cx| {
                    cx.new(|_| TerminalDragPreview {
                        project: drag_project.clone(),
                        title: drag_title.clone(),
                        theme: drag_theme.clone(),
                        metrics: m,
                        cursor_offset,
                    })
                })
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .debug_selector(move || format!("command-center-artwork-{index}"))
                        .child(artwork),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .justify_center()
                        .flex_1()
                        .min_w(px(0.0))
                        .line_height(m.scaled(14.0))
                        .debug_selector(move || format!("command-center-text-{index}"))
                        .child(
                            div()
                                .debug_selector(|| "command-center-project-name".into())
                                .truncate()
                                .text_size(m.font_body())
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(name),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(m.font_footnote())
                                .text_color(model.theme.fg_muted)
                                .child(entry.title.clone()),
                        ),
                )
                .when_some(attention, |header, label| {
                    header.child(
                        div()
                            .debug_selector(move || {
                                format!("command-center-attention-{index}-{status:?}")
                            })
                            .flex_none()
                            .px(m.spacing2())
                            .py(m.spacing1())
                            .rounded(m.radius_sm())
                            .bg(model.theme.warning.opacity(0.14))
                            .text_size(m.font_footnote())
                            .text_color(model.theme.warning)
                            .child(label),
                    )
                })
                .child(
                    super::titlebar::icon_button(
                        SharedString::from(format!("command-center-collapse-{pane}")),
                        if collapsed {
                            Icon::ChevronRight
                        } else {
                            Icon::ChevronDown
                        },
                        if collapsed {
                            tr!("Expand Terminal")
                        } else {
                            tr!("Collapse Terminal")
                        },
                        model,
                    )
                    .debug_selector(move || format!("command-center-collapse-{index}-{collapsed}"))
                    .on_click(cx.listener(move |model, _, _, cx| {
                        model.toggle_command_center_terminal(pane, cx);
                        cx.stop_propagation();
                    })),
                )
                .child(
                    super::titlebar::icon_button(
                        SharedString::from(format!("command-center-project-{pane}")),
                        Icon::ExternalLink,
                        tr!("Open in Project"),
                        model,
                    )
                    .debug_selector(move || format!("command-center-project-{pane}"))
                    .on_click(cx.listener(move |model, _, _, cx| {
                        model.open_command_center_project(pane, cx);
                        cx.stop_propagation();
                    })),
                )
                .child(
                    super::titlebar::icon_button(
                        SharedString::from(format!("command-center-close-{pane}")),
                        Icon::X,
                        tr!("Close Terminal Session…"),
                        model,
                    )
                    .debug_selector(move || format!("command-center-close-{index}"))
                    .on_click(cx.listener(move |model, _, _, cx| {
                        model.close_pane(pane, cx);
                        cx.stop_propagation();
                    })),
                )
                .when_some(progress, |header, progress| {
                    use muxy_protocol::ProgressState;
                    let color = match progress.state {
                        ProgressState::Paused => model.theme.warning,
                        ProgressState::Error => model.theme.danger,
                        _ => model.theme.accent,
                    };
                    let paused =
                        matches!(progress.state, ProgressState::Paused | ProgressState::Error);
                    header.child(
                        div()
                            .debug_selector(move || {
                                format!("command-center-progress-{index}-{:?}", progress.state)
                            })
                            .absolute()
                            .bottom_0()
                            .left_0()
                            .right_0()
                            .h(px(2.0))
                            .child(if paused {
                                div().size_full().bg(color).into_any_element()
                            } else {
                                model.spinners.activity_border(
                                    format!("command-center-working-{pane}"),
                                    px(2.0),
                                    color,
                                )
                            }),
                    )
                }),
        )
        .when(!collapsed, |tile| {
            tile.child(
                div()
                    .debug_selector(move || format!("command-center-terminal-{index}"))
                    .flex_1()
                    .min_h(px(0.0))
                    .child(model.terminal(&pane).map_or_else(
                        || div().size_full().bg(model.theme.bg).into_any_element(),
                        super::super::model::PaneView::element,
                    )),
            )
        })
        .when_some(
            entry.error.as_ref().filter(|_| !collapsed),
            |tile, error| {
                tile.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(m.spacing2())
                        .px(m.spacing3())
                        .py(m.spacing2())
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .truncate()
                                .text_size(m.font_footnote())
                                .child(error.clone()),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("command-center-retry-{pane}")))
                                .cursor_pointer()
                                .text_size(m.font_body())
                                .child(tr!("Retry"))
                                .on_click(cx.listener(move |model, _, _, cx| {
                                    model.retry_command_center(pane, cx);
                                })),
                        ),
                )
            },
        )
        .child(
            div()
                .id(SharedString::from(format!("command-center-drop-{pane}")))
                .absolute()
                .inset_0()
                .rounded(m.radius_md())
                .drag_over::<DraggedTerminal>(move |style, drag, _, _| {
                    if drag.pane == pane {
                        style
                    } else {
                        style
                            .bg(accent.opacity(0.18))
                            .border_2()
                            .border_color(accent)
                    }
                }),
        )
        .into_any_element()
}

pub(super) fn grid_preview([columns, rows]: [u8; 2], model: &AppModel) -> AnyElement {
    let m = model.metrics;
    div()
        .flex()
        .flex_col()
        .gap(m.scaled(1.0))
        .w(m.scaled(24.0))
        .h(m.scaled(16.0))
        .flex_none()
        .children((0..rows).map(|_| {
            div()
                .flex()
                .flex_1()
                .gap(m.scaled(1.0))
                .children((0..columns).map(|_| {
                    div()
                        .flex_1()
                        .h_full()
                        .rounded(m.scaled(1.0))
                        .bg(model.theme.fg_muted)
                }))
        }))
        .into_any_element()
}
