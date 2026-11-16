//! Launch composer; no terminal is opened until Send.
use std::fmt::Write;
use std::future::Future;
use std::pin::Pin;
use std::task::Poll;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::{
    AppContext, Context, Corner, Entity, Focusable, InteractiveElement, IntoElement, ParentElement,
    Render, Styled, Subscription, Task, WeakEntity, Window, anchored, deferred, div, point, px,
};
use muxy_app_core::{PaneId, ProjectId, ProjectStatus};
use muxy_protocol::{ChatImage, ChatMode, ChatModel, ChatProvider, ChatSelection, Feature};
use muxy_ui::{
    components::IconGlyph,
    controls::{self, Button, Style},
    icon::Icon,
    picker::{
        Picker, PickerConfig, PickerEvent, PickerItem, PickerLeading, PickerRow,
        PickerSelectionStyle, PickerStatus,
    },
    text_input::{InputEvent, InputStyle, TextInput},
    tr,
};

use crate::model::git::Repository;
use crate::model::{AppModel, OpenTerminal};

pub(crate) struct NewSession {
    owner: WeakEntity<AppModel>,
    input: Entity<TextInput>,
    images: Vec<ChatImage>,
    model: String,
    picker: Option<(Field, Entity<Picker>)>,
    picker_changes: Option<Subscription>,
    discovering: bool,
    discovered: bool,
    discovery_error: Option<String>,
    provider: ChatProvider,
    mode: ChatMode,
    effort: Option<String>,
    busy: bool,
    pasting: usize,
    revision: u64,
    _changes: Subscription,
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Model,
    Effort,
}

impl AppModel {
    fn card_composer_root(&self) -> ProjectId {
        let project = self.state.current_project();
        project.parent_id.unwrap_or(project.id)
    }

    pub(crate) fn card_composer(&self) -> Option<&Entity<NewSession>> {
        let project = self.state.current_project();
        (project.tabs.is_empty()
            && project.status() != ProjectStatus::Missing
            && self.server_supports(project.server_id, Feature::CHAT_PROVIDERS))
        .then(|| self.chat.card_composers.get(&self.card_composer_root()))
        .flatten()
    }

    pub(crate) fn prepare_card_composer(&mut self, cx: &mut Context<Self>) {
        self.chat
            .card_composers
            .retain(|id, _| self.state.project(*id).is_some());
        let project = self.state.current_project();
        if !cfg!(target_os = "macos")
            || !project.tabs.is_empty()
            || project.status() == ProjectStatus::Missing
            || !self.server_supports(project.server_id, Feature::CHAT_PROVIDERS)
        {
            return;
        }
        let root = self.card_composer_root();
        if self.chat.card_composers.contains_key(&root) {
            return;
        }
        let choice = self
            .settings
            .composer
            .chat_models
            .iter()
            .rev()
            .find(|choice| choice.server == project.server_id && choice.project == project.id);
        let provider = choice.map_or(ChatProvider::default(), |choice| choice.provider);
        let model = choice.map_or("", |choice| choice.model.as_str()).to_owned();
        let effort =
            choice.and_then(|choice| (!choice.effort.is_empty()).then(|| choice.effort.clone()));
        let style = InputStyle::field(&self.theme, &self.metrics);
        let owner = cx.weak_entity();
        let view = cx.new(|cx: &mut Context<NewSession>| {
            let pasted = cx.weak_entity();
            let input = cx.new(|cx| {
                TextInput::new(style, cx)
                    .multiline()
                    .with_placeholder(tr!("Or just ask — build, fix or explain…"))
                    .with_accessibility_id("new-session-prompt")
                    .with_paste_delegate(move |_, cx| {
                        let content = muxy_ui::pasteboard::read_content(cx);
                        if matches!(content, Ok(muxy_ui::pasteboard::Content::Text)) {
                            return false;
                        }
                        let _ = pasted.update(cx, |view, cx| view.paste(content, cx));
                        true
                    })
            });
            let changes = cx.subscribe(&input, |view: &mut NewSession, _, event, cx| {
                if matches!(event, InputEvent::Changed) {
                    view.revision = view.revision.wrapping_add(1);
                    cx.notify();
                }
            });
            NewSession {
                owner,
                input,
                model,
                images: Vec::new(),
                picker: None,
                picker_changes: None,
                discovering: false,
                discovered: false,
                discovery_error: None,
                provider,
                mode: ChatMode::Chat,
                effort,
                busy: false,
                pasting: 0,
                revision: 0,
                _changes: changes,
            }
        });
        self.chat.card_composers.insert(root, view);
    }

    fn card_models(&self, provider: ChatProvider) -> Vec<ChatModel> {
        let mut models = self
            .chat
            .card_catalogs
            .iter()
            .find(|(server, known, _)| {
                *server == self.state.current_project().server_id && *known == provider
            })
            .map_or_else(Vec::new, |(_, _, models)| models.clone());
        for session in self
            .chat
            .sessions
            .iter()
            .filter(|((server, _), session)| {
                *server == self.state.current_project().server_id && session.provider() == provider
            })
            .map(|(_, session)| session)
        {
            for model in &session.capabilities.models {
                if !models.iter().any(|known| known.alias == model.alias) {
                    models.push(model.clone());
                }
            }
        }
        models
    }

