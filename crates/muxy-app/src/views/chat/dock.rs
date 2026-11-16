//! Fork: the input docked at the bottom of a chat: a large multi-line box
//! whose footer holds the model and effort picker, the console composer's
//! tools (attach, screenshot, quick commands, more, clear) and the
//! send button, which turns into Stop while Claude works.

use std::ops::Range;
use std::path::{Path, PathBuf};

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, AppContext, Context, Focusable, FontWeight, InteractiveElement, IntoElement,
    MouseButton, ParentElement, PathPromptOptions, SharedString, StatefulInteractiveElement,
    Styled, Window, canvas, deferred, div, px,
};
use muxy_core::shortcuts::ShortcutId;
use muxy_protocol::{AgentProvider, ChatCommand, ChatImage, ChatImageType, ChatMode, ChatProvider};
use muxy_ui::components::{ButtonInteraction, IconGlyph, SymbolGlyph, Tooltip};
use muxy_ui::controls;
use muxy_ui::icon::Icon;
use muxy_ui::l10n::{tr_key, translate};
use muxy_ui::native_menu::Item;
use muxy_ui::popover::{self, PopoverAnchor};
use muxy_ui::tr;

use super::ChatView;
use crate::model::AppModel;
use crate::model::chat::ChatSession;

pub(crate) fn image_chip(
    id: &str,
    name: &str,
    enabled: bool,
    style: controls::Style,
    preview: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    remove: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    muxy_ui::components::attachment_chip(
        id,
        name,
        &tr!("Preview %@", name),
        enabled,
        style,
        move |event, window, cx| {
            if cfg!(target_os = "macos") {
                preview(event, window, cx);
            }
        },
        remove,
    )
    .into_any_element()
}

/// Lines the input shows before it scrolls, and at least.
const MAX_LINES: f32 = 12.0;
const MIN_LINES: f32 = 3.0;

/// Slash commands the list keeps, and the rows it shows before it scrolls.
const SLASH_MATCHES: usize = 64;
const SLASH_ROWS: f32 = 8.0;

/// Commands only Claude Code's TUI runs. Muxy doesn't send one the chat's list leaves out,
/// since headless Claude would only answer that it isn't available.
const TUI_COMMANDS: &[&str] = &[
    "resume",
    "login",
    "logout",
    "exit",
    "quit",
    "vim",
    "terminal-setup",
    "ide",
    "bug",
    "feedback",
    "release-notes",
    "status",
    "permissions",
    "hooks",
    "memory",
    "export",
    "rewind",
    "add-dir",
    "theme",
    "statusline",
    "privacy-settings",
    "upgrade",
    "install-github-app",
    // `terminal_slash_commands` in Claude Code's `system/init`.
    "doctor",
    "color",
    "focus",
    "reload-plugins",
];

/// The slash commands matching the `/word` the input starts with.
#[derive(Default)]
pub(super) struct Slash {
    /// Best first, at most `SLASH_MATCHES`; refiltered only when the text changes.
    matches: Vec<ChatCommand>,
    selected: usize,
    /// Escape closed the list until the word goes away.
    dismissed: bool,
}

/// The `/word` the input starts with, slash included, while nothing follows it.
// shortcut: ignores the caret, so the list stays while it moves back into the word; check
// the caret too if that confuses.
fn slash_word(text: &str) -> Option<Range<usize>> {
    let start = text.len() - text.trim_start().len();
    let word = &text[start..];
    (word.starts_with('/') && !word.contains(char::is_whitespace)).then_some(start..text.len())
}

/// Indices of the commands matching `query`, best first: names starting with it, then
/// namespaced ones whose part after a `:` does, then any containing it; ignoring case.
fn rank_commands(commands: &[ChatCommand], query: &str) -> Vec<usize> {
    let query = query.to_lowercase();
    let mut ranked: Vec<(u8, usize)> = commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| {
            let name = command.name.to_lowercase();
            let rank = if name.starts_with(&query) {
                0
            } else if name.split(':').skip(1).any(|part| part.starts_with(&query)) {
                1
            } else if name.contains(&query) {
                2
            } else {
                return None;
            };
            Some((rank, index))
        })
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, index)| index).collect()
}

/// What Muxy does itself instead of sending a prompt headless Claude can't run.
#[derive(Debug, PartialEq)]
enum Local {
    /// `/resume`: the past sessions panel.
    Resume,
    /// `/model` or `/effort` without an argument: the model and effort picker.
    Picker,
    /// A TUI-only command, by name.
    ConsoleOnly(String),
}

/// What to do with `text` instead of sending it, when it is one slash command. With no
/// `commands` yet, only `/resume` and the bare picker commands are kept back.
fn local_command(text: &str, commands: &[ChatCommand]) -> Option<Local> {
    let rest = text.trim().strip_prefix('/')?;
    let (name, args) = rest
        .split_once(char::is_whitespace)
        .map_or((rest, ""), |(name, args)| (name, args.trim()));
    let listed = commands.iter().any(|command| command.name == name);
    match name {
        "model" | "effort" if args.is_empty() => Some(Local::Picker),
        "resume" if !listed && args.is_empty() => Some(Local::Resume),
        _ if !commands.is_empty() && !listed && TUI_COMMANDS.contains(&name) => {
            Some(Local::ConsoleOnly(name.to_owned()))
        }
        _ => None,
    }
}

