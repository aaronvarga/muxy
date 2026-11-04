mod layout;
pub(crate) mod menu;

use gpui::{
    AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render,
    Subscription, Task, Window, actions, px,
};
use muxy_app_core::{
    composer::ComposerDraft,
    settings::{ComposerPosition, ComposerSettings},
};
use muxy_ui::{
    l10n::tr_key,
    panel::{
        PanelMode, PanelPlacement, PanelPosition, PanelResizeState, PanelSizeBounds, PanelSizing,
    },
    text_input::{InputEvent, InputStyle, TextInput},
    theme::{Metrics, Theme},
    tr,
};

actions!(
    composer,
    [Submit, Insert, Newline, Complete, Close, ToggleVoice]
);
pub(crate) const PANEL: &str = "builtin:composer";
pub(crate) const TRANSITION: std::time::Duration = std::time::Duration::from_millis(150);

/// Height of the docked composer's input row; inactive sessions draw a
/// placeholder of the same height so terminals never resize on focus changes.
pub(crate) fn dock_height(metrics: Metrics) -> gpui::Pixels {
    metrics.scaled(56.0)
}

#[derive(Clone)]
pub(crate) enum ComposerEvent {
    Changed,
    Submit(bool),
    Attach,
    Files(Vec<std::path::PathBuf>),
    Image(Vec<u8>),
    Close,
    Menu,
    QuickCommands,
    QuickCommand(usize),
    ManageQuickCommands,
    Preferences(ComposerSettings),
}

pub(crate) struct Composer {
    pub(crate) input: Entity<TextInput>,
    pub(crate) draft: ComposerDraft,
    pub(crate) settings: ComposerSettings,
    pub(crate) error: Option<String>,
    pub(crate) sending: bool,
    theme: Theme,
    metrics: Metrics,
    resize: PanelResizeState,
    header_focus: [FocusHandle; 6],
    _input_subscription: Subscription,
    voice: Option<muxy_ui::voice::Recorder>,
    voice_snapshot: muxy_ui::voice::Snapshot,
    voice_poll: Option<Task<()>>,
    /// Whether dictation can run, from the app's capability table.
    pub(crate) dictation: muxy_core::capability::Capability,
    pub(crate) workspace_inset: f32,
    pub(crate) context_label: String,
    floating_dimensions: Option<gpui::Size<gpui::Pixels>>,
    size_motion: Option<layout::SizeMotion>,
    /// Where the button that opens the menu was last drawn.
    pub(crate) commands_anchor: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
    pub(crate) menu_anchor: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
    pub(crate) visibility: muxy_ui::motion::VisibilityMotion,
    input_style: InputStyle,
    /// The session agent's skills, offered while a `/` or `$` word is typed.
    pub(crate) skills: std::sync::Arc<Vec<muxy_app_core::agent_skills::Skill>>,
    skill_selected: usize,
    /// Where the word whose suggestions Escape dismissed starts.
    skill_dismissed: Option<usize>,
    floating_resize: Option<(
        gpui::Point<gpui::Pixels>,
        gpui::Size<gpui::Pixels>,
        layout::ResizeEdge,
    )>,
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    pub(crate) fn new(
        draft: ComposerDraft,
        settings: ComposerSettings,
        theme: Theme,
        metrics: Metrics,
        cx: &mut Context<Self>,
    ) -> Self {
        let owner = cx.weak_entity();
        let input = cx.new(|cx| {
            TextInput::new(InputStyle::field(&theme, &metrics), cx)
                .multiline()
                .with_text(draft.text.clone())
                .with_placeholder_key(tr_key!("Type or speak…"))
                .with_placeholder_font(".SystemUIFont")
                .with_paste_delegate(move |_, cx| {
                    let content = muxy_ui::pasteboard::read_content(cx);
                    if matches!(content, Ok(muxy_ui::pasteboard::Content::Text)) {
                        return false;
                    }
                    let _ = owner.update(cx, |view: &mut Self, cx| match content {
                        Ok(muxy_ui::pasteboard::Content::Files(paths)) => {
                            cx.emit(ComposerEvent::Files(paths));
                        }
                        Ok(muxy_ui::pasteboard::Content::Image(bytes)) => {
                            cx.emit(ComposerEvent::Image(bytes));
                        }
                        Err(error) => {
                            view.error = Some(error);
                            cx.notify();
                        }
                        Ok(muxy_ui::pasteboard::Content::Text) => {}
                    });
                    true
                })
        });
        let subscription = cx.subscribe(&input, |view: &mut Self, input, event, cx| {
            if matches!(event, InputEvent::Cancelled) {
                if view.completion(cx).is_some() {
                    view.dismiss_completion(cx);
                } else {
                    cx.emit(ComposerEvent::Close);
                }
            }
            if matches!(event, InputEvent::Changed) {
                view.skill_selected = 0;
                if input.read(cx).text().is_empty() {
                    view.skill_dismissed = None;
                }
                input.read(cx).text().clone_into(&mut view.draft.text);
                cx.emit(ComposerEvent::Changed);
            }
        });
        let input_style = InputStyle::field(&theme, &metrics);
        let mut view = Self {
            input,
            draft,
            settings,
            error: None,
            sending: false,
            theme,
            metrics,
            resize: PanelResizeState::default(),
            header_focus: std::array::from_fn(|_| cx.focus_handle()),
            _input_subscription: subscription,
            voice: None,
            voice_snapshot: muxy_ui::voice::Snapshot::default(),
            voice_poll: None,
            dictation: muxy_core::capability::Capability::Supported,
            workspace_inset: 0.0,
            context_label: String::new(),
            floating_resize: None,
            floating_dimensions: None,
            size_motion: None,
            menu_anchor: std::rc::Rc::default(),
            commands_anchor: std::rc::Rc::default(),
            visibility: muxy_ui::motion::VisibilityMotion::showing(
                TRANSITION,
                cx.background_executor().clone(),
            ),
            input_style,
            skills: std::sync::Arc::default(),
            skill_selected: 0,
            skill_dismissed: None,
        };
        view.refresh_style(cx);
        view
    }