    fn card_send_ready(
        &mut self,
        pane: PaneId,
        selection: &ChatSelection,
        text: &str,
        images: &[ChatImage],
        cx: &mut Context<Self>,
    ) -> Result<Option<Task<Result<(), String>>>, String> {
        if self.state.pane_project(pane).is_none() {
            return Err(tr!("The new session was closed. Your draft is kept.").to_string());
        }
        let Some(session) = self.chat_session(pane) else {
            return Ok(None);
        };
        let Some(local) = &session.local else {
            return if self.chat_waiting(pane) {
                Ok(None)
            } else {
                Err(tr!("Chat startup was cancelled. Your draft is kept.").to_string())
            };
        };
        if local.discovering {
            return Ok(None);
        }
        if let Some(error) = &session.picker_error {
            return Err(error.clone());
        }
        if local.host_epoch.is_none() {
            return Err(tr!("Choose an available provider and model before sending.").to_string());
        }
        if local.selection.provider != selection.provider {
            return Err(
                tr!("The provider changed during startup. Send it from Chat when ready.")
                    .to_string(),
            );
        }
        if selection
            .model
            .as_ref()
            .is_some_and(|model| local.selection.model.as_ref() != Some(model))
            || selection
                .effort
                .as_ref()
                .is_some_and(|effort| local.selection.effort.as_ref() != Some(effort))
        {
            return Err(
                tr!("The selection changed during startup. Send it from Chat when ready.")
                    .to_string(),
            );
        }
        validate_selection(&local.selection, &session.capabilities.models)?;
        self.save_chat_choice(
            pane,
            local.selection.model.clone(),
            local.selection.effort.clone(),
            cx,
        );
        Ok(Some(self.chat_send(
            pane,
            text.to_owned(),
            images.to_vec(),
            cx,
        )))
    }
}

fn validate_selection(selection: &ChatSelection, models: &[ChatModel]) -> Result<(), String> {
    let selected = selection
        .model
        .as_ref()
        .and_then(|alias| models.iter().find(|m| &m.alias == alias));
    if selection.model.is_some() && selected.is_none() {
        return Err(tr!(
            "The selected model is unavailable. Choose another model; your draft is kept."
        )
        .to_string());
    }
    if let Some(effort) = &selection.effort
        && !selected.is_some_and(|model| model.efforts.contains(effort))
    {
        return Err(tr!(
            "The selected effort is unavailable. Choose another effort level; your draft is kept."
        )
        .to_string());
    }
    Ok(())
}

/// Decode control characters inside sh, so the terminal line editor never receives them.
fn cli_command(selection: &ChatSelection, text: &str) -> Result<String, String> {
    use crate::extensions::authoring::shell_quote;
    if text.contains('\0') {
        return Err(
            tr!("CLI prompts cannot contain a NUL character. Your draft is kept.").to_string(),
        );
    }
    if selection
        .model
        .iter()
        .chain(&selection.effort)
        .any(|value| {
            value.len() > muxy_protocol::MAX_CHAT_NAME || value.chars().any(char::is_control)
        })
    {
        return Err(
            tr!("The model or effort contains invalid characters. Your draft is kept.").to_string(),
        );
    }
    let program = match selection.provider {
        ChatProvider::Claude => "claude",
        ChatProvider::Codex => "codex",
        ChatProvider::Unrecognized(_) => {
            return Err(tr!("Choose an available provider and model before sending.").to_string());
        }
    };
    let mut words = vec![program.to_owned()];
    if let Some(model) = &selection.model {
        words.extend(["--model".into(), shell_quote(model)]);
    }
    if let Some(effort) = &selection.effort {
        if selection.provider == ChatProvider::Claude {
            words.extend(["--effort".into(), shell_quote(effort)]);
        } else {
            let value = serde_json::to_string(effort).map_err(|error| error.to_string())?;
            words.extend([
                "--config".into(),
                shell_quote(&format!("model_reasoning_effort={value}")),
            ]);
        }
    }
    let mut encoded = String::new();
    for character in text.chars() {
        if character == '\\' || character.is_control() {
            for byte in character.encode_utf8(&mut [0; 4]).bytes() {
                let _ = write!(encoded, "\\0{byte:03o}");
            }
        } else {
            encoded.push(character);
        }
    }
    // The suffix preserves trailing newlines that command substitution would remove.
    let script = format!(
        "prompt=$(printf '%b_' {}); exec \"$@\" \"${{prompt%_}}\"",
        shell_quote(&encoded)
    );
    let command = format!(
        "/bin/sh -c {} muxy-launch {} --",
        shell_quote(&script),
        words.join(" ")
    );
    // shortcut: stay below the OS argument limit; use prompt files if both CLIs add them.
    if command.len() >= 128 * 1024 {
        return Err(tr!("This prompt is too large to pass to the CLI. Shorten it or use Chat; your draft is kept.").to_string());
    }
    Ok(command)
}

