//! The chat log of one generation: items by turn, approvals, chat state and the revision.
//!
//! Pure logic, owned by the session owner thread. Every line Claude Code writes to stdout goes
//! through [`ChatLog::apply_cli_line`], which returns the events for subscribers (each one bumps
//! the revision) and the writes the owner must make to the CLI. Every event and snapshot it
//! makes passes `muxy_protocol` validation: text is shortened to its cap, and an approval too
//! large to show is denied instead.
//!
//! Retention trims server memory and snapshots only. There is no remove event, so a live
//! subscriber keeps the items it already has; the `Folded` item and `has_earlier` appear in the
//! next snapshot.

use std::collections::{HashSet, VecDeque};

use muxy_protocol::wire::{HEADER_LEN, encode};
use muxy_protocol::{
    AlwaysAllow, CONTROL, ChatBatch, ChatCapabilities, ChatCommand, ChatEvent, ChatItem,
    ChatItemKind, ChatModel, ChatQuestion, ChatQuestionOption, ChatSnapshot, ChatState, Feature,
    FileChange, MAX_CHAT_COMMAND_DESCRIPTION, MAX_CHAT_COMMANDS, MAX_CHAT_EFFORTS,
    MAX_CHAT_FILE_CHANGES, MAX_CHAT_MODELS, MAX_CHAT_NAME, MAX_CHAT_PENDING, MAX_CHAT_PREVIEW,
    MAX_CHAT_QUESTION_PREVIEW, MAX_CHAT_SNAPSHOT_ITEMS, MAX_CHAT_TEXT, Message, PermissionKind,
    PermissionMode, PermissionPrompt, PermissionRemoved, PermissionResponse, PlanDecision,
    SessionId, ToolDecision,
};
use serde_json::{Map, Value, json};

use super::normalize::{MARKER, Normalizer, Op, clip, str_at};

const MAX_TURNS: usize = 200;
const MAX_TURNS_BYTES: usize = 16 * 1024 * 1024;
/// The active turn keeps its first and last items and folds the ones between.
const KEEP_HEAD: usize = 16;
const KEEP_TAIL: usize = 64;
const MAX_PENDING: usize = 32;
const MAX_SUGGESTION: usize = 64 * 1024;
/// Commands `initialize` lists that only the TUI runs; `system/init` reports them as
/// `terminal_slash_commands`, but only once a turn starts.
const TERMINAL_COMMANDS: &[&str] = &["doctor", "color", "focus", "reload-plugins"];

pub(crate) const TOO_LARGE: &str = "Too large to show in Muxy, switch to Console to review it";
pub(crate) const TOO_MANY: &str = "Too many pending approvals in Muxy";
pub(crate) const UNSHOWN: &str =
    "Muxy can't show this request here, switch to Console to answer it";

/// `system` subtypes that say nothing about whether work is running. `init`, `result` and the
/// task subtypes are lifecycle and handled on their own; any other subtype is unknown.
const INFORMATIONAL: &[&str] = &[
    "status",
    "thinking_tokens",
    "commands_changed",
    "session_state_changed",
    "turn_duration",
    "api_retry",
    "hook_started",
    "hook_progress",
    "hook_response",
    "files_persisted",
    "compact_boundary",
    // Seen in M0 (fixture 16): a tool refused by config; its tool result follows.
    "permission_denied",
];
/// Task statuses (`task_updated.patch.status`, `task_notification.status`) that end a task.
const TASK_DONE: &[&str] = &["completed", "failed", "killed", "stopped"];

/// What the owner must do on the CLI.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    /// Write this line (a `control_response`) to the CLI's stdin.
    Write(Value),
    /// Send `set_permission_mode` with this mode, under a request id the owner numbers.
    SetPermissionMode(&'static str),
}

#[derive(Debug, Default)]
pub(crate) struct Applied {
    pub(crate) events: Vec<ChatEvent>,
    pub(crate) actions: Vec<Action>,
}

/// A pending approval and what is needed to answer the CLI.
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    pub(crate) prompt: PermissionPrompt,
    input: Value,
    /// The one suggestion `always` describes, echoed alone as `updatedPermissions`.
    suggestion: Option<Value>,
    size: usize,
}

/// A resolved approval. If writing `actions` fails, pass `taken` to [`ChatLog::restore`].
#[derive(Debug)]
pub(crate) struct Responded {
    pub(crate) events: Vec<ChatEvent>,
    pub(crate) actions: Vec<Action>,
    pub(crate) taken: Pending,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RespondError {
    /// Not pending: answered, cancelled or expired already (`ApprovalGone`).
    Gone,
    /// The response doesn't fit the prompt's kind; the prompt stays pending.
    Mismatch,
}

#[derive(Debug)]
struct Turn {
    id: String,
    items: Vec<ChatItem>,
    bytes: usize,
    folded: bool,
}

#[derive(Debug)]
pub(crate) struct ChatLog {
    turns: VecDeque<Turn>,
    /// The last turn still takes items; when it doesn't, the next item starts a turn.
    open: bool,
    has_earlier: bool,
    next_turn: u64,
    seq: u64,
    revision: u64,
    /// Last emitted state, and the working copy `sync` compares with it.
    state: ChatState,
    next: ChatState,
    capabilities: ChatCapabilities,
    pending: Vec<Pending>,
    pending_bytes: usize,
    normalizer: Normalizer,
    foreground: bool,
    tasks: HashSet<String>,
    /// Why the lifecycle is unknown; sticks until the process ends.
    uncertain: Option<String>,
    context_tokens: Option<u64>,
    /// Files the open turn changed, for its `TurnEnd`.
    changed: Vec<FileChange>,
    /// Chat features some subscriber can show; `None` with nobody subscribed, when an approval
    /// waits for the app to come back.
    shown: Option<Vec<Feature>>,
}

impl Default for ChatLog {
    fn default() -> Self {
        let state = ChatState {
            quiescent: true,
            ..ChatState::default()
        };
        Self {
            turns: VecDeque::new(),
            open: false,
            has_earlier: false,
            next_turn: 0,
            seq: 0,
            revision: 0,
            next: state.clone(),
            state,
            capabilities: ChatCapabilities::default(),
            pending: Vec::new(),
            pending_bytes: 0,
            normalizer: Normalizer::default(),
            foreground: false,
            tasks: HashSet::new(),
            uncertain: None,
            context_tokens: None,
            changed: Vec::new(),
            shown: None,
        }
    }
}

impl ChatLog {
    pub(crate) fn current_state(&self) -> &ChatState {
        &self.state
    }
    pub(crate) fn earlier(&mut self) {
        self.has_earlier = true;
    }
    /// Shared retention, revisions and snapshots for native provider changes.
    pub(crate) fn apply_changes(
        &mut self,
        changes: impl IntoIterator<Item = super::boundary::Change>,
    ) -> Vec<ChatEvent> {
        use super::boundary::Change;
        let mut events = Vec::new();
        for change in changes {
            match change {
                Change::Item { turn, id, kind } => self.native_upsert(&turn, id, kind, &mut events),
                Change::Delta {
                    turn,
                    id,
                    text,
                    thinking,
                } => {
                    if self.locate(&id).is_none() {
                        let kind = if thinking {
                            ChatItemKind::Thinking {
                                text: String::new(),
                            }
                        } else {
                            ChatItemKind::Assistant {
                                markdown: String::new(),
                                final_in_turn: false,
                            }
                        };
                        self.native_upsert(&turn, id.clone(), kind, &mut events);
                    }
                    self.op(Op::Delta { id, text, thinking }, &mut events);
                }
                Change::End {
                    turn,
                    duration_ms,
                    is_error,
                } => {
                    self.native_upsert(
                        &turn,
                        format!("end:{turn}"),
                        ChatItemKind::TurnEnd {
                            duration_ms,
                            is_error,
                            changed: Vec::new(),
                        },
                        &mut events,
                    );
                    if self.turns.back().is_some_and(|last| last.id == turn) {
                        self.open = true;
                        self.close_turn(&mut events);
                    }
                }
                Change::State(state) => {
                    self.foreground = state.busy || !state.quiescent;
                    self.next = state;
                    self.uncertain = None;
                }
                Change::Capabilities(capabilities) => self.capabilities = capabilities,
                Change::Pending(prompt) => {
                    if self
                        .pending
                        .iter()
                        .any(|pending| pending.prompt.request_id == prompt.request_id)
                    {
                        continue;
                    }
                    if let Ok(size) = prompt.validate()
                        && self.pending.len() < MAX_PENDING
                        && self.pending_bytes + size <= MAX_CHAT_PENDING
                    {
                        self.pending_bytes += size;
                        self.emit(&mut events, ChatEvent::PermissionAdded(prompt.clone()));
                        self.pending.push(Pending {
                            prompt,
                            input: Value::Null,
                            suggestion: None,
                            size,
                        });
                    } else {
                        self.notice(TOO_LARGE, &mut events);
                    }
                }
                Change::Resolved { id, reason } => self.remove(&id, reason, &mut events),
                Change::Notice(text) => self.notice(&text, &mut events),
            }
        }
        self.sync(&mut events);
        events
    }

    fn native_upsert(
        &mut self,
        turn_id: &str,
        id: String,
        kind: ChatItemKind,
        events: &mut Vec<ChatEvent>,
    ) {
        if self.locate(&id).is_some() {
            self.upsert(id, kind, events);
            return;
        }
        let at = self
            .turns
            .iter()
            .position(|turn| turn.id == turn_id)
            .unwrap_or_else(|| {
                self.turns.push_back(Turn {
                    id: turn_id.to_owned(),
                    items: Vec::new(),
                    bytes: 0,
                    folded: false,
                });
                self.turns.len() - 1
            });
        let item = ChatItem {
            id,
            turn_id: turn_id.to_owned(),
            seq: self.seq,
            kind,
        };
        self.seq += 1;
        if let Some(turn) = self.turns.get_mut(at) {
            turn.bytes += item_bytes(&item);
            turn.items.push(item.clone());
            fold(turn);
        }
        self.open = true;
        self.emit(events, ChatEvent::Upsert(item));
    }

