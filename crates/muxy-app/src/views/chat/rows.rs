//! Fork: one chat row. A finished turn shows its prompt, a summary of its
//! steps, its final text and a footer; the steps stay open while it runs.

use gpui::prelude::FluentBuilder;
use std::sync::Arc;

use gpui::{
    AnyElement, AppContext, ElementId, FontWeight, HighlightStyle, Hsla, InteractiveElement,
    InteractiveText, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    StyledText, UnderlineStyle, WeakEntity, Window, div, px, relative,
};
use muxy_app_core::PaneId;
use muxy_app_core::opener::OpenContext;
use muxy_protocol::{ChatItemKind, FileChange};
use muxy_ui::components::{IconButton, IconGlyph, Tooltip};
use muxy_ui::icon::Icon;
use muxy_ui::tr;

use super::{markdown, select};
use crate::model::AppModel;
use crate::model::chat::{
    ChatFile, ChatKey, ChatSession, EXITED_ROW, Row, ToolKind, Turn, tool_kind,
};
use files::FileKind;

/// What drawing a row needs, for one frame.
pub(crate) struct Scope<'a> {
    pub(crate) pane: PaneId,
    pub(crate) key: ChatKey,
    pub(crate) session: &'a ChatSession,
    pub(crate) owner: &'a WeakEntity<AppModel>,
    pub(crate) model: &'a AppModel,
    /// For the text style rows inherit, which markdown runs start from.
    pub(crate) window: &'a Window,
    /// Where paths the rows name resolve; `None` leaves them unclickable.
    pub(crate) context: Option<&'a OpenContext>,
    /// Fork: how the row's text takes part in selection; `None` leaves it unselectable.
    pub(crate) marks: Option<select::Marks<'a>>,
}

// Each row's element ids are its id with these numbers; markdown uses 16 and up.
const SUMMARY: usize = 0;
const THINKING: usize = 1;
const COPY: usize = 2;
const FILES: usize = 3;
const CARD: usize = 4;
const CARD_FILES: usize = 5;
const PILL: usize = 6;
const PROMPT: usize = 7;
const ATTACHMENTS: usize = 8;

pub(crate) mod files;

/// A prompt longer than this fills the width instead of hugging the right.
const SHORT_PROMPT: usize = 160;

pub(super) fn row(scope: &Scope<'_>, row: &Row) -> AnyElement {
    let summary = scope
        .session
        .turns
        .get(&row.item.turn_id)
        .filter(|turn| turn.done && turn.head.as_ref() == Some(&row.item.id))
        .map(|turn| {
            let open = scope.session.expanded.contains(&row.item.turn_id);
            summary(scope, row, turn, open)
        });
    let content = (!scope.session.hidden(row)).then(|| content(scope, row));
    match (summary, content) {
        (Some(summary), Some(content)) => div()
            .flex()
            .flex_col()
            .w_full()
            .child(summary)
            .child(content)
            .into_any_element(),
        (Some(element), None) | (None, Some(element)) => element,
        (None, None) => div().into_any_element(),
    }
}

/// "› 17 tool calls, 3 messages" and the kinds of tools used; opens and closes the steps.
fn summary(scope: &Scope<'_>, row: &Row, turn: &Turn, open: bool) -> AnyElement {
    let m = scope.model.metrics;
    let theme = &scope.model.theme;
    let (owner, key) = (scope.owner.clone(), scope.key);
    let guard = select::Guard::new(scope);
    let turn_id = row.item.turn_id.clone();
    div()
        .w_full()
        .px(m.spacing7())
        .pt(m.spacing3())
        .child(
            div()
                .id(ElementId::from((row.key.clone(), SUMMARY)))
                .debug_selector(|| "chat-turn-summary".into())
                .flex()
                .items_center()
                .gap(m.spacing3())
                .px(m.spacing2())
                .py(m.spacing1())
                .rounded(m.radius_sm())
                .text_size(m.font_emphasis())
                .text_color(theme.fg_muted)
                .cursor_pointer()
                .hover(|line| line.bg(theme.hover))
                .child(IconGlyph::new(
                    if open {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    },
                    m.icon_xs(),
                    theme.fg_muted,
                ))
                .child(turn.summary.clone())
                .children(
                    turn.kinds
                        .iter()
                        .map(|kind| IconGlyph::new(kind_icon(*kind), m.icon_sm(), theme.fg_muted)),
                )
                .on_click(move |_, _, cx| {
                    if !guard.dragged() {
                        let _ = owner.update(cx, |model, cx| model.chat_expand(key, &turn_id, cx));
                    }
                }),
        )
        .into_any_element()
}

