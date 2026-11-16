//! Fork: the approval card docked above the chat input: a tool's Allow,
//! Always allow and Deny, a question's options and own answer, or a plan's
//! Approve and Keep planning. Keys 1 to 9 pick while the card has focus, and
//! Escape hands the keyboard back to the input.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt, AnyElement, App, AppContext, Context, Div, Entity, FocusHandle,
    Focusable, FontWeight, Hsla, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, Pixels, SharedString, Stateful, StatefulInteractiveElement, Styled, WeakEntity,
    Window, div, px, relative,
};
use muxy_protocol::{
    ChatAnswer, ChatQuestion, PermissionKind, PermissionPrompt, PermissionResponse, PlanDecision,
    ToolDecision,
};
use muxy_ui::components::IconGlyph;
use muxy_ui::icon::Icon;
use muxy_ui::text_input::{InputStyle, TextInput};
use muxy_ui::theme::{Metrics, Theme};
use muxy_ui::tr;

use super::ChatView;
use super::markdown;
use super::rows::Scope;
use crate::model::chat::{ChatSession, WITHDRAWN_FOR};

/// The number keys' labels, so hints don't format a string each frame.
/// How long the input must sit untouched before a new card may take the keyboard.
const CARD_FOCUS_IDLE: Duration = Duration::from_secs(1);
const DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// A question's text, made shareable once instead of copied each frame.
#[derive(Default)]
struct QuestionText {
    header: SharedString,
    question: SharedString,
    /// Each option's label, description and preview.
    options: Vec<[SharedString; 3]>,
    /// Some option has a preview, so the question shows the preview panel.
    previews: bool,
}

/// What the user has entered on the card shown now.
pub(super) struct Card {
    pub(super) request_id: String,
    pub(super) focus: FocusHandle,
    /// The card's element id, naming its pane and request so scroll offsets
    /// don't carry over from the card before.
    id: SharedString,
    /// The approval, shared into each button's handler instead of copied.
    prompt: Rc<PermissionPrompt>,
    /// The card's title, and the tool's own title beside it.
    title: (SharedString, SharedString),
    /// A tool's detail in full, and what Always allow adds.
    detail: SharedString,
    rule: SharedString,
    /// A tool's deny message or a plan's feedback.
    note: Entity<TextInput>,
    questions: Vec<QuestionText>,
    /// Each question's picked options, by index.
    picks: Vec<Vec<usize>>,
    /// Each question's option under the pointer, whose preview shows.
    hovered: Vec<Option<usize>>,
    /// Each question's own answer.
    others: Vec<Entity<TextInput>>,
    /// A plan's row key and parse.
    plan: Option<(SharedString, markdown::Parsed)>,
}

impl Card {
    pub(super) fn new(
        prompt: PermissionPrompt,
        id: SharedString,
        style: InputStyle,
        focus: FocusHandle,
        codex: bool,
        cx: &mut Context<ChatView>,
    ) -> Self {
        let input = |placeholder: SharedString, cx: &mut Context<ChatView>| {
            cx.new(|cx| TextInput::new(style, cx).with_placeholder(placeholder))
        };
        let mut card = Self {
            request_id: prompt.request_id.clone(),
            focus,
            id,
            prompt: Rc::new(prompt),
            title: Default::default(),
            detail: SharedString::default(),
            rule: SharedString::default(),
            note: input(SharedString::default(), cx),
            questions: Vec::new(),
            picks: Vec::new(),
            hovered: Vec::new(),
            others: Vec::new(),
            plan: None,
        };
        match &card.prompt.kind {
            PermissionKind::Tool {
                tool,
                title,
                detail,
                always,
            } => {
                card.title = (tr!("Allow %@?", tool), title.clone().into());
                card.detail = detail.clone().into();
                card.rule = always.as_ref().map_or_else(SharedString::default, |rule| {
                    if rule.saved_to_settings {
                        tr!(
                            "Always allow adds %@ to this project's .claude/settings.local.json",
                            &rule.label
                        )
                    } else {
                        tr!("Always allow adds %@ for this conversation", &rule.label)
                    }
                });
                card.note = input(tr!("Tell Claude what to do instead (optional)"), cx);
            }
            PermissionKind::Questions { questions } => {
                card.title.0 = if codex {
                    tr!("Codex has a question")
                } else if questions.len() > 1 {
                    tr!("Claude has %lld questions", questions.len())
                } else {
                    tr!("Claude has a question")
                };
                card.questions = questions.iter().map(question_text).collect();
                card.picks = vec![Vec::new(); questions.len()];
                card.hovered = vec![None; questions.len()];
                card.others = questions
                    .iter()
                    .map(|_| input(tr!("Or answer in your own words"), cx))
                    .collect();
            }
            PermissionKind::Plan { .. } => {
                card.title.0 = tr!("Approve Claude's plan?");
                card.plan = Some((
                    format!("plan-{}", card.request_id).into(),
                    markdown::Parsed::default(),
                ));
                card.note = input(tr!("What should Claude change? (optional)"), cx);
            }
            PermissionKind::Native { title, detail, .. } => {
                card.title.0 = title.clone().into();
                card.detail = detail.clone().into();
            }
        }
        card
    }