    pub(crate) fn sizing(&self, window: &Window) -> PanelSizing {
        let placement = self.placement();
        let (dimension, maximum, minimum) = if placement.position == PanelPosition::Right {
            (
                self.settings.width,
                f32::from(window.viewport_size().width) - self.workspace_inset - 100.0,
                280.0,
            )
        } else {
            (
                self.settings.height,
                f32::from(window.viewport_size().height) - 160.0,
                120.0,
            )
        };
        PanelSizing::new(
            &placement,
            dimension,
            PanelSizeBounds::new(
                minimum,
                maximum
                    .max(minimum)
                    .min(if placement.position == PanelPosition::Right {
                        800.0
                    } else {
                        600.0
                    }),
            ),
            self.resize.clone(),
        )
    }

    pub(crate) fn placement(&self) -> PanelPlacement {
        PanelPlacement::new(
            PANEL,
            if self.settings.position == ComposerPosition::Right {
                PanelPosition::Right
            } else {
                PanelPosition::Bottom
            },
            if self.settings.pinned {
                PanelMode::Pinned
            } else {
                PanelMode::Floating
            },
        )
    }

    pub(crate) fn appearance(&mut self, theme: Theme, metrics: Metrics, cx: &mut Context<Self>) {
        self.theme = theme;
        self.metrics = metrics;
        self.refresh_style(cx);
    }

    pub(crate) fn refresh_style(&mut self, cx: &mut Context<Self>) {
        let mut style = InputStyle::field(&self.theme, &self.metrics);
        style.font_size = px(self.settings.font_size);
        let family = if self.settings.font_family.is_empty()
            || !cx
                .text_system()
                .all_font_names()
                .contains(&self.settings.font_family)
        {
            muxy_ui::font::monospace()
        } else {
            self.settings.font_family.clone().into()
        };
        let font = gpui::font(family.clone());
        let id = cx.text_system().resolve_font(&font);
        let height = cx.text_system().ascent(id, style.font_size)
            + px(f32::from(cx.text_system().descent(id, style.font_size)).abs());
        style.line_height = px((f32::from(height) * self.settings.line_height).ceil());
        style.cursor = self.theme.fg;
        style.placeholder = gpui::Hsla {
            a: self.theme.fg_muted.a * 0.55,
            ..self.theme.fg_muted
        };
        self.input_style = style;
        self.input.update(cx, |input, cx| {
            input.set_style(style, cx);
            input.set_font_family(Some(family), cx);
        });
        cx.notify();
    }