fn kind_icon(kind: ToolKind) -> Icon {
    match kind {
        ToolKind::Read => Icon::File,
        ToolKind::Search => Icon::Search,
        ToolKind::Run => Icon::Terminal,
        ToolKind::Edit => Icon::Pencil,
        ToolKind::Web => Icon::Globe,
        ToolKind::Agent => Icon::Sparkles,
        ToolKind::Other => Icon::Puzzle,
    }
}

pub(crate) fn summary_text(turn: &Turn) -> String {
    let tools = match turn.tools {
        0 => None,
        1 => Some(tr!("1 tool call")),
        count => Some(tr!("%lld tool calls", count)),
    };
    let messages = match turn.messages {
        0 => None,
        1 => Some(tr!("1 message")),
        count => Some(tr!("%lld messages", count)),
    };
    match (tools, messages) {
        (Some(tools), Some(messages)) => tr!("%@, %@", &tools, &messages).to_string(),
        (Some(one), None) | (None, Some(one)) => one.to_string(),
        (None, None) => tr!("Thinking").to_string(),
    }
}

fn content(scope: &Scope<'_>, row: &Row) -> AnyElement {
    let m = scope.model.metrics;
    let theme = &scope.model.theme;
    let base = div().w_full().min_w_0().px(m.spacing8());
    match &row.item.kind {
        // Spacing, not labels, separates turns.
        ChatItemKind::User { .. } => base
            .flex()
            .justify_end()
            .pt(m.spacing9())
            .pb(m.spacing4())
            .child(
                div()
                    .map(|bubble| {
                        if row.text.len() > SHORT_PROMPT || row.text.contains('\n') {
                            bubble.w_full()
                        } else {
                            bubble.max_w(relative(0.7))
                        }
                    })
                    .px(m.spacing8())
                    .py(m.spacing6())
                    .rounded(m.radius_lg())
                    .bg(theme.raised().blend(theme.warning.opacity(0.06)))
                    .text_size(m.font_headline())
                    .line_height(relative(1.5))
                    .text_color(theme.fg)
                    .child(prompt(scope, row)),
            )
            .into_any_element(),
        ChatItemKind::Assistant { .. } => base
            .relative()
            .py(m.spacing7())
            .group(row.key.clone())
            .text_color(theme.fg)
            .child(markdown::render(
                scope,
                &row.document(
                    scope.session.stretched.get(&row.key),
                    &scope.session.files,
                    scope.context,
                ),
                &row.key,
            ))
            .child(
                // Hidden, not transparent, so it takes no clicks until hovered.
                div()
                    .absolute()
                    .top_0()
                    .right(m.spacing4())
                    .invisible()
                    .group_hover(row.key.clone(), Styled::visible)
                    .child(copy_button(scope, row)),
            )
            .into_any_element(),
        ChatItemKind::Thinking { .. } => details(scope, row, None),
        ChatItemKind::Tool {
            name,
            summary,
            result: Some(result),
            ..
        } if name == "Agent" && summary == "Subagent report" => details(scope, row, Some(result)),
        ChatItemKind::Tool {
            name,
            result,
            is_error,
            ..
        } => {
            let failed = result.is_some() && *is_error;
            div()
                .w_full()
                .min_w_0()
                .child(tool(scope, row, name, result.is_some(), failed))
                .when(failed, |card| {
                    card.child(details(scope, row, result.as_deref()))
                })
                .into_any_element()
        }
        ChatItemKind::Notice { .. } | ChatItemKind::Folded { .. } => base
            .py(m.spacing2())
            .text_size(m.font_footnote())
            .text_color(if row.item.id == EXITED_ROW {
                theme.danger
            } else {
                theme.fg_muted
            })
            .child(select::plain(scope, &row.key, 0, 0, row.shared()))
            .into_any_element(),
        ChatItemKind::TurnEnd {
            duration_ms,
            is_error,
            changed,
        } => footer(scope, row, *duration_ms, *is_error, changed),
    }
}