    fn note(&self, cx: &App) -> String {
        self.note.read(cx).text().trim().to_owned()
    }

    /// Picks option `option` of question `question`; one question with one choice answers at once.
    fn pick(
        &mut self,
        questions: &[ChatQuestion],
        question: usize,
        option: usize,
        cx: &App,
    ) -> Option<PermissionResponse> {
        let asked = questions.get(question)?;
        if option >= asked.options.len() {
            return None;
        }
        let multi = asked.multi_select;
        let picks = self.picks.get_mut(question)?;
        match picks.iter().position(|picked| *picked == option) {
            Some(at) => {
                picks.remove(at);
            }
            None if multi => picks.push(option),
            None => *picks = vec![option],
        }
        let typed = self.others(cx).iter().any(|text| !text.is_empty());
        if questions.len() == 1 && !multi && !typed {
            return self.submit(questions, cx);
        }
        None
    }

    /// Notes the pointer entering or leaving option `option` of question
    /// `question`. True when that changes the preview shown.
    fn hover(&mut self, question: usize, option: usize, inside: bool) -> bool {
        let before = self.previewed(question);
        let Some(hovered) = self.hovered.get_mut(question) else {
            return false;
        };
        // Leaving one option and entering the next arrive in either order.
        if inside {
            *hovered = Some(option);
        } else if *hovered == Some(option) {
            *hovered = None;
        }
        self.previewed(question) != before
    }

    /// The option whose preview shows: the hovered one, else the last picked, else the first.
    fn previewed(&self, question: usize) -> usize {
        self.hovered
            .get(question)
            .copied()
            .flatten()
            .or_else(|| self.picks.get(question)?.last().copied())
            .unwrap_or(0)
    }

    /// The question number keys answer: the first one unanswered, else the last.
    fn keyed(&self, cx: &App) -> usize {
        (0..self.picks.len())
            .find(|at| {
                self.picks[*at].is_empty()
                    && self
                        .others
                        .get(*at)
                        .is_none_or(|input| input.read(cx).text().trim().is_empty())
            })
            .unwrap_or(self.picks.len().saturating_sub(1))
    }

    fn others(&self, cx: &App) -> Vec<String> {
        self.others
            .iter()
            .map(|input| input.read(cx).text().trim().to_owned())
            .collect()
    }

    fn submit(&self, questions: &[ChatQuestion], cx: &App) -> Option<PermissionResponse> {
        answers(questions, &self.picks, &self.others(cx)).map(PermissionResponse::Questions)
    }
}

fn question_text(question: &ChatQuestion) -> QuestionText {
    QuestionText {
        header: question.header.clone().into(),
        question: question.question.clone().into(),
        options: question
            .options
            .iter()
            .map(|option| {
                [
                    option.label.clone().into(),
                    option.description.clone().into(),
                    option.preview.clone().into(),
                ]
            })
            .collect(),
        previews: question
            .options
            .iter()
            .any(|option| !option.preview.is_empty()),
    }
}

/// Each question's answer: its picked option labels in their order, then the
/// user's own text. `None` until every question has one.
pub(crate) fn answers(
    questions: &[ChatQuestion],
    picks: &[Vec<usize>],
    others: &[String],
) -> Option<Vec<ChatAnswer>> {
    questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let picked = picks.get(index).map_or(&[][..], Vec::as_slice);
            let mut answers: Vec<String> = question
                .options
                .iter()
                .enumerate()
                .filter(|(option, _)| picked.contains(option))
                .map(|(_, option)| option.label.clone())
                .collect();
            if let Some(text) = others.get(index).filter(|text| !text.is_empty()) {
                answers.push(text.clone());
            }
            (!answers.is_empty()).then(|| ChatAnswer {
                question: question.question.clone(),
                answers,
            })
        })
        .collect()
}

/// A tool's deny, with the user's message or a default one.
fn deny(note: String) -> PermissionResponse {
    let message = if note.is_empty() {
        tr!("The user denied this in Muxy.").to_string()
    } else {
        note
    };
    PermissionResponse::Tool(ToolDecision::Deny {
        message,
        interrupt: false,
    })
}

fn keep_planning(note: String) -> PermissionResponse {
    let feedback = if note.is_empty() {
        tr!("Keep planning; the user isn't ready to approve this plan.").to_string()
    } else {
        note
    };
    PermissionResponse::Plan(PlanDecision::KeepPlanning { feedback })
}

/// The response choice `number` (1-based) stands for, as the card's buttons number them.
fn choice(
    card: &mut Card,
    kind: &PermissionKind,
    number: usize,
    cx: &App,
) -> Option<PermissionResponse> {
    match kind {
        PermissionKind::Tool { always, .. } => match (number, always.is_some()) {
            (1, _) => Some(PermissionResponse::Tool(ToolDecision::Allow)),
            (2, true) => Some(PermissionResponse::Tool(ToolDecision::AllowAlways)),
            (2, false) | (3, true) => Some(deny(card.note(cx))),
            _ => None,
        },
        PermissionKind::Questions { questions } => {
            let question = card.keyed(cx);
            card.pick(questions, question, number.checked_sub(1)?, cx)
        }
        PermissionKind::Plan { .. } => match number {
            1 => Some(PermissionResponse::Plan(PlanDecision::Approve {
                mode: None,
            })),
            2 => Some(keep_planning(card.note(cx))),
            _ => None,
        },
        PermissionKind::Native { choices, .. } => {
            choices
                .get(number.checked_sub(1)?)
                .map(|choice| PermissionResponse::NativeChoice {
                    choice: choice.id.clone(),
                })
        }
    }
}