/// Keep references outside the editable text until submission.
fn prompt_text(text: &str, files: &[(String, String)]) -> String {
    std::iter::once(text.trim())
        .chain(files.iter().map(|(_, mention)| mention.as_str()))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `images` fit one prompt.
// Mirrors the composer's chat check, which is private to its module.
pub(crate) fn images_fit(images: &[ChatImage]) -> bool {
    images.len() <= muxy_protocol::MAX_CHAT_IMAGES
        && images.iter().map(|image| image.bytes.len()).sum::<usize>()
            <= muxy_protocol::MAX_CHAT_IMAGES_BYTES
        && images
            .iter()
            .all(|image| image.bytes.len() <= muxy_protocol::MAX_CHAT_IMAGE_BYTES)
}

pub(crate) fn copied_image(bytes: Vec<u8>) -> Option<ChatImage> {
    let bytes = match image_type(&bytes) {
        Some(_) => bytes,
        None => muxy_app_core::composer::image_storage::normalize_png(&bytes).ok()?,
    };
    Some(ChatImage {
        mime: image_type(&bytes)?,
        bytes,
    })
}

fn image_type(bytes: &[u8]) -> Option<ChatImageType> {
    [
        ChatImageType::Png,
        ChatImageType::Jpeg,
        ChatImageType::Gif,
        ChatImageType::Webp,
    ]
    .into_iter()
    .find(|kind| kind.matches(bytes))
}

/// A picked file: an image sent with the prompt, or any other file or folder
/// named in the text for Claude to read.
#[derive(Debug, PartialEq)]
enum Attachment {
    Image(String, ChatImage),
    /// `None` when the path isn't absolute UTF-8, so it can't be named.
    Mention(Option<(String, String)>),
}

/// The `More` menu's actions.
#[derive(Clone, Copy)]
enum MenuAction {
    Clear,
    Console,
    ManageQuickCommands,
}

impl ChatView {
    /// Sends the input to this pane's session; the text stays until the server takes it.
    pub(super) fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.input.read(cx).text().to_owned();
        let text = prompt_text(&snapshot, &self.files);
        if self.sending || (text.is_empty() && self.images.is_empty()) {
            return;
        }
        if self.images.is_empty() && self.files.is_empty() && self.run_locally(&text, window, cx) {
            return;
        }
        let images: Vec<ChatImage> = self.images.iter().map(|(_, image)| image.clone()).collect();
        let revision = self.last_edit;
        let pane = self.pane;
        let Ok(send) = self.owner.update(cx, |model, cx| {
            model.chat_send(pane, text.clone(), images, cx)
        }) else {
            return;
        };
        self.sending = true;
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = send.await;
            let _ = view.update(cx, |view, cx| {
                view.sending = false;
                match result {
                    Ok(()) => {
                        // An edited snapshot stays intact, including removed-and-reattached images.
                        if view.last_edit == revision && view.input.read(cx).text() == snapshot {
                            view.input.update(cx, |input, cx| input.set_text("", cx));
                            view.images.clear();
                            view.files.clear();
                        }
                    }
                    Err(error) => {
                        let _ = view.owner.update(cx, |model, cx| {
                            model.show_notice(
                                tr!("Delivery not confirmed: %@ The draft is kept.", &error)
                                    .to_string(),
                                cx,
                            );
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Handles a slash command headless Claude can't run; false sends `text` as it is.
    fn run_locally(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(owner) = self.owner.upgrade() else {
            return false;
        };
        let local = {
            let model = owner.read(cx);
            let commands = model
                .chat
                .sessions
                .get(&self.key)
                .map_or(&[][..], |session| &session.capabilities.commands);
            local_command(text, commands)
        };
        match local {
            None => return false,
            Some(Local::Picker) => self.picker_open = true,
            Some(Local::Resume) => owner.update(cx, |model, cx| {
                if !model.appearance.agent_sessions_visible {
                    model.toggle_agent_sessions(window, cx);
                }
            }),
            Some(Local::ConsoleOnly(name)) => {
                let notice = owner
                    .read(cx)
                    .settings
                    .keymap
                    .chord(ShortcutId::ToggleChat)
                    .map_or_else(
                        || {
                            tr!(
                                "/%@ only works in Console. Switch to Console from the More menu.",
                                &name
                            )
                        },
                        |chord| {
                            tr!(
                                "/%@ only works in Console. Press %@ to switch.",
                                &name,
                                chord.to_string()
                            )
                        },
                    );
                self.notice(notice.to_string(), cx);
                // Kept, to run in Console.
                return true;
            }
        }
        self.input.update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
        true
    }

    /// Refilters the slash commands for the input's text.
    pub(super) fn refilter_slash(&mut self, cx: &mut Context<Self>) {
        let was_open = self.slash_open();
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let text = self.input.read(cx).text();
        match slash_word(text) {
            None => self.slash = Slash::default(),
            Some(word) => {
                let model = owner.read(cx);
                let commands = model
                    .chat
                    .sessions
                    .get(&self.key)
                    .map_or(&[][..], |session| &session.capabilities.commands);
                self.slash.matches = rank_commands(commands, &text[word.start + 1..])
                    .into_iter()
                    .take(SLASH_MATCHES)
                    .map(|index| commands[index].clone())
                    .collect();
                self.slash.selected = 0;
                self.slash_scroll.scroll_to_item(0);
            }
        }
        if was_open || self.slash_open() {
            cx.notify();
        }
    }

    pub(super) fn slash_open(&self) -> bool {
        !self.slash.dismissed && !self.slash.matches.is_empty()
    }

    /// Replaces the `/word` with the chosen command; false when no list is open.
    pub(super) fn accept_slash(&mut self, index: Option<usize>, cx: &mut Context<Self>) -> bool {
        if !self.slash_open() {
            return false;
        }
        let Some(command) = self.slash.matches.get(index.unwrap_or(self.slash.selected)) else {
            return false;
        };
        let text = format!("/{} ", command.name);
        self.input.update(cx, |input, cx| {
            if let Some(word) = slash_word(input.text()) {
                input.replace_range(word, &text, cx);
            }
        });
        true
    }

    /// Whether the word typed is exactly the highlighted command's name, so Enter has
    /// nothing left to complete.
    pub(super) fn slash_typed_exactly(&self, cx: &gpui::App) -> bool {
        let text = self.input.read(cx).text();
        slash_word(text).is_some_and(|word| {
            self.slash_open()
                && self
                    .slash
                    .matches
                    .get(self.slash.selected)
                    .is_some_and(|command| command.name == text[word.start + 1..])
        })
    }

    /// Moves the highlight, wrapping; false when no list is open.
    pub(super) fn move_slash(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        if !self.slash_open() {
            return false;
        }
        let count = self.slash.matches.len().cast_signed();
        self.slash.selected = (self.slash.selected.cast_signed() + delta)
            .rem_euclid(count)
            .cast_unsigned();
        self.slash_scroll.scroll_to_item(self.slash.selected);
        cx.notify();
        true
    }

    pub(super) fn dismiss_slash(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.slash_open() {
            return false;
        }
        self.slash.dismissed = true;
        cx.notify();
        true
    }

    /// The commands matching the `/word`, in the console composer's completion style.
    fn slash_list(&self, model: &AppModel, cx: &Context<Self>) -> AnyElement {
        let m = model.metrics;
        let theme = &model.theme;
        let row = m.scaled(28.0);
        let rows = self
            .slash
            .matches
            .iter()
            .enumerate()
            .map(|(index, command)| {
                div()
                    .id(("chat-slash", index))
                    .debug_selector(move || format!("chat-slash-{index}"))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(m.spacing4())
                    .h(row)
                    .px(m.spacing4())
                    .rounded(m.radius_sm())
                    .when(index == self.slash.selected, |row| row.bg(theme.hover))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |view, _, window, cx| {
                            cx.stop_propagation();
                            view.accept_slash(Some(index), cx);
                            view.focus_input(window, cx);
                        }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(muxy_ui::font::monospace())
                            .text_size(m.font_footnote())
                            .text_color(theme.fg)
                            .child(format!("/{}", command.name)),
                    )
                    .when(command.skill, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .px(m.spacing2())
                                .rounded(m.radius_sm())
                                .border_1()
                                .border_color(theme.border)
                                .text_size(m.font_xs())
                                .text_color(theme.fg_dim)
                                .child(tr!("skill")),
                        )
                    })
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(m.font_footnote())
                            .text_color(theme.fg_muted)
                            .child(command.description.clone()),
                    )
            });
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom_full()
            .mb(m.spacing2())
            .child(
                div()
                    .id(SharedString::from(format!("chat-slash-{}", self.pane)))
                    .debug_selector(|| "chat-slash".into())
                    .flex()
                    .flex_col()
                    .max_h(row * SLASH_ROWS + m.spacing1() * 2.0)
                    .overflow_y_scroll()
                    .track_scroll(&self.slash_scroll)
                    .p(m.spacing1())
                    .rounded(m.radius_md())
                    .bg(theme.raised())
                    .border_1()
                    .border_color(theme.border)
                    .shadow(muxy_ui::theme::Elevation::Elevated.shadow(theme.bg))
                    .cursor_default()
                    .children(rows),
            )
            .into_any_element()
    }

    /// Asks for files to send with the next prompt: images go along as images,
    /// anything else is named in the text as an `@` mention.
    fn attach(cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some(tr!("Attach")),
        });
        cx.spawn(async move |view, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let _ = view.update(cx, |_, cx| Self::attach_paths(paths, cx));
        })
        .detach();
    }

    pub(super) fn attach_paths(paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| {
            let files = cx
                .background_executor()
                .spawn(async move { read_attachments(paths) })
                .await;
            let _ = view.update(cx, |view, cx| {
                let mut images = Vec::new();
                let mut mentions = Vec::new();
                for file in files {
                    match file {
                        Attachment::Image(name, image) => images.push((name, image)),
                        Attachment::Mention(Some(mention)) => mentions.push(mention),
                        Attachment::Mention(None) => view.notice(
                            tr!("Attachment paths must be absolute UTF-8 paths").to_string(),
                            cx,
                        ),
                    }
                }
                if !mentions.is_empty() {
                    view.files.extend(mentions);
                    view.last_edit = Some(std::time::Instant::now());
                }
                view.add_images(images, cx);
            });
        })
        .detach();
    }

    fn add_images(&mut self, files: Vec<(String, ChatImage)>, cx: &mut Context<Self>) {
        let mut problem = None;
        for (name, image) in files {
            let mut images: Vec<ChatImage> =
                self.images.iter().map(|(_, image)| image.clone()).collect();
            images.push(image.clone());
            if images_fit(&images) {
                self.images.push((name.into(), image));
                self.last_edit = Some(std::time::Instant::now());
            } else {
                problem = Some(
                    tr!(
                        "Chat takes up to %lld images, 5 MiB each and 12 MiB in all.",
                        muxy_protocol::MAX_CHAT_IMAGES
                    )
                    .to_string(),
                );
            }
        }
        if let Some(problem) = problem {
            self.notice(problem, cx);
        }
        cx.notify();
    }

    fn notice(&self, notice: String, cx: &mut Context<Self>) {
        let _ = self
            .owner
            .update(cx, |model, cx| model.show_notice(notice, cx));
    }

    /// Lets the user drag out a screen region with the system picker and
    /// attaches it as an image. Escape in the picker attaches nothing.
    fn screenshot(cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { capture_screenshot() })
                .await;
            let _ = view.update(cx, |view, cx| match result {
                Ok(Some(bytes)) => match image_type(&bytes) {
                    Some(mime) => view.add_images(
                        vec![("Screenshot.png".to_owned(), ChatImage { mime, bytes })],
                        cx,
                    ),
                    None => view.notice(tr!("Screenshot failed").to_string(), cx),
                },
                Ok(None) => {}
                Err(error) => view.notice(error, cx),
            });
        })
        .detach();
    }

    /// Attaches pasted files or image data the way picked files are attached.
    pub(super) fn paste(
        &mut self,
        content: Result<muxy_ui::pasteboard::Content, String>,
        cx: &mut Context<Self>,
    ) {
        match content {
            Ok(muxy_ui::pasteboard::Content::Files(paths)) => Self::attach_paths(paths, cx),
            Ok(muxy_ui::pasteboard::Content::Image(bytes)) => {
                cx.spawn(async move |view, cx| {
                    // Chat takes PNG/JPEG/GIF/WebP; anything else (a TIFF-only copy) becomes a PNG.
                    let image = cx
                        .background_executor()
                        .spawn(async move { copied_image(bytes) })
                        .await;
                    let _ = view.update(cx, |view, cx| match image {
                        Some(image) => view.add_images(vec![("Image.png".to_owned(), image)], cx),
                        None => view.notice(tr!("Copied image can't be attached").to_string(), cx),
                    });
                })
                .detach();
            }
            Ok(muxy_ui::pasteboard::Content::Text) => {}
            Err(error) => self.notice(error, cx),
        }
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_text("", cx));
        self.images.clear();
        self.files.clear();
        self.last_edit = Some(std::time::Instant::now());
        cx.notify();
    }

    /// Pops `items` up under the button drawn at `anchor`; `chosen` gets the picked index.
    fn show_menu(
        items: Vec<Item>,
        anchor: &PopoverAnchor,
        window: &mut Window,
        cx: &mut Context<Self>,
        chosen: impl FnOnce(&mut Self, usize, &mut Context<Self>) + 'static,
    ) {
        let Some(button) = anchor.get() else {
            return;
        };
        // shortcut: off macOS there is no native menu and the chat never shows anyway.
        let Some(receiver) = muxy_ui::native_menu::show(
            items,
            gpui::point(button.left(), button.bottom() + px(1.0)),
            window,
            cx,
        ) else {
            return;
        };
        cx.spawn_in(window, async move |view, cx| {
            let selected = receiver.recv().await.ok().flatten();
            let _ = view.update_in(cx, |view, window, cx| {
                if let Some(index) = selected {
                    chosen(view, index, cx);
                }
                view.focus_input(window, cx);
            });
        })
        .detach();
    }

    /// The quick commands the console composer offers; picking one types it at the caret.
    fn quick_commands(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let enabled = !self.sending;
        // The text, not the index, crosses the menu: settings can change while it's open.
        let mut texts = Vec::new();
        let mut items = Vec::new();
        for (_, command) in owner
            .read(cx)
            .available_commands()
            .filter(|(_, command)| command.status_bar)
        {
            let mut item = Item::new(command.name.clone());
            item.enabled =
                enabled && command.validate().is_ok() && !command.command.trim().is_empty();
            items.push(item);
            texts.push(command.command.clone());
        }
        let mut manage = Item::new(tr!("Manage Quick Commands…"));
        manage.separator = !items.is_empty();
        items.push(manage);
        Self::show_menu(
            items,
            &self.commands_anchor.clone(),
            window,
            cx,
            move |view, index, cx| match texts.get(index) {
                Some(text) if !view.sending => {
                    view.input
                        .update(cx, |input, cx| input.insert_at_selection(text, cx));
                }
                Some(_) => {}
                None => view.act(MenuAction::ManageQuickCommands, cx),
            },
        );
    }

    /// Clear, Switch to Console and Manage Quick Commands.
    fn more(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut items = Vec::new();
        items.push((Item::new(tr!("Clear")), MenuAction::Clear));
        let mut console = Item::new(tr!("Switch to Console"));
        console.enabled = self.owner.upgrade().is_some_and(|owner| {
            owner
                .read(cx)
                .chat_toggle(self.pane, cx)
                .is_some_and(|toggle| toggle.chat && toggle.enabled)
        });
        items.push((console, MenuAction::Console));
        let mut manage = Item::new(tr!("Manage Quick Commands…"));
        manage.separator = true;
        items.push((manage, MenuAction::ManageQuickCommands));
        let (items, actions): (Vec<_>, Vec<_>) = items.into_iter().unzip();
        Self::show_menu(
            items,
            &self.menu_anchor.clone(),
            window,
            cx,
            move |view, index, cx| {
                if let Some(action) = actions.get(index) {
                    view.act(*action, cx);
                }
            },
        );
    }

    fn act(&mut self, action: MenuAction, cx: &mut Context<Self>) {
        let pane = self.pane;
        match action {
            MenuAction::Clear => self.clear(cx),
            MenuAction::Console => {
                let _ = self
                    .owner
                    .update(cx, |model, cx| model.toggle_chat(pane, cx));
            }
            MenuAction::ManageQuickCommands => {
                let _ = self.owner.update(cx, |model, cx| {
                    model.show_settings_soon(crate::views::settings::Category::Commands, cx);
                });
            }
        }
    }

    /// Attach, screenshot (macOS), quick commands and more, in the console's order.
    fn tools(&self, model: &AppModel, cx: &Context<Self>) -> Vec<AnyElement> {
        let pane = self.pane;
        let view = cx.entity().downgrade();
        let id = |name: &str| SharedString::from(format!("chat-{name}-{pane}"));
        let mut tools = Vec::with_capacity(4);
        let attach = view.clone();
        tools.push(tool(
            id("attach"),
            "paperclip",
            tr_key!("Attach File"),
            None,
            model,
            move |_, _, cx| {
                cx.stop_propagation();
                let _ = attach.update(cx, |_, cx| ChatView::attach(cx));
            },
        ));
        if cfg!(target_os = "macos") {
            let shoot = view.clone();
            tools.push(tool(
                id("screenshot"),
                "viewfinder",
                tr_key!("Capture Screenshot"),
                None,
                model,
                move |_, _, cx| {
                    cx.stop_propagation();
                    let _ = shoot.update(cx, |_, cx| ChatView::screenshot(cx));
                },
            ));
        }
        let commands = view.clone();
        tools.push(tool(
            id("commands"),
            "terminal",
            tr_key!("Quick Commands"),
            Some(&self.commands_anchor),
            model,
            move |_, window, cx| {
                cx.stop_propagation();
                let _ = commands.update(cx, |view, cx| view.quick_commands(window, cx));
            },
        ));
        tools.push(tool(
            id("more"),
            "ellipsis",
            tr_key!("More Composer Actions"),
            Some(&self.menu_anchor),
            model,
            move |_, window, cx| {
                cx.stop_propagation();
                let _ = view.update(cx, |view, cx| view.more(window, cx));
            },
        ));
        tools
    }

    /// The input box with its footer, and the picker when it's open.
    #[allow(
        clippy::too_many_lines,
        reason = "The input box and its footer read best in order"
    )]
    pub(super) fn dock(
        &self,
        session: &ChatSession,
        model: &AppModel,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let m = model.metrics;
        let theme = &model.theme;
        let (pane, key) = (self.pane, self.key);
        let busy = session.state.busy && session.mode == ChatMode::Chat;
        let line = m.scaled(21.0);
        let height = self
            .input
            .read(cx)
            .content_height()
            .clamp(line * MIN_LINES, line * MAX_LINES);
        let ready = session
            .local
            .as_ref()
            .map_or(session.mode == ChatMode::Chat, |draft| {
                draft.visible
                    && !draft.discovering
                    && !draft.choose_thread
                    && draft.host_epoch.is_some()
            });
        let can_send = ready
            && session.preparing.is_none()
            && session.operation.is_none()
            && !self.sending
            && (!self.empty || !self.images.is_empty() || !self.files.is_empty());
        let view = cx.entity().downgrade();
        let send = if busy {
            let owner = self.owner.clone();
            round(
                SharedString::from(format!("chat-stop-{pane}")),
                "stop.fill",
                true,
                model,
            )
            .debug_selector(|| "chat-stop".into())
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                let _ = owner.update(cx, |model, cx| model.chat_interrupt(key, cx));
            })
        } else {
            let view = view.clone();
            round(
                SharedString::from(format!("chat-send-{pane}")),
                "arrow.up",
                can_send,
                model,
            )
            .debug_selector(|| "chat-send".into())
            .when(can_send, |send| {
                send.on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    let _ = view.update(cx, |view, cx| view.send(window, cx));
                })
            })
        };
        // Only with something to clear, so an empty input shows no stray button.
        let clear = (!self.empty || !self.images.is_empty() || !self.files.is_empty()).then(|| {
            let view = view.clone();
            tool(
                SharedString::from(format!("chat-clear-{pane}")),
                "xmark.circle",
                tr_key!("Clear"),
                None,
                model,
                move |_, _, cx| {
                    cx.stop_propagation();
                    let _ = view.update(cx, ChatView::clear);
                },
            )
        });
        let chips = self
            .images
            .iter()
            .map(|(name, _)| name.clone())
            .chain(
                self.files
                    .iter()
                    .map(|(name, _)| SharedString::from(name.clone())),
            )
            .enumerate()
            .map(|(index, name)| {
                let view = view.clone();
                if index < self.images.len() {
                    let preview_view = view.clone();
                    let owner = self.owner.clone();
                    return image_chip(
                        &format!("chat-image-{pane}-{index}"),
                        &name,
                        !self.sending,
                        controls::Style { theme, metrics: &m },
                        move |_, _, cx| {
                            let image = preview_view.upgrade().and_then(|view| {
                                view.read(cx)
                                    .images
                                    .get(index)
                                    .map(|(_, image)| image.clone())
                            });
                            if let Some(image) = image {
                                let _ = owner
                                    .update(cx, |model, cx| model.preview_chat_image(image, cx));
                            }
                        },
                        move |_, window, cx| {
                            cx.stop_propagation();
                            let _ = view.update(cx, |view, cx| {
                                if index < view.images.len() {
                                    view.images.remove(index);
                                }
                                view.last_edit = Some(std::time::Instant::now());
                                view.input.read(cx).focus_handle(cx).focus(window);
                                cx.notify();
                            });
                        },
                    )
                    .into_any_element();
                }
                let file_index = index - self.images.len();
                muxy_ui::components::attachment_chip(
                    &format!("chat-file-{pane}-{index}"),
                    &name,
                    &name,
                    !self.sending,
                    controls::Style { theme, metrics: &m },
                    |_, _, _| {},
                    move |_, window, cx| {
                        let _ = view.update(cx, |view, cx| {
                            if file_index < view.files.len() {
                                view.files.remove(file_index);
                            }
                            view.last_edit = Some(std::time::Instant::now());
                            view.input.read(cx).focus_handle(cx).focus(window);
                            cx.notify();
                        });
                    },
                )
                .debug_selector(move || format!("chat-attachment-{pane}-{index}"))
                .into_any_element()
            });
        let anchor = self.picker_anchor.clone();
        let focused = self.input.read(cx).focus_handle(cx).is_focused(window);
        div()
            .debug_selector(|| "chat-input".into())
            .flex()
            .flex_col()
            .gap(m.spacing3())
            .px(m.spacing6())
            .pt(m.spacing5())
            .pb(m.spacing4())
            .rounded(m.radius_xl())
            .border_1()
            .border_color(if focused {
                theme.accent.opacity(0.6)
            } else {
                theme.border
            })
            .bg(theme.raised())
            .cursor_text()
            .relative()
            .when(self.slash_open(), |dock| {
                dock.child(self.slash_list(model, cx))
            })
            .when(!self.images.is_empty() || !self.files.is_empty(), |dock| {
                dock.child(div().flex().flex_wrap().gap(m.spacing2()).children(chips))
            })
            .child(div().w_full().h(height).flex().child(self.input.clone()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(m.spacing2())
                    .cursor_default()
                    .children(super::mode_switch(self.pane, model, &self.owner, cx))
                    .child(
                        div()
                            .id(SharedString::from(format!("chat-picker-{pane}")))
                            .debug_selector(|| "chat-picker".into())
                            .relative()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(m.spacing2())
                            .h(m.control_medium())
                            .px(m.spacing3())
                            .rounded(m.radius_md())
                            .text_size(m.font_body())
                            .text_color(theme.fg_muted)
                            .cursor_pointer()
                            .hover(|picker| picker.bg(theme.hover).text_color(theme.fg))
                            .child(crate::views::tab_activity::provider_icon(if session.provider()==ChatProvider::Codex{AgentProvider::Codex}else{AgentProvider::Claude},m.icon_sm(),model))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.fg)
                                    .child(model_label(session)),
                            )
                            .when_some(session.state.effort.clone(), |picker, effort| {
                                picker.child(effort)
                            })
                            .child(IconGlyph::new(Icon::ChevronUp, m.icon_xs(), theme.fg_muted))
                            .child(
                                canvas(
                                    move |bounds, _, _| anchor.set(Some(bounds)),
                                    |_, (), _, _| {},
                                )
                                .absolute()
                                .size_full(),
                            )
                            .on_click(cx.listener(|view, _, _, cx| {
                                cx.stop_propagation();
                                view.picker_open = !view.picker_open;
                                cx.notify();
                            })),
                    )
                    // The tools sit beside send on the right.
                    .child(div().flex_1().min_w(m.spacing4()))
                    .children(self.tools(model, cx))
                    .children(clear)
                    .child(div().flex_none().ml(m.spacing2()).child(send)),
            )
            .when(self.picker_open, |dock| {
                dock.child(self.picker(session, model, cx))
            })
            .into_any_element()
    }

    /// The popover listing the models and effort levels Claude Code offers.
    #[allow(
        clippy::too_many_lines,
        reason = "One popover: models, efforts and the refusal"
    )]
    fn picker(&self, session: &ChatSession, model: &AppModel, cx: &Context<Self>) -> AnyElement {
        let m = model.metrics;
        let theme = &model.theme;
        let (pane, key) = (self.pane, self.key);
        let current = current_model(session).map(|model| model.alias.clone());
        let efforts = current_model(session)
            .or_else(|| session.capabilities.models.first())
            .map(|model| model.efforts.clone())
            .unwrap_or_default();
        let enabled = (session.mode == ChatMode::Chat || session.local.is_some())
            && session.operation.is_none()
            && session.preparing.is_none()
            && !session.switching;
        let choose = |value: String, effort: bool| {
            let owner = self.owner.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                let value = value.clone();
                let _ = owner.update(cx, |model, cx| {
                    model.chat_choose(pane, key, value, effort, cx);
                });
            }
        };
        let provider_icon = |provider| {
            crate::views::tab_activity::provider_icon(
                if provider == ChatProvider::Codex {
                    AgentProvider::Codex
                } else {
                    AgentProvider::Claude
                },
                m.icon_sm(),
                model,
            )
        };
        let check = |on: bool| {
            div().w(m.icon_sm()).when(on, |slot| {
                slot.child(IconGlyph::new(Icon::Check, m.icon_sm(), theme.fg))
            })
        };
        let mut content = popover::surface(theme, m)
            .id(SharedString::from(format!("chat-picker-menu-{pane}")))
            .debug_selector(|| "chat-picker-menu".into())
            .min_w(px(220.0))
            .on_mouse_down_out(cx.listener(|view, _, _, cx| {
                view.picker_open = false;
                cx.notify();
            }))
            .child(popover::header(theme, m).child(tr!("Model")));
        if let Some(local) = &session.local {
            for (provider, label) in [
                (ChatProvider::Codex, "OpenAI"),
                (ChatProvider::Claude, "Anthropic"),
            ] {
                let owner = self.owner.clone();
                content = content.child(
                    popover::row(
                        theme,
                        m,
                        SharedString::from(format!("chat-provider-{pane}-{label}")),
                        enabled,
                        false,
                    )
                    .child(check(local.selection.provider == provider))
                    .child(provider_icon(provider))
                    .child(label)
                    .on_click(move |_, _, cx| {
                        let _ = owner.update(cx, |model, cx| {
                            model.chat_change_provider(pane, provider, cx);
                        });
                    }),
                );
            }
            if local.choose_thread {
                content = content
                    .child(popover::divider(theme, m))
                    .child(popover::header(theme, m).child(tr!("Choose conversation")));
                for thread in &local.threads {
                    let target = thread.target.clone();
                    let owner = self.owner.clone();
                    content = content.child(
                        popover::row(
                            theme,
                            m,
                            SharedString::from(format!("chat-thread-{}", target.thread_id)),
                            enabled,
                            false,
                        )
                        .child(
                            div()
                                .max_w(px(380.0))
                                .truncate()
                                .child(thread.title.clone()),
                        )
                        .on_click(move |_, _, cx| {
                            let target = target.clone();
                            let _ = owner
                                .update(cx, |model, cx| model.chat_resume_target(pane, target, cx));
                        }),
                    );
                }
            }
            content = content.child(popover::divider(theme, m));
        }
        if session.capabilities.models.is_empty() && session.picker_error.is_none() {
            content = content.child(
                div()
                    .px(m.scaled(popover::ROW_PADDING))
                    .py(m.spacing2())
                    .text_size(m.font_footnote())
                    .text_color(theme.fg_muted)
                    .child(tr!("Waiting for the provider's model catalog…")),
            );
        }
        for offered in &session.capabilities.models {
            content = content.child(
                popover::row(
                    theme,
                    m,
                    SharedString::from(format!("chat-model-{pane}-{}", offered.alias)),
                    enabled,
                    false,
                )
                .child(check(current.as_deref() == Some(offered.alias.as_str())))
                .child(provider_icon(session.provider()))
                .child(offered.label.clone())
                .on_click(choose(offered.alias.clone(), false)),
            );
        }
        if !efforts.is_empty() {
            content = content
                .child(popover::divider(theme, m))
                .child(popover::header(theme, m).child(tr!("Effort")));
            for effort in &efforts {
                content = content.child(
                    popover::row(
                        theme,
                        m,
                        SharedString::from(format!("chat-effort-{pane}-{effort}")),
                        enabled,
                        false,
                    )
                    .child(check(
                        session.state.effort.as_deref() == Some(effort.as_str()),
                    ))
                    .child(effort.clone())
                    .on_click(choose(effort.clone(), true)),
                );
            }
        }
        if let Some(error) = &session.picker_error {
            content = content.child(
                div()
                    .px(m.scaled(popover::ROW_PADDING))
                    .pt(m.spacing2())
                    .max_w(px(320.0))
                    .text_size(m.font_footnote())
                    .text_color(theme.danger)
                    .child(error.clone()),
            );
        }
        let view = cx.entity().downgrade();
        deferred(popover::anchored_popover_above(
            self.picker_anchor.clone(),
            content
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .into_any_element(),
            move |_, cx| {
                let _ = view.update(cx, |view, cx| {
                    view.picker_open = false;
                    cx.notify();
                });
            },
        ))
        .with_priority(1)
        .into_any_element()
    }
}