/// A prompt's text with its @mentions tinted, then a chip per attached file.
fn prompt(scope: &Scope<'_>, row: &Row) -> AnyElement {
    let Some(split) = row.prompt() else {
        return select::plain(scope, &row.key, 0, 0, row.shared());
    };
    let guard = select::Guard::new(scope);
    let m = scope.model.metrics;
    let theme = &scope.model.theme;
    let mention = HighlightStyle {
        color: Some(warm_text(theme)),
        background_color: Some(warm_tint(theme)),
        ..HighlightStyle::default()
    };
    // Mentions and attachments share one budget of lookups per row.
    let mentions = split.mentions.len().min(markdown::MAX_PATHS);
    let attachments = markdown::MAX_PATHS - mentions;
    let text =
        (!split.text.is_empty()).then(|| {
            // Mentions of files that exist open them; the `@` isn't part of the path.
            let found: Vec<_> = split.mentions[..mentions]
                .iter()
                .filter_map(|range| {
                    let file = scope
                        .session
                        .file(&split.text[range.start + 1..range.end], scope.context)?;
                    Some((range.clone(), file))
                })
                .collect();
            let highlights = split.mentions.iter().cloned().map(|range| {
                let style = if found.iter().any(|(found, _)| *found == range) {
                    HighlightStyle {
                        underline: Some(path_underline(theme)),
                        ..mention
                    }
                } else {
                    mention
                };
                (range, style)
            });
            let styled = StyledText::new(split.text.clone()).with_highlights(highlights);
            let (layout, len) = (styled.layout().clone(), split.text.len());
            // Shrinks to the bubble, so a long prompt wraps instead of overflowing.
            div().min_w_0().map(|text| {
                if found.is_empty() {
                    return text.child(select::mark(scope, &row.key, 0, 0, len, layout, styled));
                }
                let ranges = found.iter().map(|(range, _)| range.clone()).collect();
                let (owner, pane, guard) = (scope.owner.clone(), scope.pane, guard.clone());
                text.child(select::mark(
                    scope,
                    &row.key,
                    0,
                    0,
                    len,
                    layout,
                    InteractiveText::new(ElementId::from((row.key.clone(), PROMPT)), styled)
                        .on_click(ranges, move |index, window, cx| {
                            if guard.dragged() {
                                return;
                            }
                            if let Some((_, file)) = found.get(index) {
                                open_path(&owner, pane, file, window.modifiers().platform, cx);
                            }
                        }),
                ))
            })
        });
    div()
        .id(ElementId::from((row.key.clone(), ATTACHMENTS)))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(m.spacing3())
        .children(text)
        .children(split.attachments.iter().enumerate().map(|(index, path)| {
            let file = (index < attachments)
                .then(|| scope.session.file(path, scope.context))
                .flatten();
            attachment(scope, index, path, file, &guard)
        }))
        .into_any_element()
}