/// How loudly a card button asks to be pressed.
#[derive(Clone, Copy, PartialEq)]
enum Weight {
    /// Filled with the accent: the answer most people give.
    Primary,
    Secondary,
    /// Bordered like `Secondary`, its label in the danger color.
    Danger,
}

/// What every part of a card draws with.
struct Paint<'a> {
    m: Metrics,
    theme: &'a Theme,
    view: WeakEntity<ChatView>,
    prompt: &'a Rc<PermissionPrompt>,
    enabled: bool,
}

impl Paint<'_> {
    /// The card's title, the tool's own title muted beside it, and how many more wait.
    fn header(&self, (title, subtitle): &(SharedString, SharedString), more: usize) -> Div {
        let (m, theme) = (self.m, self.theme);
        div()
            .flex()
            .items_center()
            .gap(m.spacing3())
            .min_w_0()
            .child(
                div()
                    .flex_none()
                    .text_size(m.font_emphasis())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.fg)
                    .child(title.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(m.font_body())
                    .text_color(theme.fg_muted)
                    .child(subtitle.clone()),
            )
            .when(more > 0, |header| {
                header.child(
                    div()
                        .flex_none()
                        .px(m.spacing3())
                        .py(m.spacing1())
                        .rounded(m.radius_sm())
                        .bg(theme.surface)
                        .text_size(m.font_caption())
                        .text_color(theme.fg_muted)
                        .child(if more == 1 {
                            tr!("+1 more")
                        } else {
                            tr!("+%lld more", more)
                        }),
                )
            })
    }

    /// A muted key hint, such as the `1` before "Allow once".
    fn hint(&self, number: usize, color: Hsla) -> Div {
        let m = self.m;
        div()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .min_w(m.scaled(16.0))
            .h(m.scaled(16.0))
            .px(m.spacing1())
            .rounded(m.radius_sm())
            .border_1()
            .border_color(color.opacity(0.3))
            .font_family(muxy_ui::font::monospace())
            .text_size(m.font_caption())
            .text_color(color.opacity(0.7))
            .child(SharedString::new_static(DIGITS[number - 1]))
    }

    /// A button answering with choice `number`, its key hint before its label.
    fn button(&self, number: usize, label: SharedString, weight: Weight) -> Stateful<Div> {
        let (m, theme) = (self.m, self.theme);
        let (view, prompt) = (self.view.clone(), self.prompt.clone());
        self.action(("chat-card-choice", number), weight)
            .child(self.hint(
                number,
                match weight {
                    Weight::Primary => theme.accent_foreground,
                    Weight::Secondary => theme.fg_muted,
                    Weight::Danger => theme.danger,
                },
            ))
            .child(label)
            .gap(m.spacing3())
            .when(self.enabled, |button| {
                button.on_click(move |_, _, cx| {
                    let _ = view.update(cx, |view, cx| view.choose(&prompt, number, cx));
                })
            })
    }

    /// A card button's shape and colors, without a handler.
    fn action(&self, id: (&'static str, usize), weight: Weight) -> Stateful<Div> {
        let (m, theme) = (self.m, self.theme);
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .h(m.scaled(28.0))
            .px(m.spacing5())
            .rounded(m.radius_md())
            .text_size(m.font_body())
            .font_weight(FontWeight::MEDIUM)
            .map(|button| match weight {
                Weight::Primary => button.bg(theme.accent).text_color(theme.accent_foreground),
                Weight::Secondary => button
                    .border_1()
                    .border_color(theme.border)
                    .text_color(theme.fg),
                Weight::Danger => button
                    .border_1()
                    .border_color(theme.border)
                    .text_color(theme.danger),
            })
            .when(!self.enabled, |button| button.opacity(0.4))
            .when(self.enabled, |button| {
                button.cursor_pointer().hover(|button| match weight {
                    Weight::Primary => button.opacity(0.9),
                    Weight::Secondary => button.bg(theme.hover),
                    Weight::Danger => button.bg(theme.danger.opacity(0.1)),
                })
            })
    }

    /// A text field inset in the card, ringed like the chat input while it has the keyboard.
    fn field(&self, input: &Entity<TextInput>, window: &Window, cx: &App) -> Div {
        let (m, theme) = (self.m, self.theme);
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        div()
            .w_full()
            .px(m.spacing5())
            .py(m.spacing3())
            .rounded(m.radius_lg())
            .border_1()
            .border_color(if focused {
                theme.accent.opacity(0.6)
            } else {
                theme.border
            })
            .bg(theme.bg)
            .cursor_text()
            .child(input.clone())
    }

    /// Monospace text in a bordered, scrolling box.
    fn code(&self, id: (&'static str, usize), text: SharedString, max: Pixels) -> Stateful<Div> {
        let (m, theme) = (self.m, self.theme);
        div()
            .id(id)
            .w_full()
            .min_w_0()
            .max_h(max)
            .px(m.spacing5())
            .py(m.spacing4())
            .rounded(m.radius_md())
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .font_family(muxy_ui::font::monospace())
            .text_size(m.font_footnote())
            .line_height(relative(1.5))
            .text_color(theme.fg)
            .child(text)
    }

    /// A tool's request in full, never shortened: the user must see exactly what they approve.
    fn tool(&self, card: &Card, window: &Window, cx: &App) -> (AnyElement, Vec<AnyElement>) {
        let (m, theme) = (self.m, self.theme);
        let always = !card.rule.is_empty();
        let mut buttons = vec![self.button(1, tr!("Allow once"), Weight::Primary)];
        if always {
            buttons.push(self.button(2, tr!("Always allow"), Weight::Secondary));
        }
        let deny = if always { 3 } else { 2 };
        buttons.push(self.button(deny, tr!("Deny"), Weight::Danger));
        let body = div()
            .flex()
            .flex_col()
            .gap(m.spacing4())
            .when(!card.detail.is_empty(), |body| {
                // Long commands wrap and scroll here rather than run off the card.
                body.child(
                    self.code(("chat-approval-detail", 0), card.detail.clone(), px(200.0))
                        .overflow_y_scroll(),
                )
            })
            .child(self.field(&card.note, window, cx));
        let mut footer: Vec<AnyElement> = buttons
            .into_iter()
            .map(IntoElement::into_any_element)
            .collect();
        if always {
            footer.push(
                div()
                    .w_full()
                    .text_size(m.font_footnote())
                    .text_color(theme.fg_muted)
                    .child(card.rule.clone())
                    .into_any_element(),
            );
        }
        (body.into_any_element(), footer)
    }

    fn native(
        &self,
        card: &Card,
        choices: &[muxy_protocol::ChatApprovalChoice],
    ) -> (AnyElement, Vec<AnyElement>) {
        let body = self
            .code(("chat-native-detail", 0), card.detail.clone(), px(240.0))
            .overflow_y_scroll()
            .into_any_element();
        let footer = choices
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                div()
                    .flex()
                    .flex_col()
                    .gap(self.m.spacing1())
                    .child(self.button(
                        index + 1,
                        choice.label.clone().into(),
                        if choice.label.starts_with("Deny") {
                            Weight::Danger
                        } else {
                            Weight::Secondary
                        },
                    ))
                    .child(
                        div()
                            .max_w(px(360.0))
                            .text_size(self.m.font_footnote())
                            .text_color(self.theme.fg_muted)
                            .child(choice.scope.clone()),
                    )
                    .into_any_element()
            })
            .collect();
        (body, footer)
    }

    /// Each question with its options, the focused option's preview beside
    /// them when it has one, and a field for the user's own answer.
    fn questions(
        &self,
        card: &Card,
        questions: &[ChatQuestion],
        max: Pixels,
        window: &Window,
        cx: &App,
    ) -> (AnyElement, Vec<AnyElement>) {
        let (m, theme) = (self.m, self.theme);
        let keyed = card.keyed(cx);
        let mut body = div()
            .id("chat-questions")
            .max_h(max)
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(m.spacing7());
        for (index, (question, text)) in questions.iter().zip(&card.questions).enumerate() {
            let picked = card.picks.get(index).map_or(&[][..], Vec::as_slice);
            let previewed = card.previewed(index);
            let options = (0..text.options.len()).map(|option| {
                self.option(
                    question,
                    text,
                    index,
                    option,
                    picked.contains(&option),
                    keyed,
                )
                .when(
                    option == previewed && text.previews && !picked.contains(&option),
                    |row| row.bg(theme.hover),
                )
            });
            let list = div()
                .flex_1()
                .flex_basis(m.scaled(240.0))
                .min_w_0()
                .flex()
                .flex_col()
                .gap(m.spacing1())
                .children(options);
            // Side by side when the card is wide enough, the preview below the list when not.
            let choices = div()
                .flex()
                .flex_wrap()
                .items_start()
                .gap(m.spacing5())
                .child(list)
                .when(text.previews, |choices| {
                    choices.child(self.preview(index, previewed, text))
                });
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(m.spacing4())
                    .child(self.heading(text))
                    .when(question.multi_select, |block| {
                        block.child(
                            div()
                                .text_size(m.font_footnote())
                                .text_color(theme.fg_muted)
                                .child(tr!("Select all that apply")),
                        )
                    })
                    .child(choices)
                    .children(
                        card.others
                            .get(index)
                            .filter(|_| question.allow_other != Some(false))
                            .map(|input| self.field(input, window, cx)),
                    ),
            );
        }
        let submit = {
            let (view, prompt) = (self.view.clone(), self.prompt.clone());
            self.action(("chat-card-submit", 0), Weight::Primary)
                .child(tr!("Submit"))
                .when(self.enabled, |button| {
                    button.on_click(move |_, _, cx| {
                        let _ = view.update(cx, |view, cx| view.submit(&prompt, cx));
                    })
                })
        };
        (body.into_any_element(), vec![submit.into_any_element()])
    }

    /// A question with its header as a chip before it.
    fn heading(&self, text: &QuestionText) -> Div {
        let (m, theme) = (self.m, self.theme);
        div()
            .flex()
            .items_center()
            .gap(m.spacing3())
            .when(!text.header.is_empty(), |line| {
                line.child(
                    div()
                        .flex_none()
                        .px(m.spacing3())
                        .py(m.spacing1())
                        .rounded(m.radius_sm())
                        .bg(theme.accent_soft)
                        .text_size(m.font_caption())
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.fg)
                        .child(text.header.clone()),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(m.font_emphasis())
                    .text_color(theme.fg)
                    .child(text.question.clone()),
            )
    }

    /// One option: its radio or checkbox, label and description, and its key
    /// hint while number keys answer its question.
    fn option(
        &self,
        question: &ChatQuestion,
        text: &QuestionText,
        index: usize,
        option: usize,
        chosen: bool,
        keyed: usize,
    ) -> Stateful<Div> {
        let (m, theme) = (self.m, self.theme);
        let [label, description, _] = &text.options[option];
        let (view, prompt) = (self.view.clone(), self.prompt.clone());
        let enabled = self.enabled;
        div()
            .id(("chat-option", index << 8 | option))
            .flex()
            .items_start()
            .gap(m.spacing4())
            .px(m.spacing4())
            .py(m.spacing3())
            .rounded(m.radius_md())
            .border_1()
            .border_color(if chosen {
                theme.accent.opacity(0.5)
            } else {
                gpui::transparent_black()
            })
            .when(chosen, |row| row.bg(theme.accent_soft))
            .when(enabled, |row| {
                row.cursor_pointer()
                    .when(!chosen, |row| row.hover(|row| row.bg(theme.hover)))
            })
            .child(mark(question.multi_select, chosen, m, theme))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(m.spacing1())
                    .child(
                        div()
                            .text_size(m.font_body())
                            .text_color(theme.fg)
                            .child(label.clone()),
                    )
                    .when(!description.is_empty(), |texts| {
                        texts.child(
                            div()
                                .text_size(m.font_footnote())
                                .text_color(theme.fg_muted)
                                .child(description.clone()),
                        )
                    }),
            )
            .when(index == keyed && option < DIGITS.len(), |row| {
                row.child(self.hint(option + 1, theme.fg_muted))
            })
            .when(text.previews, |row| {
                let view = view.clone();
                row.on_hover(move |inside, _, cx| {
                    let _ = view.update(cx, |view, cx| view.hover(index, option, *inside, cx));
                })
            })
            .on_click(move |_, _, cx| {
                if enabled {
                    let _ = view.update(cx, |view, cx| view.pick(&prompt, index, option, cx));
                }
            })
    }

    /// The focused option's preview as written: monospace, unwrapped, scrolling both ways.
    fn preview(&self, question: usize, option: usize, text: &QuestionText) -> AnyElement {
        let (m, theme) = (self.m, self.theme);
        let preview = text.options.get(option).map(|texts| texts[2].clone());
        // The option is in the id, so each one opens scrolled to its top.
        div()
            .id(("chat-option-preview", question << 8 | option))
            .flex_1()
            .flex_basis(m.scaled(280.0))
            .min_w_0()
            .max_h(m.scaled(240.0))
            .overflow_scroll()
            .flex()
            .flex_col()
            .items_start()
            .rounded(m.radius_md())
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .child(match preview.filter(|preview| !preview.is_empty()) {
                Some(preview) => div()
                    .flex_none()
                    .px(m.spacing5())
                    .py(m.spacing4())
                    .whitespace_nowrap()
                    .font_family(muxy_ui::font::monospace())
                    .text_size(m.font_footnote())
                    .line_height(relative(1.5))
                    .text_color(theme.fg)
                    .child(preview),
                None => div()
                    .px(m.spacing5())
                    .py(m.spacing4())
                    .text_size(m.font_footnote())
                    .text_color(theme.fg_muted)
                    .child(tr!("No preview for this option")),
            })
            .into_any_element()
    }

    fn plan(
        &self,
        card: &Card,
        markdown_text: &str,
        scope: &Scope<'_>,
        max: Pixels,
        window: &Window,
        cx: &App,
    ) -> (AnyElement, Vec<AnyElement>) {
        let (m, theme) = (self.m, self.theme);
        let body = div()
            .flex()
            .flex_col()
            .gap(m.spacing4())
            .child(
                div()
                    .id("chat-plan")
                    .max_h(max)
                    .overflow_y_scroll()
                    .py(m.spacing4())
                    .rounded(m.radius_md())
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.bg)
                    .text_color(theme.fg)
                    .children(card.plan.as_ref().map(|(key, parsed)| {
                        let open = scope.session.stretched.get(key);
                        markdown::render(
                            scope,
                            &parsed.get(markdown_text, open, 0, &|_| {
                                crate::model::chat::Lookup::Found(None)
                            }),
                            key,
                        )
                    })),
            )
            .child(self.field(&card.note, window, cx));
        (
            body.into_any_element(),
            vec![
                self.button(1, tr!("Approve"), Weight::Primary)
                    .into_any_element(),
                self.button(2, tr!("Keep planning"), Weight::Secondary)
                    .into_any_element(),
            ],
        )
    }
}

