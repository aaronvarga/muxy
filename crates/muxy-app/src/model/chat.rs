//! Fork: sessions whose Claude Code runs as chat on the server.
//!
//! One [`ChatSession`] per `(server, session)` follows the baseline rules in
//! `muxy_protocol::chat`. The server keeps chat subscriptions per connection,
//! and `Unsubscribe` drops every pane on it, so the app subscribes once per
//! session however many panes show it and unsubscribes when the last one goes.

use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext, Context, Entity, SharedString, Task};
use muxy_app_core::opener::{FileLocation, OpenContext, Target};
use muxy_app_core::{PaneId, ServerId};
use muxy_protocol::{
    AgentProvider, AgentState, ChannelId, ChatBatch, ChatCapabilities, ChatEvent, ChatImage,
    ChatItem, ChatItemKind, ChatMode, ChatProvider, ChatRefusal, ChatReply, ChatRequest,
    ChatSelection, ChatSnapshot, ChatState, ChatTarget, ChatThread, Feature, OperationId,
    PermissionPrompt, PermissionRemoved, PermissionResponse, SessionId, SubmissionOutcome,
};
use muxy_ui::tr;

use super::AppModel;
use crate::boot::Work;
use crate::views::chat::ChatView;
use crate::views::chat::markdown::{self, Document};

pub(crate) type CardCatalogs = Vec<(ChatProvider, Result<Vec<muxy_protocol::ChatModel>, String>)>;

pub(crate) type ChatKey = (ServerId, SessionId);

/// Live items the app keeps between snapshots; past either bound it subscribes
/// again and takes the server's trimmed snapshot instead.
// shortcut: these don't match the server's retention (200 turns, 16 MiB); align them if a long
// chat resubscribes often or the server trims what the app still shows.
const MAX_ROWS: usize = muxy_protocol::MAX_CHAT_SNAPSHOT_ITEMS;
const MAX_BYTES: usize = 24 * 1024 * 1024;
/// Failed `Subscribe`s retried after 1, 2 and 4 seconds before the app gives up.
const SUBSCRIBE_RETRIES: u8 = 3;
/// How long "Resume in Chat" waits for Claude Code to start in its terminal.
const ENTER_WHEN_READY: Duration = Duration::from_secs(60);
/// The id of the row this app adds when Claude Code exits while chat shows.
pub(crate) const EXITED_ROW: &str = "muxy:exited";
/// An approval shows only once it has waited this long: a hook can answer it
/// about 13 ms after it arrives, and a card mustn't flash.
pub(crate) const SHOW_APPROVAL_AFTER: Duration = Duration::from_millis(200);
/// How long a withdrawn approval stays, fading, to say Claude withdrew it.
pub(crate) const WITHDRAWN_FOR: Duration = Duration::from_secs(2);
/// How long "New Claude chat" waits for its terminal's shell to be idle.
const START_WHEN_IDLE: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct ChatRuntime {
    image_preview: Option<Task<()>>,
    pub(crate) card_composers:
        HashMap<muxy_app_core::ProjectId, Entity<crate::views::workspace::new_session::NewSession>>,
    pub(crate) card_selections: HashMap<PaneId, ChatSelection>,
    pub(crate) card_catalogs: Vec<(ServerId, ChatProvider, Vec<muxy_protocol::ChatModel>)>,
    pub(crate) card_probes: HashSet<(ServerId, muxy_app_core::ProjectId)>,
    pub(crate) sessions: HashMap<ChatKey, ChatSession>,
    /// The chat view of each pane that shows one.
    pub(crate) views: HashMap<PaneId, Entity<ChatView>>,
    next_token: u32,
    /// Panes from "Resume in Chat" that switch once Claude Code shows up there,
    /// each with the task that forgets it after [`ENTER_WHEN_READY`].
    pub(crate) enter_when_ready: HashMap<PaneId, Task<()>>,
    /// Panes from "New Claude chat" that start chat once their shell is idle,
    /// each with the task that checks.
    start_when_idle: HashMap<PaneId, Task<()>>,
    prompt: Option<super::dialogs::DialogId>,
    next_draft: u64,
    next_submission: u64,
    pub(super) resume_when_idle: HashMap<PaneId, ChatTarget>,
}

#[derive(Clone)]
pub(crate) struct LocalDraft {
    pub(crate) selection: ChatSelection,
    pub(crate) generation: u64,
    pub(crate) host_epoch: Option<u64>,
    pub(crate) discovering: bool,
    pub(crate) visible: bool,
    pub(crate) choose_thread: bool,
    pub(crate) threads: Vec<ChatThread>,
}

pub(super) struct ChatSend {
    pub(super) key: ChatKey,
    pub(super) id: u64,
    generation: u64,
    local: Option<LocalDraft>,
}

/// One conversation entry. Streamed text grows in `text`; drawing shares a copy
/// made once per change, so each delta costs only its own length.
pub(crate) struct Row {
    /// The item without its text, which moved to `text`.
    pub(crate) item: ChatItem,
    /// The item's id, shared, for element ids drawn each frame.
    pub(crate) key: SharedString,
    /// A tool's pill: its command, path or pattern; other rows' own text.
    pub(crate) text: String,
    /// A tool's short description, such as "Read" or a command's purpose.
    pub(crate) verb: SharedString,
    shared: OnceCell<SharedString>,
    /// A thinking row's first line, shown while it's closed.
    preview: OnceCell<SharedString>,
    /// The parsed markdown, made when the row is first drawn after its text or
    /// open stretches change.
    // Bounded like `text` by MAX_BYTES: a document keeps at most
    // `markdown::MAX_TABLE_CELLS` table cells, each padded to at most `MAX_CELL` chars.
    document: markdown::Parsed,
    /// A prompt's mentions and attachments, split once per change.
    prompt: OnceCell<Option<Arc<crate::views::chat::rows::files::Prompt>>>,
    /// A Read, Edit or Write step's whole path; the pill shows its end.
    pub(crate) path: Option<String>,
}

/// A path chat names that exists on this Mac.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ChatFile {
    pub(crate) location: FileLocation,
    /// Opening it could run something, so a click shows it in Finder instead:
    /// chat text is model output and may be steered by what Claude read.
    pub(crate) reveal: bool,
}

/// What drawing knows about a path candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Lookup {
    /// Asked; the worker hasn't answered yet.
    Pending,
    /// Answered: the file, or `None` when it names nothing here.
    Found(Option<Arc<ChatFile>>),
}

/// Answers a session remembers; past it the cache starts over.
const MAX_FILES: usize = 4096;

/// Path candidates a session's rows name, looked up on the opener worker so
/// drawing never touches the disk (a hung network mount would freeze it).
#[derive(Default)]
pub(crate) struct Files {
    /// The link context the answers are for; another directory starts over.
    // shortcut: one directory per session, as every pane of a session shares its
    // terminal's cwd; key by directory if panes of one session ever differ.
    context: Option<OpenContext>,
    found: HashMap<String, Lookup>,
    /// Asked while drawing and not yet sent to the worker.
    wanted: Vec<String>,
    /// `wanted` gained a candidate since the last flush was scheduled.
    schedule: bool,
    /// Bumped when answers naming a file land, so parses that waited redo.
    pub(crate) epoch: u64,
}

impl Files {
    /// What is known of `text` in `context`, queueing it when nothing is. Pure
    /// map work: remote and context-less rows never resolve.
    pub(crate) fn lookup(&mut self, text: &str, context: Option<&OpenContext>) -> Lookup {
        let Some(context) = context.filter(|context| context.server.is_local()) else {
            return Lookup::Found(None);
        };
        if self
            .context
            .as_ref()
            .is_none_or(|known| known.directory != context.directory)
        {
            self.context = Some(context.clone());
            self.found.clear();
            self.wanted.clear();
        }
        if let Some(lookup) = self.found.get(text) {
            return lookup.clone();
        }
        if self.found.len() >= MAX_FILES {
            self.found.clear();
        }
        self.found.insert(text.to_owned(), Lookup::Pending);
        self.wanted.push(text.to_owned());
        self.schedule = true;
        Lookup::Pending
    }

    /// True once per batch of new candidates: the caller schedules the flush.
    pub(crate) fn take_schedule(&mut self) -> bool {
        std::mem::take(&mut self.schedule)
    }

    /// Records the worker's answers; true when one names a file, so rows redraw.
    fn land(&mut self, directory: &Path, answers: Vec<(String, Option<Arc<ChatFile>>)>) -> bool {
        if self
            .context
            .as_ref()
            .is_none_or(|context| context.directory != directory)
        {
            return false;
        }
        let found = answers.iter().any(|(_, file)| file.is_some());
        for (text, file) in answers {
            self.found.insert(text, Lookup::Found(file));
        }
        if found {
            self.epoch += 1;
        }
        found
    }

    /// The worker couldn't take them; the next draw asks again.
    fn forget(&mut self, texts: &[String]) {
        for text in texts {
            if self.found.get(text) == Some(&Lookup::Pending) {
                self.found.remove(text);
            }
        }
    }
}

/// Runs on the opener worker: the file `text` names in `context`, if any, and
/// whether opening it could run something.
fn resolve_file(text: &str, context: &OpenContext) -> Option<Arc<ChatFile>> {
    let Some(Target::File(location)) = crate::opener::resolve(text, context) else {
        return None;
    };
    let reveal = runnable(&location.path);
    Some(Arc::new(ChatFile { location, reveal }))
}

/// Opening `path` could launch something: a folder or bundle, an executable,
/// or a type macOS runs or installs, by its name or a symlink's target's.
/// Touches the disk, so only on a worker.
pub(crate) fn runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    const RUNS: &[&str] = &[
        "app",
        "command",
        "tool",
        "terminal",
        "workflow",
        "pkg",
        "mpkg",
        "dmg",
        "jar",
        "prefpane",
        "action",
        "scpt",
        "applescript",
        "shortcut",
        "fileloc",
        "webloc",
        "inetloc",
    ];
    let named = |path: &Path| {
        path.extension()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|ext| RUNS.iter().any(|runs| runs.eq_ignore_ascii_case(ext)))
    };
    named(path)
        || std::fs::canonicalize(path).is_ok_and(|real| named(&real))
        // Unreadable counts as runnable: revealing is the safe side.
        || path.metadata().map_or(true, |meta| {
            meta.is_dir() || meta.permissions().mode() & 0o111 != 0
        })
}

impl Row {
    fn bytes(&self) -> usize {
        self.text.len() + self.tool_details(true).map_or(0, str::len)
    }

    /// Failed tools show their first error line until expanded; reports stay folded.
    pub(crate) fn tool_details(&self, expanded: bool) -> Option<&str> {
        let ChatItemKind::Tool {
            result: Some(result),
            is_error,
            ..
        } = &self.item.kind
        else {
            return None;
        };
        if expanded {
            (!result.is_empty()).then_some(result.as_str())
        } else if *is_error {
            result.lines().find(|line| !line.trim().is_empty())
        } else {
            None
        }
    }