/// An attached file's chip; one that exists opens when clicked.
fn attachment(
    scope: &Scope<'_>,
    index: usize,
    path: &str,
    file: Option<Arc<ChatFile>>,
    guard: &select::Guard,
) -> impl IntoElement {
    let (m, theme) = (scope.model.metrics, &scope.model.theme);
    let (icon, color) = file_icon(files::kind(path), theme);
    let (owner, pane, guard) = (scope.owner.clone(), scope.pane, guard.clone());
    div()
        .id(("chat-attachment", index))
        .flex()
        .items_center()
        .gap(m.spacing2())
        .min_w_0()
        .max_w_full()
        .px(m.spacing3())
        .rounded(m.radius_sm())
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface)
        .text_size(m.font_emphasis())
        .child(
            div()
                .flex_none()
                .child(IconGlyph::new(icon, m.icon_sm(), color)),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .child(files::name(path).to_owned()),
        )
        .when_some(file, |chip, file| {
            chip.cursor_pointer()
                .hover(|chip| chip.bg(theme.hover))
                .on_click(move |event, _, cx| {
                    cx.stop_propagation();
                    if !guard.dragged() {
                        open_path(&owner, pane, &file, event.modifiers().platform, cx);
                    }
                })
        })
}

/// Opens a file chat names; with ⌘, or when it could run something, shows it in Finder instead.
fn open_path(
    owner: &WeakEntity<AppModel>,
    pane: PaneId,
    file: &ChatFile,
    reveal: bool,
    cx: &mut gpui::App,
) {
    let _ = owner.update(cx, |model, cx| model.chat_open_path(pane, file, reveal, cx));
}

/// The faint underline under a path that opens when clicked.
pub(super) fn path_underline(theme: &muxy_ui::theme::Theme) -> UnderlineStyle {
    UnderlineStyle {
        thickness: px(1.0),
        color: Some(theme.fg_muted.opacity(0.5)),
        wavy: false,
    }
}

/// The warm tint behind paths and mentions, from the theme's yellow so it suits light themes too.
fn warm_tint(theme: &muxy_ui::theme::Theme) -> Hsla {
    theme.warning.opacity(0.14)
}

/// Text on `warm_tint`: the foreground leaning toward the same yellow.
fn warm_text(theme: &muxy_ui::theme::Theme) -> Hsla {
    theme.fg.blend(theme.warning.opacity(0.35))
}

/// A file kind's icon and its color.
fn file_icon(kind: FileKind, theme: &muxy_ui::theme::Theme) -> (Icon, Hsla) {
    match kind {
        FileKind::Image => (Icon::File, theme.diff_add),
        FileKind::React => (Icon::Code, theme.diff_hunk),
        FileKind::Style => (Icon::Palette, theme.diff_hunk),
        FileKind::Rust => (Icon::Code, theme.warning),
        FileKind::Code => (Icon::Code, theme.fg_muted),
        FileKind::Other => (Icon::File, theme.fg_muted),
    }
}