    pub(crate) fn set_draft(&mut self, draft: ComposerDraft, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_text(draft.text.clone(), cx));
        self.draft = draft;
        cx.notify();
    }

    pub(crate) fn cancel_voice(&mut self, cx: &mut Context<Self>) {
        self.voice = None;
        self.voice_poll = None;
        self.error = None;
        cx.notify();
    }

    pub(crate) fn recording(&self) -> bool {
        self.voice.is_some()
    }

    fn submit(&self, enter: bool, cx: &mut Context<Self>) {
        if !self.sending && self.voice.is_none() {
            cx.emit(ComposerEvent::Submit(enter));
        }
    }

    pub(crate) fn toggle_voice(&mut self, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        if let Some(recorder) = &self.voice {
            if matches!(
                self.voice_snapshot.phase,
                muxy_ui::voice::Phase::Starting | muxy_ui::voice::Phase::Transcribing
            ) {
                self.voice = None;
                self.voice_poll = None;
            } else {
                recorder.finish();
            }
            cx.notify();
            return;
        }
        if let Some(reason) = self.dictation.reason(muxy_core::capability::Feature::Voice) {
            self.error = Some(reason.into_owned());
            cx.notify();
            return;
        }
        self.error = None;
        match muxy_ui::voice::Recorder::start(
            self.settings.language.clone(),
            self.settings.voice_model.clone(),
        ) {
            Ok(recorder) => self.record(recorder, cx),
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }

    fn record(&mut self, recorder: muxy_ui::voice::Recorder, cx: &mut Context<Self>) {
        self.voice = Some(recorder);
        self.voice_snapshot = muxy_ui::voice::Snapshot::default();
        self.voice_poll = Some(cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(80))
                    .await;
                let keep = view
                    .update(cx, |view, cx| {
                        let Some(recorder) = &view.voice else {
                            return false;
                        };
                        view.voice_snapshot = recorder.snapshot();
                        view.error.clone_from(&view.voice_snapshot.error);
                        if view.voice_snapshot.phase == muxy_ui::voice::Phase::Failed {
                            view.voice = None;
                        } else if let Some(text) = recorder.finished() {
                            view.voice = None;
                            if text.is_empty() {
                                view.error =
                                    Some(tr!("No speech detected. Try dictation again.").into());
                            } else {
                                view.input
                                    .update(cx, |input, cx| input.insert_at_selection(&text, cx));
                            }
                        }
                        cx.notify();
                        view.voice.is_some()
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn zoom(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.settings.font_size = (self.settings.font_size + delta).clamp(9.0, 32.0);
        self.refresh_style(cx);
        self.preferences(cx);
        cx.stop_propagation();
    }

    fn preferences(&mut self, cx: &mut Context<Self>) {
        cx.emit(ComposerEvent::Preferences(self.settings.clone()));
        cx.notify();
    }

    pub(crate) fn pause_voice(&mut self, cx: &mut Context<Self>) {
        if self.voice_snapshot.phase == muxy_ui::voice::Phase::Transcribing {
            return;
        }
        if let Some(voice) = &self.voice {
            voice.pause(self.voice_snapshot.phase != muxy_ui::voice::Phase::Paused);
        }
        cx.notify();
    }
}

/// Suggestions for the word before the caret, at most this many.
const COMPLETIONS: usize = 8;

impl Composer {
    /// The word being completed and the skills it matches, best first. `/`
    /// completes only at the start of the message, as agents read it there.
    pub(crate) fn completion(
        &self,
        cx: &gpui::App,
    ) -> Option<(
        std::ops::Range<usize>,
        Vec<&muxy_app_core::agent_skills::Skill>,
    )> {
        if self.skills.is_empty() || self.recording() {
            return None;
        }
        let input = self.input.read(cx);
        let cursor = input.cursor_offset();
        if !input.selected_range().is_empty() {
            return None;
        }
        let text = input.text().get(..cursor)?;
        let start = text
            .char_indices()
            .rev()
            .find(|(_, character)| character.is_whitespace())
            .map_or(0, |(index, character)| index + character.len_utf8());
        let word = &text[start..];
        let trigger = word.chars().next()?;
        if !(trigger == '$' || trigger == '/' && start == 0) || self.skill_dismissed == Some(start)
        {
            return None;
        }
        let query = word[1..].to_lowercase();
        let mut matches: Vec<_> = self
            .skills
            .iter()
            .filter(|skill| skill.text.starts_with(trigger))
            .filter_map(|skill| {
                let name = skill.text[1..].to_lowercase();
                let rank = if name.starts_with(&query) {
                    0
                } else if name.split(':').any(|part| part.starts_with(&query)) {
                    1
                } else if name.contains(&query) {
                    2
                } else {
                    return None;
                };
                Some((rank, skill))
            })
            .collect();
        matches.sort_by_key(|(rank, _)| *rank);
        let matches: Vec<_> = matches
            .into_iter()
            .take(COMPLETIONS)
            .map(|(_, skill)| skill)
            .collect();
        (!matches.is_empty()).then_some((start..cursor, matches))
    }

    /// Replaces the word with the chosen skill; false when nothing was offered.
    pub(crate) fn accept_completion(
        &mut self,
        index: Option<usize>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((range, matches)) = self.completion(cx) else {
            return false;
        };
        let Some(skill) = matches.get(index.unwrap_or(self.skill_selected).min(matches.len() - 1))
        else {
            return false;
        };
        let text = format!("{} ", skill.text);
        self.input
            .update(cx, |input, cx| input.replace_range(range, &text, cx));
        true
    }

    fn move_completion(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let Some((_, matches)) = self.completion(cx) else {
            return false;
        };
        let count = matches.len();
        self.skill_selected = (self.skill_selected.min(count - 1).cast_signed() + delta)
            .rem_euclid(count.cast_signed())
            .cast_unsigned();
        cx.notify();
        true
    }

    fn dismiss_completion(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((range, _)) = self.completion(cx) else {
            return false;
        };
        self.skill_dismissed = Some(range.start);
        cx.notify();
        true
    }
}

impl Focusable for Composer {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_composer(window, cx)
    }
}

#[cfg(test)]
mod tests;