/// The offered model the chat runs: the one chosen here, else the one whose
/// alias the resolved model's name contains.
// shortcut: the server reports resolved model ids, not aliases; matching by
// substring guesses after a reconnect. Report the alias in ChatState if it misleads.
fn current_model(session: &ChatSession) -> Option<&muxy_protocol::ChatModel> {
    let models = &session.capabilities.models;
    session
        .chosen_model
        .as_ref()
        .and_then(|chosen| models.iter().find(|model| &model.alias == chosen))
        .or_else(|| {
            let running = session.state.model.as_deref()?;
            models
                .iter()
                .find(|model| model.alias == running)
                .or_else(|| {
                    models.iter().find(|model| {
                        session.provider() == ChatProvider::Claude
                            && model.alias != "default"
                            && running.contains(model.alias.as_str())
                    })
                })
        })
}

fn model_label(session: &ChatSession) -> SharedString {
    let label: SharedString = current_model(session).map_or_else(
        || {
            session
                .state
                .model
                .clone()
                .map_or_else(|| tr!("Default model"), SharedString::from)
        },
        |model| model.label.clone().into(),
    );
    format!(
        "{} / {}",
        if session.provider() == ChatProvider::Codex {
            "OpenAI"
        } else {
            "Anthropic"
        },
        label
    )
    .into()
}