/// A step: its kind's icon, what it does, and a pill with what it works on.
fn tool(scope: &Scope<'_>, row: &Row, name: &str, finished: bool, failed: bool) -> AnyElement {
    let m = scope.model.metrics;
    let theme = &scope.model.theme;
    let kind = tool_kind(name);
    // Paths get the warm pill; commands, patterns and URLs stay neutral.
    let (tint, ink) = if matches!(kind, ToolKind::Read | ToolKind::Edit) {
        (warm_tint(theme), warm_text(theme))
    } else {
        (theme.surface, theme.fg_muted)
    };
    div()
        .w_full()
        .min_w_0()
        .px(m.spacing8())
        .flex()
        .items_center()
        .gap(m.spacing4())
        .py(m.spacing2())
        .text_size(m.font_body())
        .text_color(theme.fg)
        .child(div().flex_none().child(IconGlyph::new(
            kind_icon(kind),
            m.icon_sm(),
            theme.fg_muted,
        )))
        .child(
            div()
                .flex_none()
                .max_w(relative(0.5))
                .truncate()
                .text_color(if failed {
                    theme.danger
                } else if finished {
                    theme.accent
                } else {
                    theme.fg
                })
                .child(select::plain(scope, &row.key, 0, 0, row.verb.clone())),
        )
        .when(!row.text.is_empty(), |line| {
            let file: Option<Arc<ChatFile>> = row
                .path
                .as_deref()
                .and_then(|path| scope.session.file(path, scope.context));
            let tip = row
                .path
                .as_ref()
                .map_or_else(|| row.shared(), |path| path.clone().into());
            let (text, raised, fg, border, bg) =
                (tip, theme.raised(), theme.fg, theme.border, theme.bg);
            let (owner, pane, guard) = (scope.owner.clone(), scope.pane, select::Guard::new(scope));
            line.child(
                div()
                    .id(ElementId::from((row.key.clone(), PILL)))
                    .min_w_0()
                    .truncate()
                    .px(m.spacing3())
                    .py(m.spacing1())
                    .rounded(m.radius_sm())
                    .bg(tint)
                    .font_family(muxy_ui::font::monospace())
                    .text_size(m.font_footnote())
                    .text_color(ink)
                    .child(select::plain(
                        scope,
                        &row.key,
                        0,
                        select::pill_base(row),
                        row.shared(),
                    ))
                    // Cut to fit, so the whole command or path shows on hover.
                    .tooltip(move |_, cx| {
                        cx.new(|_| Tooltip::new(text.clone(), raised, fg, border, bg))
                            .into()
                    })
                    .when_some(file, |pill, file| {
                        pill.cursor_pointer()
                            .hover(Styled::underline)
                            .on_click(move |event, _, cx| {
                                cx.stop_propagation();
                                if !guard.dragged() {
                                    open_path(&owner, pane, &file, event.modifiers().platform, cx);
                                }
                            })
                    }),
            )
        })
        .when(failed, |line| {
            line.child(
                div()
                    .flex_none()
                    .text_size(m.font_footnote())
                    .text_color(theme.danger)
                    .child(tr!("failed")),
            )
        })
        .into_any_element()
}

/// Thinking and reports stay folded; errors show one line before opening.
fn details(scope: &Scope<'_>, row: &Row, output: Option<&str>) -> AnyElement {
    let m = scope.model.metrics;
    let theme = &scope.model.theme;
    let open = scope.session.expanded.contains(&row.item.id);
    let failed = matches!(row.item.kind, ChatItemKind::Tool { is_error: true, .. });
    let text = if output.is_some() {
        row.tool_details(open).map(|text| text.to_owned().into())
    } else {
        open.then(|| row.shared())
    };
    let (owner, key) = (scope.owner.clone(), scope.key);
    let guard = select::Guard::new(scope);
    let id = row.item.id.clone();
    div()
        .w_full()
        .min_w_0()
        .px(m.spacing8())
        .py(m.spacing2())
        .text_size(m.font_footnote())
        .text_color(if failed { theme.danger } else { theme.fg_muted })
        .child(
            div()
                .id(ElementId::from((row.key.clone(), THINKING)))
                .debug_selector({
                    let id = id.clone();
                    move || format!("chat-details-{id}")
                })
                .role(gpui::Role::Button)
                .aria_label(if failed {
                    tr!("Error details")
                } else if output.is_some() {
                    tr!("Subagent report")
                } else {
                    tr!("Thinking")
                })
                .flex()
                .items_center()
                .gap(m.spacing3())
                .min_w_0()
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    if !guard.dragged() {
                        let _ = owner.update(cx, |model, cx| model.chat_expand(key, &id, cx));
                    }
                })
                .child(IconGlyph::new(
                    if open {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    },
                    m.icon_xs(),
                    theme.fg_muted,
                ))
                .child(
                    div()
                        .flex_none()
                        .when(output.is_none(), Styled::italic)
                        .child(if failed {
                            tr!("Error details")
                        } else if output.is_some() {
                            tr!("Subagent report")
                        } else {
                            tr!("Thinking")
                        }),
                ),
        )
        .when_some(text, |thinking, text| {
            thinking.child(
                div()
                    .pl(m.spacing7())
                    .pt(m.spacing2())
                    .when(output.is_none(), Styled::italic)
                    .when(failed, |error| {
                        error.border_l_2().border_color(theme.danger.opacity(0.35))
                    })
                    .when(!open, Styled::truncate)
                    .text_size(m.font_body())
                    .line_height(relative(1.5))
                    .child(select::plain(
                        scope,
                        &row.key,
                        usize::from(output.is_some()),
                        0,
                        text,
                    )),
            )
        })
        .into_any_element()
}

