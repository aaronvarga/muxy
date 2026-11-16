use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Context, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Pixels, Point, Role, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Window, div, point, px,
};
use muxy_core::shortcuts::ShortcutId;
use muxy_ui::components::IconGlyph;
use muxy_ui::icon::Icon;
use muxy_ui::theme::Metrics;
use muxy_ui::tr;

use crate::model::AppModel;

#[cfg(target_os = "linux")]
pub(crate) mod frame;
#[cfg(target_os = "linux")]
pub(crate) mod linux;

pub(crate) fn content_size(window: &Window) -> gpui::Size<Pixels> {
    #[cfg(target_os = "linux")]
    return frame::bounds(window).size;
    #[cfg(not(target_os = "linux"))]
    window.viewport_size()
}

pub(crate) fn overlay(element: impl IntoElement, window: &Window) -> AnyElement {
    #[cfg(target_os = "linux")]
    return frame::overlay(element, window);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = window;
        element.into_any_element()
    }
}

pub(super) fn top_corners<T: Styled>(element: T, window: &Window, left: bool, right: bool) -> T {
    #[cfg(target_os = "linux")]
    return frame::top_corners(element, window, left, right);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (window, left, right);
        element
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "Upstream project launchers and configurable fork titlebar actions"
)]
pub(super) fn actions(model: &AppModel, window: &Window, cx: &mut Context<AppModel>) -> (Div, f32) {
    let project = model.state.current_project();
    let available =
        !model.command_center.active && project.status() == muxy_app_core::ProjectStatus::Available;
    let existing_count = if available {
        model.existing_terminal_count()
    } else {
        0
    };
    let shown = |key| !model.appearance.hidden_titlebar_buttons.contains(key);
    let existing_count = if shown(EXISTING) { existing_count } else { 0 };
    let browser = available && model.browser_enabled() && shown(BROWSER);
    let layouts = available && model.has_project_layouts() && shown(LAYOUTS);
    let zoom_tab = project.tabs.iter().find(|tab| {
        available
            && !model.appearance.pane_headers
            && shown(ZOOM)
            && Some(tab.id) == model.active_tab()
            && (tab.zoomed.is_some() || tab.panes.len() > 1)
    });
    let settings = !model.sidebar_footer_shown();
    let mut split = available && model.active_pane().is_some() && shown(SPLIT);
    let sessions = !model.command_center.active && shown(SESSIONS);
    let notifications = shown(NOTIFICATIONS);
    let controls = u8::from(sessions)
        + u8::from(settings)
        + u8::from(settings && model.fork_update_release().is_some())
        + 2 * u8::from(split)
        + u8::from(existing_count > 0)
        + u8::from(browser)
        + u8::from(zoom_tab.is_some())
        + u8::from(layouts);
    let mut width = f32::from(controls)
        * f32::from(model.metrics.control_medium() + model.metrics.spacing2())
        + model.extension_toolbar_width();
    #[cfg(target_os = "linux")]
    let window_controls = linux::controls_width(window, model.metrics);
    #[cfg(not(target_os = "linux"))]
    let window_controls = 0.0;
    if notifications {
        width += f32::from(model.metrics.control_medium() + model.metrics.spacing2());
    }
    let room = f32::from(content_size(window).width)
        - navigation_width(model, window).max(model.sidebar_width())
        - width
        - window_controls;
    let mut project_apps = super::project_apps::control(model, room, window, cx);
    let minimum_tabs =
        44.0 + f32::from(model.metrics.control_medium() + model.metrics.spacing2() * 2.0);
    if split && room - project_apps.as_ref().map_or(0.0, |(_, used)| *used) < minimum_tabs {
        // Pane split controls remain available when the titlebar needs room for a tab.
        split = false;
        let recovered = 2.0 * f32::from(model.metrics.control_medium() + model.metrics.spacing2());
        width -= recovered;
        project_apps = super::project_apps::control(model, room + recovered, window, cx);
    }
    let project_apps = project_apps.map(|(control, used)| {
        width += used;
        control
    });
    let bar = div()
        .debug_selector(|| "titlebar-actions".into())
        .flex()
        .flex_none()
        .items_center()
        .when(cfg!(target_os = "linux"), Styled::h_full)
        .when(notifications, |bar| {
            bar.child(notifications_button(model, cx))
        })
        .children(project_apps)
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|model, event: &gpui::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                let items = model.titlebar_button_items();
                model.open_menu(items, event.position, window, cx);
            }),
        )
        .when(existing_count > 0, |bar| {
            bar.child(super::tab_strip::existing_terminals_button(
                existing_count,
                model,
                cx,
            ))
        })
        .when(browser, |bar| {
            bar.child(super::tab_strip::browser_button(model, cx))
        })
        .when(split, |bar| {
            bar.child(super::tab_strip::split_button(
                None,
                muxy_app_core::Direction::Right,
                model,
                cx,
            ))
            .child(super::tab_strip::split_button(
                None,
                muxy_app_core::Direction::Down,
                model,
                cx,
            ))
        })
        .when_some(zoom_tab, |bar, tab| {
            bar.child(super::tab_strip::zoom_control(
                tab.zoomed.is_some(),
                model,
                cx,
            ))
        })
        .when(layouts, |bar| {
            bar.child(super::project_layouts::button(model, cx))
        })
        .when(!model.command_center.active, |bar| {
            bar.child(model.extension_toolbar(cx))
        })
        .when(sessions, |bar| {
            bar.child(super::agent_sessions::button(model, cx))
        })
        .when(settings, |bar| {
            bar.children(
                super::sidebar::fork_update_button(model, cx)
                    .map(|button| button.size(model.metrics.control_medium())),
            )
            .child(super::tab_strip::settings_button(model, cx))
        });
    #[cfg(target_os = "linux")]
    if let Some(controls) = linux::controls(
        window,
        &model.theme,
        model.metrics,
        cx.listener(|model, _, _, cx| model.quit(cx)),
    ) {
        return (
            bar.child(controls.border_l_1().border_color(model.theme.border)),
            width + linux::controls_width(window, model.metrics) + 1.0,
        );
    }
    (bar, width)
}