/// The console composer's toolbar button: a muted symbol that brightens over
/// a soft hover, with its tooltip.
fn tool(
    id: SharedString,
    symbol: &'static str,
    label: &'static str,
    anchor: Option<&PopoverAnchor>,
    model: &AppModel,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let m = model.metrics;
    let theme = &model.theme;
    let (raised, fg, border, bg) = (theme.raised(), theme.fg, theme.border, theme.bg);
    div()
        .id(id.clone())
        .group(id.clone())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(m.control_medium())
        .rounded(m.radius_md())
        .hover(|button| button.bg(theme.hover))
        .cursor_pointer()
        .when_some(anchor.cloned(), |button, anchor| {
            button.relative().child(
                canvas(
                    move |bounds, _, _| anchor.set(Some(bounds)),
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
        })
        .child(
            SymbolGlyph::new(symbol, m.font_footnote(), theme.fg_muted)
                .hover_in_group(id, theme.fg),
        )
        .tooltip(move |_, cx| {
            cx.new(|_| Tooltip::new(translate(label), raised, fg, border, bg))
                .into()
        })
        .button_interaction(on_click)
        .into_any_element()
}

/// The round send or stop button, in the console composer's accent.
fn round(
    id: SharedString,
    symbol: &'static str,
    enabled: bool,
    model: &AppModel,
) -> gpui::Stateful<gpui::Div> {
    let m = model.metrics;
    let theme = &model.theme;
    // Disabled reads as off, not as a faint accent.
    let (bg, fg) = if enabled {
        (theme.accent, theme.accent_foreground)
    } else {
        (theme.surface, theme.fg_dim)
    };
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(m.control_large())
        .rounded(m.radius_lg())
        .bg(bg)
        .when(enabled, |button| {
            button.cursor_pointer().hover(|button| button.opacity(0.9))
        })
        .child(SymbolGlyph::new(symbol, m.font_body(), fg).bold())
}

/// The path quoted as the composer pastes it (`'/a b/c'`), which the prompt bubble shows as a
/// chip; `None` unless it's absolute UTF-8.
// shortcut: a local path; a chat on a remote server can't read it. Upload it as the composer does if that's needed.
fn mention(path: &Path) -> Option<String> {
    path.to_str()?;
    let bytes = muxy_client::input::paths(
        std::slice::from_ref(&path.to_path_buf()),
        muxy_protocol::Modes::default(),
    )?;
    String::from_utf8(bytes).ok()
}

/// Each picked path as an image when it's a small enough PNG, JPEG, GIF or WebP, else as a mention.
fn read_attachments(paths: Vec<PathBuf>) -> Vec<Attachment> {
    paths
        .into_iter()
        .map(|path| {
            let small = std::fs::metadata(&path).is_ok_and(|meta| {
                meta.is_file() && meta.len() <= muxy_protocol::MAX_CHAT_IMAGE_BYTES as u64
            });
            let image = small
                .then(|| std::fs::read(&path).ok())
                .flatten()
                .and_then(|bytes| {
                    Some(ChatImage {
                        mime: image_type(&bytes)?,
                        bytes,
                    })
                });
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            match image {
                Some(image) => Attachment::Image(name, image),
                None => Attachment::Mention(mention(&path).map(|mention| (name, mention))),
            }
        })
        .collect()
}

/// The PNG of a region the user drags out, or `None` when they cancel.
// Mirrors the composer's capture, which is private to its module; its own file so both can run.
fn capture_screenshot() -> Result<Option<Vec<u8>>, String> {
    // A fresh name per capture, so two at once can't delete each other's file.
    let file = tempfile::Builder::new()
        .prefix("muxy-chat-screenshot-")
        .suffix(".png")
        .tempfile()
        .map_err(|error| error.to_string())?;
    let path = file.into_temp_path();
    let _ = std::fs::remove_file(&path);
    let status = std::process::Command::new("/usr/sbin/screencapture")
        .args(["-i", "-x"])
        .arg(&path)
        .status()
        .map_err(|error| error.to_string())?;
    // A cancelled selection exits cleanly without writing the file.
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    let _ = std::fs::remove_file(&path);
    if bytes.is_none() && !status.success() {
        return Err(tr!("Screenshot failed").to_string());
    }
    Ok(bytes)
}

#[cfg(test)]
impl ChatView {
    pub(crate) fn test_attach_paths(paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        Self::attach_paths(paths, cx);
    }

    pub(crate) fn test_paste_image(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.paste(Ok(muxy_ui::pasteboard::Content::Image(bytes)), cx);
    }

    pub(crate) fn test_file_names(&self) -> Vec<String> {
        self.files.iter().map(|(name, _)| name.clone()).collect()
    }

    pub(crate) fn input_text(&self, cx: &gpui::App) -> String {
        self.input.read(cx).text().to_owned()
    }

    pub(crate) fn set_input_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_text(text.to_owned(), cx));
    }

    pub(crate) fn test_image(&mut self, image: Option<ChatImage>) -> Vec<ChatImage> {
        if let Some(image) = image {
            self.images.push(("draft.png".into(), image));
            self.last_edit = Some(std::time::Instant::now());
        }
        self.images.iter().map(|(_, image)| image.clone()).collect()
    }

    pub(crate) fn slash_names(&self) -> Vec<&str> {
        if !self.slash_open() {
            return Vec::new();
        }
        self.slash
            .matches
            .iter()
            .map(|command| command.name.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(names: &[&str]) -> Vec<ChatCommand> {
        names
            .iter()
            .map(|name| ChatCommand {
                name: (*name).into(),
                description: String::new(),
                skill: false,
            })
            .collect()
    }

    #[test]
    fn slash_commands_rank_prefix_then_namespace_part_then_substring_ignoring_case() {
        let listed = commands(&[
            "codex:review",
            "review",
            "Code-Review",
            "preview",
            "compact",
        ]);
        let ranked = |query| {
            rank_commands(&listed, query)
                .into_iter()
                .map(|index| listed[index].name.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ranked("rev"),
            ["review", "codex:review", "Code-Review", "preview"]
        );
        assert_eq!(ranked("CO"), ["codex:review", "Code-Review", "compact"]);
        assert_eq!(ranked("").len(), listed.len());
        assert!(ranked("zzz").is_empty());
    }

    #[test]
    fn only_a_leading_slash_word_with_nothing_after_it_completes() {
        assert_eq!(slash_word("/com"), Some(0..4));
        assert_eq!(slash_word("  /"), Some(2..3));
        assert_eq!(slash_word("/compact now"), None);
        assert_eq!(slash_word("/compact "), None);
        assert_eq!(slash_word("say /com"), None);
        assert_eq!(slash_word(""), None);
    }

    #[test]
    fn tui_only_commands_stay_local_only_when_the_list_leaves_them_out() {
        let listed = commands(&["compact", "model", "effort", "status"]);
        let console = |name: &str| Some(Local::ConsoleOnly(name.to_owned()));
        assert_eq!(local_command("/login", &listed), console("login"));
        assert_eq!(local_command("  /doctor  ", &listed), console("doctor"));
        // Listed, so this Claude Code runs it headless.
        assert_eq!(local_command("/status", &listed), None);
        // No list yet: sent as typed.
        assert_eq!(local_command("/login", &[]), None);
        assert_eq!(local_command("/resume", &[]), Some(Local::Resume));
        assert_eq!(local_command("  /resume \n", &[]), Some(Local::Resume));
        // With arguments it is a prompt for Claude, even across lines.
        assert_eq!(local_command("/resume abc", &[]), None);
        assert_eq!(local_command("/resume\nmore text", &[]), None);
        assert_eq!(local_command("/model", &listed), Some(Local::Picker));
        assert_eq!(local_command("/effort  ", &[]), Some(Local::Picker));
        assert_eq!(local_command("/model opus", &listed), None);
        assert_eq!(local_command("/effort high", &listed), None);
        // Unknown commands and prose reach Claude.
        assert_eq!(local_command("/foo", &listed), None);
        assert_eq!(local_command("fix /login", &listed), None);
    }

    #[test]
    fn non_images_become_mentions_instead_of_being_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("shot.png");
        let notes = dir.path().join("my notes.txt");
        let bytes = b"\x89PNG\r\n\x1a\nrest".to_vec();
        std::fs::write(&png, &bytes).unwrap();
        std::fs::write(&notes, "hello").unwrap();
        let picked = read_attachments(vec![
            png,
            notes.clone(),
            dir.path().to_path_buf(),
            PathBuf::from("relative.txt"),
        ]);
        assert_eq!(
            picked,
            vec![
                Attachment::Image(
                    "shot.png".into(),
                    ChatImage {
                        mime: ChatImageType::Png,
                        bytes
                    }
                ),
                Attachment::Mention(Some(("my notes.txt".into(), mention(&notes).unwrap()))),
                Attachment::Mention(Some((
                    dir.path().file_name().unwrap().to_str().unwrap().into(),
                    mention(dir.path()).unwrap(),
                ))),
                Attachment::Mention(None),
            ]
        );
        // The bubble shows what the dock inserts as a chip, spaces and all.
        let typed = mention(&notes).unwrap();
        let split = crate::views::chat::rows::files::prompt(&format!("read {typed}")).unwrap();
        assert_eq!(split.attachments, [notes.to_str().unwrap()]);
    }
}