/// A radio for a one-choice question, a checkbox for a many-choice one.
fn mark(multi: bool, on: bool, m: Metrics, theme: &Theme) -> Div {
    let mark = div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        // Centers the mark on the label's first line.
        .mt(m.scaled(1.5))
        .size(m.scaled(14.0))
        .border_1()
        .border_color(if on { theme.accent } else { theme.border_solid() });
    if multi {
        mark.rounded(m.radius_sm()).when(on, |mark| {
            mark.bg(theme.accent).child(IconGlyph::new(
                Icon::Check,
                m.scaled(10.0),
                theme.accent_foreground,
            ))
        })
    } else {
        mark.rounded_full().when(on, |mark| {
            mark.child(div().size(m.scaled(6.0)).rounded_full().bg(theme.accent))
        })
    }
}

impl ChatView {
    /// Answers the shown card with choice `number`, if it is one.
    fn choose(&mut self, prompt: &PermissionPrompt, number: usize, cx: &mut Context<Self>) {
        let Some(card) = self
            .card
            .as_mut()
            .filter(|card| card.request_id == prompt.request_id)
        else {
            return;
        };
        let response = choice(card, &prompt.kind, number, cx);
        cx.notify();
        if let Some(response) = response {
            self.respond(prompt, response, cx);
        }
    }