const EXISTING: &str = "existing_terminals";
const BROWSER: &str = "browser";
const SPLIT: &str = "split";
const ZOOM: &str = "zoom";
const LAYOUTS: &str = "layouts";
const SESSIONS: &str = "agent_sessions";
const NOTIFICATIONS: &str = "notifications";

fn notifications_button(model: &AppModel, cx: &mut Context<AppModel>) -> Stateful<Div> {
    let snoozed = model.notifications_snoozed();
    let m = model.metrics;
    let color = if snoozed {
        model.theme.warning
    } else {
        model.theme.diff_add
    };
    with_tooltip(
        div()
            .id("snooze-notifications")
            .flex()
            .flex_none()
            .items_center()
            .size(m.control_medium())
            .justify_center()
            .mr(m.spacing2())
            .rounded(m.radius_md())
            .border_1()
            .border_color(color.opacity(0.3))
            .bg(color.opacity(0.1))
            .cursor_pointer()
            .hover(move |style| style.bg(color.opacity(0.18)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(IconGlyph::new(
                if snoozed { Icon::Pause } else { Icon::Bell },
                m.icon_md(),
                color,
            )),
        if snoozed {
            tr!("Muxy notifications snoozed")
        } else {
            tr!("Snooze Muxy notifications")
        },
        model,
    )
    .debug_selector(|| "snooze-notifications".into())
    .on_click(cx.listener(|model, event: &gpui::ClickEvent, window, cx| {
        use super::menu::{Command, Item};
        use std::time::Duration;

        cx.stop_propagation();
        let mut items = vec![
            Item::action(
                tr!("Snooze for 30 minutes"),
                Command::SnoozeNotifications(Some(Duration::from_secs(30 * 60))),
            ),
            Item::action(
                tr!("Snooze for 1 hour"),
                Command::SnoozeNotifications(Some(Duration::from_secs(60 * 60))),
            ),
        ];
        if model.notifications_snoozed() {
            items.push(Item::action(
                tr!("Resume notifications"),
                Command::SnoozeNotifications(None),
            ));
        }
        model.open_menu(items, event.position(), window, cx);
    }))
}

impl AppModel {
    /// Every titlebar button that can be hidden, by key and name. Settings
    /// always stays, since its menu brings the others back.
    fn titlebar_buttons(&self) -> Vec<(String, SharedString)> {
        let mut buttons: Vec<(String, SharedString)> = vec![
            (EXISTING.into(), tr!("Existing Terminals")),
            (BROWSER.into(), tr!("Browser")),
            (SPLIT.into(), tr!("Split Buttons")),
            (ZOOM.into(), tr!("Zoom")),
            (LAYOUTS.into(), tr!("Layouts")),
        ];
        buttons.extend(self.topbar_items().into_iter().map(|item| {
            (
                format!("top:{}:{}", item.owner, item.id),
                item.tooltip.into(),
            )
        }));
        buttons.push((SESSIONS.into(), tr!("Agent Sessions")));
        buttons.push((NOTIFICATIONS.into(), tr!("Notifications")));
        buttons
    }

    pub(crate) fn titlebar_button_items(&self) -> Vec<super::menu::Item> {
        self.titlebar_buttons()
            .into_iter()
            .enumerate()
            .map(|(index, (key, label))| {
                super::menu::Item::action(label, super::menu::Command::ToggleTitlebarButton(index))
                    .checked_if(!self.appearance.hidden_titlebar_buttons.contains(&key))
            })
            .collect()
    }

    pub(crate) fn toggle_titlebar_button(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((key, _)) = self.titlebar_buttons().into_iter().nth(index) else {
            return;
        };
        let hidden = &mut self.appearance.hidden_titlebar_buttons;
        if !hidden.remove(&key) {
            hidden.insert(key);
        }
        self.save_appearance(cx);
        cx.notify();
    }
}

#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = muxy, no_json)]
pub(crate) struct BeginWindowMove;

pub(crate) fn begin_window_move(
    model: &mut AppModel,
    _: &BeginWindowMove,
    window: &mut Window,
    _: &mut Context<AppModel>,
) {
    #[cfg(target_os = "macos")]
    let _ = &window;
    if !model.window_can_move() {
        return;
    }
    #[cfg(test)]
    {
        model.window_moves_begun += 1;
    }
    #[cfg(target_os = "macos")]
    if let Some(drag) = &model.window_drag {
        drag.begin();
    }
    #[cfg(not(target_os = "macos"))]
    start_window_move(window);
}

/// The test platform has no window to move.
#[cfg(not(target_os = "macos"))]
fn start_window_move(window: &Window) {
    #[cfg(not(test))]
    window.start_window_move();
    #[cfg(test)]
    let _ = window;
}

pub(crate) fn background(id: &'static str) -> Stateful<Div> {
    background_with(id, |_, window, cx| {
        window.dispatch_action(Box::new(BeginWindowMove), cx);
    })
}

/// Empty space that behaves like the titlebar's, with `begin_move` called for
/// a single press.
pub(crate) fn background_with(
    id: &'static str,
    begin_move: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let background = div()
        .id(id)
        .debug_selector(move || id.into())
        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
            cx.stop_propagation();
            if event.click_count == 1 {
                begin_move(event, window, cx);
            }
        })
        .on_click(background_click);
    #[cfg(target_os = "linux")]
    let background = background.on_mouse_down(MouseButton::Right, |event, window, cx| {
        cx.stop_propagation();
        if window.window_controls().window_menu {
            window.show_window_menu(event.position);
        }
    });
    background
}