    /// The text to draw.
    pub(crate) fn shared(&self) -> SharedString {
        self.shared.get_or_init(|| self.text.clone().into()).clone()
    }

    /// The first non-empty line.
    pub(crate) fn preview(&self) -> SharedString {
        self.preview
            .get_or_init(|| {
                let line = self.text.lines().find(|line| !line.trim().is_empty());
                line.unwrap_or_default().to_owned().into()
            })
            .clone()
    }

    /// The text as markdown with the `open` stretches expanded, parsed once
    /// per change and again when lookups it waited on land; code naming a
    /// file `files` found in `context` can be clicked.
    pub(crate) fn document(
        &self,
        open: Option<&markdown::Open>,
        files: &RefCell<Files>,
        context: Option<&OpenContext>,
    ) -> Arc<Document> {
        let epoch = files.borrow().epoch;
        self.document.get(&self.text, open, epoch, &|text| {
            files.borrow_mut().lookup(text, context)
        })
    }

    /// A prompt's text split into mentions and attachments, or `None` when it has neither.
    pub(crate) fn prompt(&self) -> Option<Arc<crate::views::chat::rows::files::Prompt>> {
        self.prompt
            .get_or_init(|| crate::views::chat::rows::files::prompt(&self.text).map(Arc::new))
            .clone()
    }

    fn changed(&mut self) {
        self.shared.take();
        self.preview.take();
        self.prompt.take();
        self.document = markdown::Parsed::default();
    }
}

/// What a tool does, for its icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolKind {
    Read,
    Search,
    Run,
    Edit,
    Web,
    Agent,
    Other,
}

pub(crate) fn tool_kind(name: &str) -> ToolKind {
    match name {
        "Read" | "NotebookRead" | "LS" => ToolKind::Read,
        "Grep" | "Glob" | "ToolSearch" => ToolKind::Search,
        "Bash" | "BashOutput" | "KillShell" | "KillBash" => ToolKind::Run,
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" | "TodoWrite" => ToolKind::Edit,
        "WebFetch" | "WebSearch" => ToolKind::Web,
        "Task" | "Agent" => ToolKind::Agent,
        _ => ToolKind::Other,
    }
}

/// What a turn's summary row shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Turn {
    /// The first row the summary stands in for; it draws the summary.
    pub(crate) head: Option<String>,
    pub(crate) tools: usize,
    pub(crate) messages: usize,
    /// The turn ended, so its steps collapse unless opened.
    pub(crate) done: bool,
    /// It has a final text, which its footer copies.
    pub(crate) has_final: bool,
    /// Up to four tool kinds it used, in first-use order.
    pub(crate) kinds: Vec<ToolKind>,
    /// "17 tool calls, 3 messages", made once per change.
    pub(crate) summary: SharedString,
}

/// Rows a finished turn hides behind its summary: all but the prompt, the
/// final text, failed tools, notices and the footer.
pub(crate) fn collapsible(kind: &ChatItemKind) -> bool {
    matches!(
        kind,
        ChatItemKind::Thinking { .. }
            | ChatItemKind::Tool {
                is_error: false,
                ..
            }
            | ChatItemKind::Folded { .. }
            | ChatItemKind::Assistant {
                final_in_turn: false,
                ..
            }
    )
}

fn tally(rows: &[Row]) -> Turn {
    let mut turn = Turn::default();
    for row in rows {
        match &row.item.kind {
            ChatItemKind::Tool { name, .. } => {
                turn.tools += 1;
                let kind = tool_kind(name);
                if turn.kinds.len() < 4 && !turn.kinds.contains(&kind) {
                    turn.kinds.push(kind);
                }
            }
            // Mostly tool calls; the server drops them from snapshots, so nothing counts twice.
            ChatItemKind::Folded { count } => {
                turn.tools += usize::try_from(*count).unwrap_or(usize::MAX);
            }
            ChatItemKind::Assistant {
                final_in_turn: false,
                ..
            } => turn.messages += 1,
            ChatItemKind::Assistant {
                final_in_turn: true,
                ..
            } => (turn.done, turn.has_final) = (true, true),
            ChatItemKind::TurnEnd { .. } => turn.done = true,
            _ => {}
        }
        if turn.head.is_none() && collapsible(&row.item.kind) {
            turn.head = Some(row.item.id.clone());
        }
    }
    turn.summary = crate::views::chat::rows::summary_text(&turn).into();
    turn
}

#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent flags from the server and this app"
)]
pub(crate) struct ChatSession {
    pub(crate) local: Option<LocalDraft>,
    pub(crate) operation: Option<OperationId>,
    pub(crate) preparing: Option<u64>,
    pub(crate) mode: ChatMode,
    pub(crate) generation: u64,
    /// The token of the latest `Subscribe`, recorded before sending: the
    /// snapshot can arrive before the reply.
    token: Option<u32>,
    /// The snapshot for `token` arrived, so ordinary batches apply.
    synced: bool,
    revision: u64,
    pub(crate) rows: Vec<Row>,
    index: HashMap<String, usize>,
    bytes: usize,
    pub(crate) state: ChatState,
    pub(crate) capabilities: ChatCapabilities,
    pub(crate) pending: Vec<PermissionPrompt>,
    pub(crate) has_earlier: bool,
    pub(crate) blocked: Option<String>,
    /// Approvals answered here and not yet removed by the server.
    pub(crate) answering: HashSet<String>,
    /// This app's Enter or Leave is running.
    pub(crate) switching: bool,
    /// `Subscribe`s for the current token's chat that failed in a row.
    failures: u8,
    /// Claude Code exited on its own, so the notices that follow outlive the chat.
    exited: bool,
    /// Each turn's summary, by turn id.
    pub(crate) turns: HashMap<String, Turn>,
    /// Finished turns and thinking rows the user opened, by id.
    pub(crate) expanded: HashSet<String>,
    /// Expanded `{{short|long}}` stretches' ids, by the row's item id.
    pub(crate) stretched: HashMap<SharedString, markdown::Open>,
    /// The rows the list shows, by row index, in order: a closed finished
    /// turn's steps are left out, so the list never lays out empty rows.
    pub(crate) visible: Vec<usize>,
    /// Counts prompt rows added or replaced, so the timeline knows to redo its ticks.
    pub(crate) prompts: u64,
    /// When the running turn started here, for its timer.
    pub(crate) busy_since: Option<Instant>,
    /// This app asked to start a new chat here and it hasn't shown yet.
    pub(crate) starting: bool,
    /// When each pending approval arrived, so a card shows only after [`SHOW_APPROVAL_AFTER`].
    pub(crate) arrived: HashMap<String, Instant>,
    /// The approval Claude withdrew after it showed, and when.
    pub(crate) withdrawn: Option<(PermissionPrompt, Instant)>,
    /// The model alias chosen here; the state names the resolved model instead.
    pub(crate) chosen_model: Option<String>,
    /// Why the last model or effort change was refused, shown in the picker.
    pub(crate) picker_error: Option<String>,
    /// The paths the rows name, looked up off the UI thread.
    pub(crate) files: RefCell<Files>,
}

/// How a pane's list changed, in visible rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Splice {
    Reset(usize),
    Replace(usize),
    Insert(usize),
    /// The visible rows in the range became this many.
    Range(Range<usize>, usize),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Applied {
    pub(crate) splices: Vec<Splice>,
    /// State, approvals or the blocked reason changed.
    pub(crate) changed: bool,
    pub(crate) resubscribe: bool,
    pub(crate) toasts: Vec<String>,
}

impl ChatSession {
    pub(crate) fn provider(&self) -> ChatProvider {
        self.local.as_ref().map_or_else(
            || self.state.provider.unwrap_or_default(),
            |draft| draft.selection.provider,
        )
    }
    /// The pane draws chat: in Chat or Blocked, or leaving them.
    pub(crate) fn showing(&self) -> bool {
        if self.local.as_ref().is_some_and(|draft| draft.visible) {
            return true;
        }
        match self.mode {
            ChatMode::Chat | ChatMode::Blocked => true,
            ChatMode::Transitioning => self.synced || self.starting,
            ChatMode::Console => self.starting,
            ChatMode::Unrecognized(_) => false,
        }
    }

    /// The file `text` names, once the worker has said so; drawing queues
    /// what it hasn't asked yet and shows it plain meanwhile.
    pub(crate) fn file(&self, text: &str, context: Option<&OpenContext>) -> Option<Arc<ChatFile>> {
        match self.files.borrow_mut().lookup(text, context) {
            Lookup::Found(file) => file,
            Lookup::Pending => None,
        }
    }

    /// The first approval that has waited long enough to show.
    pub(crate) fn shown_approval(&self, now: Instant) -> Option<&PermissionPrompt> {
        self.pending.iter().find(|prompt| {
            self.arrived
                .get(&prompt.request_id)
                .is_none_or(|at| now.duration_since(*at) >= SHOW_APPROVAL_AFTER)
        })
    }

    pub(crate) fn wants_subscription(&self) -> bool {
        matches!(self.mode, ChatMode::Chat | ChatMode::Blocked) && self.token.is_none()
    }

    pub(crate) fn subscribe(&mut self, token: u32) {
        self.token = Some(token);
        self.synced = false;
    }

    pub(crate) fn set_mode(&mut self, mode: ChatMode, generation: u64) {
        if mode == ChatMode::Console && self.local.is_some() {
            self.mode = mode;
            self.generation = generation;
            self.switching = false;
            return;
        }
        if mode == ChatMode::Console {
            // An Enter's reply can come after this report; the switch stays off until then.
            *self = Self {
                exited: self.exited,
                operation: self.operation,
                preparing: self.preparing,
                switching: self.switching,
                starting: self.starting,
                ..Self::default()
            };
            return;
        }
        if generation > self.generation {
            self.clear(generation);
        }
        self.mode = mode;
        if matches!(mode, ChatMode::Chat | ChatMode::Blocked) {
            self.starting = false;
            if let Some(draft) = self.local.take() {
                self.state.provider = Some(draft.selection.provider);
            }
        }
    }

    fn clear(&mut self, generation: u64) {
        *self = Self {
            local: self.local.take(),
            operation: self.operation,
            preparing: self.preparing,
            mode: self.mode,
            generation,
            switching: self.switching,
            starting: self.starting,
            ..Self::default()
        };
    }

    pub(crate) fn apply(&mut self, batch: ChatBatch) -> Applied {
        let mut out = Applied::default();
        if self.mode == ChatMode::Console {
            // The server's last notices, such as the command to resume by hand. The exit
            // can arrive after the Console report: batches wait out a pending attach.
            for event in batch.events {
                match event {
                    ChatEvent::Exited { reason } => {
                        self.exited = true;
                        out.toasts.push(reason);
                    }
                    ChatEvent::Upsert(ChatItem {
                        kind: ChatItemKind::Notice { text },
                        ..
                    }) if self.exited => out.toasts.push(text),
                    _ => {}
                }
            }
            return out;
        }
        if batch.generation != self.generation {
            if batch.generation > self.generation {
                self.clear(batch.generation);
                out.splices.push(Splice::Reset(0));
                out.changed = true;
                out.resubscribe = true;
            }
            return out;
        }
        if let Some(snapshot) = batch.snapshot {
            if self.token == Some(snapshot.subscription) {
                self.replace(*snapshot);
                self.revision = batch.to_rev;
                self.synced = true;
                out.splices.push(Splice::Reset(self.visible.len()));
                out.changed = true;
            }
            return out;
        }
        if !self.synced {
            return out;
        }
        if batch.from_rev != self.revision {
            self.synced = false;
            out.resubscribe = true;
            return out;
        }
        self.revision = batch.to_rev;
        for event in batch.events {
            self.event(event, &mut out);
        }
        if self.rows.len() > MAX_ROWS || self.bytes > MAX_BYTES {
            out.resubscribe = true;
        }
        if out.resubscribe {
            self.synced = false;
        }
        out
    }