    pub(crate) fn seed_changes(
        &mut self,
        changes: impl IntoIterator<Item = super::boundary::Change>,
    ) {
        self.apply_changes(changes);
        self.revision = 0;
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> &ChatState {
        &self.state
    }

    pub(crate) fn capabilities(&self) -> &ChatCapabilities {
        &self.capabilities
    }

    pub(crate) fn pending(&self) -> impl Iterator<Item = &PermissionPrompt> {
        self.pending.iter().map(|pending| &pending.prompt)
    }

    /// Why quiet can't be confirmed.
    #[cfg(test)]
    pub(crate) fn uncertain(&self) -> Option<&str> {
        self.uncertain.as_deref()
    }

    /// Quiet: no foreground turn, no known background task, no pending approval, and nothing
    /// unknown seen in the lifecycle.
    pub(crate) fn is_quiet(&self) -> bool {
        !self.foreground
            && self.tasks.is_empty()
            && self.pending.is_empty()
            && self.uncertain.is_none()
    }

    /// Seeds the log with history from the transcript, before the generation's first event.
    /// Emits nothing and leaves the revision at 0.
    pub(crate) fn seed(&mut self, lines: &[Value]) {
        let mut scratch = Vec::new();
        for line in lines {
            for op in self.normalizer.ops(line) {
                self.op(op, &mut scratch);
            }
        }
        self.close_turn(&mut scratch);
        self.revision = 0;
    }

    /// The owner sent a prompt: a turn is running from now, before `system/init` arrives.
    pub(crate) fn prompt_sent(&mut self) -> Vec<ChatEvent> {
        self.foreground = true;
        let mut events = Vec::new();
        self.sync(&mut events);
        events
    }

    /// The owner interrupted: approvals expire and no turn counts as running any more, even if
    /// `system/init` never arrived for it.
    pub(crate) fn interrupted(&mut self) -> Vec<ChatEvent> {
        self.foreground = false;
        self.expire_all()
    }

    /// The background tasks known to be running.
    pub(crate) fn tasks(&self) -> impl Iterator<Item = &str> {
        self.tasks.iter().map(String::as_str)
    }

    /// A `Notice` item from the host itself.
    pub(crate) fn host_notice(&mut self, text: &str) -> Vec<ChatEvent> {
        let mut events = Vec::new();
        self.notice(text, &mut events);
        events
    }

    /// Sets what the subscribers can show, before a line that may ask for approval.
    pub(crate) fn set_shown(&mut self, shown: Option<Vec<Feature>>) {
        self.shown = shown;
    }

    /// Applies a change the owner confirmed, such as `set_model`'s resolved model or an effort.
    pub(crate) fn update_state(&mut self, change: impl FnOnce(&mut ChatState)) -> Vec<ChatEvent> {
        change(&mut self.next);
        let mut events = Vec::new();
        self.sync(&mut events);
        events
    }

    /// Feeds one stdout line.
    pub(crate) fn apply_cli_line(&mut self, line: &Value) -> Applied {
        let mut out = Applied::default();
        match str_at(line, "type").unwrap_or_default() {
            kind @ ("assistant" | "user" | "stream_event") => {
                if kind == "assistant" && !line["parent_tool_use_id"].is_string() {
                    let usage = &line["message"]["usage"];
                    let tokens = [
                        "input_tokens",
                        "cache_creation_input_tokens",
                        "cache_read_input_tokens",
                    ]
                    .iter()
                    .filter_map(|key| usage[key].as_u64())
                    .sum::<u64>();
                    if tokens > 0 {
                        self.context_tokens = Some(tokens);
                    }
                }
                for op in self.normalizer.ops(line) {
                    self.op(op, &mut out.events);
                }
            }
            "system" => self.system(line, &mut out.events),
            "result" => self.result(line, &mut out.events),
            "control_request" => self.control_request(line, &mut out),
            "control_cancel_request" => {
                if let Some(id) = str_at(line, "request_id") {
                    self.remove(id, PermissionRemoved::Cancelled, &mut out.events);
                }
            }
            // Replies to the owner's own requests; the owner correlates them. Only initialize
            // carries capabilities.
            "control_response" => {
                let response = &line["response"]["response"];
                if response["models"].is_array() {
                    self.initialize(response);
                }
            }
            "rate_limit_event" => {}
            other => self.mark_uncertain(other),
        }
        self.sync(&mut out.events);
        out
    }

    /// Answers the pending approval `request_id`, once.
    pub(crate) fn respond(
        &mut self,
        request_id: &str,
        response: PermissionResponse,
    ) -> Result<Responded, RespondError> {
        let index = self
            .pending
            .iter()
            .position(|pending| pending.prompt.request_id == request_id)
            .ok_or(RespondError::Gone)?;
        let pending = &self.pending[index];
        let input = pending.input.clone();
        let mut mode = None;
        let body = match (&pending.prompt.kind, response) {
            (_, PermissionResponse::Dismiss { message }) => deny(&message, false),
            (PermissionKind::Tool { .. }, PermissionResponse::Tool(ToolDecision::Allow)) => {
                json!({"behavior": "allow", "updatedInput": input})
            }
            (
                PermissionKind::Tool {
                    always: Some(_), ..
                },
                PermissionResponse::Tool(ToolDecision::AllowAlways),
            ) => {
                let Some(suggestion) = pending.suggestion.clone() else {
                    return Err(RespondError::Mismatch);
                };
                json!({"behavior": "allow", "updatedInput": input, "updatedPermissions": [suggestion]})
            }
            (
                PermissionKind::Tool { .. },
                PermissionResponse::Tool(ToolDecision::Deny { message, interrupt }),
            ) => deny(&message, interrupt),
            (PermissionKind::Questions { .. }, PermissionResponse::Questions(answers)) => {
                // M0 verified one single-select answer. UNVERIFIED: joining several answers
                // (multi-select) with ", ".
                let answers: Map<String, Value> = answers
                    .into_iter()
                    .map(|answer| (answer.question, Value::String(answer.answers.join(", "))))
                    .collect();
                let mut input = input;
                if let Some(fields) = input.as_object_mut() {
                    fields.insert("answers".into(), Value::Object(answers));
                }
                json!({"behavior": "allow", "updatedInput": input})
            }
            (
                PermissionKind::Plan { .. },
                PermissionResponse::Plan(PlanDecision::Approve { mode: next }),
            ) => {
                mode = next.and_then(cli_mode);
                json!({"behavior": "allow", "updatedInput": input})
            }
            (
                PermissionKind::Plan { .. },
                PermissionResponse::Plan(PlanDecision::KeepPlanning { feedback }),
            ) => deny(&feedback, false),
            _ => return Err(RespondError::Mismatch),
        };
        let taken = self.pending.remove(index);
        self.pending_bytes -= taken.size;
        let mut events = Vec::new();
        self.emit(
            &mut events,
            ChatEvent::PermissionRemoved {
                request_id: request_id.to_owned(),
                reason: PermissionRemoved::Answered,
            },
        );
        self.sync(&mut events);
        let mut actions = vec![Action::Write(success(request_id, &body))];
        actions.extend(mode.map(Action::SetPermissionMode));
        Ok(Responded {
            events,
            actions,
            taken,
        })
    }

    /// Puts back an approval whose answer could not be written.
    pub(crate) fn restore(&mut self, taken: Pending) -> Vec<ChatEvent> {
        let mut events = Vec::new();
        self.pending_bytes += taken.size;
        self.emit(
            &mut events,
            ChatEvent::PermissionAdded(taken.prompt.clone()),
        );
        self.pending.push(taken);
        self.sync(&mut events);
        events
    }

    /// Drops every pending approval, on interrupt or exit.
    pub(crate) fn expire_all(&mut self) -> Vec<ChatEvent> {
        let mut events = Vec::new();
        for pending in std::mem::take(&mut self.pending) {
            self.emit(
                &mut events,
                ChatEvent::PermissionRemoved {
                    request_id: pending.prompt.request_id,
                    reason: PermissionRemoved::Expired,
                },
            );
        }
        self.pending_bytes = 0;
        self.sync(&mut events);
        events
    }

    /// Claude Code exited: approvals expire, nothing is running any more, and subscribers get
    /// `Exited` in order. `reason` ends with the tail of its stderr.
    pub(crate) fn exited(&mut self, reason: &str) -> Vec<ChatEvent> {
        let mut events = self.expire_all();
        self.foreground = false;
        self.tasks.clear();
        // The process is gone, so nothing unknown can still be running.
        self.uncertain = None;
        self.close_turn(&mut events);
        self.emit(
            &mut events,
            ChatEvent::Exited {
                reason: clip(reason, MAX_CHAT_PREVIEW),
            },
        );
        self.sync(&mut events);
        events
    }

    /// Claude Code could not be confirmed stopped; Muxy keeps the conversation reserved.
    pub(crate) fn blocked(&mut self, reason: &str) -> Vec<ChatEvent> {
        let mut events = Vec::new();
        self.emit(
            &mut events,
            ChatEvent::Blocked {
                reason: clip(reason, MAX_CHAT_PREVIEW),
            },
        );
        events
    }

    /// An error for subscribers, in revision order (such as a failed write to the CLI).
    pub(crate) fn error(&mut self, text: &str) -> Vec<ChatEvent> {
        let mut events = Vec::new();
        self.emit(
            &mut events,
            ChatEvent::Error {
                text: clip(text, MAX_CHAT_PREVIEW),
            },
        );
        events
    }

    /// Everything at the current revision, as a batch of its own whose encoding fits `budget`:
    /// all pending approvals, then the newest items that fit.
    pub(crate) fn snapshot(
        &self,
        session: SessionId,
        generation: u64,
        subscription: u32,
        budget: usize,
    ) -> ChatBatch {
        let items: Vec<&ChatItem> = self.turns.iter().flat_map(|turn| &turn.items).collect();
        let batch = |start: usize| ChatBatch {
            session,
            generation,
            from_rev: self.revision,
            to_rev: self.revision,
            events: Vec::new(),
            snapshot: Some(Box::new(ChatSnapshot {
                subscription,
                items: items[start..].iter().map(|item| (*item).clone()).collect(),
                state: self.state.clone(),
                capabilities: self.capabilities.clone(),
                pending: self.pending().cloned().collect(),
                has_earlier: self.has_earlier || start > 0,
            })),
        };
        // Pre-trim by an estimate (string bytes plus generous framing), then confirm by encoding.
        let mut size = encoded(batch(items.len())).1;
        let mut start = items.len();
        while start > 0
            && items.len() - start < MAX_CHAT_SNAPSHOT_ITEMS
            && size + item_bytes(items[start - 1]) <= budget
        {
            size += item_bytes(items[start - 1]);
            start -= 1;
        }
        loop {
            let (snapshot, size) = encoded(batch(start));
            if size <= budget || start == items.len() {
                return snapshot;
            }
            start += 1;
        }
    }

    fn initialize(&mut self, response: &Value) {
        let models = response["models"].as_array().into_iter().flatten();
        self.capabilities.models = models
            .filter_map(|model| {
                let alias = str_at(model, "value")?;
                Some(ChatModel {
                    images: None,
                    default_effort: None,
                    alias: clip(alias, MAX_CHAT_NAME),
                    label: clip(str_at(model, "displayName").unwrap_or(alias), MAX_CHAT_NAME),
                    efforts: model["supportedEffortLevels"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .take(MAX_CHAT_EFFORTS)
                        .map(|effort| clip(effort, MAX_CHAT_NAME))
                        .collect(),
                })
            })
            .take(MAX_CHAT_MODELS)
            .collect();
        // Dropped, not refused: one odd name (an MCP prompt's has spaces) mustn't lose the rest.
        let commands = response["commands"].as_array().into_iter().flatten();
        self.capabilities.commands = commands
            .filter_map(|command| {
                let name = str_at(command, "name")?;
                (ChatCommand::valid_name(name) && !TERMINAL_COMMANDS.contains(&name)).then(|| {
                    ChatCommand {
                        name: name.to_owned(),
                        description: clip(
                            str_at(command, "description").unwrap_or_default(),
                            MAX_CHAT_COMMAND_DESCRIPTION,
                        ),
                        skill: !command["builtin"].as_bool().unwrap_or(false),
                    }
                })
            })
            .take(MAX_CHAT_COMMANDS)
            .collect();
        if let Some(mode) = str_at(response, "current_permission_mode") {
            self.next.permission_mode = parse_mode(mode);
        }
    }

    fn system(&mut self, line: &Value, events: &mut Vec<ChatEvent>) {
        let subtype = str_at(line, "subtype").unwrap_or_default();
        let task = || str_at(line, "task_id").map(str::to_owned);
        match subtype {
            // Starts every turn, including the CLI's own after a background task finishes.
            "init" => {
                self.foreground = true;
                self.close_turn(events);
                if let Some(model) = str_at(line, "model") {
                    self.next.model = Some(clip(model, MAX_CHAT_NAME));
                }
                if let Some(mode) = str_at(line, "permissionMode") {
                    self.next.permission_mode = parse_mode(mode);
                }
            }
            "task_started" => self.tasks.extend(task()),
            "task_updated" | "task_notification" => {
                let status = str_at(&line["patch"], "status").or(str_at(line, "status"));
                if let Some(id) = str_at(line, "task_id")
                    .filter(|_| status.is_some_and(|status| TASK_DONE.contains(&status)))
                {
                    self.tasks.remove(id);
                }
            }
            // The full list of running background tasks.
            "background_tasks_changed" => {
                self.tasks = line["tasks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|task| str_at(task, "task_id").map(str::to_owned))
                    .collect();
            }
            subtype if INFORMATIONAL.contains(&subtype) => {}
            subtype => self.mark_uncertain(&format!("system/{subtype}")),
        }
    }

    fn result(&mut self, line: &Value, events: &mut Vec<ChatEvent>) {
        self.foreground = false;
        if let Some(tokens) = self.context_tokens {
            let usage = line["modelUsage"].as_object().into_iter().flatten();
            let window = usage
                .filter_map(|(_, usage)| usage["contextWindow"].as_u64())
                .max()
                .filter(|window| *window > 0);
            self.next.context_percent =
                window.map(|window| u8::try_from((tokens * 100 / window).min(100)).unwrap_or(100));
        }
        let is_error = line["is_error"].as_bool().unwrap_or(false)
            || str_at(line, "subtype").is_some_and(|subtype| subtype != "success");
        let id =
            str_at(line, "uuid").map_or_else(|| format!("turn-end:{}", self.seq), str::to_owned);
        let changed = std::mem::take(&mut self.changed);
        self.upsert(
            clip(&id, MAX_CHAT_NAME),
            ChatItemKind::TurnEnd {
                duration_ms: line["duration_ms"].as_u64().unwrap_or(0),
                is_error,
                changed,
            },
            events,
        );
        self.close_turn(events);
    }

    fn control_request(&mut self, line: &Value, out: &mut Applied) {
        let request = &line["request"];
        let subtype = str_at(request, "subtype").unwrap_or_default();
        let Some(request_id) = str_at(line, "request_id") else {
            self.notice(
                &format!("Claude Code sent a control request ({subtype}) without an id"),
                &mut out.events,
            );
            return;
        };
        if subtype == "can_use_tool" {
            self.permission(request_id, request, out);
            return;
        }
        out.actions.push(Action::Write(json!({
            "type": "control_response",
            "response": {
                "subtype": "error",
                "request_id": request_id,
                "error": format!("Unsupported control request subtype: {subtype}"),
            },
        })));
        self.notice(&format!("Claude Code asked for \"{subtype}\", which Muxy doesn't support; switch to Console if it's needed"),
            &mut out.events,
        );
    }

    fn permission(&mut self, request_id: &str, request: &Value, out: &mut Applied) {
        let tool = str_at(request, "tool_name").unwrap_or_default();
        let input = request["input"].clone();
        let mut suggestion = None;
        let kind = match tool {
            "AskUserQuestion" => PermissionKind::Questions {
                questions: input["questions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(question)
                    .collect(),
            },
            "ExitPlanMode" => PermissionKind::Plan {
                markdown: str_at(&input, "plan").unwrap_or_default().to_owned(),
            },
            _ => {
                let always = always_allow(&request["permission_suggestions"]);
                suggestion = always.as_ref().map(|(_, raw)| raw.clone());
                PermissionKind::Tool {
                    tool: tool.to_owned(),
                    title: str_at(request, "description")
                        .or(str_at(request, "display_name"))
                        .unwrap_or(tool)
                        .to_owned(),
                    detail: str_at(&input, "command").map_or_else(
                        || serde_json::to_string_pretty(&input).unwrap_or_default(),
                        str::to_owned,
                    ),
                    always: always.map(|(always, _)| always),
                }
            }
        };
        let mut prompt = PermissionPrompt {
            request_id: request_id.to_owned(),
            tool_use_id: str_at(request, "tool_use_id").map(str::to_owned),
            kind,
        };
        // Previews only illustrate; drop them rather than deny questions that fit without them.
        let over = match prompt.validate() {
            Ok(size) => self.pending_bytes + size > MAX_CHAT_PENDING,
            Err(_) => true,
        };
        if over && let PermissionKind::Questions { questions } = &mut prompt.kind {
            for option in questions
                .iter_mut()
                .flat_map(|question| &mut question.options)
            {
                option.preview.clear();
            }
        }
        let needs = match prompt.kind {
            PermissionKind::Tool { .. } => None,
            PermissionKind::Questions { .. } => Some(Feature::CHAT_QUESTIONS),
            PermissionKind::Plan { .. } => Some(Feature::CHAT_PLAN),
            PermissionKind::Native { .. } => Some(Feature::CHAT_NATIVE_APPROVALS),
        };
        let unshown = needs.is_some_and(|needs| {
            self.shown
                .as_ref()
                .is_some_and(|shown| !shown.contains(&needs))
        });
        // Never shortened: the user must see exactly what they approve.
        let refusal = match prompt.validate() {
            _ if unshown => Some(UNSHOWN),
            Err(_) => Some(TOO_LARGE),
            Ok(size)
                if self.pending.len() >= MAX_PENDING
                    || self.pending_bytes + size > MAX_CHAT_PENDING =>
            {
                Some(TOO_MANY)
            }
            Ok(size) => {
                self.pending_bytes += size;
                self.emit(&mut out.events, ChatEvent::PermissionAdded(prompt.clone()));
                self.pending.push(Pending {
                    prompt,
                    input,
                    suggestion,
                    size,
                });
                None
            }
        };
        if let Some(message) = refusal {
            out.actions
                .push(Action::Write(success(request_id, &deny(message, false))));
            self.notice(&format!("Muxy denied {tool}: {message}"), &mut out.events);
        }
    }

    fn remove(&mut self, request_id: &str, reason: PermissionRemoved, events: &mut Vec<ChatEvent>) {
        if let Some(index) = self
            .pending
            .iter()
            .position(|pending| pending.prompt.request_id == request_id)
        {
            let pending = self.pending.remove(index);
            self.pending_bytes -= pending.size;
            self.emit(
                events,
                ChatEvent::PermissionRemoved {
                    request_id: pending.prompt.request_id,
                    reason,
                },
            );
        }
    }

    fn mark_uncertain(&mut self, what: &str) {
        self.uncertain.get_or_insert_with(|| {
            // Until Claude Code exits this keeps the chat "working"; the log names what caused it.
            log::info!("chat counts as working until Claude Code exits: unknown {what}");
            clip(
                &format!("Claude Code sent {what}, which Muxy doesn't know"),
                MAX_CHAT_PREVIEW,
            )
        });
    }

    fn op(&mut self, op: Op, events: &mut Vec<ChatEvent>) {
        match op {
            Op::Upsert { id, kind } => {
                // A prompt starts a turn (live, `system/init` already closed the last one).
                if matches!(kind, ChatItemKind::User { .. }) {
                    self.close_turn(events);
                }
                self.upsert(id, kind, events);
            }
            Op::Delta { id, text, thinking } => {
                let Some(at) = self.locate(&id) else {
                    let text = clip(&text, MAX_CHAT_TEXT);
                    let kind = if thinking {
                        ChatItemKind::Thinking { text }
                    } else {
                        ChatItemKind::Assistant {
                            markdown: text,
                            final_in_turn: false,
                        }
                    };
                    self.upsert(id, kind, events);
                    return;
                };
                let mut appended = false;
                self.update(at, |item| {
                    if let ChatItemKind::Assistant { markdown: body, .. }
                    | ChatItemKind::Thinking { text: body } = &mut item.kind
                    {
                        // Past the cap, the final block's upsert carries the shortened text.
                        if body.len() + text.len() + MARKER.len() <= MAX_CHAT_TEXT {
                            body.push_str(&text);
                            appended = true;
                        }
                    }
                });
                if appended {
                    self.emit(events, ChatEvent::Delta { item_id: id, text });
                }
            }
            Op::ToolResult {
                id,
                result,
                is_error,
            } => self.patch_tool(&id, events, |result_slot, _, error_slot| {
                *result_slot = Some(result);
                *error_slot = is_error;
            }),
            Op::Subagent { parent } => self.patch_tool(&parent, events, |_, count, _| {
                *count = count.saturating_add(1);
            }),
            Op::FileChanged(change) => {
                let full = self.changed.len() >= MAX_CHAT_FILE_CHANGES;
                match self
                    .changed
                    .iter_mut()
                    .find(|known| known.path == change.path)
                {
                    Some(known) => {
                        known.added = known.added.saturating_add(change.added);
                        known.removed = known.removed.saturating_add(change.removed);
                    }
                    None if !full => self.changed.push(change),
                    None => {}
                }
            }
        }
    }

    fn patch_tool(
        &mut self,
        id: &str,
        events: &mut Vec<ChatEvent>,
        patch: impl FnOnce(&mut Option<String>, &mut u32, &mut bool),
    ) {
        let Some(at) = self.locate(id) else { return };
        let mut patched = false;
        let item = self.update(at, |item| {
            if let ChatItemKind::Tool {
                result,
                subagent_count,
                is_error,
                ..
            } = &mut item.kind
            {
                patch(result, subagent_count, is_error);
                patched = true;
            }
        });
        if patched {
            self.emit(events, ChatEvent::Upsert(item));
        }
    }

    /// Where item `id` is, as (turn, item) indexes; the newest items are searched first.
    fn locate(&self, id: &str) -> Option<(usize, usize)> {
        self.turns
            .iter()
            .enumerate()
            .rev()
            .find_map(|(turn_index, turn)| {
                let item_index = turn.items.iter().rposition(|item| item.id == id)?;
                Some((turn_index, item_index))
            })
    }

    /// Changes an item in place, keeping its turn's byte count, and returns a copy of it.
    fn update(
        &mut self,
        (turn, index): (usize, usize),
        change: impl FnOnce(&mut ChatItem),
    ) -> ChatItem {
        let turn = &mut self.turns[turn];
        let item = &mut turn.items[index];
        let before = item_bytes(item);
        change(item);
        turn.bytes = (turn.bytes + item_bytes(item)).saturating_sub(before);
        item.clone()
    }

    fn upsert(&mut self, id: String, kind: ChatItemKind, events: &mut Vec<ChatEvent>) {
        if let Some(at) = self.locate(&id) {
            let item = self.update(at, |item| {
                // The final tool block keeps a result or subagent steps already patched in.
                item.kind = match (kind, &item.kind) {
                    (
                        ChatItemKind::Tool {
                            name,
                            summary,
                            input_preview,
                            result: None,
                            subagent_count: 0,
                            is_error: false,
                        },
                        ChatItemKind::Tool {
                            result,
                            subagent_count,
                            is_error,
                            ..
                        },
                    ) => ChatItemKind::Tool {
                        name,
                        summary,
                        input_preview,
                        result: result.clone(),
                        subagent_count: *subagent_count,
                        is_error: *is_error,
                    },
                    (kind, _) => kind,
                };
            });
            self.emit(events, ChatEvent::Upsert(item));
            return;
        }
        if !self.open {
            self.turns.push_back(Turn {
                id: format!("turn:{}", self.next_turn),
                items: Vec::new(),
                bytes: 0,
                folded: false,
            });
            self.next_turn += 1;
            self.open = true;
        }
        let Some(turn) = self.turns.back_mut() else {
            return;
        };
        let item = ChatItem {
            id,
            turn_id: turn.id.clone(),
            seq: self.seq,
            kind,
        };
        self.seq += 1;
        turn.bytes += item_bytes(&item);
        turn.items.push(item.clone());
        fold(turn);
        self.emit(events, ChatEvent::Upsert(item));
    }

    fn notice(&mut self, text: &str, events: &mut Vec<ChatEvent>) {
        let id = format!("notice:{}", self.seq);
        self.upsert(
            id,
            ChatItemKind::Notice {
                text: clip(text, MAX_CHAT_TEXT),
            },
            events,
        );
    }

    /// Ends the open turn: its last assistant text is final, and old turns are evicted.
    fn close_turn(&mut self, events: &mut Vec<ChatEvent>) {
        self.changed.clear();
        if !std::mem::take(&mut self.open) {
            return;
        }
        let last = self.turns.back_mut().and_then(|turn| {
            turn.items
                .iter_mut()
                .rev()
                .find_map(|item| match &mut item.kind {
                    ChatItemKind::Assistant { final_in_turn, .. } => {
                        *final_in_turn = true;
                        Some(item.clone())
                    }
                    _ => None,
                })
        });
        if let Some(item) = last {
            self.emit(events, ChatEvent::Upsert(item));
        }
        let mut bytes: usize = self.turns.iter().map(|turn| turn.bytes).sum();
        while self.turns.len() > MAX_TURNS || (bytes > MAX_TURNS_BYTES && self.turns.len() > 1) {
            let Some(turn) = self.turns.pop_front() else {
                break;
            };
            bytes = bytes.saturating_sub(turn.bytes);
            self.has_earlier = true;
        }
    }

    fn emit(&mut self, events: &mut Vec<ChatEvent>, event: ChatEvent) {
        debug_assert!(event.validate().is_ok(), "invalid chat event {event:?}");
        self.revision += 1;
        events.push(event);
    }

    fn sync(&mut self, events: &mut Vec<ChatEvent>) {
        self.next.busy = self.foreground;
        self.next.quiescent = self.is_quiet();
        if self.next != self.state {
            self.state = self.next.clone();
            self.emit(events, ChatEvent::State(self.state.clone()));
        }
    }
}

/// Keeps the active turn at `KEEP_HEAD` items, one `Folded`, then `KEEP_TAIL` items.
fn fold(turn: &mut Turn) {
    if turn.items.len() <= KEEP_HEAD + KEEP_TAIL + usize::from(turn.folded) {
        return;
    }
    let removed = if turn.folded {
        let removed = turn.items.remove(KEEP_HEAD + 1);
        if let ChatItemKind::Folded { count } = &mut turn.items[KEEP_HEAD].kind {
            *count += 1;
        }
        removed
    } else {
        turn.folded = true;
        let folded = ChatItem {
            id: clip(&format!("{}:folded", turn.id), MAX_CHAT_NAME),
            turn_id: turn.id.clone(),
            seq: turn.items[KEEP_HEAD].seq,
            kind: ChatItemKind::Folded { count: 1 },
        };
        turn.bytes += item_bytes(&folded);
        std::mem::replace(&mut turn.items[KEEP_HEAD], folded)
    };
    turn.bytes = turn.bytes.saturating_sub(item_bytes(&removed));
}

/// An item's string bytes plus generous CBOR framing; over- rather than under-estimates.
fn item_bytes(item: &ChatItem) -> usize {
    let text = match &item.kind {
        ChatItemKind::User { text }
        | ChatItemKind::Thinking { text }
        | ChatItemKind::Notice { text }
        | ChatItemKind::Assistant { markdown: text, .. } => text.len(),
        ChatItemKind::Tool {
            name,
            summary,
            input_preview,
            result,
            ..
        } => {
            name.len()
                + summary.len()
                + input_preview.len()
                + result.as_ref().map_or(0, String::len)
        }
        ChatItemKind::TurnEnd { changed, .. } => {
            changed.iter().map(|change| change.path.len() + 8).sum()
        }
        ChatItemKind::Folded { .. } => 0,
    };
    item.id.len() + item.turn_id.len() + text + 48
}

/// The batch and the size of its frame; a batch too large to frame counts as `usize::MAX`.
fn encoded(batch: ChatBatch) -> (ChatBatch, usize) {
    let message = Message::Chat(batch);
    let mut buffer = Vec::new();
    let size = encode(&message, CONTROL, &mut buffer).map_or(usize::MAX, |()| buffer.len());
    let Message::Chat(batch) = message else {
        unreachable!("built as a chat message")
    };
    debug_assert!(size == usize::MAX || size >= HEADER_LEN);
    (batch, size)
}

fn question(value: &Value) -> ChatQuestion {
    ChatQuestion {
        allow_other: None,
        question: str_at(value, "question").unwrap_or_default().to_owned(),
        header: str_at(value, "header").unwrap_or_default().to_owned(),
        options: value["options"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|option| ChatQuestionOption {
                label: str_at(option, "label").unwrap_or_default().to_owned(),
                description: str_at(option, "description").unwrap_or_default().to_owned(),
                preview: clip(
                    str_at(option, "preview").unwrap_or_default(),
                    MAX_CHAT_QUESTION_PREVIEW,
                ),
            })
            .collect(),
        multi_select: value["multiSelect"].as_bool().unwrap_or(false),
    }
}

/// The `addRules` allow suggestion "Always allow" applies, as shown to the user, with its raw
/// JSON. Other suggestions (`addDirectories`, `setMode`) are never sent.
fn always_allow(suggestions: &Value) -> Option<(AlwaysAllow, Value)> {
    let raw = suggestions.as_array()?.iter().find(|suggestion| {
        str_at(suggestion, "type") == Some("addRules")
            && str_at(suggestion, "behavior") == Some("allow")
    })?;
    if raw.to_string().len() > MAX_SUGGESTION {
        return None;
    }
    let label = raw["rules"]
        .as_array()?
        .iter()
        .filter_map(|rule| {
            let tool = str_at(rule, "toolName")?;
            Some(
                str_at(rule, "ruleContent")
                    .map_or_else(|| tool.to_owned(), |content| format!("{tool}({content})")),
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    if label.is_empty() || label.len() > MAX_CHAT_PREVIEW {
        return None;
    }
    let saved_to_settings = matches!(
        str_at(raw, "destination"),
        Some("localSettings" | "projectSettings" | "userSettings")
    );
    Some((
        AlwaysAllow {
            label,
            saved_to_settings,
        },
        raw.clone(),
    ))
}

fn deny(message: &str, interrupt: bool) -> Value {
    json!({"behavior": "deny", "message": message, "interrupt": interrupt})
}

fn success(request_id: &str, response: &Value) -> Value {
    json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request_id, "response": response},
    })
}

fn parse_mode(mode: &str) -> PermissionMode {
    match mode {
        "acceptEdits" => PermissionMode::AcceptEdits,
        "plan" => PermissionMode::Plan,
        "auto" => PermissionMode::Auto,
        "bypassPermissions" => PermissionMode::BypassPermissions,
        _ => PermissionMode::Default,
    }
}

/// The CLI's name for a mode; `None` for one this build doesn't know.
pub(crate) fn cli_mode(mode: PermissionMode) -> Option<&'static str> {
    match mode {
        PermissionMode::Default => Some("default"),
        PermissionMode::AcceptEdits => Some("acceptEdits"),
        PermissionMode::Plan => Some("plan"),
        PermissionMode::Auto => Some("auto"),
        PermissionMode::BypassPermissions => Some("bypassPermissions"),
        PermissionMode::Unrecognized(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxy_protocol::{
        ChatAnswer, MAX_CHAT_BATCH_BYTES, MAX_CHAT_PLAN, MAX_CHAT_QUESTION_ENTRIES,
        MAX_CHAT_SNAPSHOT_BYTES,
    };
    use std::collections::HashSet;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/claude_stream");

    fn session() -> SessionId {
        SessionId::new(1).unwrap()
    }

    fn names() -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(FIXTURES)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| {
                std::path::Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext == "jsonl")
            })
            .collect();
        names.sort();
        assert!(names.len() >= 20);
        names
    }

    /// The fixture's records as (direction, message).
    fn records(name: &str) -> Vec<(String, Value)> {
        std::fs::read_to_string(format!("{FIXTURES}/{name}"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|record| record["msg"].is_object())
            .map(|record| {
                (
                    str_at(&record, "dir").unwrap().to_owned(),
                    record["msg"].clone(),
                )
            })
            .collect()
    }

    /// Every event list must split into valid batches that encode within the batch cap.
    fn check(events: &[ChatEvent]) {
        for chunk in events.chunks(muxy_protocol::MAX_CHAT_EVENTS) {
            let batch = ChatBatch {
                session: session(),
                generation: 1,
                from_rev: 0,
                to_rev: chunk.len() as u64,
                events: chunk.to_vec(),
                snapshot: None,
            };
            batch.validate().unwrap();
            assert!(encoded(batch).1 <= MAX_CHAT_BATCH_BYTES);
        }
    }

    fn check_snapshot(log: &ChatLog, budget: usize) -> ChatSnapshot {
        let batch = log.snapshot(session(), 1, 7, budget);
        batch.validate().unwrap();
        let (batch, size) = encoded(batch);
        assert!(size <= budget, "snapshot {size} > {budget}");
        assert_eq!(
            (batch.from_rev, batch.to_rev),
            (log.revision(), log.revision())
        );
        let snapshot = *batch.snapshot.unwrap();
        let ids: HashSet<&str> = snapshot.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids.len(), snapshot.items.len(), "item ids are unique");
        snapshot
    }

    /// The answer the M0 host recorded, as the client would have sent it.
    fn decision(log: &ChatLog, request_id: &str, body: &Value) -> PermissionResponse {
        let prompt = log
            .pending()
            .find(|prompt| prompt.request_id == request_id)
            .unwrap();
        let message = str_at(body, "message").unwrap_or_default().to_owned();
        match (&prompt.kind, str_at(body, "behavior").unwrap()) {
            (PermissionKind::Questions { .. }, "allow") => PermissionResponse::Questions(
                body["updatedInput"]["answers"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(question, answer)| ChatAnswer {
                        question: question.clone(),
                        answers: vec![answer.as_str().unwrap().to_owned()],
                    })
                    .collect(),
            ),
            (PermissionKind::Plan { .. }, "allow") => {
                PermissionResponse::Plan(PlanDecision::Approve { mode: None })
            }
            (PermissionKind::Plan { .. }, _) => {
                PermissionResponse::Plan(PlanDecision::KeepPlanning { feedback: message })
            }
            (_, "allow") if body.get("updatedPermissions").is_some() => {
                PermissionResponse::Tool(ToolDecision::AllowAlways)
            }
            (_, "allow") => PermissionResponse::Tool(ToolDecision::Allow),
            _ => PermissionResponse::Tool(ToolDecision::Deny {
                message,
                interrupt: body["interrupt"].as_bool().unwrap_or(false),
            }),
        }
    }

    #[derive(Default)]
    struct Run {
        log: ChatLog,
        events: Vec<ChatEvent>,
        actions: Vec<Action>,
        /// (what Muxy wrote, what the M0 host wrote) for each answered approval.
        answers: Vec<(Value, Value)>,
        /// `is_quiet()` and `busy` after each stdout line.
        quiet: Vec<(bool, bool)>,
    }

    /// Replays stdout through the log, answering approvals the way the recording did.
    fn run(name: &str) -> Run {
        let mut run = Run::default();
        for (dir, message) in records(name) {
            match dir.as_str() {
                "out" => {
                    let applied = run.log.apply_cli_line(&message);
                    check(&applied.events);
                    run.events.extend(applied.events);
                    run.actions.extend(applied.actions);
                    run.quiet.push((run.log.is_quiet(), run.log.state().busy));
                }
                "in" if str_at(&message, "type") == Some("control_response")
                    && str_at(&message["response"], "subtype") == Some("success") =>
                {
                    let id = str_at(&message["response"], "request_id").unwrap();
                    if run.log.pending().any(|prompt| prompt.request_id == id) {
                        let response = decision(&run.log, id, &message["response"]["response"]);
                        let responded = run.log.respond(id, response).unwrap();
                        check(&responded.events);
                        run.events.extend(responded.events);
                        let Action::Write(ours) = &responded.actions[0] else {
                            panic!()
                        };
                        run.answers.push((ours.clone(), message));
                    }
                }
                _ => {}
            }
        }
        run
    }

    fn comparable(snapshot: &ChatSnapshot) -> Vec<(String, ChatItemKind)> {
        snapshot
            .items
            .iter()
            .filter(|item| {
                !item.id.starts_with("notice:")
                    && !matches!(item.kind, ChatItemKind::TurnEnd { .. })
            })
            .map(|item| {
                let mut kind = item.kind.clone();
                if let ChatItemKind::Assistant { final_in_turn, .. } = &mut kind {
                    *final_in_turn = false;
                }
                (item.id.clone(), kind)
            })
            .collect()
    }

    #[test]
    fn every_fixture_replays_into_valid_stable_items() {
        for name in names() {
            let run = run(&name);
            let snapshot = check_snapshot(&run.log, MAX_CHAT_SNAPSHOT_BYTES);
            for item in &snapshot.items {
                if let ChatItemKind::Tool { result, .. } = &item.kind {
                    assert!(result.is_some(), "{name}: tool {} has no result", item.id);
                }
            }
            // Every streamed delta lands on an item the final assistant block also names.
            let ids: HashSet<&str> = snapshot.items.iter().map(|item| item.id.as_str()).collect();
            for event in &run.events {
                if let ChatEvent::Delta { item_id, .. } = event {
                    assert!(
                        ids.contains(item_id.as_str()),
                        "{name}: delta for {item_id}"
                    );
                }
            }

            // The same conversation read as a transcript gets the same ids and items.
            let lines: Vec<Value> = records(&name)
                .into_iter()
                .filter(|(dir, message)| {
                    dir == "out" && matches!(str_at(message, "type"), Some("user" | "assistant"))
                })
                .map(|(_, mut message)| {
                    message.as_object_mut().unwrap().remove("isReplay");
                    message
                })
                .collect();
            let mut seeded = ChatLog::default();
            seeded.seed(&lines);
            assert_eq!(seeded.revision(), 0);
            let history = check_snapshot(&seeded, MAX_CHAT_SNAPSHOT_BYTES);
            assert_eq!(comparable(&history), comparable(&snapshot), "{name}");

            // The host's error reply in 05 leaves its approval pending; everything else settles.
            if name != "05_error_reply_to_can_use_tool.jsonl" {
                assert!(
                    run.log.is_quiet(),
                    "{name} ends quiet: {:?}",
                    run.log.uncertain()
                );
            }
        }
    }

    #[test]
    fn replayed_prompt_is_keyed_by_its_uuid() {
        let run = run("17_thinking_summaries_replay.jsonl");
        let snapshot = check_snapshot(&run.log, MAX_CHAT_SNAPSHOT_BYTES);
        assert_eq!(snapshot.items[0].id, "af35a53b-247a-4ca5-abd5-573c6ef95f3f");
        assert!(matches!(snapshot.items[0].kind, ChatItemKind::User { .. }));
        assert!(
            snapshot
                .items
                .iter()
                .all(|item| item.turn_id == snapshot.items[0].turn_id)
        );
        let last = snapshot
            .items
            .iter()
            .rev()
            .find(|item| matches!(item.kind, ChatItemKind::Assistant { .. }));
        assert!(matches!(
            last.unwrap().kind,
            ChatItemKind::Assistant {
                final_in_turn: true,
                ..
            }
        ));
    }

    #[test]
    fn answers_match_the_cli_shapes_from_m0() {
        for name in [
            "02_tools_parallel_allow.jsonl",
            "03_deny.jsonl",
            "03b_deny_interrupt.jsonl",
            "08_ask_user_question.jsonl",
            "08b_exit_plan_mode.jsonl",
            "10_hooks.jsonl",
        ] {
            let run = run(name);
            assert!(!run.answers.is_empty(), "{name}");
            for (ours, recorded) in &run.answers {
                assert_eq!(ours, recorded, "{name}");
            }
        }
    }

    #[test]
    fn allow_always_sends_only_the_rule_it_showed() {
        let run = run("04_allow_updated_permissions.jsonl");
        let added = run.events.iter().find_map(|event| match event {
            ChatEvent::PermissionAdded(prompt) => Some(prompt.clone()),
            _ => None,
        });
        let PermissionKind::Tool { always, .. } = added.unwrap().kind else {
            panic!()
        };
        assert_eq!(
            always,
            Some(AlwaysAllow {
                label: "Bash(touch rule.txt)".into(),
                saved_to_settings: true
            })
        );
        let [(ours, recorded)] = run.answers.as_slice() else {
            panic!()
        };
        // The M0 host sent all three suggestions; Muxy sends only the one it described.
        let body = &ours["response"]["response"];
        assert_eq!(
            body["updatedInput"],
            recorded["response"]["response"]["updatedInput"]
        );
        assert_eq!(
            body["updatedPermissions"],
            json!([{"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "touch rule.txt"}],
                "behavior": "allow", "destination": "localSettings"}])
        );
    }

    #[test]
    fn parallel_approvals_resolve_independently_and_once() {
        let mut log = ChatLog::default();
        for (dir, message) in records("02_tools_parallel_allow.jsonl") {
            if dir == "out" {
                check(&log.apply_cli_line(&message).events);
            }
        }
        let ids: Vec<String> = log
            .pending()
            .map(|prompt| prompt.request_id.clone())
            .collect();
        assert_eq!(ids.len(), 2);
        assert!(!log.is_quiet());
        let second = log
            .respond(&ids[1], PermissionResponse::Tool(ToolDecision::Allow))
            .unwrap();
        assert_eq!(
            second.actions[0],
            Action::Write(success(
                &ids[1],
                &json!({"behavior": "allow",
                "updatedInput": {"command": "touch p2.txt", "description": "Create empty file p2.txt"}})
            ))
        );
        assert_eq!(
            log.respond(&ids[1], PermissionResponse::Tool(ToolDecision::Allow))
                .unwrap_err(),
            RespondError::Gone
        );
        // A response of the wrong kind leaves the prompt pending.
        let wrong = PermissionResponse::Plan(PlanDecision::Approve { mode: None });
        assert_eq!(
            log.respond(&ids[0], wrong).unwrap_err(),
            RespondError::Mismatch
        );
        // A failed write puts it back.
        let first = log
            .respond(&ids[0], PermissionResponse::Tool(ToolDecision::Allow))
            .unwrap();
        assert!(log.pending().next().is_none());
        let events = log.restore(first.taken);
        assert!(
            matches!(&events[0], ChatEvent::PermissionAdded(prompt) if prompt.request_id == ids[0])
        );
        assert_eq!(log.expire_all().len(), 2, "removed + state");
        assert!(log.pending().next().is_none());
    }

    fn removals(run: &Run) -> Vec<(String, PermissionRemoved)> {
        run.events
            .iter()
            .filter_map(|event| match event {
                ChatEvent::PermissionRemoved { request_id, reason } => {
                    Some((request_id.clone(), *reason))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn cancel_removes_the_pending_prompt() {
        let run = run("07_cancel_pending_permission.jsonl");
        assert_eq!(
            removals(&run),
            [(
                "dd4cbd74-6b23-4307-9878-16c7d1b49663".to_owned(),
                PermissionRemoved::Cancelled
            )]
        );
        assert!(run.log.pending().next().is_none());
    }

    #[test]
    fn hook_decision_cancels_right_after_the_prompt() {
        let run = run("10_hooks.jsonl");
        let hooked = "89a53d0f-1289-4b1b-89e5-b8ea1c2cf285";
        let added = run.events.iter().position(
            |event| matches!(event, ChatEvent::PermissionAdded(prompt) if prompt.request_id == hooked),
        );
        let removed = run.events.iter().position(|event| {
            matches!(event, ChatEvent::PermissionRemoved { request_id, reason: PermissionRemoved::Cancelled } if request_id == hooked)
        });
        assert!(added.unwrap() < removed.unwrap());
        assert_eq!(removals(&run).len(), 2, "the other one was answered");
    }

    #[test]
    fn question_previews_are_kept_and_long_ones_cut_on_a_char_boundary() {
        // "é" is two bytes, so an odd cap would split one without the boundary check.
        let long = "é".repeat(MAX_CHAT_QUESTION_PREVIEW);
        let asked = question(&json!({"question": "Which?", "header": "H", "options": [
            {"label": "A", "description": "a", "preview": "+--+\n|A |\n+--+"},
            {"label": "B", "description": "b", "preview": long},
            {"label": "C", "description": "c"},
        ]}));
        assert_eq!(asked.options[0].preview, "+--+\n|A |\n+--+");
        let cut = &asked.options[1].preview;
        assert!(cut.len() <= MAX_CHAT_QUESTION_PREVIEW && cut.ends_with(MARKER));
        assert!(long.starts_with(cut.trim_end_matches(MARKER)));
        assert_eq!(asked.options[2].preview, "");
    }

    #[test]
    fn questions_whose_previews_overflow_the_total_keep_the_questions_without_previews() {
        // Each preview fits its own cap, but together they pass the questions' total.
        let options: Vec<Value> = (0..MAX_CHAT_QUESTION_ENTRIES)
            .map(|index| {
                json!({"label": format!("L{index}"), "description": "d",
                    "preview": "x".repeat(MAX_CHAT_QUESTION_PREVIEW)})
            })
            .collect();
        let mut log = ChatLog::default();
        let applied = log.apply_cli_line(&json!({"type": "control_request", "request_id": "q",
            "request": {"subtype": "can_use_tool", "tool_name": "AskUserQuestion", "tool_use_id": "t",
                "input": {"questions": [{"question": "Which?", "header": "H", "options": options}]}}}));
        assert!(
            applied.actions.is_empty(),
            "not denied: {:?}",
            applied.actions
        );
        let Some(PermissionKind::Questions { questions }) =
            log.pending().next().map(|prompt| &prompt.kind)
        else {
            panic!("the question was not kept");
        };
        assert_eq!(questions[0].options.len(), MAX_CHAT_QUESTION_ENTRIES);
        assert!(
            questions[0]
                .options
                .iter()
                .all(|option| option.preview.is_empty())
        );
        assert_eq!(questions[0].options[3].label, "L3");
    }

    #[test]
    fn questions_whose_previews_overflow_the_pending_budget_keep_the_questions() {
        let mut log = ChatLog {
            pending_bytes: MAX_CHAT_PENDING - 2_000,
            ..ChatLog::default()
        };
        let applied = log.apply_cli_line(&json!({"type": "control_request", "request_id": "q",
            "request": {"subtype": "can_use_tool", "tool_name": "AskUserQuestion", "tool_use_id": "t",
                "input": {"questions": [{"question": "Which?", "header": "H", "options": [
                    {"label": "A", "description": "d", "preview": "x".repeat(4_000)}]}]}}}));
        assert!(
            applied.actions.is_empty(),
            "not denied: {:?}",
            applied.actions
        );
        let Some(PermissionKind::Questions { questions }) =
            log.pending().next().map(|prompt| &prompt.kind)
        else {
            panic!("the question was not kept");
        };
        assert!(questions[0].options[0].preview.is_empty());
    }

    #[test]
    fn initialize_lists_typeable_commands_without_tui_only_ones() {
        let mut log = ChatLog::default();
        let wordy = "é".repeat(MAX_CHAT_COMMAND_DESCRIPTION);
        log.apply_cli_line(
            &json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "init", "response": {"models": [], "commands": [
                {"name": "compact", "description": "Free up context", "builtin": true},
                {"name": "codex:review", "description": wordy},
                {"name": "doctor", "description": "TUI only", "builtin": true},
                {"name": "claude.ai Robinhood:market-overview (MCP)", "description": "spaces"},
                {"description": "no name"},
            ]}}}),
        );
        let commands = &log.capabilities().commands;
        let names: Vec<_> = commands
            .iter()
            .map(|command| command.name.as_str())
            .collect();
        assert_eq!(names, ["compact", "codex:review"]);
        assert!(!commands[0].skill && commands[1].skill);
        assert!(commands[1].description.len() <= MAX_CHAT_COMMAND_DESCRIPTION);
        assert!(log.capabilities().validate().is_ok());
    }

    #[test]
    fn plan_and_question_answers() {
        let mut log = ChatLog::default();
        let request = |id: &str, tool: &str, input: Value| {
            json!({"type": "control_request", "request_id": id,
                "request": {"subtype": "can_use_tool", "tool_name": tool, "input": input, "tool_use_id": "t"}})
        };
        log.apply_cli_line(&request(
            "plan",
            "ExitPlanMode",
            json!({"plan": "# Plan", "planFilePath": "/p"}),
        ));
        let approved = log
            .respond(
                "plan",
                PermissionResponse::Plan(PlanDecision::Approve {
                    mode: Some(PermissionMode::AcceptEdits),
                }),
            )
            .unwrap();
        assert_eq!(
            approved.actions[1],
            Action::SetPermissionMode("acceptEdits")
        );

        log.apply_cli_line(&request("plan2", "ExitPlanMode", json!({"plan": "# Plan"})));
        let keep = log
            .respond(
                "plan2",
                PermissionResponse::Plan(PlanDecision::KeepPlanning {
                    feedback: "more".into(),
                }),
            )
            .unwrap();
        assert_eq!(
            keep.actions,
            [Action::Write(success("plan2", &deny("more", false)))]
        );

        let questions = json!({"questions": [{"question": "Which?", "header": "H", "multiSelect": true,
            "options": [{"label": "A", "description": ""}, {"label": "B", "description": ""}]}]});
        log.apply_cli_line(&request("q", "AskUserQuestion", questions.clone()));
        let answered = log
            .respond(
                "q",
                PermissionResponse::Questions(vec![ChatAnswer {
                    question: "Which?".into(),
                    answers: vec!["A".into(), "B".into()],
                }]),
            )
            .unwrap();
        let mut input = questions;
        input["answers"] = json!({"Which?": "A, B"});
        assert_eq!(
            answered.actions,
            [Action::Write(success(
                "q",
                &json!({"behavior": "allow", "updatedInput": input})
            ))]
        );

        log.apply_cli_line(&request("q2", "AskUserQuestion", json!({"questions": []})));
        let dismissed = log
            .respond(
                "q2",
                PermissionResponse::Dismiss {
                    message: "no".into(),
                },
            )
            .unwrap();
        assert_eq!(
            dismissed.actions,
            [Action::Write(success("q2", &deny("no", false)))]
        );
    }

    fn tool_request(id: &str, detail: usize) -> Value {
        json!({"type": "control_request", "request_id": id, "request": {"subtype": "can_use_tool",
            "tool_name": "Bash", "input": {"command": "x".repeat(detail)}, "tool_use_id": id,
            "permission_suggestions": [{"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "x"}],
                "behavior": "allow", "destination": "localSettings"}]}})
    }

    fn denied_with(applied: &Applied, id: &str, message: &str) -> bool {
        applied.actions == [Action::Write(success(id, &deny(message, false)))]
            && !applied
                .events
                .iter()
                .any(|event| matches!(event, ChatEvent::PermissionAdded(_)))
            && applied.events.iter().any(|event| {
                matches!(
                    event,
                    ChatEvent::Upsert(ChatItem {
                        kind: ChatItemKind::Notice { .. },
                        ..
                    })
                )
            })
    }

    #[test]
    fn approvals_no_subscriber_can_show_are_denied_but_kept_with_nobody_watching() {
        let plan = json!({"type": "control_request", "request_id": "p", "request": {"subtype": "can_use_tool",
            "tool_name": "ExitPlanMode", "input": {"plan": "# Plan"}}});
        let mut log = ChatLog::default();
        log.set_shown(Some(Vec::new()));
        assert!(denied_with(&log.apply_cli_line(&plan), "p", UNSHOWN));
        // Tool approvals need no feature.
        assert!(
            log.apply_cli_line(&tool_request("t", 10))
                .actions
                .is_empty()
        );

        log.set_shown(Some(vec![Feature::CHAT_PLAN]));
        assert!(log.apply_cli_line(&plan).actions.is_empty());

        let mut log = ChatLog::default();
        log.set_shown(None);
        assert!(log.apply_cli_line(&plan).actions.is_empty());
        assert_eq!(log.pending().count(), 1);
    }

    #[test]
    fn approvals_over_their_caps_are_denied_never_shortened() {
        let mut log = ChatLog::default();
        let plan = json!({"type": "control_request", "request_id": "big", "request": {"subtype": "can_use_tool",
            "tool_name": "ExitPlanMode", "input": {"plan": "p".repeat(MAX_CHAT_PLAN + 1)}}});
        let applied = log.apply_cli_line(&plan);
        check(&applied.events);
        assert!(denied_with(&applied, "big", TOO_LARGE));

        for index in 0..MAX_PENDING {
            let applied = log.apply_cli_line(&tool_request(&format!("r{index}"), 30_000));
            check(&applied.events);
            assert!(applied.actions.is_empty());
        }
        let applied = log.apply_cli_line(&tool_request("r32", 10));
        assert!(denied_with(&applied, "r32", TOO_MANY));
        assert_eq!(log.pending().count(), MAX_PENDING);

        // A snapshot keeps every pending approval and trims old items around them.
        for index in 0..20 {
            log.apply_cli_line(&json!({"type": "system", "subtype": "init"}));
            log.apply_cli_line(
                &json!({"type": "assistant", "message": {"id": format!("m{index}"),
                "content": [{"type": "text", "text": "é".repeat(MAX_CHAT_TEXT)}]}}),
            );
            log.apply_cli_line(
                &json!({"type": "result", "subtype": "success", "uuid": format!("end{index}")}),
            );
        }
        let snapshot = check_snapshot(&log, MAX_CHAT_SNAPSHOT_BYTES);
        assert_eq!(snapshot.pending.len(), MAX_PENDING);
        assert!(snapshot.has_earlier);
        assert!(!snapshot.items.is_empty());
        assert_eq!(snapshot.items.last().unwrap().id, "end19");
    }

    #[test]
    fn total_pending_bytes_are_capped() {
        let mut log = ChatLog::default();
        // Each prompt is just under its 64 KiB cap, so the 1 MiB total runs out before 32.
        let mut refused = None;
        for index in 0..MAX_PENDING {
            let applied = log.apply_cli_line(&tool_request(&format!("r{index}"), 65_000));
            if !applied.actions.is_empty() {
                refused = Some((index, applied));
                break;
            }
        }
        let (index, applied) = refused.unwrap();
        assert_eq!(index, MAX_CHAT_PENDING / 65_000);
        assert!(denied_with(&applied, &format!("r{index}"), TOO_MANY));
    }

    #[test]
    fn oversized_suggestion_hides_always_allow() {
        let mut log = ChatLog::default();
        let mut request = tool_request("r", 10);
        request["request"]["permission_suggestions"][0]["rules"][0]["ruleContent"] =
            json!("x".repeat(MAX_SUGGESTION));
        log.apply_cli_line(&request);
        let PermissionKind::Tool { always, .. } = &log.pending().next().unwrap().kind else {
            panic!()
        };
        assert!(always.is_none());
        assert_eq!(
            log.respond("r", PermissionResponse::Tool(ToolDecision::AllowAlways))
                .unwrap_err(),
            RespondError::Mismatch
        );
    }

    #[test]
    fn unsupported_control_request_gets_the_sdk_error_envelope() {
        let mut log = ChatLog::default();
        let applied = log.apply_cli_line(&json!({"type": "control_request", "request_id": "r1",
            "request": {"subtype": "elicitation"}}));
        check(&applied.events);
        assert_eq!(
            applied.actions,
            [Action::Write(
                json!({"type": "control_response", "response": {"subtype": "error",
                "request_id": "r1", "error": "Unsupported control request subtype: elicitation"}})
            )]
        );
        assert!(log.is_quiet(), "answered, so nothing is left unresolved");
    }

    #[test]
    fn unknown_lifecycle_stays_uncertain() {
        let mut log = ChatLog::default();
        log.apply_cli_line(&json!({"type": "system", "subtype": "status"}));
        log.apply_cli_line(&json!({"type": "rate_limit_event"}));
        assert!(log.is_quiet());
        log.apply_cli_line(&json!({"type": "system", "subtype": "mystery_started"}));
        assert!(!log.is_quiet());
        log.apply_cli_line(&json!({"type": "system", "subtype": "init"}));
        log.apply_cli_line(&json!({"type": "result", "subtype": "success"}));
        assert!(
            !log.is_quiet(),
            "only the process ending resolves an unknown event"
        );
        assert!(log.uncertain().unwrap().contains("system/mystery_started"));
        let mut log = ChatLog::default();
        log.apply_cli_line(&json!({"type": "keep_alive_v9"}));
        assert!(!log.is_quiet());
    }

    #[test]
    fn a_turns_end_lists_the_files_its_writes_and_edits_changed() {
        // 08b: Claude writes its plan file and then hello.txt ("hi"), each a `create`.
        let run = run("08b_exit_plan_mode.jsonl");
        let changed: Vec<Vec<FileChange>> = run
            .events
            .iter()
            .filter_map(|event| match event {
                ChatEvent::Upsert(ChatItem {
                    kind: ChatItemKind::TurnEnd { changed, .. },
                    ..
                }) => Some(changed.clone()),
                _ => None,
            })
            .collect();
        let all: Vec<&FileChange> = changed.iter().flatten().collect();
        assert!(
            all.iter()
                .any(|change| change.path == "/tmp/m0/repo/hello.txt"
                    && (change.added, change.removed) == (1, 0)),
            "{changed:?}"
        );

        // Edits to one file add up; hunk lines count by their +/- prefix.
        let mut log = ChatLog::default();
        let edit = |lines: Value| {
            json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t",
                "content": "ok"}]}, "tool_use_result": {"type": "update", "filePath": "/r/a.rs",
                "structuredPatch": [{"oldStart": 1, "oldLines": 2, "newStart": 1, "newLines": 3,
                "lines": lines}]}})
        };
        log.apply_cli_line(&edit(json!([" keep", "-old", "+new", "+more"])));
        log.apply_cli_line(&edit(json!(["-gone"])));
        log.apply_cli_line(&json!({"type": "user", "message": {"content": [{"type": "tool_result",
            "tool_use_id": "e", "content": "failed", "is_error": true}]}, "tool_use_result": "Error"}));
        let applied = log.apply_cli_line(&json!({"type": "result", "subtype": "success",
            "uuid": "end", "duration_ms": 5}));
        let end = applied.events.iter().find_map(|event| match event {
            ChatEvent::Upsert(ChatItem {
                kind: ChatItemKind::TurnEnd { changed, .. },
                ..
            }) => Some(changed.clone()),
            _ => None,
        });
        assert_eq!(
            end,
            Some(vec![FileChange {
                path: "/r/a.rs".into(),
                added: 2,
                removed: 2
            }])
        );
    }

    #[test]
    fn background_task_after_result_starts_an_autonomous_turn() {
        let done = run("15_background.jsonl");
        // After the first result: idle in the foreground, but the task keeps it from quiet.
        assert!(done.quiet.contains(&(false, false)));
        assert_eq!(done.quiet.last(), Some(&(true, false)));
        let snapshot = check_snapshot(&done.log, MAX_CHAT_SNAPSHOT_BYTES);
        let ends: Vec<&ChatItem> = snapshot
            .items
            .iter()
            .filter(|item| matches!(item.kind, ChatItemKind::TurnEnd { .. }))
            .collect();
        assert_eq!(ends.len(), 2);
        assert_ne!(ends[0].turn_id, ends[1].turn_id);

        let stopped = run("15b_background_stopped.jsonl");
        assert!(stopped.quiet.contains(&(false, false)));
        assert!(stopped.log.is_quiet(), "killed/stopped resolves the task");
    }

    #[test]
    fn state_follows_init_initialize_and_result() {
        let run = run("01_startup_text.jsonl");
        let state = run.log.state();
        assert_eq!(state.model.as_deref(), Some("claude-haiku-5-5"));
        assert_eq!(state.permission_mode, PermissionMode::Default);
        assert_eq!(state.context_percent, Some(2));
        assert!(!state.busy && state.quiescent);
        let models = &run.log.capabilities().models;
        assert!(!models.is_empty() && models.len() <= MAX_CHAT_MODELS);
        assert_eq!(models[1].alias, "opus");
        assert_eq!(models[1].label, "Opus 5.5");
        assert!(models[1].efforts.contains(&"max".to_owned()));
        assert!(
            run.events
                .iter()
                .any(|event| matches!(event, ChatEvent::State(ChatState { busy: true, .. })))
        );
    }

    #[test]
    fn active_turn_folds_its_middle() {
        let mut log = ChatLog::default();
        log.apply_cli_line(&json!({"type": "system", "subtype": "init"}));
        for index in 0..100 {
            log.apply_cli_line(
                &json!({"type": "assistant", "message": {"id": format!("m{index}"),
                "content": [{"type": "text", "text": "hi"}]}}),
            );
        }
        let items = check_snapshot(&log, MAX_CHAT_SNAPSHOT_BYTES).items;
        assert_eq!(items.len(), KEEP_HEAD + 1 + KEEP_TAIL);
        assert_eq!(items[KEEP_HEAD - 1].id, "m15:0");
        assert_eq!(items[KEEP_HEAD].kind, ChatItemKind::Folded { count: 20 });
        assert_eq!(items[KEEP_HEAD + 1].id, "m36:0");
        assert!(items.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    }

    #[test]
    fn completed_turns_are_capped_by_count_and_bytes() {
        let mut log = ChatLog::default();
        for index in 0..=MAX_TURNS {
            log.apply_cli_line(&json!({"type": "system", "subtype": "init"}));
            log.apply_cli_line(
                &json!({"type": "result", "subtype": "success", "uuid": format!("e{index}")}),
            );
        }
        let snapshot = check_snapshot(&log, MAX_CHAT_SNAPSHOT_BYTES);
        assert_eq!(snapshot.items.len(), MAX_TURNS);
        assert_eq!(snapshot.items[0].id, "e1");
        assert!(snapshot.has_earlier);

        let mut log = ChatLog::default();
        for index in 0..70 {
            log.apply_cli_line(&json!({"type": "system", "subtype": "init"}));
            log.apply_cli_line(
                &json!({"type": "assistant", "message": {"id": format!("m{index}"),
                "content": [{"type": "text", "text": "x".repeat(MAX_CHAT_TEXT)}]}}),
            );
            log.apply_cli_line(&json!({"type": "result", "subtype": "success"}));
        }
        let bytes: usize = log.turns.iter().map(|turn| turn.bytes).sum();
        assert!(bytes <= MAX_TURNS_BYTES);
        assert!(log.turns.len() < 70 && log.has_earlier);
    }

    #[test]
    fn deltas_stop_at_the_text_cap() {
        let mut log = ChatLog::default();
        let delta = |text: String| {
            json!({"type": "stream_event", "api_message_id": "m", "parent_tool_use_id": null,
            "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}})
        };
        let mut streamed = 0;
        for _ in 0..40 {
            for event in log.apply_cli_line(&delta("é".repeat(5000))).events {
                match event {
                    ChatEvent::Delta { text, .. } => streamed += text.len(),
                    ChatEvent::Upsert(ChatItem {
                        kind: ChatItemKind::Assistant { markdown, .. },
                        ..
                    }) => {
                        streamed += markdown.len();
                    }
                    _ => {}
                }
            }
        }
        assert!(streamed <= MAX_CHAT_TEXT);
        check_snapshot(&log, MAX_CHAT_SNAPSHOT_BYTES);
    }

    #[test]
    fn transcript_prompts_and_tool_results_in_jsonl_shapes() {
        let mut log = ChatLog::default();
        log.seed(&[
            json!({"type": "user", "uuid": "u1", "message": {"content": "first"}}),
            json!({"type": "assistant", "message": {"id": "m", "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "/a"}}]}}),
            json!({"type": "user", "uuid": "r1", "message": {"content": [{"type": "tool_result",
                "tool_use_id": "toolu_1", "content": [{"type": "text", "text": "line"}, {"type": "text", "text": "two"}]}]}}),
            json!({"type": "user", "uuid": "meta", "isMeta": true, "message": {"content": "caveat"}}),
            json!({"type": "user", "uuid": "u2", "message": {"content": "second"}}),
        ]);
        let items = check_snapshot(&log, MAX_CHAT_SNAPSHOT_BYTES).items;
        let ids: Vec<&str> = items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["u1", "toolu_1", "u2"]);
        assert_eq!(
            items[0].kind,
            ChatItemKind::User {
                text: "first".into()
            }
        );
        assert!(
            matches!(&items[1].kind, ChatItemKind::Tool { result: Some(result), .. } if result == "line\ntwo")
        );
        assert_ne!(
            items[0].turn_id, items[2].turn_id,
            "each prompt starts a turn"
        );
    }

    #[test]
    fn exit_expires_approvals_and_settles() {
        let mut log = ChatLog::default();
        log.apply_cli_line(&json!({"type": "system", "subtype": "init"}));
        log.apply_cli_line(&json!({"type": "system", "subtype": "mystery"}));
        log.apply_cli_line(&tool_request("r", 10));
        let events = log.exited(&"e".repeat(MAX_CHAT_PREVIEW * 2));
        check(&events);
        assert!(matches!(
            events[0],
            ChatEvent::PermissionRemoved {
                reason: PermissionRemoved::Expired,
                ..
            }
        ));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ChatEvent::Exited { .. }))
        );
        assert!(log.is_quiet() && !log.state().busy);
        assert_eq!(
            log.revision(),
            log.snapshot(session(), 1, 1, MAX_CHAT_SNAPSHOT_BYTES)
                .to_rev
        );
    }

    #[test]
    fn subagent_steps_count_on_the_parent_tool() {
        let mut log = ChatLog::default();
        log.apply_cli_line(&json!({"type": "assistant", "message": {"id": "m", "content": [
            {"type": "tool_use", "id": "toolu_task", "name": "Task", "input": {"description": "look"}}]}}));
        for _ in 0..2 {
            log.apply_cli_line(
                &json!({"type": "assistant", "parent_tool_use_id": "toolu_task",
                "message": {"id": "sub", "content": [{"type": "text", "text": "inner"}]}}),
            );
        }
        let items = check_snapshot(&log, MAX_CHAT_SNAPSHOT_BYTES).items;
        assert_eq!(items.len(), 1);
        assert!(matches!(
            items[0].kind,
            ChatItemKind::Tool {
                subagent_count: 2,
                ..
            }
        ));
    }
}