fn background_click(event: &gpui::ClickEvent, window: &mut Window, cx: &mut App) {
    if event.click_count() == 2 {
        cx.stop_propagation();
        window.dispatch_action(Box::new(super::workspace::Zoom), cx);
    }
}

pub(crate) fn action_button(
    id: impl Into<ElementId>,
    tooltip: impl Into<SharedString>,
    model: &AppModel,
) -> Stateful<Div> {
    let m = model.metrics;
    let hover = model.theme.hover;
    let tooltip = tooltip.into();
    with_tooltip(
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(tooltip.clone())
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(m.control_medium())
            .mr(m.spacing2())
            .rounded(m.radius_sm())
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()),
        tooltip,
        model,
    )
}

pub(super) fn with_tooltip(
    element: Stateful<Div>,
    tooltip: impl Into<SharedString>,
    model: &AppModel,
) -> Stateful<Div> {
    element.tooltip(muxy_ui::components::tooltip(tooltip, &model.theme))
}

pub(super) fn icon_button(
    id: impl Into<ElementId>,
    icon: Icon,
    tooltip: impl Into<SharedString>,
    model: &AppModel,
) -> Stateful<Div> {
    scaled_icon_button(id, icon, 1.0, tooltip, model)
}

/// An `icon_button` with the button and its icon scaled by `factor`.
pub(super) fn scaled_icon_button(
    id: impl Into<ElementId>,
    icon: Icon,
    factor: f32,
    tooltip: impl Into<SharedString>,
    model: &AppModel,
) -> Stateful<Div> {
    let m = model.metrics;
    action_button(id, tooltip, model)
        .size(px((f32::from(m.control_medium()) * factor).round()))
        .child(IconGlyph::new(icon, m.icon_md(), model.theme.fg_muted).scaled(factor))
}

pub(crate) fn navigation(
    model: &AppModel,
    sidebar_width: f32,
    window: &Window,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let m = model.metrics;
    let theme = &model.theme;
    let toggle = sidebar_toggle(model, cx);
    background("titlebar-navigation")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .w(px(sidebar_width.max(navigation_width(model, window))))
        .h(m.title_bar_height())
        .flex()
        .items_center()
        .justify_end()
        .pr(m.spacing4())
        .bg(if model.appearance.sidebar_expanded {
            gpui::transparent_black()
        } else {
            theme.bg
        })
        // Past the sidebar, the buttons run straight into the first tab.
        .when(model.command_center.active, |bar| {
            bar.border_b_1().border_color(theme.border)
        })
        .when(sidebar_width >= navigation_width(model, window), |bar| {
            bar.border_r_1().border_color(theme.border)
        })
        .map(|bar| top_corners(bar, window, true, false))
        .map(|bar| {
            if cfg!(target_os = "linux") {
                bar.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .w(model.collapsed_sidebar_width())
                        .child(toggle),
                )
                .when(model.appearance.sidebar_expanded, |bar| {
                    bar.child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .text_size(m.font_body())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.fg_muted)
                            .child("Muxy"),
                    )
                })
                .child(arrow(model, false, cx))
                .child(arrow(model, true, cx))
            } else {
                bar.gap(m.spacing1())
                    .child(arrow(model, false, cx))
                    .child(arrow(model, true, cx))
                    .child(toggle)
            }
        })
        .into_any_element()
}