/// More changed files than this fold into a card instead of chips.
const MAX_CHIPS: usize = 6;

/// "1m 33s · copy" and a chip per changed file, wrapping; more than
/// [`MAX_CHIPS`] files go in a card under it instead.
fn footer(
    scope: &Scope<'_>,
    row: &Row,
    duration_ms: u64,
    is_error: bool,
    changed: &[FileChange],
) -> AnyElement {
    let m = scope.model.metrics;
    let theme = &scope.model.theme;
    let has_text = scope
        .session
        .turns
        .get(&row.item.turn_id)
        .is_some_and(|turn| turn.has_final);
    let chips = changed.len() <= MAX_CHIPS;
    let line = div()
        // Scopes the chips' ids to this row.
        .id(ElementId::from((row.key.clone(), FILES)))
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(m.spacing3())
        .gap_y(m.spacing3())
        .w_full()
        .text_size(m.font_body())
        .text_color(theme.fg_muted)
        .when(is_error, |footer| {
            footer
                .child(
                    div()
                        .text_color(theme.danger)
                        .font_weight(FontWeight::MEDIUM)
                        .child(tr!("Ended with an error")),
                )
                .child("·")
        })
        .child(duration(duration_ms))
        .when(has_text, |footer| {
            footer.child("·").child(copy_button(scope, row))
        })
        .when(chips, |footer| {
            footer.children(
                changed
                    .iter()
                    .enumerate()
                    .map(|(index, change)| file_chip(scope, index, change)),
            )
        });
    div()
        .flex()
        .flex_col()
        .gap(m.spacing3())
        .w_full()
        .px(m.spacing8())
        .pt(m.spacing2())
        .pb(m.spacing4())
        .child(line)
        .when(!chips, |footer| {
            footer.child(files_card(scope, row, changed))
        })
        .into_any_element()
}

/// "› 9 files changed  [+30 -9]", closed until clicked; open, it lists a chip per file.
fn files_card(scope: &Scope<'_>, row: &Row, changed: &[FileChange]) -> AnyElement {
    let m = scope.model.metrics;
    let theme = &scope.model.theme;
    let open = scope.session.expanded.contains(&row.item.id);
    let (added, removed) = files::totals(changed);
    let (owner, key, id) = (scope.owner.clone(), scope.key, row.item.id.clone());
    let guard = select::Guard::new(scope);
    let header = div()
        .id(ElementId::from((row.key.clone(), CARD)))
        .debug_selector(|| "chat-files-card".into())
        .flex()
        .items_center()
        .gap(m.spacing3())
        .px(m.spacing4())
        .py(m.spacing3())
        .text_size(m.font_emphasis())
        .text_color(theme.fg)
        .cursor_pointer()
        .hover(|header| header.bg(theme.hover))
        .child(IconGlyph::new(
            if open {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            },
            m.icon_xs(),
            theme.fg_muted,
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(tr!("%lld files changed", changed.len())),
        )
        .child(
            div()
                .flex_none()
                .px(m.spacing3())
                .rounded(m.radius_sm())
                .border_1()
                .border_color(theme.border)
                .child(counts(scope, added, removed)),
        )
        .on_click(move |_, _, cx| {
            if !guard.dragged() {
                let _ = owner.update(cx, |model, cx| model.chat_expand(key, &id, cx));
            }
        });
    div()
        // Scopes the chips' ids to this row.
        .id(ElementId::from((row.key.clone(), CARD_FILES)))
        .flex()
        .flex_col()
        .rounded(m.radius_md())
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .child(header)
        .when(open, |card| {
            card.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(m.spacing3())
                    .px(m.spacing4())
                    .pb(m.spacing4())
                    .children(
                        changed
                            .iter()
                            .enumerate()
                            .map(|(index, change)| file_chip(scope, index, change)),
                    ),
            )
        })
        .into_any_element()
}