    fn respond(
        &mut self,
        prompt: &PermissionPrompt,
        response: PermissionResponse,
        cx: &mut Context<Self>,
    ) {
        let (key, request, owner) = (self.key, prompt.request_id.clone(), self.owner.clone());
        // Deferred: answering redraws every view of this chat, this one included.
        cx.defer(move |cx| {
            let _ = owner.update(cx, |model, cx| {
                model.chat_respond(key, request, response, cx);
            });
        });
    }

    /// Redraws only when the pointer changes which preview shows.
    fn hover(&mut self, question: usize, option: usize, inside: bool, cx: &mut Context<Self>) {
        if self
            .card
            .as_mut()
            .is_some_and(|card| card.hover(question, option, inside))
        {
            cx.notify();
        }
    }

    /// Keeps the card's inputs for `prompt`, the approval showing now, and drops them with it.
    pub(super) fn sync_card(
        &mut self,
        prompt: Option<PermissionPrompt>,
        style: InputStyle,
        codex: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let same = |card: &Card| {
            prompt
                .as_ref()
                .is_some_and(|prompt| card.request_id == prompt.request_id)
        };
        if self.card.as_ref().is_some_and(same) {
            return;
        }
        // A card that goes while it has the keyboard hands it back to the input.
        let focused = self
            .card
            .take()
            .is_some_and(|card| card.focus.contains_focused(window, cx));
        // A new card takes the keyboard from an empty input, so its keys work
        // at once; a draft keeps it, so typing on can't answer by accident.
        let idle = self.empty
            && self.input.read(cx).focus_handle(cx).is_focused(window)
            && self
                .last_edit
                .is_none_or(|at| at.elapsed() > CARD_FOCUS_IDLE);
        self.card = prompt.map(|prompt| {
            let id = format!("chat-approval-{}-{}", self.pane, prompt.request_id).into();
            Card::new(prompt, id, style, cx.focus_handle(), codex, cx)
        });
        match &self.card {
            Some(card) if idle || focused => card.focus.focus(window),
            _ if focused => self.focus_input(window, cx),
            _ => {}
        }
    }