fn sidebar_toggle(model: &AppModel, cx: &mut Context<AppModel>) -> Div {
    let m = model.metrics;
    let theme = &model.theme;
    div()
        .debug_selector(|| "sidebar-toggle".into())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            muxy_ui::components::IconButton::new(
                "toggle-sidebar",
                tr!("Toggle Sidebar"),
                Icon::PanelLeft,
                m.font_body(),
                m.scaled(22.0),
                theme.fg_muted,
                theme.fg,
            )
            .expanded(model.appearance.sidebar_expanded)
            .tooltip(theme)
            .shortcut(model.shortcut_label(ShortcutId::ToggleSidebar))
            .on_click(cx.listener(|model, _, window, cx| {
                cx.stop_propagation();
                model.toggle_sidebar(window, cx);
            })),
        )
}

/// Where the macOS traffic lights sit: centered in the title bar, on the
/// nearest device pixel.
pub(crate) fn traffic_light_position(metrics: Metrics, scale_factor: f32) -> Point<Pixels> {
    const INSET: f32 = 9.0;
    let growth = metrics.title_bar_height() - Metrics::new(1.0).title_bar_height();
    let y = INSET + f32::from(growth) / 2.0;
    point(px(INSET), px((y * scale_factor).round() / scale_factor))
}

pub(crate) fn initial_traffic_lights(
    appearance: &muxy_app_core::settings::Appearance,
) -> Point<Pixels> {
    traffic_light_position(Metrics::new(appearance.interface_size.multiplier()), 1.0)
}

pub(super) fn navigation_width(model: &AppModel, _: &Window) -> f32 {
    if cfg!(target_os = "linux") {
        return f32::from(model.collapsed_sidebar_width() + model.metrics.scaled(52.0));
    }
    let arrows = model.metrics.navigation_arrows_width();
    f32::from(if cfg!(target_os = "macos") {
        model.metrics.traffic_light_width() + arrows
    } else {
        arrows
    })
}

fn arrow(model: &AppModel, forward: bool, cx: &mut Context<AppModel>) -> AnyElement {
    let m = model.metrics;
    let theme = &model.theme;
    let enabled = model.can_navigate(forward);
    let id = if forward { "nav-forward" } else { "nav-back" };
    let color = if enabled {
        theme.fg_muted
    } else {
        gpui::Hsla {
            a: theme.fg_muted.a * 0.35,
            ..theme.fg_muted
        }
    };
    let glyph = IconGlyph::new(
        if forward {
            Icon::ChevronRight
        } else {
            Icon::ChevronLeft
        },
        m.font_body(),
        color,
    );
    let arrow = div()
        .id(id)
        .role(Role::Button)
        .accessibility_id(id)
        .aria_label(if forward { tr!("Forward") } else { tr!("Back") })
        .aria_disabled(!enabled)
        .debug_selector(move || id.into())
        .group(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(m.scaled(22.0))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    let arrow = with_tooltip(
        arrow,
        if forward {
            model.tooltip(tr!("Forward"), ShortcutId::NavigateForward)
        } else {
            model.tooltip(tr!("Back"), ShortcutId::NavigateBack)
        },
        model,
    );
    if enabled {
        arrow
            .cursor_pointer()
            .on_click(cx.listener(move |model, _, _, cx| {
                cx.stop_propagation();
                model.navigate(forward, cx);
            }))
            .child(glyph.hover_in_group(id, theme.fg))
            .into_any_element()
    } else {
        arrow
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(glyph)
            .into_any_element()
    }
}

#[cfg(test)]
mod traffic_light_tests {
    use super::*;

    #[test]
    fn traffic_lights_stay_centered_as_the_title_bar_grows() {
        for (multiplier, scale_factor, y) in [
            (1.0, 2.0, 9.0),
            (1.12, 2.0, 11.0),
            (1.24, 2.0, 13.0),
            (1.40, 2.0, 15.5),
            (1.40, 1.0, 15.0),
        ] {
            assert_eq!(
                traffic_light_position(Metrics::new(multiplier), scale_factor),
                point(px(9.0), px(y)),
            );
        }
    }
}
