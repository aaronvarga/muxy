use crate::model::AppModel;
use crate::model::chat::ChatToggle;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, InteractiveElement, IntoElement, MouseButton, ParentElement, Role, SharedString,
    StatefulInteractiveElement, Styled, Window, div, svg,
};
use muxy_app_core::PaneId;
use muxy_core::shortcuts::ShortcutId;
use muxy_ui::{icon::Icon, tr};
use std::rc::Rc;

/// Fork: the CLI|Chat switch, with the current view highlighted.
pub(crate) fn mode_switch(
    id: PaneId,
    model: &AppModel,
    owner: &gpui::WeakEntity<AppModel>,
    cx: &gpui::App,
) -> Option<AnyElement> {
    let toggle = model.chat_toggle(id, cx)?;
    let owner = owner.clone();
    Some(mode_control(
        id.to_string().into(),
        toggle,
        model,
        model.settings.keymap.chord(ShortcutId::ToggleChat),
        move |chat, window, cx| {
            let _ = owner.update(cx, |model, cx| {
                if model
                    .chat_toggle(id, cx)
                    .is_some_and(|toggle| toggle.enabled && toggle.chat != chat)
                {
                    model.toggle_chat(id, cx);
                }
                model.focus_active(window, cx);
            });
        },
    ))
}

pub(crate) fn mode_control(
    id: SharedString,
    toggle: ChatToggle,
    model: &AppModel,
    shortcut: Option<&muxy_app_core::settings::KeyChord>,
    on_select: impl Fn(bool, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let foreground = model.theme.fg_muted;
    let m = model.metrics;
    let hover = foreground.opacity(0.12);
    let on_select = Rc::new(on_select);
    let segment = |name: &'static str, icon: Icon, label: SharedString, selected: bool| {
        let chat = name != "show-console";
        let tooltip =
            shortcut.map_or_else(|| label.to_string(), |chord| format!("{label} ({chord})"));
        let on_select = on_select.clone();
        let selector = SharedString::from(format!("chat-{name}-{id}"));
        let element = div()
            .id(selector.clone())
            .debug_selector(move || selector.to_string())
            .role(Role::Button)
            .aria_label(label)
            .aria_selected(selected)
            .aria_disabled(!toggle.enabled)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(m.control_small())
            .rounded(m.radius_sm())
            .when(selected, |button| {
                button.bg(model.theme.accent.opacity(0.22))
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .when(toggle.enabled, |button| {
                button
                    .cursor_pointer()
                    .when(!selected, |button| {
                        button.hover(move |style| style.bg(hover))
                    })
                    .on_click(move |_, window: &mut Window, cx| {
                        cx.stop_propagation();
                        on_select(chat, window, cx);
                    })
            })
            .child(
                svg()
                    .path(icon.path())
                    .size(m.icon_md())
                    .text_color(if selected {
                        model.theme.accent
                    } else {
                        foreground.opacity(0.55)
                    }),
            );
        super::super::titlebar::with_tooltip(element, tooltip, model)
    };
    let switch = div()
        .flex()
        .items_center()
        .rounded(m.radius_sm())
        .border_1()
        .border_color(foreground.opacity(0.2))
        .child(segment(
            "show-console",
            Icon::Terminal,
            tr!("CLI"),
            !toggle.chat,
        ))
        .child(if toggle.start {
            segment("start", Icon::Chat, tr!("New Chat"), false)
        } else {
            segment("show-chat", Icon::Chat, tr!("Chat"), toggle.chat)
        });
    switch
        .flex_none()
        .debug_selector(move || format!("chat-switch-{id}"))
        .when(!toggle.enabled, |switch| switch.opacity(0.4))
        .into_any_element()
}