    /// The card for the first approval that has waited long enough, or the
    /// fading one Claude withdrew.
    pub(super) fn approval(
        &self,
        session: &ChatSession,
        scope: &Scope<'_>,
        now: Instant,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let Some(prompt) = session.shown_approval(now) else {
            let (prompt, at) = session.withdrawn.as_ref()?;
            return (now.duration_since(*at) < WITHDRAWN_FOR)
                .then(|| withdrawn(prompt, *at, scope));
        };
        let card = self
            .card
            .as_ref()
            .filter(|card| card.request_id == prompt.request_id)?;
        let m = scope.model.metrics;
        let theme = &scope.model.theme;
        let paint = Paint {
            m,
            theme,
            view: cx.entity().downgrade(),
            prompt: &card.prompt,
            enabled: !session.answering.contains(&prompt.request_id),
        };
        // Tall plans and question lists scroll, so the input stays on screen.
        let viewport = window.viewport_size().height;
        let max = px((f32::from(viewport) * 0.4).clamp(160.0, 480.0));
        let (body, footer) = match &card.prompt.kind {
            PermissionKind::Tool { .. } => paint.tool(card, window, cx),
            PermissionKind::Questions { questions } => {
                paint.questions(card, questions, max, window, cx)
            }
            PermissionKind::Plan { markdown } => paint.plan(card, markdown, scope, max, window, cx),
            PermissionKind::Native { choices, .. } => paint.native(card, choices),
        };
        let focus = card.focus.clone();
        let focused = focus.contains_focused(window, cx);
        let keys = {
            let prompt = card.prompt.clone();
            cx.listener(move |view, event: &KeyDownEvent, window, cx| {
                let Some(focus) = view.card.as_ref().map(|card| card.focus.clone()) else {
                    return;
                };
                let key = event.keystroke.key.as_str();
                let own = focus.is_focused(window);
                if key == "escape" {
                    // Out of a field to the card's keys, then out to the input.
                    if own {
                        view.focus_input(window, cx);
                    } else {
                        focus.focus(window);
                    }
                    cx.stop_propagation();
                } else if !own {
                    // Typing in a field, where numbers are text.
                } else if let Some(number) =
                    key.parse::<usize>().ok().filter(|n| (1..=9).contains(n))
                {
                    if !event.keystroke.modifiers.modified() {
                        view.choose(&prompt, number, cx);
                        cx.stop_propagation();
                    }
                } else if key == "tab" {
                    view.focus_input(window, cx);
                    cx.stop_propagation();
                }
            })
        };
        Some(
            div()
                .id(card.id.clone())
                .debug_selector(|| "chat-approval".into())
                .track_focus(&focus)
                .on_key_down(keys)
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    // Clicks inside keep the keyboard here, not in the input below.
                    if !focus.contains_focused(window, cx) {
                        focus.focus(window);
                    }
                    cx.stop_propagation();
                })
                .flex()
                .flex_col()
                .gap(m.spacing6())
                .p(m.spacing6())
                .rounded(m.radius_xl())
                .border_1()
                .border_color(if focused {
                    theme.accent.opacity(0.6)
                } else {
                    theme.border
                })
                .bg(theme.raised())
                .child(paint.header(&card.title, session.pending.len().saturating_sub(1)))
                .child(body)
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(m.spacing3())
                        .children(footer),
                )
                .into_any_element(),
        )
    }

    fn pick(
        &mut self,
        prompt: &PermissionPrompt,
        question: usize,
        option: usize,
        cx: &mut Context<Self>,
    ) {
        let PermissionKind::Questions { questions } = &prompt.kind else {
            return;
        };
        let response = self
            .card
            .as_mut()
            .filter(|card| card.request_id == prompt.request_id)
            .and_then(|card| card.pick(questions, question, option, cx));
        cx.notify();
        if let Some(response) = response {
            self.respond(prompt, response, cx);
        }
    }

    fn submit(&mut self, prompt: &PermissionPrompt, cx: &mut Context<Self>) {
        let PermissionKind::Questions { questions } = &prompt.kind else {
            return;
        };
        let Some(card) = self.card.as_ref() else {
            return;
        };
        match card.submit(questions, cx) {
            Some(response) => self.respond(prompt, response, cx),
            None => {
                let _ = self.owner.update(cx, |model, cx| {
                    model.show_notice(tr!("Answer every question first.").to_string(), cx);
                });
            }
        }
    }
}