impl NewSession {
    fn paste(
        &mut self,
        content: Result<muxy_ui::pasteboard::Content, String>,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.pasting += 1;
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match content? {
                        muxy_ui::pasteboard::Content::Image(bytes) => {
                            crate::views::chat::copied_image(bytes)
                                .map(|image| vec![image])
                                .ok_or_else(|| tr!("Copied image can't be attached").to_string())
                        }
                        muxy_ui::pasteboard::Content::Files(paths) => {
                            if paths.len() > muxy_protocol::MAX_CHAT_IMAGES {
                                return Err(tr!(
                                    "Chat takes up to %lld images, 5 MiB each and 12 MiB in all.",
                                    muxy_protocol::MAX_CHAT_IMAGES
                                )
                                .to_string());
                            }
                            paths
                                .into_iter()
                                .map(|path| {
                                    let size =
                                        std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
                                    if size > 25 * 1024 * 1024 {
                                        return Err(
                                            tr!("Copied image is larger than 25 MiB").to_string()
                                        );
                                    }
                                    crate::views::chat::copied_image(
                                        std::fs::read(path).map_err(|e| e.to_string())?,
                                    )
                                    .ok_or_else(|| {
                                        tr!("Copied image can't be attached").to_string()
                                    })
                                })
                                .collect()
                        }
                        muxy_ui::pasteboard::Content::Text => Ok(Vec::new()),
                    }
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.pasting -= 1;
                cx.notify();
                let result = result.and_then(|images| {
                    let mut combined = view.images.clone();
                    combined.extend(images);
                    if !crate::views::chat::images_fit(&combined) {
                        return Err(tr!(
                            "Chat takes up to %lld images, 5 MiB each and 12 MiB in all.",
                            muxy_protocol::MAX_CHAT_IMAGES
                        )
                        .to_string());
                    }
                    view.images = combined;
                    view.revision = view.revision.wrapping_add(1);
                    cx.notify();
                    Ok(())
                });
                if let Err(error) = result {
                    let _ = view
                        .owner
                        .update(cx, |model, cx| model.show_notice(error, cx));
                }
            });
        })
        .detach();
    }

    #[allow(
        clippy::too_many_lines,
        reason = "One launch and its draft-preserving delivery flow"
    )]
    fn submit(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().to_owned();
        if self.busy || self.pasting > 0 || (text.trim().is_empty() && self.images.is_empty()) {
            return;
        }
        let model = self.model.trim();
        let selection = ChatSelection {
            provider: self.provider,
            model: (!model.is_empty()).then(|| model.to_owned()),
            effort: self.effort.clone(),
            target: None,
            launch_context: None,
        };
        let chat = self.mode == ChatMode::Chat;
        if !chat && !self.images.is_empty() {
            let _ = self.owner.update(cx, |model, cx| {
                model.show_notice(
                    tr!("Use Chat to send images. Your draft is kept.").to_string(),
                    cx,
                );
            });
            return;
        }
        let images = self.images.clone();
        let command = if chat {
            None
        } else {
            match cli_command(&selection, &text) {
                Ok(command) => Some(command),
                Err(error) => {
                    let _ = self
                        .owner
                        .update(cx, |model, cx| model.show_notice(error, cx));
                    return;
                }
            }
        };
        let owner = self.owner.clone();
        let opened = owner.update(cx, |model, cx| {
            let project = model.state.current_project();
            if !project.tabs.is_empty()
                || model.git.select_after_catalog.is_some()
                || model
                    .git
                    .projects
                    .get(&project.id)
                    .is_some_and(Repository::busy)
            {
                return Ok(None);
            }
            let directory = project.directory.clone();
            let Some((_, pane)) = model.open_terminal(
                OpenTerminal {
                    project: project.id,
                    title: command.as_ref().map(|_| {
                        if selection.provider == ChatProvider::Claude {
                            "Claude Code"
                        } else {
                            "Codex"
                        }
                    }),
                    command: command.as_deref(),
                    directory: Some(&directory),
                },
                cx,
            )?
            else {
                return Ok(None);
            };
            if chat {
                model.chat.card_selections.insert(pane, selection.clone());
                model.chat_start_when_idle(pane, cx);
            }
            Ok::<_, String>(Some(pane))
        });
        let pane = match opened {
            Ok(Ok(Some(pane))) => pane,
            Ok(Err(error)) => {
                let _ = owner.update(cx, |model, cx| model.show_notice(error, cx));
                return;
            }
            _ => return,
        };
        self.busy = true;
        let revision = self.revision;
        cx.notify();
        cx.spawn(async move |view, cx| {
            let mut seeded: Option<(Entity<crate::views::chat::ChatView>, Option<Instant>)> = None;
            let result = async {
                let deadline = Instant::now() + Duration::from_secs(45);
                loop {
                    let send = owner
                        .update(cx, |model, cx| {
                            if !chat {
                                if model.state.pane_project(pane).is_none() {
                                    return Err(tr!("The new session was closed. Your draft is kept.").to_string());
                                }
                                return Ok((model.state.startup_command(pane).is_none()
                                    && model.state.pane(pane).is_some_and(|pane| pane.session().is_some()))
                                    .then(|| Task::ready(Ok(()))));
                            }
                            if let Some((chat, revision)) = &seeded
                                && !chat.read(cx).seeded_prompt_unchanged(*revision) {
                                    return Err(tr!("The prompt changed during startup. Send it from Chat when ready.").to_string());
                            }
                            if seeded.is_none()
                                && let Some(chat) = model.chat.views.get(&pane)
                            {
                                seeded = Some((
                                    chat.clone(),
                                    chat.update(cx, |chat, cx| chat.seed_prompt(&text, images.clone(), cx)),
                                ));
                            }
                            model.card_send_ready(pane, &selection, &text, &images, cx)
                        })
                        .map_err(|error| error.to_string())??;
                    if let Some(mut send) = send {
                        // Outlasts chat_send's own ~120s delivery polling; a server that never
                        // replies must not leave the card disabled.
                        let mut timer = cx.background_executor().timer(Duration::from_secs(150));
                        let finished = std::future::poll_fn(|context| {
                            if let Poll::Ready(result) = Pin::new(&mut send).poll(context) {
                                return Poll::Ready(Some(result));
                            }
                            Pin::new(&mut timer).poll(context).map(|()| None)
                        })
                        .await;
                        return finished.unwrap_or_else(|| {
                            // Dropping would cancel the send mid-flight and skip its cleanup.
                            send.detach();
                            Err(tr!("Sending is taking too long. Check Chat before resending; your draft is kept.").to_string())
                        });
                    }
                    if Instant::now() >= deadline {
                        return Err(tr!("Session startup timed out. Your draft is kept.").to_string());
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                }
            }
            .await;
            let _ = view.update(cx, |view, cx| {
                view.busy = false;
                match result {
                    Ok(()) => {
                        if view.revision == revision && view.input.read(cx).text() == text {
                            view.input.update(cx, |input, cx| input.set_text("", cx));
                            view.images.clear();
                        }
                        if let Some((chat, revision)) = seeded {
                            chat.update(cx, |chat, cx| {
                                chat.clear_seeded_prompt(&text, revision, cx);
                            });
                        }
                    }
                    Err(error) => {
                        let _ = owner.update(cx, |model, cx| model.show_notice(error, cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Rows for the model and effort dropdown share one filter"
    )]
    fn picker_items(&self, field: Field, query: &str, cx: &gpui::App) -> Vec<PickerItem> {
        let Some(owner) = self.owner.upgrade() else {
            return Vec::new();
        };
        let app = owner.read(cx);
        let entered = query.trim();
        let query = entered.to_lowercase();
        let mut items = Vec::new();
        match field {
            Field::Model => {
                for (provider, key, name) in [
                    (ChatProvider::Claude, "claude", "Anthropic"),
                    (ChatProvider::Codex, "codex", "OpenAI"),
                ] {
                    let mut models = app.card_models(provider);
                    for choice in app.settings.composer.chat_models.iter().filter(|choice| {
                        choice.provider == provider
                            && choice.server == app.state.current_project().server_id
                    }) {
                        if !choice.model.is_empty()
                            && !models.iter().any(|model| model.alias == choice.model)
                        {
                            models.push(ChatModel {
                                alias: choice.model.clone(),
                                label: choice.model.clone(),
                                efforts: Vec::new(),
                                default_effort: None,
                                images: None,
                            });
                        }
                    }
                    let mut rows = vec![(String::new(), tr!("Default model").to_string())];
                    rows.extend(models.into_iter().map(|model| (model.alias, model.label)));
                    let mut matches = Vec::new();
                    for (alias, label) in rows {
                        if !format!("{name} {alias} {label}")
                            .to_lowercase()
                            .contains(&query)
                        {
                            continue;
                        }
                        let mut row = PickerRow::new(format!("{key}:{alias}"), label);
                        row.selection_style = PickerSelectionStyle::Highlight;
                        row.leading = Some(PickerLeading::Image(
                            if key == "codex" {
                                "icons/provider-codex.png"
                            } else {
                                "icons/provider-claude.svg"
                            }
                            .into(),
                        ));
                        row.trailing = Some(name.into());
                        row.current = provider == self.provider && alias == self.model;
                        row.disabled = self.discovery_error.as_ref().is_some_and(|errors| {
                            errors
                                .lines()
                                .any(|error| error.starts_with(&format!("{name}:")))
                        });
                        matches.push(PickerItem::Row(row));
                    }
                    if !matches.is_empty() {
                        items.push(PickerItem::section(name));
                        items.extend(matches);
                    }
                }
                if items.is_empty() && !entered.is_empty() {
                    for (key, name) in [("claude", "Anthropic"), ("codex", "OpenAI")] {
                        let mut row =
                            PickerRow::new(format!("{key}:{entered}"), entered.to_owned());
                        row.badge = Some(tr!("Custom"));
                        row.disabled = self.discovery_error.as_ref().is_some_and(|errors| {
                            errors
                                .lines()
                                .any(|error| error.starts_with(&format!("{name}:")))
                        });
                        row.selection_style = PickerSelectionStyle::Highlight;
                        row.leading = Some(PickerLeading::Image(
                            if key == "codex" {
                                "icons/provider-codex.png"
                            } else {
                                "icons/provider-claude.svg"
                            }
                            .into(),
                        ));
                        row.trailing = Some(name.into());
                        items.push(PickerItem::Row(row));
                    }
                }
            }
            Field::Effort => {
                let models = app.card_models(self.provider);
                let mut efforts = vec![String::new()];
                for model in models
                    .iter()
                    .filter(|model| self.model.is_empty() || model.alias == self.model)
                {
                    efforts.extend(model.efforts.clone());
                }
                // shortcut: without a catalog offer common efforts; use project discovery when the server supports it.
                if efforts.len() == 1 {
                    efforts.extend(["low", "medium", "high"].map(str::to_owned));
                }
                let mut seen = std::collections::HashSet::new();
                for effort in efforts {
                    if !seen.insert(effort.clone()) {
                        continue;
                    }
                    let label = if effort.is_empty() {
                        tr!("Default").to_string()
                    } else {
                        effort.clone()
                    };
                    if !label.to_lowercase().contains(&query) {
                        continue;
                    }
                    let mut row = PickerRow::new(effort.clone(), label);
                    row.current = self.effort.as_deref().unwrap_or_default() == effort;
                    items.push(PickerItem::Row(row));
                }
            }
        }
        if field == Field::Model
            && let Some(error) = &self.discovery_error
        {
            let mut row = PickerRow::new("discovery-error", tr!("Some models could not be loaded"));
            row.detail = Some(error.clone().into());
            row.disabled = true;
            items.push(PickerItem::Row(row));
        }
        items
    }

    fn refresh_picker(&self, cx: &mut Context<Self>) {
        let Some((field, picker)) = &self.picker else {
            return;
        };
        let query = picker.read(cx).query().to_owned();
        let items = self.picker_items(*field, &query, cx);
        let status = if self.discovering && query.is_empty() {
            PickerStatus::Loading(tr!("Loading models…"))
        } else if query.is_empty()
            && !self.discovered
            && let Some(error) = &self.discovery_error
        {
            PickerStatus::Error(error.clone().into())
        } else {
            PickerStatus::Ready
        };
        picker.update(cx, |picker, cx| {
            picker.set_items(items, cx);
            picker.set_status(status, cx);
        });
    }

    fn discover_models(&mut self, cx: &mut Context<Self>) {
        if self.discovering || self.discovered {
            return;
        }
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let (server, task) = owner.update(cx, |model, cx| {
            (
                model.state.current_project().server_id,
                model.discover_card_models(cx),
            )
        });
        self.discovering = true;
        self.discovery_error = None;
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |view, cx| {
                view.discovering = false;
                let mut errors = Vec::new();
                let mut found = false;
                match result {
                    Ok(catalogs) => {
                        owner.update(cx, |model, _| {
                            for (provider, result) in catalogs {
                                match result {
                                    Ok(models) => {
                                        found |= !models.is_empty();
                                        model.chat.card_catalogs.retain(
                                            |(known_server, known_provider, _)| {
                                                *known_server != server
                                                    || *known_provider != provider
                                            },
                                        );
                                        model.chat.card_catalogs.push((server, provider, models));
                                    }
                                    Err(error) => errors.push(format!(
                                        "{}: {error}",
                                        if provider == ChatProvider::Codex {
                                            "OpenAI"
                                        } else {
                                            "Anthropic"
                                        }
                                    )),
                                }
                            }
                        });
                    }
                    Err(error) => errors.push(error),
                }
                view.discovered = found;
                view.discovery_error = (!errors.is_empty()).then(|| errors.join("\n"));
                view.refresh_picker(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.picker_changes = None;
        self.input.read(cx).focus_handle(cx).focus(window);
        cx.notify();
    }

    fn menu(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .picker
            .as_ref()
            .is_some_and(|(current, _)| *current == field)
        {
            self.close_picker(window, cx);
            return;
        }
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let app = owner.read(cx);
        let mut config = PickerConfig::popover(
            "new-session-options",
            if field == Field::Model {
                tr!("Search models")
            } else {
                tr!("Effort")
            },
        );
        config.width = Some(380.0);
        let theme = app.theme.clone();
        let metrics = app.metrics;
        let items = self.picker_items(field, "", cx);
        let picker = cx.new(|cx| {
            let mut picker = Picker::new(config, theme, metrics, cx);
            picker.set_items(items, cx);
            picker
        });
        self.picker_changes = Some(cx.subscribe_in(
            &picker,
            window,
            move |view, _, event, window, cx| match event {
                PickerEvent::QueryChanged { .. } => view.refresh_picker(cx),
                PickerEvent::Confirmed(selection) => {
                    match field {
                        Field::Model => {
                            if let Some((provider, alias)) = selection.id.split_once(':') {
                                view.provider = if provider == "codex" {
                                    ChatProvider::Codex
                                } else {
                                    ChatProvider::Claude
                                };
                                alias.clone_into(&mut view.model);
                                view.effort = None;
                            }
                        }
                        Field::Effort => {
                            view.effort =
                                (!selection.id.is_empty()).then(|| selection.id.to_string());
                        }
                    }
                    view.close_picker(window, cx);
                }
                PickerEvent::Dismissed => view.close_picker(window, cx),
                _ => {}
            },
        ));
        picker.focus_handle(cx).focus(window);
        self.picker = Some((field, picker));
        self.discover_models(cx);
        self.refresh_picker(cx);
        cx.notify();
    }
}

impl Render for NewSession {
    #[allow(
        clippy::too_many_lines,
        reason = "One composer and its branch/worktree controls"
    )]
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(owner) = self.owner.upgrade() else {
            return div().into_any_element();
        };
        let style = {
            let model = owner.read(cx);
            InputStyle::field(&model.theme, &model.metrics)
        };
        self.input
            .update(cx, |input, cx| input.set_style(style, cx));
        let model = owner.read(cx);
        let m = model.metrics;
        let theme = &model.theme;
        let project = model.state.current_project();
        let id = project.id;
        let root = project.parent_id.unwrap_or(id);
        let repository = model.git.projects.get(&id);
        let branch = repository
            .and_then(|git| git.summary.as_ref())
            .and_then(|summary| summary.branch.clone());
        let ready = !self.busy
            && self.pasting == 0
            && (!self.input.read(cx).text().trim().is_empty() || !self.images.is_empty())
            && model.git.select_after_catalog.is_none()
            && !repository.is_some_and(Repository::busy);
        let selected_model = model
            .card_models(self.provider)
            .into_iter()
            .find(|model| model.alias == self.model)
            .map_or_else(
                || {
                    if self.model.is_empty() {
                        tr!("Default model").to_string()
                    } else {
                        self.model.clone()
                    }
                },
                |model| model.label,
            );
        let provider_label = if self.provider == ChatProvider::Codex {
            "OpenAI"
        } else {
            "Anthropic"
        };
        let model_label = format!("{provider_label} / {selected_model}");
        let button = |id: &str, label: &str, field| {
            let view = cx.entity().downgrade();
            let picker = self
                .picker
                .as_ref()
                .filter(|(current, _)| *current == field)
                .map(|(_, picker)| picker.clone());
            let control = controls::button(
                Style { theme, metrics: &m },
                id,
                Button::secondary(if field == Field::Model { "" } else { label })
                    .named(label)
                    .enabled(!self.busy),
                move |_, window, cx| {
                    let _ = view.update(cx, |view, cx| view.menu(field, window, cx));
                },
            )
            .debug_selector({
                let id = id.to_owned();
                move || id.clone()
            })
            .line_height(m.line_height_compact())
            .bg(if field == Field::Model {
                theme.hover
            } else {
                gpui::transparent_black()
            })
            .border_color(gpui::transparent_black())
            .rounded(m.radius_md())
            .px(m.spacing4())
            .when(field == Field::Model, |control| {
                control
                    .child(super::super::tab_activity::provider_icon(
                        if self.provider == ChatProvider::Codex {
                            muxy_protocol::AgentProvider::Codex
                        } else {
                            muxy_protocol::AgentProvider::Claude
                        },
                        m.icon_sm(),
                        model,
                    ))
                    .child(div().text_color(theme.fg_muted).child(provider_label))
                    .child(div().text_color(theme.fg_muted).child("/"))
                    .child(selected_model.clone())
            })
            .child(IconGlyph::new(
                Icon::ChevronDown,
                m.icon_xs(),
                theme.fg_muted,
            ));
            div()
                .relative()
                .child(control)
                .when_some(picker, |body, picker| {
                    body.child(
                        deferred(
                            anchored()
                                .anchor(Corner::TopLeft)
                                .offset(point(px(0.0), m.control_medium() + m.spacing2()))
                                .snap_to_window_with_margin(px(8.0))
                                .child(
                                    div()
                                        .id("new-session-options-popover")
                                        .on_mouse_down_out(cx.listener(|view, _, window, cx| {
                                            view.close_picker(window, cx);
                                        }))
                                        .child(picker),
                                ),
                        )
                        .with_priority(2),
                    )
                })
        };
        let view = cx.entity().downgrade();
        let mode_view = cx.entity().downgrade();
        let mode = crate::views::chat::mode_control(
            "new-session".into(),
            crate::model::chat::ChatToggle {
                chat: self.mode == ChatMode::Chat,
                enabled: !self.busy,
                start: false,
            },
            model,
            None,
            move |chat, window, cx| {
                let _ = mode_view.update(cx, |view, cx| {
                    if !view.busy {
                        view.mode = if chat {
                            ChatMode::Chat
                        } else {
                            ChatMode::Console
                        };
                        view.input.read(cx).focus_handle(cx).focus(window);
                        cx.notify();
                    }
                });
            },
        );
        let branch_owner = self.owner.clone();
        let worktree_owner = self.owner.clone();
        let anchor = model.git.branch_anchor.clone();
        div()
            .debug_selector(|| "new-session-composer".into())
            .w_full()
            .max_w(m.scaled(super::NEW_SESSION_WIDTH))
            .min_w_0()
            .flex()
            .flex_col()
            .gap(m.spacing3())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(m.spacing4())
                    .p(m.spacing6())
                    .rounded(m.scaled(16.0))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .key_context("Composer")
                    .on_action(cx.listener(
                        |view, _: &crate::views::composer::Newline, window, cx| {
                            if view.input.read(cx).focus_handle(cx).is_focused(window) {
                                view.input
                                    .update(cx, |input, cx| input.insert_at_selection("\n", cx));
                            } else {
                                cx.propagate();
                            }
                        },
                    ))
                    .on_action(cx.listener(
                        |view, _: &crate::views::composer::Submit, window, cx| {
                            if view.input.read(cx).focus_handle(cx).is_focused(window) {
                                view.submit(cx);
                            } else {
                                cx.propagate();
                            }
                        },
                    ))
                    .capture_action(cx.listener(
                        |view, _: &muxy_ui::text_input::InsertNewline, window, cx| {
                            if view.input.read(cx).focus_handle(cx).is_focused(window) {
                                cx.stop_propagation();
                                view.submit(cx);
                            }
                        },
                    ))
                    .child(
                        div()
                            .debug_selector(|| "new-session-textbox".into())
                            .flex()
                            .w_full()
                            .h(m.scaled(72.0))
                            .child(self.input.clone()),
                    )
                    .when(!self.images.is_empty(), |body| {
                        body.child(div().flex().flex_wrap().gap(m.spacing2()).children(
                            self.images.iter().enumerate().map(|(index, _)| {
                                let view = cx.entity().downgrade();
                                let owner = self.owner.clone();
                                let preview_view = view.clone();
                                let name = format!("Image {}", index + 1);
                                crate::views::chat::image_chip(
                                    &format!("new-session-image-{index}"),
                                    &name,
                                    !self.busy,
                                    Style { theme, metrics: &m },
                                    move |_, _, cx| {
                                        let image = preview_view.upgrade().and_then(|view| {
                                            view.read(cx).images.get(index).cloned()
                                        });
                                        if let Some(image) = image {
                                            let _ = owner.update(cx, |model, cx| {
                                                model.preview_chat_image(image, cx);
                                            });
                                        }
                                    },
                                    move |_, window, cx| {
                                        cx.stop_propagation();
                                        let _ = view.update(cx, |view, cx| {
                                            if !view.busy && index < view.images.len() {
                                                view.images.remove(index);
                                                view.revision = view.revision.wrapping_add(1);
                                                view.input.read(cx).focus_handle(cx).focus(window);
                                                cx.notify();
                                            }
                                        });
                                    },
                                )
                            }),
                        ))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(m.spacing3())
                            .flex_wrap()
                            .child(mode)
                            .child(button("new-session-models", &model_label, Field::Model))
                            .child(button(
                                "new-session-effort",
                                self.effort.as_deref().unwrap_or(&tr!("Default")),
                                Field::Effort,
                            ))
                            .child(div().flex_1())
                            .child(
                                controls::button(
                                    Style { theme, metrics: &m },
                                    "new-session-send",
                                    Button::secondary("")
                                        .icon("arrow.up")
                                        .named(&if self.mode == ChatMode::Chat {
                                            tr!("Start Chat")
                                        } else {
                                            tr!("Start CLI")
                                        })
                                        .enabled(ready),
                                    move |_, _, cx| {
                                        let _ = view.update(cx, NewSession::submit);
                                    },
                                )
                                .debug_selector(|| "new-session-send".into())
                                .line_height(m.line_height_compact())
                                .w(m.control_medium())
                                .px(m.spacing2())
                                .rounded(m.radius_lg()),
                            ),
                    ),
            )
            .when(
                repository.is_some_and(|git| git.summary.is_some()),
                |body| {
                    body.child(
                        div()
                            .flex()
                            .items_center()
                            .flex_wrap()
                            .px(m.spacing3())
                            .gap(m.spacing3())
                            .child(super::super::sidebar::project_tile(
                                project,
                                model,
                                m.scaled(16.0),
                                None,
                            ))
                            .child(
                                div()
                                    .text_size(m.font_footnote())
                                    .child(project.name.clone()),
                            )
                            .child(div().text_color(theme.fg_muted).child("/"))
                            .child(IconGlyph::new(Icon::AppWindow, m.icon_sm(), theme.fg_muted))
                            .child(
                                div().text_size(m.font_footnote()).child(
                                    model
                                        .server_name(project.server_id)
                                        .unwrap_or("Local")
                                        .to_owned(),
                                ),
                            )
                            .child(div().size(m.scaled(4.0)).rounded_full().bg(theme.accent))
                            .child(
                                controls::button(
                                    Style { theme, metrics: &m },
                                    "new-session-branch",
                                    Button::secondary(branch.as_deref().unwrap_or("HEAD"))
                                        .icon("arrow.triangle.branch")
                                        .enabled(!self.busy),
                                    move |_, window, cx| {
                                        let _ = branch_owner.update(cx, |model, cx| {
                                            model.open_git_picker(
                                                id,
                                                crate::views::git::Kind::Branches,
                                                window,
                                                cx,
                                            );
                                        });
                                    },
                                )
                                .line_height(m.line_height_compact())
                                .bg(gpui::transparent_black())
                                .border_color(gpui::transparent_black())
                                .px(m.spacing2())
                                .relative()
                                .child(
                                    gpui::canvas(
                                        move |bounds, _, _| anchor.set(Some(bounds)),
                                        |_, (), _, _| {},
                                    )
                                    .absolute()
                                    .size_full(),
                                ),
                            )
                            .child(
                                controls::button(
                                    Style { theme, metrics: &m },
                                    "new-session-worktree",
                                    Button::secondary(&tr!("New Worktree…"))
                                        .icon("plus")
                                        .enabled(!self.busy),
                                    move |_, window, cx| {
                                        let _ = worktree_owner.update(cx, |model, cx| {
                                            model.open_git_form(root, true, cx);
                                            model.focus_git_form(window, cx);
                                        });
                                    },
                                )
                                .line_height(m.line_height_compact())
                                .bg(gpui::transparent_black())
                                .border_color(gpui::transparent_black())
                                .px(m.spacing2()),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .line_height(m.line_height_compact())
                                    .text_size(m.font_footnote())
                                    .text_color(theme.fg_muted)
                                    .max_w(m.scaled(220.0))
                                    .truncate()
                                    .child(project.directory.display().to_string()),
                            ),
                    )
                },
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_receives_literal_prompt_model_effort_and_worktree_directory() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;
        let root = tempfile::tempdir().expect("fixture");
        let directory = root.path().join("worktree 'quoted' $HOME");
        std::fs::create_dir(&directory).expect("worktree");
        let directory = directory.canonicalize().expect("absolute worktree");
        for program in ["claude", "codex"] {
            let executable = root.path().join(program);
            std::fs::write(&executable, "#!/bin/sh\nprintf '%s\\000' \"$PWD\" \"$@\"\n")
                .expect("fake CLI");
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
                .expect("executable");
        }
        let text = "--not-a-flag 'quotes' \"double\" $HOME $(touch injected) `touch injected` \\ literal\\n 🐈\n\t\r\u{1b}\n\n";
        for provider in [ChatProvider::Claude, ChatProvider::Codex] {
            let selection = ChatSelection {
                provider,
                model: Some("model 'quoted' $(touch injected)".into()),
                effort: Some("xhigh".into()),
                target: None,
                launch_context: None,
            };
            let command = cli_command(&selection, text).expect("launch command");
            assert!(!command.chars().any(char::is_control));
            for shell in ["/bin/sh", "/bin/bash", "/bin/zsh", "/opt/homebrew/bin/fish"] {
                if !std::path::Path::new(shell).is_file() {
                    continue;
                }
                let output = Command::new(shell)
                    .arg("-c")
                    .arg(&command)
                    .env("PATH", root.path())
                    .current_dir(&directory)
                    .output()
                    .expect("launch fake CLI");
                assert!(
                    output.status.success(),
                    "{shell}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let received = output
                    .stdout
                    .strip_suffix(&[0])
                    .expect("arguments")
                    .split(|byte| *byte == 0)
                    .map(|word| std::str::from_utf8(word).expect("UTF-8"))
                    .collect::<Vec<_>>();
                let mut expected = vec![
                    directory.to_str().expect("directory"),
                    "--model",
                    selection.model.as_deref().expect("model"),
                ];
                if provider == ChatProvider::Claude {
                    expected.extend(["--effort", "xhigh"]);
                } else {
                    expected.extend(["--config", "model_reasoning_effort=\"xhigh\""]);
                }
                expected.extend(["--", text]);
                assert_eq!(received, expected, "{shell}, {provider:?}");
                assert!(!directory.join("injected").exists());
            }
            assert!(cli_command(&selection, "\0").is_err());
            assert!(cli_command(&selection, &"x".repeat(128 * 1024)).is_err());
            let mut invalid = selection;
            invalid.model = Some("model\ncommand".into());
            assert!(cli_command(&invalid, text).is_err());
        }
    }

    #[gpui::test]
    fn pasted_image_survives_rejected_launch_and_invalid_paste(cx: &mut gpui::TestAppContext) {
        use std::io::Cursor;
        let (boot, _requests) =
            crate::model::tests::stub_boot(muxy_app_core::AppState::bootstrap().expect("state"));
        let (owner, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
        let composer = owner.update(cx, draft);
        let mut tiff = Vec::new();
        image::RgbaImage::new(2, 2)
            .write_to(&mut Cursor::new(&mut tiff), image::ImageFormat::Tiff)
            .unwrap();
        composer.update(cx, |view, cx| {
            view.input
                .update(cx, |input, cx| input.set_text("describe this", cx));
            view.paste(Ok(muxy_ui::pasteboard::Content::Image(tiff)), cx);
            view.submit(cx);
            assert_eq!(view.pasting, 1);
            assert!(!view.busy, "wait for the pending image before launching");
        });
        cx.run_until_parked();
        composer.update(cx, |view, cx| {
            assert_eq!(view.images.len(), 1);
            assert_eq!(view.images[0].mime, muxy_protocol::ChatImageType::Png);
            assert!(muxy_protocol::ChatImageType::Png.matches(&view.images[0].bytes));
            view.mode = ChatMode::Console;
            view.submit(cx);
            assert_eq!(view.images.len(), 1);
            assert!(!view.busy);
            view.paste(Ok(muxy_ui::pasteboard::Content::Image(vec![0, 1, 2])), cx);
        });
        cx.run_until_parked();
        composer.read_with(cx, |view, _| assert_eq!(view.images.len(), 1));
        owner.read_with(cx, |model, _| {
            assert!(model.state.current_project().tabs.is_empty());
        });
    }

    #[gpui::test]
    fn rejected_cli_launch_keeps_the_draft_and_opens_nothing(cx: &mut gpui::TestAppContext) {
        let (boot, _requests) =
            crate::model::tests::stub_boot(muxy_app_core::AppState::bootstrap().expect("state"));
        let (owner, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
        let composer = owner.update(cx, draft);
        composer.update(cx, |view, cx| {
            assert_eq!(view.mode, ChatMode::Chat, "new drafts default to Chat");
            view.mode = ChatMode::Console;
            let text = "keep this\0draft";
            view.input.update(cx, |input, cx| input.set_text(text, cx));
            view.submit(cx);
            assert_eq!(view.input.read(cx).text(), text);
            assert!(!view.busy);
        });
        owner.read_with(cx, |model, _| {
            assert!(model.state.current_project().tabs.is_empty());
        });
    }

    #[gpui::test]
    fn discovered_model_labels_resolve_to_aliases_and_model_specific_efforts(
        cx: &mut gpui::TestAppContext,
    ) {
        let (boot, _requests) =
            crate::model::tests::stub_boot(muxy_app_core::AppState::bootstrap().expect("state"));
        let (owner, cx) = cx.add_window_view(|window, cx| AppModel::new(boot, window, cx));
        let composer = owner.update(cx, |model, cx| {
            model.chat.card_catalogs.push((
                model.state.current_project().server_id,
                ChatProvider::Codex,
                vec![ChatModel {
                    alias: "api-alias".into(),
                    label: "Friendly Model".into(),
                    efforts: vec!["maximum".into()],
                    default_effort: None,
                    images: None,
                }],
            ));
            draft(model, cx)
        });
        composer.update(cx, |view, cx| {
            let items = view.picker_items(Field::Model, "friendly", cx);
            let rows: Vec<_> = items.into_iter().filter_map(PickerItem::row_data).collect();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].id.as_ref(), "codex:api-alias");
            assert_eq!(rows[0].title.as_ref(), "Friendly Model");
            view.provider = ChatProvider::Codex;
            view.model = "api-alias".into();
            let efforts = view.picker_items(Field::Effort, "maximum", cx);
            assert_eq!(
                efforts
                    .into_iter()
                    .find_map(PickerItem::row_data)
                    .expect("effort")
                    .id
                    .as_ref(),
                "maximum"
            );
        });
    }

    fn draft(model: &mut AppModel, cx: &mut Context<AppModel>) -> Entity<NewSession> {
        let owner = cx.weak_entity();
        let style = InputStyle::field(&model.theme, &model.metrics);
        cx.new(|cx| {
            let input = cx.new(|cx| TextInput::new(style, cx));
            let changes = cx.subscribe(&input, |_: &mut NewSession, _, _, _| {});
            NewSession {
                owner,
                input,
                model: String::new(),
                images: Vec::new(),
                provider: ChatProvider::Claude,
                mode: ChatMode::Chat,
                effort: None,
                picker: None,
                picker_changes: None,
                discovering: false,
                discovered: false,
                discovery_error: None,
                busy: false,
                pasting: 0,
                revision: 0,
                _changes: changes,
            }
        })
    }

    #[test]
    fn unavailable_model_or_effort_never_reaches_first_send() {
        let models = vec![ChatModel {
            alias: "offered".into(),
            label: "Offered".into(),
            efforts: vec!["high".into()],
            default_effort: Some("high".into()),
            images: None,
        }];
        let mut choice = ChatSelection {
            provider: ChatProvider::Codex,
            model: Some("removed".into()),
            effort: None,
            target: None,
            launch_context: None,
        };
        assert!(validate_selection(&choice, &models).is_err());
        choice.model = Some("offered".into());
        choice.effort = Some("unsupported".into());
        assert!(validate_selection(&choice, &models).is_err());
        choice.effort = Some("high".into());
        assert!(validate_selection(&choice, &models).is_ok());
    }
}