    /// The row hides behind its finished turn's summary.
    pub(crate) fn hidden(&self, row: &Row) -> bool {
        collapsible(&row.item.kind)
            && self
                .turns
                .get(&row.item.turn_id)
                .is_some_and(|turn| turn.done)
            && !self.expanded.contains(&row.item.turn_id)
    }

    /// The list shows row `at`: it isn't hidden, or it draws its turn's summary.
    fn shown(&self, at: usize) -> bool {
        let row = &self.rows[at];
        !self.hidden(row)
            || self
                .turns
                .get(&row.item.turn_id)
                .is_some_and(|turn| turn.head.as_ref() == Some(&row.item.id))
    }

    /// Re-derives which rows of `range` show and splices the list to match.
    fn reshow(&mut self, range: Range<usize>, out: &mut Applied) {
        let start = self.visible.partition_point(|&at| at < range.start);
        let end = self.visible.partition_point(|&at| at < range.end);
        let shown: Vec<usize> = range.filter(|&at| self.shown(at)).collect();
        out.splices.push(Splice::Range(start..end, shown.len()));
        self.visible.splice(start..end, shown);
    }

    /// Row `at` changed: redraws it, or adds or drops it when it starts or stops showing.
    fn redraw(&mut self, at: usize, out: &mut Applied) {
        match (self.visible.binary_search(&at), self.shown(at)) {
            (Ok(on), true) => out.splices.push(Splice::Replace(on)),
            (Ok(on), false) => {
                self.visible.remove(on);
                out.splices.push(Splice::Range(on..on + 1, 0));
            }
            (Err(on), true) => {
                self.visible.insert(on, at);
                out.splices.push(Splice::Insert(on));
            }
            (Err(_), false) => {}
        }
    }

    /// Opens or closes a turn or thinking row; the splices re-measure what it shows.
    pub(crate) fn toggle(&mut self, id: &str) -> Vec<Splice> {
        if !self.expanded.remove(id) {
            self.expanded.insert(id.to_owned());
        }
        let row = self
            .turns
            .get(id)
            .and_then(|turn| turn.head.as_ref())
            .map_or(id, String::as_str);
        let Some(&at) = self.index.get(row) else {
            return Vec::new();
        };
        let mut out = Applied::default();
        if self.turns.contains_key(id) {
            self.reshow(self.turn_range(at), &mut out);
        } else {
            self.redraw(at, &mut out);
        }
        out.splices
    }

    /// What a copy button on the row copies: a footer copies its turn's final text.
    /// Fork: the index in `rows` of the row with item id `id`.
    pub(crate) fn row_index(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }

    pub(crate) fn copy_text(&self, id: &str) -> Option<&str> {
        let at = *self.index.get(id)?;
        if !matches!(self.rows[at].item.kind, ChatItemKind::TurnEnd { .. }) {
            return Some(&self.rows[at].text);
        }
        self.rows[self.turn_range(at)]
            .iter()
            .rev()
            .find(|row| {
                matches!(
                    row.item.kind,
                    ChatItemKind::Assistant {
                        final_in_turn: true,
                        ..
                    }
                )
            })
            .map(|row| row.text.as_str())
    }

    /// The rows of the turn that row `at` is in; a turn's rows are adjacent.
    fn turn_range(&self, at: usize) -> Range<usize> {
        let turn = &self.rows[at].item.turn_id;
        let start = self.rows[..at]
            .iter()
            .rposition(|row| row.item.turn_id != *turn)
            .map_or(0, |before| before + 1);
        let end = self.rows[at..]
            .iter()
            .position(|row| row.item.turn_id != *turn)
            .map_or(self.rows.len(), |after| at + after);
        start..end
    }

    /// Row `at` changed: updates its turn's summary and splices what the list shows.
    fn retally(&mut self, at: usize, out: &mut Applied) {
        if self.rows[at].item.turn_id.is_empty() {
            self.redraw(at, out);
            return;
        }
        let range = self.turn_range(at);
        let turn = tally(&self.rows[range.clone()]);
        let before = self
            .turns
            .insert(self.rows[at].item.turn_id.clone(), turn.clone());
        match before {
            // What the turn hides changed, so all of it is re-measured.
            Some(before) if before.done == turn.done && before.head == turn.head => {
                self.redraw(at, out);
                let head = turn.head.as_ref().and_then(|head| self.index.get(head));
                if before != turn
                    && turn.done
                    && let Some(&head) = head
                    && head != at
                {
                    self.redraw(head, out);
                }
            }
            _ => self.reshow(range, out),
        }
    }

    fn replace(&mut self, snapshot: ChatSnapshot) {
        self.rows = snapshot.items.into_iter().map(row).collect();
        self.turns.clear();
        let mut start = 0;
        while start < self.rows.len() {
            let end = self.turn_range(start).end;
            let id = &self.rows[start].item.turn_id;
            if !id.is_empty() {
                self.turns.insert(id.clone(), tally(&self.rows[start..end]));
            }
            start = end;
        }
        self.reindex(0);
        self.visible = (0..self.rows.len()).filter(|&at| self.shown(at)).collect();
        self.bytes = self.rows.iter().map(Row::bytes).sum();
        self.busy_since = self
            .busy_since
            .or_else(|| Some(Instant::now()))
            .filter(|_| snapshot.state.busy);
        self.state = snapshot.state;
        self.capabilities = snapshot.capabilities;
        // Snapshot approvals waited already; only ones added later wait to show.
        self.arrived.clear();
        self.pending = snapshot.pending;
        self.has_earlier = snapshot.has_earlier;
        self.answering.clear();
    }

    fn reindex(&mut self, from: usize) {
        if from == 0 {
            self.index.clear();
        }
        for (at, row) in self.rows.iter().enumerate().skip(from) {
            self.index.insert(row.item.id.clone(), at);
        }
    }

    fn event(&mut self, event: ChatEvent, out: &mut Applied) {
        match event {
            ChatEvent::Upsert(item) => self.upsert(item, out),
            // An unknown id is an item older than the snapshot, not a gap.
            ChatEvent::Delta { item_id, text } => {
                if let Some(&at) = self.index.get(&item_id)
                    && let Some(row) = self.rows.get_mut(at)
                    && matches!(
                        row.item.kind,
                        ChatItemKind::Assistant { .. } | ChatItemKind::Thinking { .. }
                    )
                {
                    self.bytes += text.len();
                    row.text.push_str(&text);
                    row.changed();
                    if let Ok(on) = self.visible.binary_search(&at) {
                        out.splices.push(Splice::Replace(on));
                    }
                }
            }
            ChatEvent::State(state) => {
                self.busy_since = self
                    .busy_since
                    .filter(|_| self.state.busy)
                    .or_else(|| Some(Instant::now()))
                    .filter(|_| state.busy);
                self.state = state;
                out.changed = true;
            }
            ChatEvent::PermissionAdded(prompt) => {
                if !self
                    .pending
                    .iter()
                    .any(|pending| pending.request_id == prompt.request_id)
                {
                    self.arrived
                        .insert(prompt.request_id.clone(), Instant::now());
                    self.pending.push(prompt);
                }
                out.changed = true;
            }
            ChatEvent::PermissionRemoved { request_id, reason } => {
                let now = Instant::now();
                let shown = self
                    .arrived
                    .remove(&request_id)
                    .is_none_or(|at| now.duration_since(at) >= SHOW_APPROVAL_AFTER);
                if let Some(at) = self
                    .pending
                    .iter()
                    .position(|pending| pending.request_id == request_id)
                {
                    let prompt = self.pending.remove(at);
                    if reason == PermissionRemoved::Cancelled && shown {
                        self.withdrawn = Some((prompt, now));
                    }
                }
                self.answering.remove(&request_id);
                out.changed = true;
            }
            ChatEvent::Resync => out.resubscribe = true,
            ChatEvent::Error { text } => out.toasts.push(text),
            ChatEvent::Exited { reason } => {
                // Shown until the switch back to Console clears the chat; the toast outlasts it.
                let seq = self
                    .rows
                    .last()
                    .map_or(0, |row| row.item.seq.saturating_add(1));
                let notice = ChatItem {
                    id: EXITED_ROW.to_owned(),
                    turn_id: String::new(),
                    seq,
                    kind: ChatItemKind::Notice {
                        text: reason.clone(),
                    },
                };
                self.upsert(notice, out);
                self.exited = true;
                out.toasts.push(reason);
            }
            ChatEvent::Blocked { reason } => {
                self.blocked = Some(reason);
                out.changed = true;
            }
        }
    }

    fn upsert(&mut self, item: ChatItem, out: &mut Applied) {
        if self.exited
            && let ChatItemKind::Notice { text } = &item.kind
        {
            out.toasts.push(text.clone());
        }
        let mut row = row(item);
        let prompt = |row: &Row| matches!(row.item.kind, ChatItemKind::User { .. });
        if let Some(&at) = self.index.get(&row.item.id) {
            self.bytes = self.bytes + row.bytes() - self.rows[at].bytes().min(self.bytes);
            if self.rows[at].text == row.text {
                // Such as the final-text flag at a turn's end: no need to parse again.
                row.document = std::mem::take(&mut self.rows[at].document);
            } else if prompt(&row) || prompt(&self.rows[at]) {
                self.prompts = self.prompts.wrapping_add(1);
            }
            self.rows[at] = row;
            self.retally(at, out);
            return;
        }
        self.bytes += row.bytes();
        if prompt(&row) {
            self.prompts = self.prompts.wrapping_add(1);
        }
        let at = self
            .rows
            .partition_point(|existing| existing.item.seq <= row.item.seq);
        self.rows.insert(at, row);
        self.reindex(at);
        let after = self.visible.partition_point(|&shown| shown < at);
        for shown in &mut self.visible[after..] {
            *shown += 1;
        }
        self.retally(at, out);
    }
}

