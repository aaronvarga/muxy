use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};
use muxy_protocol::AgentProvider;
use muxy_ui::components::{ButtonInteraction, IconGlyph};
use muxy_ui::icon::Icon;
use muxy_ui::tr;

use super::menu::{Command, Item};
use crate::model::AppModel;

/// A row below the tabs with a button per custom command, each running it in
/// a split beside the active pane, and a menu choosing which commands it shows.
pub(crate) fn commands_bar(model: &AppModel, cx: &mut Context<AppModel>) -> Option<AnyElement> {
    let commands = &model.settings.commands;
    if !model.appearance.commands_bar_visible {
        return None;
    }
    let m = model.metrics;
    let theme = &model.theme;
    let glyph = |icon, group: &SharedString| {
        // A 14pt symbol scaled to body size; a 12pt symbol is drawn heavier.
        IconGlyph::new(icon, m.icon_md(), theme.fg_muted)
            .scaled(f32::from(m.font_body()) / f32::from(m.icon_md()))
            .hover_in_group(group.clone(), theme.fg)
            .into_any_element()
    };
    let button = |id: SharedString, icon: AnyElement, label: Option<SharedString>| {
        let group = id.clone();
        div()
            .id(id.clone())
            .debug_selector(move || id.to_string())
            .group(group.clone())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .h(m.control_medium())
            .px(px(8.0))
            .rounded(m.radius_md())
            .cursor_pointer()
            .hover(|style| style.bg(theme.hover).text_color(theme.fg))
            .child(icon)
            .children(label)
    };
    let menu_id = SharedString::from("commands-bar-menu");
    let menu = button(menu_id.clone(), glyph(Icon::Settings, &menu_id), None)
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(|model, event: &ClickEvent, window, cx| {
            cx.stop_propagation();
            model.open_menu(menu_items(model), event.position(), window, cx);
        }));
    Some(
        div()
            .debug_selector(|| "commands-bar".into())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .h(m.tab_bar_height())
            .px(px(6.0))
            .overflow_hidden()
            .bg(background(model))
            .border_b_1()
            .border_color(theme.border)
            .text_size(m.font_footnote())
            .text_color(theme.fg_muted)
            .child(menu)
            .children(
                commands
                    .iter()
                    .filter(|command| !command.hidden)
                    .map(|command| {
                        let id = command.id.clone();
                        let element = SharedString::from(format!("commands-bar-{id}"));
                        let icon = agent(command).map_or_else(
                            || glyph(Icon::Terminal, &element),
                            |agent| super::tab_activity::provider_icon(agent, m.icon_md(), model),
                        );
                        button(element, icon, Some(command.name.clone().into())).button_interaction(
                            cx.listener(move |model, _, window, cx| {
                                model.run_custom_command_in_split(&id, cx);
                                model.focus_active(window, cx);
                            }),
                        )
                    }),
            )
            .into_any_element(),
    )
}

/// The bar's background, which the selected tab above it also uses; the
/// content's background when the bar is hidden.
pub(crate) fn background(model: &AppModel) -> gpui::Hsla {
    let bg = model.theme.bg;
    if model.appearance.commands_bar_visible {
        gpui::Hsla {
            l: (bg.l - 0.025).max(0.0),
            ..bg
        }
    } else {
        bg
    }
}

/// The agent a command starts, so its button shows the agent's logo: the
/// program it runs, else the first word of its name, for commands run
/// through an alias.
pub(super) fn agent(command: &muxy_app_core::settings::CustomCommand) -> Option<AgentProvider> {
    let program = command
        .command
        .split_whitespace()
        .find(|word| !word.contains('='))
        .and_then(|word| word.rsplit('/').next());
    let name = command.name.split_whitespace().next();
    [program, name].into_iter().flatten().find_map(|word| {
        Some(match word.to_lowercase().as_str() {
            "claude" => AgentProvider::Claude,
            "codex" => AgentProvider::Codex,
            "opencode" => AgentProvider::OpenCode,
            "cursor" | "cursor-agent" => AgentProvider::Cursor,
            "copilot" => AgentProvider::Copilot,
            "droid" => AgentProvider::Droid,
            "pi" => AgentProvider::Pi,
            "grok" => AgentProvider::Grok,
            "kiro" | "kiro-cli" => AgentProvider::Kiro,
            "omp" => AgentProvider::OhMyPi,
            _ => return None,
        })
    })
}

fn menu_items(model: &AppModel) -> Vec<Item> {
    let mut items: Vec<_> = model
        .settings
        .commands
        .iter()
        .enumerate()
        .map(|(index, command)| {
            Item::action(command.name.clone(), Command::ToggleCommandInBar(index))
                .checked_if(!command.hidden)
        })
        .collect();
    items.push(
        Item::action(tr!("Show Commands Bar"), Command::CommandsBar(false))
            .checked_if(true)
            .separated(),
    );
    items.push(Item::action(tr!("Manage Commands…"), Command::ManageCommands).separated());
    items
}

#[cfg(test)]
mod tests {
    use muxy_app_core::settings::CustomCommand;
    use muxy_protocol::AgentProvider;

    #[test]
    fn commands_show_the_agent_they_start() {
        let agent = |name: &str, command: &str| {
            super::agent(&CustomCommand::new(name.into(), command.into()))
        };
        assert_eq!(
            agent("Agent", "FOO=1 /opt/bin/claude --resume"),
            Some(AgentProvider::Claude)
        );
        assert_eq!(agent("Codex", "cx"), Some(AgentProvider::Codex));
        assert_eq!(agent("Terminal", ""), None);
        assert_eq!(agent("Tests", "cargo test"), None);
    }
}