/// Green "+N" and red "-M", monospace.
fn counts(scope: &Scope<'_>, added: u64, removed: u64) -> gpui::Div {
    let m = scope.model.metrics;
    let theme = &scope.model.theme;
    div()
        .flex()
        .flex_none()
        .gap(m.spacing2())
        .font_family(muxy_ui::font::monospace())
        .text_size(m.font_footnote())
        .child(div().text_color(theme.diff_add).child(format!("+{added}")))
        .child(
            div()
                .text_color(theme.diff_remove)
                .child(format!("-{removed}")),
        )
}

/// A changed file: its kind's icon, name, and lines added and removed; a
/// click opens it, hovering shows its whole path.
fn file_chip(scope: &Scope<'_>, index: usize, change: &FileChange) -> AnyElement {
    let m = scope.model.metrics;
    let theme = &scope.model.theme;
    let (icon, color) = file_icon(files::kind(&change.path), theme);
    let path = SharedString::from(change.path.clone());
    let (owner, pane, target) = (scope.owner.clone(), scope.pane, path.clone());
    let guard = select::Guard::new(scope);
    let (raised, fg, border, bg) = (theme.raised(), theme.fg, theme.border, theme.bg);
    div()
        .id(("chat-file", index))
        .debug_selector(|| "chat-file-chip".into())
        .flex()
        .items_center()
        .gap(m.spacing3())
        .min_w_0()
        .max_w_full()
        .px(m.spacing3())
        .py(m.spacing1())
        .rounded(m.radius_md())
        .border_1()
        .border_color(theme.border)
        .text_size(m.font_body())
        .text_color(theme.fg)
        .cursor_pointer()
        .hover(|chip| chip.bg(theme.hover))
        .child(
            div()
                .flex_none()
                .child(IconGlyph::new(icon, m.icon_sm(), color)),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .child(files::name(&change.path).to_owned()),
        )
        .child(counts(
            scope,
            u64::from(change.added),
            u64::from(change.removed),
        ))
        .tooltip(move |_, cx| {
            cx.new(|_| Tooltip::new(path.clone(), raised, fg, border, bg))
                .into()
        })
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            if !guard.dragged() {
                let _ = owner.update(cx, |model, cx| model.chat_open_file(pane, &target, cx));
            }
        })
        .into_any_element()
}

fn copy_button(scope: &Scope<'_>, row: &Row) -> IconButton {
    let m = scope.model.metrics;
    let theme = &scope.model.theme;
    let (owner, key) = (scope.owner.clone(), scope.key);
    let guard = select::Guard::new(scope);
    let id = row.item.id.clone();
    IconButton::new(
        ElementId::from((row.key.clone(), COPY)),
        tr!("Copy Response"),
        Icon::Overlap,
        m.icon_sm(),
        m.control_small(),
        theme.fg_muted,
        theme.fg,
    )
    .tooltip(theme)
    .on_click(move |_, _, cx| {
        cx.stop_propagation();
        if !guard.dragged() {
            let _ = owner.update(cx, |model, cx| model.chat_copy(key, &id, cx));
        }
    })
}

/// "8s", "3m 12s" or "1h 5m".
pub(super) fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    let (hours, minutes) = (seconds / 3600, seconds / 60 % 60);
    if hours > 0 {
        tr!("%lldh %lldm", hours, minutes).to_string()
    } else if minutes > 0 {
        tr!("%lldm %llds", minutes, seconds % 60).to_string()
    } else {
        tr!("%llds", seconds).to_string()
    }
}