/// Moves an item's text out, so it is kept once and drawn without copying.
fn row(mut item: ChatItem) -> Row {
    let mut verb = SharedString::default();
    let mut path = None;
    let text = match &mut item.kind {
        ChatItemKind::User { text }
        | ChatItemKind::Thinking { text }
        | ChatItemKind::Notice { text } => std::mem::take(text),
        ChatItemKind::Assistant { markdown, .. } => std::mem::take(markdown),
        ChatItemKind::Tool {
            name,
            summary,
            input_preview,
            result,
            is_error,
            ..
        } => {
            let (step, pill, full) = tool_step(name, summary, input_preview);
            verb = step;
            path = full;
            // Failed calls and handoff reports expand; successful tools keep their summary.
            *input_preview = String::new();
            if *is_error && let Some(result) = result.as_mut() {
                if result.trim().is_empty() {
                    *result = tr!("The tool did not provide error details.").to_string();
                }
            } else if !(name == "Agent" && summary == "Subagent report")
                && let Some(result) = result
            {
                *result = String::new();
            }
            pill
        }
        ChatItemKind::Folded { count: 1 } => {
            tr!("1 item hidden to keep the chat small").to_string()
        }
        ChatItemKind::Folded { count } => {
            tr!("%lld items hidden to keep the chat small", *count).to_string()
        }
        ChatItemKind::TurnEnd { .. } => String::new(),
    };
    Row {
        key: item.id.clone().into(),
        item,
        text,
        verb,
        shared: OnceCell::new(),
        preview: OnceCell::new(),
        document: markdown::Parsed::default(),
        prompt: OnceCell::new(),
        path,
    }
}

/// Longest pill kept; the view cuts it to fit with an ellipsis.
const MAX_PILL: usize = 240;

/// A tool's line: what it does, the command, path or pattern it works on,
/// and a file step's whole path.
fn tool_step(name: &str, summary: &str, input: &str) -> (SharedString, String, Option<String>) {
    // A preview past the server's cap is cut and no longer parses; `summary` stands in.
    let input: serde_json::Value = serde_json::from_str(input).unwrap_or_default();
    let field = |key: &str| input.get(key).and_then(serde_json::Value::as_str);
    let full = field("file_path").or_else(|| field("path"));
    let path = || full.map(short_path);
    let (verb, pill) = match (tool_kind(name), name) {
        (ToolKind::Run, "Bash") => (
            field("description").map_or_else(|| tr!("Run"), |text| text.to_owned().into()),
            field("command").map(str::to_owned),
        ),
        (ToolKind::Read, _) => (tr!("Read"), path()),
        (ToolKind::Edit, "Write") => (tr!("Write"), path()),
        (ToolKind::Edit, "TodoWrite") => (tr!("Update the to-do list"), Some(String::new())),
        (ToolKind::Edit, _) => (tr!("Edit"), path()),
        (ToolKind::Search, _) => (tr!("Search"), field("pattern").map(str::to_owned)),
        (ToolKind::Web, "WebSearch") => (tr!("Search the web"), field("query").map(str::to_owned)),
        (ToolKind::Web, _) => (tr!("Fetch"), field("url").map(str::to_owned)),
        (ToolKind::Agent, _) => (
            field("description")
                .map_or_else(|| name.to_owned().into(), |text| text.to_owned().into()),
            Some(String::new()),
        ),
        _ => (name.to_owned().into(), None),
    };
    let mut pill = pill.unwrap_or_else(|| summary.to_owned());
    // One line: the first, cut on a char boundary.
    let mut end = pill.find('\n').unwrap_or(pill.len()).min(MAX_PILL);
    while !pill.is_char_boundary(end) {
        end -= 1;
    }
    pill.truncate(end);
    let full = matches!(tool_kind(name), ToolKind::Read | ToolKind::Edit)
        .then(|| full.map(str::to_owned))
        .flatten();
    (verb, pill, full)
}

/// The last two parts of a path, which say the most in little room.
fn short_path(path: &str) -> String {
    let parts = path
        .trim_end_matches('/')
        .rsplitn(3, '/')
        .collect::<Vec<_>>();
    match parts.as_slice() {
        [name, parent, _] => format!("{parent}/{name}"),
        _ => path.to_owned(),
    }
}

/// The toast text for a refused request.
fn refusal(reason: ChatRefusal, detail: Option<String>) -> String {
    let reason = match reason {
        ChatRefusal::StaleGeneration => tr!("The chat changed meanwhile. Try again."),
        ChatRefusal::Busy => tr!("Claude is busy switching. Try again in a moment."),
        ChatRefusal::UnsupportedArguments => {
            tr!("Claude was started with options chat doesn't support.")
        }
        ChatRefusal::UnsupportedEnvironment => tr!(
            "Chat supports only Claude's subscription login; this terminal sets an Anthropic key or another provider."
        ),
        ChatRefusal::PersistenceDisabled => tr!(
            "This Claude session doesn't save its conversation, so switching back would lose it."
        ),
        ChatRefusal::NoClaudeSession => tr!(
            "There is no saved Claude conversation here yet. Send your first message, then switch to Chat."
        ),
        ChatRefusal::NoShell => {
            tr!("Claude isn't running under a shell here, so it couldn't return to the terminal.")
        }
        ChatRefusal::ConversationInUse => tr!("This conversation is open in another terminal."),
        ChatRefusal::PlatformUnsupported => tr!("Chat isn't available on this server."),
        ChatRefusal::CliIncompatible => tr!("The provider CLI can't run as chat."),
        ChatRefusal::SpawnFailed => tr!("Claude Code couldn't switch."),
        ChatRefusal::StartupTimeout => tr!("Claude Code took too long to start as chat."),
        ChatRefusal::NotChat => tr!("This session isn't in Chat."),
        ChatRefusal::ApprovalGone => tr!("That request was already answered or withdrawn."),
        ChatRefusal::TooManyApprovals => tr!("Too many approvals are waiting."),
        ChatRefusal::Unrecognized(_) => tr!("The server refused the chat request."),
    };
    match detail.filter(|detail| !detail.is_empty()) {
        Some(detail) => format!("{reason} {detail}"),
        None => reason.to_string(),
    }
}

/// Whether the Chat|Console switch shows for a pane, and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChatToggle {
    pub(crate) chat: bool,
    pub(crate) enabled: bool,
    /// The pane sits at an idle shell prompt: the switch starts a new chat there.
    pub(crate) start: bool,
}