/// A withdrawn card fades out over [`WITHDRAWN_FOR`].
fn withdrawn(prompt: &PermissionPrompt, at: Instant, scope: &Scope<'_>) -> AnyElement {
    let m = scope.model.metrics;
    let theme = &scope.model.theme;
    let total = WITHDRAWN_FOR.as_secs_f32();
    let start = (at.elapsed().as_secs_f32() / total).min(1.0);
    div()
        .debug_selector(|| "chat-approval-withdrawn".into())
        .flex()
        .items_center()
        .gap(m.spacing4())
        .p(m.spacing6())
        .rounded(m.radius_xl())
        .border_1()
        .border_color(theme.border)
        .bg(theme.raised())
        .text_size(m.font_body())
        .text_color(theme.fg_muted)
        .child(IconGlyph::new(Icon::X, m.icon_sm(), theme.fg_muted))
        .child(tr!("This request was withdrawn"))
        .with_animation(
            SharedString::from(format!("chat-withdrawn-{}", prompt.request_id)),
            Animation::new(WITHDRAWN_FOR.mul_f32(1.0 - start)),
            move |card, delta| card.opacity((1.0 - start) * (1.0 - delta)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, SharedString, TestAppContext, px};
    use muxy_protocol::{
        AlwaysAllow, ChatQuestion, ChatQuestionOption, PermissionKind, PermissionResponse,
        PlanDecision, ToolDecision,
    };
    use muxy_ui::text_input::{InputStyle, TextInput};

    use super::{Card, answers, choice};

    fn card(cx: &mut TestAppContext) -> Card {
        let color = gpui::black();
        let style = InputStyle {
            text: color,
            placeholder: color,
            ghost: color,
            cursor: color,
            selection: color,
            scrollbar: color,
            font_size: px(13.0),
            line_height: px(18.0),
        };
        Card {
            request_id: "r".into(),
            focus: cx.update(|cx| cx.focus_handle()),
            id: "card".into(),
            prompt: std::rc::Rc::new(muxy_protocol::PermissionPrompt {
                request_id: "r".into(),
                tool_use_id: None,
                kind: tool(false),
            }),
            title: Default::default(),
            detail: SharedString::default(),
            rule: SharedString::default(),
            note: cx.new(|cx| TextInput::new(style, cx)),
            questions: Vec::new(),
            picks: Vec::new(),
            hovered: Vec::new(),
            others: Vec::new(),
            plan: None,
        }
    }

    fn tool(always: bool) -> PermissionKind {
        PermissionKind::Tool {
            tool: "Bash".into(),
            title: String::new(),
            detail: "touch x".into(),
            always: always.then(|| AlwaysAllow {
                label: "Bash(touch x)".into(),
                saved_to_settings: true,
            }),
        }
    }

    #[gpui::test]
    fn numbers_answer_as_the_cards_buttons_are_numbered(cx: &mut TestAppContext) {
        let mut card = card(cx);
        let pick = |card: &mut Card, kind: &PermissionKind, number, cx: &mut TestAppContext| {
            cx.update(|cx| choice(card, kind, number, cx))
        };
        let allow = Some(PermissionResponse::Tool(ToolDecision::Allow));
        let deny = |response: Option<PermissionResponse>| {
            matches!(
                response,
                Some(PermissionResponse::Tool(ToolDecision::Deny { .. }))
            )
        };
        assert_eq!(pick(&mut card, &tool(false), 1, cx), allow);
        // Without a rule there is no Always allow: 2 denies.
        assert!(deny(pick(&mut card, &tool(false), 2, cx)));
        assert_eq!(pick(&mut card, &tool(false), 3, cx), None);
        assert_eq!(
            pick(&mut card, &tool(true), 2, cx),
            Some(PermissionResponse::Tool(ToolDecision::AllowAlways))
        );
        assert!(deny(pick(&mut card, &tool(true), 3, cx)));

        let plan = PermissionKind::Plan {
            markdown: "do it".into(),
        };
        assert_eq!(
            pick(&mut card, &plan, 1, cx),
            Some(PermissionResponse::Plan(PlanDecision::Approve {
                mode: None
            }))
        );
        assert!(matches!(
            pick(&mut card, &plan, 2, cx),
            Some(PermissionResponse::Plan(PlanDecision::KeepPlanning { .. }))
        ));
    }

    fn question(text: &str, multi: bool) -> ChatQuestion {
        ChatQuestion {
            allow_other: None,
            question: text.into(),
            header: String::new(),
            options: ["A", "B", "C"]
                .into_iter()
                .map(|label| ChatQuestionOption {
                    label: label.into(),
                    description: String::new(),
                    preview: String::new(),
                })
                .collect(),
            multi_select: multi,
        }
    }

    #[gpui::test]
    fn number_keys_answer_the_first_open_question_and_ignore_missing_options(
        cx: &mut TestAppContext,
    ) {
        let mut card = card(cx);
        card.picks = vec![Vec::new(); 2];
        let kind = PermissionKind::Questions {
            questions: vec![question("Which?", false), question("Why?", false)],
        };
        for number in [1, 9, 2, 9] {
            assert_eq!(cx.update(|cx| choice(&mut card, &kind, number, cx)), None);
        }
        assert_eq!(card.picks, [vec![0], vec![1]]);
    }

    #[gpui::test]
    fn previews_follow_the_pointer_in_any_event_order_and_picks_still_answer(
        cx: &mut TestAppContext,
    ) {
        let mut card = card(cx);
        card.picks = vec![Vec::new()];
        card.hovered = vec![None];
        assert_eq!(card.previewed(0), 0);
        assert!(card.hover(0, 2, true));
        // Entering the next option can arrive before leaving the last.
        assert!(card.hover(0, 1, true));
        assert!(!card.hover(0, 2, false));
        assert_eq!(card.previewed(0), 1);
        assert!(card.hover(0, 1, false));
        assert_eq!(card.previewed(0), 0);
        // Out of range is ignored rather than panicking.
        assert!(!card.hover(5, 0, true));

        assert!(card.hover(0, 2, true));
        let kind = PermissionKind::Questions {
            questions: vec![question("Which?", false)],
        };
        let answered = cx.update(|cx| choice(&mut card, &kind, 2, cx));
        let Some(PermissionResponse::Questions(answered)) = answered else {
            panic!("one single-choice question answers on its pick");
        };
        assert_eq!(answered[0].answers, ["B"]);
        // With the pointer gone, the pick's preview shows.
        card.hover(0, 2, false);
        assert_eq!(card.previewed(0), 1);
    }

    #[test]
    fn answers_name_each_question_with_its_picked_labels_then_its_own_text() {
        let questions = [question("Which?", true), question("Why?", false)];
        let answered = answers(
            &questions,
            &[vec![2, 0], vec![]],
            &[String::new(), "because".into()],
        )
        .expect("every question has an answer");
        assert_eq!(answered[0].question, "Which?");
        // Option order, not click order: the server joins them for the CLI.
        assert_eq!(answered[0].answers, ["A", "C"]);
        assert_eq!(answered[1].answers, ["because"]);
    }

    #[test]
    fn an_unanswered_question_holds_the_whole_response_back() {
        let questions = [question("Which?", false), question("Why?", false)];
        assert!(
            answers(
                &questions,
                &[vec![1], vec![]],
                &[String::new(), String::new()]
            )
            .is_none()
        );
        assert!(answers(&questions, &[vec![9], vec![1]], &[]).is_none());
    }
}