impl AppModel {
    pub(crate) fn preview_chat_image(&mut self, image: ChatImage, cx: &mut Context<Self>) {
        self.chat.image_preview = None;
        let opened = crate::opener::submit(move || crate::opener::preview::Preview::open(&image));
        self.chat.image_preview = Some(cx.spawn(async move |model, cx| {
            let result = async {
                let mut preview = opened
                    .map_err(|e| e.to_string())?
                    .recv()
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())?;
                while !preview.finished().map_err(|e| e.to_string())? {
                    cx.background_executor()
                        .timer(Duration::from_millis(250))
                        .await;
                }
                Ok::<_, String>(())
            }
            .await;
            if let Err(error) = result {
                let _ = model.update(cx, |model, cx| model.show_notice(error, cx));
            }
        }));
    }

    /// A promptless probe uses a temporary project shell, removed by its creation token.
    pub(crate) fn discover_card_models(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Task<Result<CardCatalogs, String>> {
        let project = self.state.current_project();
        let (project, server, directory) =
            (project.id, project.server_id, project.directory.clone());
        let environment = self
            .proxy
            .settings
            .as_ref()
            .filter(|_| {
                server.is_local()
                    && self.proxy.global()
                    && self.server_supports(server, Feature::SESSION_ENVIRONMENT)
            })
            .map(muxy_app_core::settings::ProxySettings::environment)
            .unwrap_or_default();
        let (sender, receiver) = async_channel::bounded(1);
        let sent = self.send(server, Work::ExtensionConnection(sender), cx);
        self.chat.card_probes.insert((server, project));
        cx.spawn(async move |model, cx| {
            let result = async {
                if !sent {
                    return Err(tr!("Connect to the server first").to_string());
                }
                let client = receiver.recv().await.map_err(|error| error.to_string())??;
                cx.background_executor()
                    .spawn(async move {
                        let operation = OperationId::new();
                        let result = (|| {
                            let session = client
                                .create_project_session_with_environment(
                                    project,
                                    operation,
                                    &directory,
                                    muxy_protocol::Size { cols: 80, rows: 24 },
                                    environment,
                                    None,
                                )
                                .map_err(|error| error.to_string())?;
                            std::thread::scope(|scope| {
                                let probes: Vec<_> = [ChatProvider::Claude, ChatProvider::Codex]
                                    .into_iter()
                                    .map(|provider| {
                                        let client = &client;
                                        scope.spawn(move || {
                                            let result = client
                                                .chat(ChatRequest::Discover {
                                                    session: session.id,
                                                    provider,
                                                    features: chat_features(),
                                                })
                                                .map_err(|error| error.to_string())
                                                .and_then(|reply| {
                                                    match reply {
                                                ChatReply::Discovered { capabilities, .. } => {
                                                    Ok(capabilities.models)
                                                }
                                                ChatReply::Refused { reason, detail } => {
                                                    Err(refusal(reason, detail))
                                                }
                                                _ => Err(tr!(
                                                    "The provider did not return a model catalog."
                                                )
                                                .to_string()),
                                            }
                                                });
                                            (provider, result)
                                        })
                                    })
                                    .collect();
                                probes
                                    .into_iter()
                                    .map(|probe| {
                                        probe
                                            .join()
                                            .map_err(|_| tr!("Model discovery failed").to_string())
                                    })
                                    .collect::<Result<Vec<_>, _>>()
                            })
                        })();
                        // Cancel the exact creation even if its reply was lost; no existing shell is touched.
                        client.cancel_creation(operation).map_err(|error| {
                            tr!(
                                "Could not remove the model-discovery shell: %@",
                                &error.to_string()
                            )
                            .to_string()
                        })?;
                        result
                    })
                    .await
            }
            .await;
            let _ = model.update(cx, |model, cx| {
                model.finish_card_probe(server, project, cx);
            });
            result
        })
    }

    pub(crate) fn chat_make_draft(
        &mut self,
        pane: PaneId,
        provider: Option<ChatProvider>,
        choose_thread: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.chat_key(pane) else {
            return;
        };
        let card_selection = self.chat.card_selections.remove(&pane);
        let provider = card_selection
            .as_ref()
            .map(|selection| selection.provider)
            .or(provider)
            .unwrap_or_else(|| {
                self.opener_context(pane)
                    .and_then(|context| {
                        self.settings
                            .composer
                            .chat_models
                            .iter()
                            .rev()
                            .find(|choice| {
                                choice.server == context.server && choice.project == context.project
                            })
                            .map(|choice| choice.provider)
                    })
                    .unwrap_or_default()
            });
        let (model, effort) = card_selection.map_or_else(
            || self.chat_choice_for(pane, provider),
            |selection| (selection.model, selection.effort),
        );
        self.chat.next_draft = self.chat.next_draft.wrapping_add(1);
        let generation = self.chat.next_draft;
        let session = self.chat.sessions.entry(key).or_default();
        session.starting = false;
        session.local = Some(LocalDraft {
            selection: ChatSelection {
                provider,
                model: model.clone(),
                effort: effort.clone(),
                target: None,
                launch_context: None,
            },
            generation,
            host_epoch: None,
            discovering: true,
            visible: true,
            choose_thread,
            threads: Vec::new(),
        });
        session.state = ChatState {
            provider: Some(provider),
            model: model.clone(),
            effort,
            quiescent: true,
            ..ChatState::default()
        };
        session.chosen_model = model;
        session.capabilities = ChatCapabilities::default();
        session.picker_error = None;
        self.sync_chat(cx);
        self.chat_probe(pane, key, generation, provider, cx);
        cx.notify();
    }

    fn chat_probe(
        &mut self,
        pane: PaneId,
        key: ChatKey,
        generation: u64,
        provider: ChatProvider,
        cx: &mut Context<Self>,
    ) {
        if !self.server_supports(key.0, Feature::CHAT_PROVIDERS) {
            if let Some(session) = self.chat.sessions.get_mut(&key) {
                if let Some(local) = session.local.as_mut() {
                    local.discovering = false;
                }
                session.picker_error=Some(tr!("Update the desktop and server to select a provider before sending. Your draft is kept.").to_string());
            }
            self.notify_chat_views(key, &[], cx);
            return;
        }
        let context = self.opener_context(pane).map(|context| context.directory);
        self.chat_request(key.0,ChatRequest::Discover {session:key.1,provider,features:chat_features()},cx,move|model,result,cx|{
            if model.chat_key(pane)!=Some(key)||model.opener_context(pane).map(|context|context.directory)!=context{return;}
            let Some(session)=model.chat.sessions.get_mut(&key)else{return};
            let Some(local)=session.local.as_mut().filter(|local|local.generation==generation&&local.selection.provider==provider)else{return};
            local.discovering=false;
            match result {
                Ok(ChatReply::Discovered {host_epoch,capabilities,model:default_model,effort:default_effort,threads,launch_context,..})=>{
                    local.host_epoch=Some(host_epoch);local.selection.launch_context=launch_context;local.threads=threads;
                    if local.selection.model.is_none(){local.selection.model=default_model;}
                    let offered=capabilities.models.iter().find(|offered|Some(&offered.alias)==local.selection.model.as_ref());
                    if local.selection.effort.is_none(){local.selection.effort=offered.and_then(|model|model.default_effort.clone()).or(default_effort);}
                    if local.selection.effort.as_ref().is_some_and(|effort|offered.is_none_or(|model|!model.efforts.contains(effort))){session.picker_error=Some(tr!("The saved effort is unavailable. Choose another effort level; your draft is kept.").to_string());}
                    if local.selection.model.is_some()&&offered.is_none(){session.picker_error=Some(tr!("The saved model is unavailable. Choose another model; your draft is kept.").to_string());}
                    session.state.model=local.selection.model.clone();session.state.effort=local.selection.effort.clone();
                    model.chat.card_catalogs.retain(|(server, known, _)| *server != key.0 || *known != provider);
                    model.chat.card_catalogs.push((key.0, provider, capabilities.models.clone()));
                    session.chosen_model=local.selection.model.clone();session.capabilities=capabilities;
                }
                Ok(_)=>session.picker_error=Some(tr!("The provider did not return a model catalog.").to_string()),
                Err(error)=>session.picker_error=Some(error),
            }
            model.notify_chat_views(key,&[],cx);
        });
    }

    pub(crate) fn chat_change_provider(
        &mut self,
        pane: PaneId,
        provider: ChatProvider,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.chat_session(pane) else {
            return;
        };
        if session.local.is_none()
            || session.operation.is_some()
            || session.preparing.is_some()
            || session.switching
        {
            return;
        }
        if session.mode != ChatMode::Console {
            if !session.state.quiescent {
                return;
            }
            let Some(key) = self.chat_key(pane) else {
                return;
            };
            let request = if session.provider() == ChatProvider::Codex {
                ChatRequest::LeaveProvider {
                    session: key.1,
                    generation: session.generation,
                    interrupt: false,
                }
            } else {
                ChatRequest::Leave {
                    session: key.1,
                    generation: session.generation,
                }
            };
            if let Some(session) = self.chat.sessions.get_mut(&key) {
                session.switching = true;
            }
            self.chat_request(key.0, request, cx, move |model, result, cx| {
                if let Some(session) = model.chat.sessions.get_mut(&key) {
                    session.switching = false;
                }
                match result {
                    Ok(ChatReply::Left { .. }) => {
                        model.chat_make_draft(pane, Some(provider), false, cx);
                        model.save_chat_choice(pane, None, None, cx);
                    }
                    Err(error) => {
                        if let Some(session) = model.chat.sessions.get_mut(&key) {
                            session.picker_error = Some(error);
                        }
                        model.notify_chat_views(key, &[], cx);
                    }
                    _ => {}
                }
            });
            return;
        }
        self.chat_make_draft(pane, Some(provider), false, cx);
        self.save_chat_choice(pane, None, None, cx);
    }

    pub(crate) fn chat_resume_target(
        &mut self,
        pane: PaneId,
        target: ChatTarget,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.chat_key(pane) else {
            return;
        };
        let Some(session) = self.chat.sessions.get_mut(&key) else {
            return;
        };
        session.switching = true;
        self.chat_request(
            key.0,
            ChatRequest::EnterProvider {
                session: key.1,
                selection: Box::new(ChatSelection {
                    provider: target.provider,
                    model: None,
                    effort: None,
                    target: Some(target),
                    launch_context: None,
                }),
                features: chat_features(),
            },
            cx,
            move |model, result, cx| {
                if let Some(session) = model.chat.sessions.get_mut(&key) {
                    session.switching = false;
                    match result {
                        Ok(ChatReply::Entered { .. }) => {
                            session.local = None;
                            session.state.provider = Some(ChatProvider::Codex);
                        }
                        Err(error) => session.picker_error = Some(error),
                        _ => {}
                    }
                }
                model.sync_chat(cx);
                model.notify_chat_views(key, &[], cx);
            },
        );
    }
    pub(crate) fn chat_key(&self, pane: PaneId) -> Option<ChatKey> {
        self.pane_session(pane)
    }

    pub(crate) fn chat_session(&self, pane: PaneId) -> Option<&ChatSession> {
        self.chat.sessions.get(&self.chat_key(pane)?)
    }

    /// The mode of the pane's session; Console unless the server said otherwise.
    pub(crate) fn chat_mode_of(&self, pane: PaneId) -> ChatMode {
        self.chat_session(pane)
            .map_or(ChatMode::Console, |session| {
                if session.local.as_ref().is_some_and(|draft| draft.visible) {
                    ChatMode::Chat
                } else {
                    session.mode
                }
            })
    }

    /// The chat view the pane draws instead of its terminal, if any.
    pub(crate) fn chat_view(&self, pane: PaneId) -> Option<&Entity<ChatView>> {
        self.chat_session(pane)
            .filter(|session| session.showing())?;
        self.chat.views.get(&pane)
    }

    pub(crate) fn chat_toggle(&self, pane: PaneId, cx: &gpui::App) -> Option<ChatToggle> {
        let session = self.chat_session(pane);
        let mode = session.map_or(ChatMode::Console, |session| session.mode);
        let supported = cfg!(target_os = "macos")
            && self
                .pane_server(pane)
                .is_some_and(|server| self.server_supports(server, Feature::AGENT_CHAT));
        // The server's activity follows the foreground, and names Claude before
        // its session metadata arrives (Enter starts a new chat if nothing was saved).
        let claude = supported
            && self
                .pane_agent(pane)
                .is_some_and(|agent| agent.provider == AgentProvider::Claude);
        let codex = supported
            && self
                .pane_agent(pane)
                .is_some_and(|agent| agent.provider == AgentProvider::Codex);
        let start = supported
            && mode == ChatMode::Console
            && !claude
            && !codex
            && self.shell_idle(pane, cx);
        let starting = session.is_some_and(|session| session.starting);
        (mode != ChatMode::Console
            || claude
            || codex
            || start
            || starting
            || session.is_some_and(|session| session.local.is_some()))
        .then(|| ChatToggle {
            chat: session.is_some_and(ChatSession::showing),
            enabled: mode != ChatMode::Transitioning
                && !session.is_some_and(|session| session.switching),
            start: start && !starting,
        })
    }

    /// The pane's terminal is attached and its shell is in the foreground.
    fn shell_idle(&self, pane: PaneId, cx: &gpui::App) -> bool {
        self.grids.get(&pane).is_some_and(|terminal| {
            let terminal = terminal.view.read(cx);
            terminal.attachment().is_some()
                && terminal
                    .process
                    .as_ref()
                    .is_some_and(|process| process.is_shell)
        })
    }

    /// The Chat|Console switch: asks first when it would stop Claude's work.
    pub(crate) fn toggle_chat(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        let Some(toggle) = self.chat_toggle(pane, cx) else {
            return;
        };
        if !toggle.enabled || self.chat.prompt.is_some() {
            return;
        }
        if self
            .chat_session(pane)
            .is_some_and(|session| session.local.is_some() && session.mode == ChatMode::Console)
        {
            if self
                .chat_session(pane)
                .is_some_and(|session| session.operation.is_some() || session.preparing.is_some())
            {
                return;
            }
            let local = self
                .chat_key(pane)
                .and_then(|key| self.chat.sessions.get_mut(&key))
                .and_then(|session| session.local.as_mut());
            if let Some(local) = local {
                local.visible = !local.visible;
            }
            self.sync_chat(cx);
            cx.notify();
            return;
        }
        // "Starting…" before Start was sent: Console cancels it.
        if self.chat.start_when_idle.remove(&pane).is_some() {
            return self.chat_stop_starting(pane, cx);
        }
        match self.chat_mode_of(pane) {
            ChatMode::Console if toggle.start => self.chat_make_draft(pane, None, false, cx),
            ChatMode::Console => self.request_chat_enter(pane, cx),
            ChatMode::Chat => {
                let quiet = self
                    .chat_session(pane)
                    .is_none_or(|session| session.state.quiescent);
                if quiet
                    || self
                        .chat_session(pane)
                        .is_some_and(|session| session.state.shared_engine)
                {
                    self.chat_leave(pane, cx);
                } else {
                    self.confirm_chat_switch(pane, false, cx);
                }
            }
            ChatMode::Blocked => {
                if self
                    .chat_session(pane)
                    .is_some_and(|session| !session.state.shared_engine && !session.state.quiescent)
                {
                    self.confirm_chat_switch(pane, false, cx);
                } else {
                    self.chat_leave(pane, cx);
                }
            }
            ChatMode::Transitioning | ChatMode::Unrecognized(_) => {}
        }
    }

    /// Enters Chat, asking first when it would interrupt Claude's work.
    fn request_chat_enter(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if self
            .pane_agent(pane)
            .is_some_and(|agent| agent.provider == AgentProvider::Codex)
        {
            self.chat_make_draft(pane, Some(ChatProvider::Codex), true, cx);
            return;
        }
        let working = self
            .pane_agent(pane)
            .is_some_and(|agent| agent.state == AgentState::Working);
        if working {
            self.confirm_chat_switch(pane, true, cx);
        } else {
            self.chat_enter(pane, cx);
        }
    }

    fn confirm_chat_switch(&mut self, pane: PaneId, entering: bool, cx: &mut Context<Self>) {
        let id = self.ask_dialog(
            self.window,
            async move |parent, cx| {
                crate::views::confirm::prompt_chat_switch(parent, entering, cx)
                    .await
                    .unwrap_or(false)
            },
            move |model, confirmed, cx| {
                model.chat.prompt = None;
                match (confirmed, entering) {
                    (true, true) => model.chat_enter(pane, cx),
                    (true, false) => model.chat_leave(pane, cx),
                    (false, _) => {}
                }
            },
            cx,
        );
        self.chat.prompt = Some(id);
    }

    pub(crate) fn chat_enter(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        let Some(key) = self.chat_key(pane) else {
            return;
        };
        self.chat.sessions.entry(key).or_default().switching = true;
        cx.notify();
        let (model, effort) = self.chat_choice(pane);
        let request = ChatRequest::Enter {
            session: key.1,
            model,
            effort,
        };
        self.chat_request(key.0, request, cx, move |model, result, cx| {
            model.chat_switched(key, cx);
            if let Err(error) = result {
                model.show_toast(tr!("Couldn't switch to Chat").to_string(), Some(error), cx);
            }
        });
    }

    pub(crate) fn chat_leave(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        let Some(key) = self.chat_key(pane) else {
            return;
        };
        let Some(session) = self.chat.sessions.get_mut(&key) else {
            return;
        };
        session.switching = true;
        let request = if session.provider() == ChatProvider::Codex {
            ChatRequest::LeaveProvider {
                session: key.1,
                generation: session.generation,
                interrupt: !session.state.quiescent && !session.state.shared_engine,
            }
        } else {
            ChatRequest::Leave {
                session: key.1,
                generation: session.generation,
            }
        };
        cx.notify();
        self.chat_request(key.0, request, cx, move |model, result, cx| {
            model.chat_switched(key, cx);
            match result {
                Ok(ChatReply::Left {
                    resumed: false,
                    detail,
                }) => model.show_toast(
                    tr!("The agent wasn't restarted in the terminal").to_string(),
                    detail,
                    cx,
                ),
                Ok(_) => {}
                Err(error) => {
                    model.show_toast(
                        tr!("Couldn't switch to Console").to_string(),
                        Some(error),
                        cx,
                    );
                }
            }
        });
    }

    fn chat_switched(&mut self, key: ChatKey, cx: &mut Context<Self>) {
        if let Some(session) = self.chat.sessions.get_mut(&key) {
            session.switching = false;
        }
        cx.notify();
    }

    /// Opens or closes a finished turn's steps or a thinking row.
    pub(crate) fn chat_expand(&mut self, key: ChatKey, id: &str, cx: &mut Context<Self>) {
        let Some(session) = self.chat.sessions.get_mut(&key) else {
            return;
        };
        let splices = session.toggle(id);
        self.notify_chat_views(key, &splices, cx);
    }

    /// Expands or collapses a stretch, by its row's item id and its own id, in
    /// every pane showing the chat; only that row is parsed and measured again.
    pub(crate) fn chat_stretch(
        &mut self,
        key: ChatKey,
        (item, id): (SharedString, SharedString),
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.chat.sessions.get_mut(&key) else {
            return;
        };
        let open = session.stretched.entry(item.clone()).or_default();
        if !open.remove(&id) {
            open.insert(id);
        }
        // A plan card's stretches have no row; its view redraws all the same.
        let splices: Vec<Splice> = session
            .visible
            .iter()
            .position(|&at| session.rows.get(at).is_some_and(|row| row.key == item))
            .map(Splice::Replace)
            .into_iter()
            .collect();
        self.notify_chat_views(key, &splices, cx);
    }

    /// Copies a row's markdown, or a footer's final text of its turn.
    pub(crate) fn chat_copy(&mut self, key: ChatKey, id: &str, cx: &mut Context<Self>) {
        let Some(text) = self
            .chat
            .sessions
            .get(&key)
            .and_then(|session| session.copy_text(id))
        else {
            return;
        };
        let text = text.to_owned();
        self.chat_copy_text(text, cx);
    }

    /// Copies `text` from chat and says so.
    pub(crate) fn chat_copy_text(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.show_notice(tr!("Copied to clipboard").to_string(), cx);
    }

    /// Opens a web link from chat the way terminal links open; anything else is ignored.
    pub(crate) fn chat_open_link(&mut self, pane: PaneId, url: &str, cx: &mut Context<Self>) {
        if let Some(target) = Target::web_url(url) {
            self.open_terminal_link(pane, target, cx);
        }
    }

    /// Stops Claude's running turn.
    pub(crate) fn chat_interrupt(&mut self, key: ChatKey, cx: &mut Context<Self>) {
        let Some(session) = self.chat.sessions.get(&key) else {
            return;
        };
        let request = ChatRequest::Interrupt {
            session: key.1,
            generation: session.generation,
        };
        self.chat_request(key.0, request, cx, Self::chat_failed);
    }

    /// Answers an approval once; another click waits for the server.
    pub(crate) fn chat_respond(
        &mut self,
        key: ChatKey,
        request_id: String,
        response: PermissionResponse,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.chat.sessions.get_mut(&key) else {
            return;
        };
        if !session.answering.insert(request_id.clone()) {
            return;
        }
        let request = ChatRequest::Respond {
            session: key.1,
            generation: session.generation,
            request_id: request_id.clone(),
            response,
        };
        self.notify_chat_views(key, &[], cx);
        self.chat_request(key.0, request, cx, move |model, result, cx| {
            if let Err(error) = result {
                if let Some(session) = model.chat.sessions.get_mut(&key) {
                    session.answering.remove(&request_id);
                }
                model.notify_chat_views(key, &[], cx);
                model.show_notice(error, cx);
            }
        });
    }

    pub(super) fn chat_capture(
        &mut self,
        pane: PaneId,
        cx: &mut Context<Self>,
    ) -> Result<ChatSend, String> {
        let key = self
            .chat_key(pane)
            .ok_or_else(|| tr!("This session isn't in Chat.").to_string())?;
        let session = self
            .chat
            .sessions
            .get(&key)
            .ok_or_else(|| tr!("This session isn't in Chat.").to_string())?;
        if session.operation.is_some() || session.preparing.is_some() {
            return Err(tr!("The previous delivery is still pending or uncertain. Check Console before sending again.").to_string());
        }
        if session.local.as_ref().is_some_and(|draft| {
            draft.discovering || draft.host_epoch.is_none() || draft.choose_thread
        }) {
            return Err(tr!("Choose an available provider and model before sending.").to_string());
        }
        let generation = session.generation;
        let local = session.local.clone();
        self.chat.next_submission = self.chat.next_submission.wrapping_add(1);
        let id = self.chat.next_submission;
        if let Some(session) = self.chat.sessions.get_mut(&key) {
            session.preparing = Some(id);
        }
        let model = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = model.update(cx, |model, cx| model.notify_chat_views(key, &[], cx));
        });
        Ok(ChatSend {
            key,
            id,
            generation,
            local,
        })
    }

    /// A submitted snapshot clears only after provider acknowledgment; uncertain sends are never retried.
    pub(crate) fn chat_send(
        &mut self,
        pane: PaneId,
        text: String,
        images: Vec<ChatImage>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        match self.chat_capture(pane, cx) {
            Ok(capture) => self.chat_send_captured(capture, text, images, cx),
            Err(error) => Task::ready(Err(error)),
        }
    }

    pub(super) fn chat_finish_preparing(&mut self, key: ChatKey, id: u64, cx: &mut Context<Self>) {
        if let Some(session) = self.chat.sessions.get_mut(&key)
            && session.preparing == Some(id)
        {
            session.preparing = None;
        }
        self.notify_chat_views(key, &[], cx);
    }

    pub(super) fn chat_send_captured(
        &mut self,
        capture: ChatSend,
        text: String,
        images: Vec<ChatImage>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let ChatSend {
            key,
            generation,
            local,
            id,
        } = capture;
        let prepare = self.chat_call(key.0, ChatRequest::PrepareSubmission { session: key.1 }, cx);
        cx.spawn(async move |model,cx|{
            let result=async {
            let ChatReply::Prepared {host_epoch,operation}=prepare.await? else{return Err(tr!("The server did not prepare this submission.").to_string())};
            let call=model.update(cx,|model,cx|{
                let session=model.chat.sessions.get_mut(&key).ok_or_else(||tr!("This session isn't in Chat.").to_string())?;
                if session.operation.is_some(){return Err(tr!("Another submission is already pending.").to_string())}
                let request=if let Some(frozen)=local {
                    if frozen.host_epoch!=Some(host_epoch) || session.local.as_ref().is_none_or(|current| current.generation != frozen.generation){
                        return Err(tr!("The draft launch context changed. Reopen the provider picker.").to_string());
                    }
                    ChatRequest::Begin {session:key.1,host_epoch,operation,selection:Box::new(frozen.selection),text,images,features:chat_features()}
                }else{
                    if session.generation!=generation{return Err(tr!("The conversation changed before sending.").to_string())}
                    ChatRequest::Submit {session:key.1,generation,operation,text,images}
                };
                session.operation=Some(operation);
                Ok(model.chat_call(key.0,request,cx))
            }).map_err(|error|error.to_string())??;
            let mut result=call.await;
            let mut warned=false;
            for _ in 0..120 {
                if let Ok(ChatReply::Submission(status))=&result {
                    match status.outcome {
                        SubmissionOutcome::Accepted=>{
                            let _=model.update(cx,|model,cx|{
                                if let Some(session)=model.chat.sessions.get_mut(&key){session.operation=None;session.local=None;session.picker_error=None;}
                                model.sync_chat(cx);model.notify_chat_views(key,&[],cx);
                            });return Ok(());
                        }
                        SubmissionOutcome::Rejected=>{
                            let message=status.detail.clone().unwrap_or_else(||tr!("The provider rejected this message.").to_string());
                            let _=model.update(cx,|model,cx|{
                                if let Some(session)=model.chat.sessions.get_mut(&key){session.operation=None;session.picker_error=Some(message.clone());}
                                model.notify_chat_views(key,&[],cx);
                            });return Err(message);
                        }
                        SubmissionOutcome::Uncertain if !warned=>{
                            warned=true;
                            let _=model.update(cx,|model,cx|{
                                if let Some(session)=model.chat.sessions.get_mut(&key){session.picker_error=Some(tr!("Delivery is uncertain. Checking native history; the message has not been resent.").to_string());}
                                model.notify_chat_views(key,&[],cx);
                            });
                        }
                        _=>{},
                    }
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let status=model.update(cx,|model,cx|model.chat_call(key.0,ChatRequest::OperationStatus {session:key.1,operation},cx))
                    .map_err(|error|error.to_string())?;
                result=status.await;
            }
            Err(tr!("Delivery remains uncertain. Check Console; the message has not been resent and the draft is kept.").to_string())
            }.await;
            let _=model.update(cx,|model,cx|model.chat_finish_preparing(key,id,cx));
            result
        })
    }

    fn chat_failed(&mut self, result: Result<ChatReply, String>, cx: &mut Context<Self>) {
        if let Err(error) = result {
            self.show_notice(error, cx);
        }
    }

    /// Sends `request` without waiting; a refusal comes back as its text.
    pub(super) fn chat_call(
        &mut self,
        server: ServerId,
        request: ChatRequest,
        cx: &mut Context<Self>,
    ) -> impl Future<Output = Result<ChatReply, String>> + use<> {
        let (reply, receiver) = async_channel::bounded(1);
        let sent = self.send(server, Work::Chat { request, reply }, cx);
        async move {
            let unreachable = || tr!("Connect to the server first").to_string();
            if !sent {
                return Err(unreachable());
            }
            match receiver.recv().await.map_err(|_| unreachable())?? {
                ChatReply::Refused { reason, detail } => Err(refusal(reason, detail)),
                reply => Ok(reply),
            }
        }
    }

    fn chat_request(
        &mut self,
        server: ServerId,
        request: ChatRequest,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<ChatReply, String>, &mut Context<Self>) + 'static,
    ) {
        let call = self.chat_call(server, request, cx);
        cx.spawn(async move |model, cx| {
            let result = call.await;
            let _ = model.update(cx, |model, cx| done(model, result, cx));
        })
        .detach();
    }

    fn chat_subscribe(&mut self, key: ChatKey, cx: &mut Context<Self>) {
        self.chat.next_token = self.chat.next_token.wrapping_add(1);
        let token = self.chat.next_token;
        let Some(session) = self.chat.sessions.get_mut(&key) else {
            return;
        };
        session.subscribe(token);
        let request = ChatRequest::Subscribe {
            session: key.1,
            subscription: token,
            features: chat_features(),
        };
        self.chat_request(key.0, request, cx, move |model, result, cx| {
            let Some(session) = model
                .chat
                .sessions
                .get_mut(&key)
                .filter(|session| session.token == Some(token))
            else {
                return;
            };
            if matches!(result, Ok(ChatReply::Subscribed { .. })) {
                session.failures = 0;
                return;
            }
            session.failures += 1;
            if session.failures > SUBSCRIBE_RETRIES {
                // The failed token stays, so only a new chat generation subscribes again.
                model.show_toast(tr!("Couldn't load the chat").to_string(), result.err(), cx);
                return;
            }
            let delay = Duration::from_secs(1 << (session.failures - 1));
            cx.spawn(async move |model, cx| {
                cx.background_executor().timer(delay).await;
                let _ = model.update(cx, |model, cx| {
                    let wanted = model.chat.sessions.get(&key).is_some_and(|session| {
                        session.token == Some(token)
                            && matches!(session.mode, ChatMode::Chat | ChatMode::Blocked)
                    });
                    if wanted {
                        model.chat_subscribe(key, cx);
                    }
                });
            })
            .detach();
        });
    }

    /// A pane's channel reported its session's mode.
    pub(super) fn chat_mode(
        &mut self,
        server: ServerId,
        channel: ChannelId,
        mode: ChatMode,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        let Some((_, session)) = self
            .grids
            .iter()
            .find(|(_, pane)| pane.view.read(cx).attachment() == Some((server, channel)))
            .and_then(|(pane, _)| self.pane_session(*pane))
        else {
            return;
        };
        let key = (server, session);
        if mode == ChatMode::Console && !self.chat.sessions.contains_key(&key) {
            return;
        }
        let session = self.chat.sessions.entry(key).or_default();
        let before = (session.mode, session.generation);
        session.set_mode(mode, generation);
        // Each attachment reports the mode; only a change moves the other panes' lists.
        let changed = before != (session.mode, session.generation);
        self.sync_chat(cx);
        if changed {
            let rows = self
                .chat
                .sessions
                .get(&key)
                .map_or(0, |session| session.visible.len());
            self.notify_chat_views(key, &[Splice::Reset(rows)], cx);
        }
        cx.notify();
    }

    pub(super) fn receive_chat(
        &mut self,
        server: ServerId,
        batch: ChatBatch,
        cx: &mut Context<Self>,
    ) {
        let key = (server, batch.session);
        let Some(session) = self.chat.sessions.get_mut(&key) else {
            return;
        };
        let applied = session.apply(batch);
        if applied.changed || !applied.splices.is_empty() {
            self.notify_chat_views(key, &applied.splices, cx);
        }
        if applied.resubscribe {
            self.chat_subscribe(key, cx);
        }
        for toast in applied.toasts {
            self.show_notice(toast, cx);
        }
    }

    /// Splices and redraws only the chat views of `key`.
    fn notify_chat_views(&self, key: ChatKey, splices: &[Splice], cx: &mut Context<Self>) {
        let rows = self
            .chat
            .sessions
            .get(&key)
            .map_or(0, |session| session.visible.len());
        for view in self.chat.views.values() {
            if view.read(cx).key == key {
                view.update(cx, |view, cx| view.apply(splices, rows, cx));
            }
        }
    }

    /// Chat subscriptions belong to the connection that ended; attaching again replays the modes.
    pub(super) fn chat_disconnected(&mut self, server: ServerId) {
        self.chat
            .card_catalogs
            .retain(|(known, _, _)| *known != server);
        self.chat.sessions.retain(|key, session| {
            key.0 != server
                || session.local.is_some()
                || session.operation.is_some()
                || session.preparing.is_some()
        });
        for (key, session) in &mut self.chat.sessions {
            if key.0 == server {
                session.token = None;
            }
        }
    }

    /// Keeps sessions some pane in the layout shows, hidden or not, drops the
    /// rest, and gives each visible chat pane its view.
    pub(super) fn sync_chat(&mut self, cx: &mut Context<Self>) {
        let layout: HashSet<PaneId> = self
            .state
            .projects()
            .iter()
            .flat_map(|project| &project.tabs)
            .flat_map(|tab| &tab.panes)
            .chain(self.state.quick_terminal())
            .map(|pane| pane.id)
            .chain(self.command_center.tiles.iter().map(|tile| tile.pane))
            .chain(self.grids.keys().copied())
            .collect();
        self.chat
            .enter_when_ready
            .retain(|pane, _| layout.contains(pane));
        self.chat
            .start_when_idle
            .retain(|pane, _| layout.contains(pane));
        self.chat
            .resume_when_idle
            .retain(|pane, _| layout.contains(pane));
        self.chat
            .card_selections
            .retain(|pane, _| layout.contains(pane));
        let kept: HashSet<ChatKey> = layout
            .iter()
            .filter_map(|pane| self.chat_key(*pane))
            .collect();
        let mut shown: HashMap<ChatKey, Vec<PaneId>> = HashMap::new();
        for pane in self.grids.keys() {
            if let Some(key) = self.chat_key(*pane) {
                shown.entry(key).or_default().push(*pane);
            }
        }
        let gone: Vec<_> = self
            .chat
            .sessions
            .keys()
            .filter(|key| !kept.contains(key))
            .copied()
            .collect();
        for key in gone {
            if let Some(session) = self.chat.sessions.remove(&key)
                && session.token.is_some()
                && session.mode != ChatMode::Console
            {
                let request = ChatRequest::Unsubscribe { session: key.1 };
                self.chat_request(key.0, request, cx, |_, _, _| {});
            }
        }
        let owner = cx.weak_entity();
        let mut views = std::mem::take(&mut self.chat.views);
        views.retain(|pane, _| shown.values().flatten().any(|shown| shown == pane));
        for (key, panes) in &shown {
            let Some(session) = self
                .chat
                .sessions
                .get(key)
                .filter(|session| session.showing())
            else {
                if !self.chat.sessions.get(key).is_some_and(|session| {
                    session.local.is_some()
                        || session.operation.is_some()
                        || session.preparing.is_some()
                }) {
                    for pane in panes {
                        views.remove(pane);
                    }
                }
                continue;
            };
            for pane in panes {
                if views.contains_key(pane) {
                    continue;
                }
                let Some(focus) = self
                    .grids
                    .get(pane)
                    .map(|terminal| terminal.view.read(cx).focus.clone())
                else {
                    continue;
                };
                let style = muxy_ui::text_input::InputStyle::field(&self.theme, &self.metrics);
                let view = cx
                    .new(|cx| ChatView::new(*pane, *key, session, focus, owner.clone(), style, cx));
                views.insert(*pane, view);
            }
        }
        self.chat.views = views;
        let wanting: Vec<_> = self
            .chat
            .sessions
            .iter()
            .filter(|(_, session)| session.wants_subscription())
            .map(|(key, _)| *key)
            .collect();
        for key in wanting {
            self.chat_subscribe(key, cx);
        }
    }

    /// The pane's agent changed: a "Resume in Chat" pane switches once it's Claude.
    pub(super) fn chat_agent_detected(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        cx.notify();
        let claude = self
            .grids
            .get(&pane)
            .is_some_and(|terminal| terminal.view.read(cx).agent == Some(AgentProvider::Claude));
        if claude && self.chat.enter_when_ready.remove(&pane).is_some() {
            self.request_chat_enter(pane, cx);
        }
    }

    /// "Resume in Chat": the pane switches once Claude Code shows up there.
    pub(crate) fn chat_enter_when_ready(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        let expiry = cx.spawn(async move |model, cx| {
            cx.background_executor().timer(ENTER_WHEN_READY).await;
            let _ = model.update(cx, |model, _| {
                if let Some(expiry) = model.chat.enter_when_ready.remove(&pane) {
                    expiry.detach();
                }
            });
        });
        self.chat.enter_when_ready.insert(pane, expiry);
    }

    /// Opens a terminal and starts a new chat there once its shell is idle. A
    /// server without chat gets Claude Code in the terminal instead.
    pub(crate) fn new_claude_chat(&mut self, cx: &mut Context<Self>) {
        if !cfg!(target_os = "macos") {
            if self.open_command_tab(None, "claude", cx).is_some() {
                self.show_notice(
                    tr!("Send your first message to Claude, then switch to Chat.").to_string(),
                    cx,
                );
            }
            return;
        }
        self.new_tab(cx);
        if let Some(pane) = self.active_pane() {
            self.chat_start_when_idle(pane, cx);
        }
    }

    /// Starts a chat in `pane` once its terminal attaches at an idle shell,
    /// showing "Starting…" meanwhile; gives up after [`START_WHEN_IDLE`].
    pub(crate) fn chat_start_when_idle(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        let check = cx.spawn(async move |model, cx| {
            let deadline = Instant::now() + START_WHEN_IDLE;
            // shortcut: polls four times a second; a foreground-process event would avoid it.
            while Instant::now() < deadline {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let done = model
                    .update(cx, |model, cx| model.chat_start_if_idle(pane, cx))
                    .unwrap_or(true);
                if done {
                    return;
                }
            }
            let _ = model.update(cx, |model, cx| {
                if model.chat.start_when_idle.remove(&pane).is_some() {
                    model.chat_stop_starting(pane, cx);
                    model.show_notice(
                        tr!("Chat did not start: the terminal never reached an idle prompt.")
                            .to_string(),
                        cx,
                    );
                }
            });
        });
        self.chat.start_when_idle.insert(pane, check);
        // The pane shows "Starting…" over its terminal from now on.
        cx.notify();
    }

    /// Sends Start once the waiting pane's shell is idle; true when it no longer waits.
    pub(crate) fn chat_start_if_idle(&mut self, pane: PaneId, cx: &mut Context<Self>) -> bool {
        if !self.chat.start_when_idle.contains_key(&pane) {
            return true;
        }
        let Some(key) = self.chat_key(pane).filter(|_| {
            self.grids
                .get(&pane)
                .is_some_and(|terminal| terminal.view.read(cx).attachment().is_some())
        }) else {
            return false;
        };
        let session = self.chat.sessions.entry(key).or_default();
        if !session.starting {
            session.starting = true;
            self.sync_chat(cx);
            cx.notify();
        }
        if !self.shell_idle(pane, cx) {
            return false;
        }
        if let Some(check) = self.chat.start_when_idle.remove(&pane) {
            // This runs inside the task; detaching lets it finish instead of dropping it.
            check.detach();
        }
        if let Some(target) = self.chat.resume_when_idle.remove(&pane) {
            self.chat.sessions.entry(key).or_default();
            self.chat_resume_target(pane, target, cx);
        } else {
            self.chat_start(pane, cx);
        }
        true
    }

    /// Starts a local draft from the pane's idle shell; no provider conversation exists yet.
    pub(crate) fn chat_start(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        self.chat_make_draft(pane, None, false, cx);
    }

    fn chat_stop_starting(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if let Some(session) = self
            .chat_key(pane)
            .and_then(|key| self.chat.sessions.get_mut(&key))
        {
            session.starting = false;
        }
        self.sync_chat(cx);
        cx.notify();
    }

    /// The model and effort last chosen for the pane's project, if any.
    fn chat_choice(&self, pane: PaneId) -> (Option<String>, Option<String>) {
        self.chat_choice_for(pane, ChatProvider::Claude)
    }

    fn chat_choice_for(
        &self,
        pane: PaneId,
        provider: ChatProvider,
    ) -> (Option<String>, Option<String>) {
        let Some(context) = self.opener_context(pane) else {
            return (None, None);
        };
        self.settings
            .composer
            .chat_models
            .iter()
            .find(|choice| {
                choice.server == context.server
                    && choice.project == context.project
                    && choice.provider == provider
            })
            .map_or((None, None), |choice| {
                let some = |text: &str| (!text.is_empty()).then(|| text.to_owned());
                (some(&choice.model), some(&choice.effort))
            })
    }

    /// Remembers the pane's project's model or effort for the next chat there.
    pub(crate) fn save_chat_choice(
        &mut self,
        pane: PaneId,
        model: Option<String>,
        effort: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(context) = self.opener_context(pane) else {
            return;
        };
        let provider = self
            .chat_session(pane)
            .map_or(ChatProvider::Claude, ChatSession::provider);
        let mut settings = self.settings.clone();
        let choices = &mut settings.composer.chat_models;
        let at = choices.iter().position(|choice| {
            choice.server == context.server
                && choice.project == context.project
                && choice.provider == provider
        });
        let mut choice = at.map_or_else(
            || muxy_app_core::settings::ChatChoice {
                server: context.server,
                project: context.project,
                provider,
                model: String::new(),
                effort: String::new(),
            },
            |at| choices.remove(at),
        );
        if let Some(model) = model {
            choice.model = model;
        }
        if let Some(effort) = effort {
            choice.effort = effort;
        }
        // Newest last; the oldest projects' choices go first.
        choices.push(choice);
        let excess = choices
            .len()
            .saturating_sub(muxy_app_core::settings::MAX_CHAT_CHOICES);
        choices.drain(..excess);
        match settings.save_composer(&self.path.with_file_name("settings.toml")) {
            Ok(()) => self.settings.composer = settings.composer,
            Err(error) => self.show_notice(
                tr!("Couldn't save the chat model: %@", error.to_string()).to_string(),
                cx,
            ),
        }
    }

    /// Switches the chat's model, or its effort level when `effort`; a refusal shows in the picker.
    pub(crate) fn chat_choose(
        &mut self,
        pane: PaneId,
        key: ChatKey,
        value: String,
        effort: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.chat.sessions.get_mut(&key)
            && session.local.is_some()
        {
            if session.operation.is_some() || session.preparing.is_some() {
                return;
            }
            session.picker_error = None;
            if effort {
                session.state.effort = Some(value.clone());
                if let Some(local) = session.local.as_mut() {
                    local.selection.effort = Some(value.clone());
                }
            } else {
                let selected = session
                    .capabilities
                    .models
                    .iter()
                    .find(|model| model.alias == value);
                let Some(selected) = selected else { return };
                session.state.model = Some(value.clone());
                session.state.effort = selected.default_effort.clone();
                session.chosen_model = Some(value.clone());
                if let Some(local) = session.local.as_mut() {
                    local.selection.model = Some(value.clone());
                    local.selection.effort.clone_from(&selected.default_effort);
                }
            }
            let selected_effort = session.state.effort.clone().or_else(|| Some(String::new()));
            if effort {
                self.save_chat_choice(pane, None, Some(value), cx);
            } else {
                self.save_chat_choice(pane, Some(value), selected_effort, cx);
            }
            self.notify_chat_views(key, &[], cx);
            return;
        }
        let Some(session) = self.chat.sessions.get_mut(&key) else {
            return;
        };
        session.picker_error = None;
        let generation = session.generation;
        let request = if effort {
            ChatRequest::SetEffort {
                session: key.1,
                generation,
                effort: value.clone(),
            }
        } else {
            ChatRequest::SetModel {
                session: key.1,
                generation,
                model: value.clone(),
            }
        };
        self.notify_chat_views(key, &[], cx);
        self.chat_request(key.0, request, cx, move |model, result, cx| {
            let Some(session) = model.chat.sessions.get_mut(&key) else {
                return;
            };
            match result {
                Ok(_) if effort => model.save_chat_choice(pane, None, Some(value), cx),
                Ok(_) => {
                    session.chosen_model = Some(value.clone());
                    model.save_chat_choice(pane, Some(value), None, cx);
                }
                Err(error) => session.picker_error = Some(error),
            }
            model.notify_chat_views(key, &[], cx);
        });
    }

    /// Opens a file a turn changed the way terminal links open files; on this
    /// Mac, the worker first checks it can't run something.
    pub(crate) fn chat_open_file(&mut self, pane: PaneId, path: &str, cx: &mut Context<Self>) {
        let location = FileLocation {
            path: path.into(),
            line: None,
            column: None,
        };
        if !self.chat_local(pane) {
            return self.open_terminal_link(pane, Target::File(location), cx);
        }
        let result = crate::opener::submit(move || {
            let reveal = runnable(&location.path);
            ChatFile { location, reveal }
        });
        match result {
            Ok(result) => cx
                .spawn(async move |model, cx| {
                    if let Ok(file) = result.recv().await {
                        let _ = model.update(cx, |model, cx| {
                            model.chat_open_path(pane, &file, false, cx);
                        });
                    }
                })
                .detach(),
            Err(error) => self.show_notice(
                tr!("Could not open link: %@", error.to_string()).to_string(),
                cx,
            ),
        }
    }

    /// Opens a file chat names the way terminal links open files, at its
    /// line and column; `reveal`, or a file that could run something, shows it
    /// in Finder instead, on this Mac only.
    pub(crate) fn chat_open_path(
        &mut self,
        pane: PaneId,
        file: &ChatFile,
        reveal: bool,
        cx: &mut Context<Self>,
    ) {
        if !reveal && !file.reveal {
            return self.open_terminal_link(pane, Target::File(file.location.clone()), cx);
        }
        if self.chat_local(pane) {
            cx.reveal_path(&file.location.path);
        }
    }

    fn chat_local(&self, pane: PaneId) -> bool {
        self.opener_context(pane)
            .is_some_and(|context| context.server.is_local())
    }

    /// Sends the paths drawing asked about to the opener worker; their answers
    /// redraw the session's rows when one names a file.
    pub(crate) fn chat_resolve_files(&mut self, key: ChatKey, cx: &mut Context<Self>) {
        let Some(session) = self.chat.sessions.get(&key) else {
            return;
        };
        let (context, wanted) = {
            let mut files = session.files.borrow_mut();
            (files.context.clone(), std::mem::take(&mut files.wanted))
        };
        let Some(context) = context.filter(|_| !wanted.is_empty()) else {
            return;
        };
        let directory: PathBuf = context.directory.clone();
        let texts = wanted.clone();
        let result = crate::opener::submit(move || {
            wanted
                .into_iter()
                .map(|text| {
                    let file = resolve_file(&text, &context);
                    (text, file)
                })
                .collect::<Vec<_>>()
        });
        let Ok(result) = result else {
            session.files.borrow_mut().forget(&texts);
            return;
        };
        cx.spawn(async move |model, cx| {
            let Ok(answers) = result.recv().await else {
                return;
            };
            let _ = model.update(cx, |model, cx| {
                let landed =
                    model.chat.sessions.get(&key).is_some_and(|session| {
                        session.files.borrow_mut().land(&directory, answers)
                    });
                // A link only adds an underline and a cursor, so no row is re-measured.
                if landed {
                    model.notify_chat_views(key, &[], cx);
                }
            });
        })
        .detach();
    }

    /// Where the pane's chat resolves the paths it names: the terminal's directory.
    pub(crate) fn chat_link_context(&self, pane: PaneId, cx: &gpui::App) -> Option<OpenContext> {
        self.terminal(&pane)?.view.read(cx).link_context()
    }

    /// "New Claude chat" is waiting for the pane's terminal to start it.
    pub(crate) fn chat_waiting(&self, pane: PaneId) -> bool {
        self.chat.start_when_idle.contains_key(&pane)
    }

    /// Drops the withdrawn card once it has faded.
    pub(crate) fn chat_forget_withdrawn(&mut self, key: ChatKey, cx: &mut Context<Self>) {
        if let Some(session) = self.chat.sessions.get_mut(&key)
            && session
                .withdrawn
                .as_ref()
                .is_some_and(|(_, at)| at.elapsed() >= WITHDRAWN_FOR)
        {
            session.withdrawn = None;
            self.notify_chat_views(key, &[], cx);
        }
    }
}

pub(super) fn chat_features() -> Vec<Feature> {
    vec![
        Feature::CHAT_PROVIDERS,
        Feature::CHAT_OPERATIONS,
        Feature::CHAT_NATIVE_APPROVALS,
        Feature::CHAT_QUESTIONS,
        Feature::CHAT_PLAN,
    ]
}

#[cfg(test)]
mod refusal_tests {
    use super::*;

    #[test]
    fn codex_discovery_refusal_keeps_its_detail_without_a_claude_diagnosis() {
        let detail = "This Codex environment has account/backend overrides; use Console";
        let message = refusal(ChatRefusal::CliIncompatible, Some(detail.into()));
        assert!(message.contains(detail));
        assert!(!message.contains("Claude"));
    }
}
