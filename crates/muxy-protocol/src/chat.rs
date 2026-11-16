//! Fork: a session's Claude Code conversation shown as chat instead of its terminal.
//!
//! The server owns each session's mode. Clients send `ChatRequest`s, read
//! `ChatReply`s, and keep a copy of the conversation from `ChatBatch`es.
//!
//! A client keeps chat state per `generation` and drops batches from any other
//! generation. A batch with a snapshot naming the client's latest
//! subscription replaces the state, and its `to_rev` becomes the client's
//! revision; other snapshots are dropped. Before that snapshot,
//! ordinary batches are dropped; afterwards a batch applies only when its
//! `from_rev` is the client's revision, and a gap means subscribing again
//! with a new token. Revisions restart at 0 with each generation.
//!
//! Event lists skip events a newer server added, so a batch from a newer
//! build still applies and keeps revisions continuous. A client lists the chat
//! features it can show when it subscribes; the server never sends a newer
//! approval kind or event to a client that didn't list its feature, and
//! instead denies it with a `Notice` or tells the user to switch to Console.

use minicbor::{Decode, Encode};
use serde::{Deserialize, Serialize};

use crate::wire::cbor::open_enum;
use crate::{
    ErrorCode, MAX_CHAT_COMMAND_DESCRIPTION, MAX_CHAT_COMMAND_NAME, MAX_CHAT_COMMANDS,
    MAX_CHAT_EFFORTS, MAX_CHAT_EVENTS, MAX_CHAT_FEATURES, MAX_CHAT_FILE_CHANGES,
    MAX_CHAT_IMAGE_BYTES, MAX_CHAT_IMAGES, MAX_CHAT_IMAGES_BYTES, MAX_CHAT_MODELS, MAX_CHAT_NAME,
    MAX_CHAT_PENDING, MAX_CHAT_PENDING_PROMPTS, MAX_CHAT_PLAN, MAX_CHAT_PREVIEW,
    MAX_CHAT_QUESTION_ENTRIES, MAX_CHAT_QUESTION_PREVIEW, MAX_CHAT_QUESTIONS,
    MAX_CHAT_SNAPSHOT_ITEMS, MAX_CHAT_TEXT, MAX_CHAT_TOOL_PROMPT, OperationId, ServerPath,
    SessionId,
};

open_enum! {
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
    pub enum ChatProvider {
        #[default]
        Claude = 0,
        Codex = 1,
    }
}

/// A native thread and the engine that owns it. An API target need not have a rollout file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatTarget {
    #[n(0)]
    pub provider: ChatProvider,
    #[n(1)]
    pub thread_id: String,
    #[n(2)]
    pub home: ServerPath,
    #[n(3)]
    pub endpoint: Option<ServerPath>,
    #[n(4)]
    pub executable: ServerPath,
    #[n(5)]
    pub engine: Option<ChatEngine>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatEngine {
    #[n(0)]
    pub pid: u32,
    #[n(1)]
    pub started_seconds: u64,
    #[n(2)]
    pub started_micros: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatThread {
    #[n(0)]
    pub target: ChatTarget,
    #[n(1)]
    pub title: String,
    #[n(2)]
    pub updated_at: i64,
    #[n(3)]
    pub busy: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatSelection {
    #[n(0)]
    pub provider: ChatProvider,
    #[n(1)]
    pub model: Option<String>,
    #[n(2)]
    pub effort: Option<String>,
    #[n(3)]
    pub target: Option<ChatTarget>,
    #[n(4)]
    pub launch_context: Option<u64>,
}

open_enum! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
    pub enum SubmissionOutcome {
        Pending = 0,
        Accepted = 1,
        Rejected = 2,
        Uncertain = 3,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatSubmission {
    #[n(0)]
    pub operation: OperationId,
    #[n(1)]
    pub outcome: SubmissionOutcome,
    #[n(2)]
    pub generation: u64,
    #[n(3)]
    pub detail: Option<String>,
}

/// What a client asks of a session's chat. Every request after `Enter` names
/// the generation it was made for, so a request for an older one is refused
/// with `StaleGeneration` instead of reaching a newer conversation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub enum ChatRequest {
    /// Switch the session from its terminal to chat, optionally with a model
    /// alias and effort level from the capabilities.
    #[n(0)]
    Enter {
        #[n(0)]
        session: SessionId,
        #[n(1)]
        model: Option<String>,
        #[n(2)]
        effort: Option<String>,
    },
    /// Return to the terminal, from chat or from a blocked chat.
    #[n(1)]
    Leave {
        #[n(0)]
        session: SessionId,
        #[n(1)]
        generation: u64,
    },
    #[n(2)]
    Send {
        #[n(0)]
        session: SessionId,
        #[n(1)]
        generation: u64,
        #[n(2)]
        text: String,
        #[n(3)]
        images: Vec<ChatImage>,
    },
    #[n(3)]
    Interrupt {
        #[n(0)]
        session: SessionId,
        #[n(1)]
        generation: u64,
    },
    /// Answers the pending approval `request_id`.
    #[n(4)]
    Respond {
        #[n(0)]
        session: SessionId,
        #[n(1)]
        generation: u64,
        #[n(2)]
        request_id: String,
        #[n(3)]
        response: PermissionResponse,
    },
    #[n(5)]
    SetModel {
        #[n(0)]
        session: SessionId,
        #[n(1)]
        generation: u64,
        #[n(2)]
        model: String,
    },
    #[n(6)]
    SetEffort {
        #[n(0)]
        session: SessionId,
        #[n(1)]
        generation: u64,
        #[n(2)]
        effort: String,
    },
    /// Starts or restarts this connection's chat batches for the session.
    /// The first batch is a snapshot naming `subscription`.
    #[n(7)]
    Subscribe {
        #[n(0)]
        session: SessionId,
        #[n(1)]
        subscription: u32,
        /// Chat features this client can show. The server never sends a newer
        /// approval kind or event to a client that didn't list its feature,
        /// and instead denies it with a `Notice` or tells the user to switch
        /// to Console.
        #[n(2)]
        #[cbor(default)]
        features: Vec<crate::Feature>,
    },
    #[n(8)]
    Unsubscribe {
        #[n(0)]
        session: SessionId,
    },
    /// Like `Enter`, but starts a new conversation from the session's idle
    /// shell prompt instead of taking over a running Claude Code. Leave
    /// resumes it in the terminal once a prompt was sent, else types nothing.
    #[n(9)]
    Start {
        #[n(0)]
        session: SessionId,
        #[n(1)]
        model: Option<String>,
        #[n(2)]
        effort: Option<String>,
    },
    /// Promptless discovery in the same launch context as the eventual conversation.
    #[n(10)]
    Discover {
        #[n(0)]
        session: SessionId,
        #[n(1)]
        provider: ChatProvider,
        #[n(2)]
        features: Vec<crate::Feature>,
    },
    /// Opens a local draft only on first send. The operation is also the first prompt's ID.
    #[n(11)]
    Begin {
        #[n(0)]
        session: SessionId,
        #[n(1)]
        host_epoch: u64,
        #[n(2)]
        operation: OperationId,
        #[n(3)]
        selection: Box<ChatSelection>,
        #[n(4)]
        text: String,
        #[n(5)]
        images: Vec<ChatImage>,
        #[n(6)]
        features: Vec<crate::Feature>,
    },
    #[n(12)]
    EnterProvider {
        #[n(0)]
        session: SessionId,
        #[n(1)]
        selection: Box<ChatSelection>,
        #[n(2)]
        features: Vec<crate::Feature>,
    },
    #[n(13)]
    Submit {
        #[n(0)]
        session: SessionId,
        #[n(1)]
        generation: u64,
        #[n(2)]
        operation: OperationId,
        #[n(3)]
        text: String,
        #[n(4)]
        images: Vec<ChatImage>,
    },
    #[n(14)]
    OperationStatus {
        #[n(0)]
        session: SessionId,
        #[n(1)]
        operation: OperationId,
    },
    /// Reserves a server-issued operation ID. Unknown/evicted IDs can never execute.
    #[n(15)]
    PrepareSubmission {
        #[n(0)]
        session: SessionId,
    },
    #[n(16)]
    LeaveProvider {
        #[n(0)]
        session: SessionId,
        #[n(1)]
        generation: u64,
        #[n(2)]
        interrupt: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub enum ChatReply {
    #[n(0)]
    Entered {
        #[n(0)]
        generation: u64,
    },
    /// Back in the terminal; `resumed` says whether Claude Code was started
    /// again there, and `detail` why not.
    #[n(1)]
    Left {
        #[n(0)]
        resumed: bool,
        #[n(1)]
        detail: Option<String>,
    },
    #[n(2)]
    Ok,
    #[n(3)]
    Subscribed {
        #[n(0)]
        generation: u64,
        #[n(1)]
        subscription: u32,
    },
    #[n(4)]
    Refused {
        #[n(0)]
        reason: ChatRefusal,
        #[n(1)]
        detail: Option<String>,
    },
    #[n(5)]
    Discovered {
        #[n(0)]
        host_epoch: u64,
        #[n(1)]
        provider: ChatProvider,
        #[n(2)]
        capabilities: ChatCapabilities,
        #[n(3)]
        model: Option<String>,
        #[n(4)]
        effort: Option<String>,
        #[n(5)]
        threads: Vec<ChatThread>,
        #[n(6)]
        launch_context: Option<u64>,
    },
    #[n(6)]
    Submission(#[n(0)] ChatSubmission),
    #[n(7)]
    Prepared {
        #[n(0)]
        host_epoch: u64,
        #[n(1)]
        operation: OperationId,
    },
}

open_enum! {
    /// Why the server refused a chat request.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
    pub enum ChatRefusal {
        StaleGeneration = 0,
        Busy = 1,
        UnsupportedArguments = 2,
        UnsupportedEnvironment = 3,
        PersistenceDisabled = 4,
        NoClaudeSession = 5,
        NoShell = 6,
        ConversationInUse = 7,
        PlatformUnsupported = 8,
        CliIncompatible = 9,
        SpawnFailed = 10,
        StartupTimeout = 11,
        NotChat = 12,
        ApprovalGone = 13,
        TooManyApprovals = 14,
    }
}

open_enum! {
    /// Which view a session's server-side owner is in; sent as
    /// `MetadataEvent::ChatMode` with the current generation.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
    pub enum ChatMode {
        #[default]
        Console = 0,
        Transitioning = 1,
        Chat = 2,
        /// Claude Code's headless process could not be confirmed stopped.
        Blocked = 3,
    }
}

/// An image sent with a prompt, inline. Its bytes must start with its type's
/// signature.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatImage {
    #[n(0)]
    pub mime: ChatImageType,
    #[n(1)]
    #[cbor(with = "crate::wire::cbor::bytes")]
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub enum ChatImageType {
    #[n(0)]
    Png,
    #[n(1)]
    Jpeg,
    #[n(2)]
    Gif,
    #[n(3)]
    Webp,
}

impl ChatImageType {
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }

    /// Whether `bytes` start with this type's signature.
    pub fn matches(self, bytes: &[u8]) -> bool {
        match self {
            Self::Png => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            Self::Jpeg => bytes.starts_with(b"\xff\xd8\xff"),
            Self::Gif => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
            Self::Webp => {
                bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP"
            }
        }
    }
}

/// The answer to a pending approval, matching its `PermissionKind`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub enum PermissionResponse {
    #[n(0)]
    Tool(#[n(0)] ToolDecision),
    #[n(1)]
    Questions(#[n(0)] Vec<ChatAnswer>),
    #[n(2)]
    Plan(#[n(0)] PlanDecision),
    /// Declines any kind of approval, including ones this client can't show;
    /// the server answers the CLI with deny.
    #[n(3)]
    Dismiss {
        #[n(0)]
        message: String,
    },
    /// An opaque choice offered by the native provider, validated by the server.
    #[n(4)]
    NativeChoice {
        #[n(0)]
        choice: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub enum ToolDecision {
    #[n(0)]
    Allow,
    /// Allow and add the rule the prompt's `always` describes.
    #[n(1)]
    AllowAlways,
    #[n(2)]
    Deny {
        #[n(0)]
        message: String,
        /// True ends the turn (the CLI's `interrupt: true`); false lets the
        /// model carry on.
        #[n(1)]
        #[cbor(default)]
        interrupt: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub enum PlanDecision {
    #[n(0)]
    Approve {
        /// The permission mode to continue in; `None` keeps the CLI's default
        /// after leaving plan mode.
        #[n(0)]
        #[cbor(default)]
        mode: Option<PermissionMode>,
    },
    #[n(1)]
    KeepPlanning {
        #[n(0)]
        feedback: String,
    },
}

/// The answer to one question, by the question's text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatAnswer {
    #[n(0)]
    pub question: String,
    /// The chosen option labels, or the user's own text; the server joins
    /// them the way the CLI expects.
    #[n(1)]
    pub answers: Vec<String>,
}

/// Chat events for one session, from revision `from_rev` to `to_rev`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatBatch {
    #[n(0)]
    pub session: SessionId,
    #[n(1)]
    pub generation: u64,
    #[n(2)]
    pub from_rev: u64,
    #[n(3)]
    pub to_rev: u64,
    #[n(4)]
    #[cbor(with = "crate::wire::cbor::known")]
    pub events: Vec<ChatEvent>,
    /// When present, the batch carries no events and replaces the client's
    /// state, under the baseline rules in the module docs.
    #[n(5)]
    #[cbor(default)]
    pub snapshot: Option<Box<ChatSnapshot>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub enum ChatEvent {
    // Index 0 was the snapshot, which now travels in `ChatBatch::snapshot`.
    /// Adds an item or replaces the one with its id.
    #[n(1)]
    Upsert(#[n(0)] ChatItem),
    /// Appends streamed text to an item.
    #[n(2)]
    Delta {
        #[n(0)]
        item_id: String,
        #[n(1)]
        text: String,
    },
    #[n(3)]
    State(#[n(0)] ChatState),
    #[n(4)]
    PermissionAdded(#[n(0)] PermissionPrompt),
    #[n(5)]
    PermissionRemoved {
        #[n(0)]
        request_id: String,
        #[n(1)]
        reason: PermissionRemoved,
    },
    /// The server lost track of this subscriber; subscribe again.
    #[n(6)]
    Resync,
    #[n(7)]
    Error {
        #[n(0)]
        text: String,
    },
    /// Claude Code exited on its own; `reason` ends with its last error output.
    #[n(8)]
    Exited {
        #[n(0)]
        reason: String,
    },
    #[n(9)]
    Blocked {
        #[n(0)]
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatSnapshot {
    #[n(0)]
    pub subscription: u32,
    /// The latest turns' items, oldest first.
    #[n(1)]
    #[cbor(with = "crate::wire::cbor::known")]
    pub items: Vec<ChatItem>,
    #[n(2)]
    pub state: ChatState,
    #[n(3)]
    pub capabilities: ChatCapabilities,
    #[n(4)]
    #[cbor(with = "crate::wire::cbor::known")]
    pub pending: Vec<PermissionPrompt>,
    /// Older turns exist that the snapshot left out.
    #[n(5)]
    pub has_earlier: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatState {
    #[n(0)]
    pub busy: bool,
    /// No turn, approval or background task is running.
    #[n(1)]
    pub quiescent: bool,
    #[n(2)]
    pub model: Option<String>,
    #[n(3)]
    pub effort: Option<String>,
    #[n(4)]
    pub permission_mode: PermissionMode,
    #[n(5)]
    pub context_percent: Option<u8>,
    #[n(6)]
    #[cbor(default)]
    #[serde(default)]
    pub provider: Option<ChatProvider>,
    #[n(7)]
    #[cbor(default)]
    #[serde(default)]
    pub shared_engine: bool,
    #[n(8)]
    #[cbor(default)]
    #[serde(default)]
    pub target: Option<ChatTarget>,
}

open_enum! {
    /// Claude Code's permission mode.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
    pub enum PermissionMode {
        #[default]
        Default = 0,
        AcceptEdits = 1,
        Plan = 2,
        Auto = 3,
        BypassPermissions = 4,
    }
}

open_enum! {
    /// Why a pending approval went away.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
    pub enum PermissionRemoved {
        Answered = 0,
        Cancelled = 1,
        Expired = 2,
    }
}

/// The models and effort levels Claude Code offers, from its startup handshake.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatCapabilities {
    #[n(0)]
    pub models: Vec<ChatModel>,
    /// The slash commands and skills a prompt can start with, as Claude Code
    /// lists them; empty from an older server.
    #[n(1)]
    #[cbor(default)]
    #[serde(default)]
    pub commands: Vec<ChatCommand>,
}

/// A slash command a prompt can start with, such as `compact` or `codex:review`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatCommand {
    /// Without the leading `/`; printable ASCII without spaces, at most
    /// `MAX_CHAT_COMMAND_NAME` bytes.
    #[n(0)]
    pub name: String,
    /// At most `MAX_CHAT_COMMAND_DESCRIPTION` bytes; the server shortens longer ones.
    #[n(1)]
    pub description: String,
    /// A skill or custom command rather than one built into Claude Code.
    #[n(2)]
    pub skill: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatModel {
    /// What `Enter` and `SetModel` name, such as `opus`.
    #[n(0)]
    pub alias: String,
    #[n(1)]
    pub label: String,
    /// The effort levels this model accepts; empty when it has none.
    #[n(2)]
    pub efforts: Vec<String>,
    /// None preserves the legacy Claude image behavior.
    #[n(3)]
    #[cbor(default)]
    #[serde(default)]
    pub images: Option<bool>,
    #[n(4)]
    #[cbor(default)]
    #[serde(default)]
    pub default_effort: Option<String>,
}

/// One entry of the conversation. Ids are stable across snapshots.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatItem {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub turn_id: String,
    /// Orders items within the generation.
    #[n(2)]
    pub seq: u64,
    #[n(3)]
    pub kind: ChatItemKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub enum ChatItemKind {
    #[n(0)]
    User {
        #[n(0)]
        text: String,
    },
    #[n(1)]
    Assistant {
        #[n(0)]
        markdown: String,
        #[n(1)]
        final_in_turn: bool,
    },
    #[n(2)]
    Thinking {
        #[n(0)]
        text: String,
    },
    #[n(3)]
    Tool {
        #[n(0)]
        name: String,
        #[n(1)]
        summary: String,
        #[n(2)]
        input_preview: String,
        /// `None` while the tool runs.
        #[n(3)]
        result: Option<String>,
        #[n(4)]
        subagent_count: u32,
        /// The tool's result is an error (`tool_result.is_error`).
        #[n(5)]
        #[cbor(default)]
        is_error: bool,
    },
    /// Items left out to keep the conversation small.
    #[n(4)]
    Folded {
        #[n(0)]
        count: u32,
    },
    #[n(5)]
    TurnEnd {
        #[n(0)]
        duration_ms: u64,
        #[n(1)]
        is_error: bool,
        /// Files the turn's edits changed, at most `MAX_CHAT_FILE_CHANGES`.
        #[n(2)]
        #[cbor(default)]
        changed: Vec<FileChange>,
    },
    #[n(6)]
    Notice {
        #[n(0)]
        text: String,
    },
}

/// A file a turn changed, and how many lines its edits added and removed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct FileChange {
    /// As Claude Code reported it, usually absolute; at most `MAX_CHAT_PREVIEW` bytes.
    #[n(0)]
    pub path: String,
    #[n(1)]
    pub added: u32,
    #[n(2)]
    pub removed: u32,
}

/// An approval Claude Code is waiting for. It is never shortened: the
/// server refuses one over its cap instead.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct PermissionPrompt {
    #[n(0)]
    pub request_id: String,
    #[n(1)]
    pub tool_use_id: Option<String>,
    #[n(2)]
    pub kind: PermissionKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub enum PermissionKind {
    #[n(0)]
    Tool {
        #[n(0)]
        tool: String,
        #[n(1)]
        title: String,
        #[n(2)]
        detail: String,
        /// The rule "Always allow" adds; `None` hides it.
        #[n(3)]
        always: Option<AlwaysAllow>,
    },
    #[n(1)]
    Questions {
        #[n(0)]
        questions: Vec<ChatQuestion>,
    },
    #[n(2)]
    Plan {
        #[n(0)]
        markdown: String,
    },
    #[n(3)]
    Native {
        #[n(0)]
        title: String,
        #[n(1)]
        detail: String,
        #[n(2)]
        choices: Vec<ChatApprovalChoice>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatApprovalChoice {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub label: String,
    #[n(2)]
    pub scope: String,
}

/// The rule an "Always allow" answer adds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct AlwaysAllow {
    /// The rule shown to the user, such as `Bash(touch rule.txt)`.
    #[n(0)]
    pub label: String,
    /// Answering writes the project's `.claude/settings.local.json`, so the UI
    /// must say so.
    #[n(1)]
    pub saved_to_settings: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatQuestion {
    #[n(0)]
    pub question: String,
    #[n(1)]
    pub header: String,
    #[n(2)]
    pub options: Vec<ChatQuestionOption>,
    #[n(3)]
    pub multi_select: bool,
    #[n(4)]
    #[cbor(default)]
    #[serde(default)]
    pub allow_other: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct ChatQuestionOption {
    #[n(0)]
    pub label: String,
    #[n(1)]
    pub description: String,
    /// Monospace text shown beside the options when this one is focused, such as a
    /// mockup or snippet; empty for none. Unlike the rest of an approval, the server
    /// shortens it to `MAX_CHAT_QUESTION_PREVIEW` bytes: it only illustrates, and the
    /// answer names the label.
    #[n(2)]
    #[cbor(default)]
    #[serde(default)]
    pub preview: String,
}

impl ChatCommand {
    /// Whether `name` can follow a `/` as one typed word within its cap.
    pub fn valid_name(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= MAX_CHAT_COMMAND_NAME
            && name.bytes().all(|byte| byte.is_ascii_graphic())
    }
}

fn check(valid: bool) -> Result<(), ErrorCode> {
    if valid {
        Ok(())
    } else {
        Err(ErrorCode::BadRequest)
    }
}

fn name(text: &str) -> bool {
    text.len() <= MAX_CHAT_NAME
}

fn optional_name(text: Option<&String>) -> bool {
    text.is_none_or(|text| name(text))
}

fn detail(text: Option<&String>) -> bool {
    text.is_none_or(|text| text.len() <= MAX_CHAT_PREVIEW)
}

impl ChatRequest {
    pub fn session(&self) -> SessionId {
        match self {
            Self::Enter { session, .. }
            | Self::Start { session, .. }
            | Self::Leave { session, .. }
            | Self::Send { session, .. }
            | Self::Interrupt { session, .. }
            | Self::Respond { session, .. }
            | Self::SetModel { session, .. }
            | Self::SetEffort { session, .. }
            | Self::Subscribe { session, .. }
            | Self::Unsubscribe { session }
            | Self::Discover { session, .. }
            | Self::Begin { session, .. }
            | Self::EnterProvider { session, .. }
            | Self::Submit { session, .. }
            | Self::OperationStatus { session, .. }
            | Self::PrepareSubmission { session }
            | Self::LeaveProvider { session, .. } => *session,
        }
    }

    pub fn validate(&self) -> Result<(), ErrorCode> {
        check(match self {
            Self::Enter { model, effort, .. } | Self::Start { model, effort, .. } => {
                optional_name(model.as_ref()) && optional_name(effort.as_ref())
            }
            Self::Send { text, images, .. } | Self::Submit { text, images, .. } => {
                valid_prompt(text, images)
            }
            Self::Respond {
                request_id,
                response,
                ..
            } => {
                name(request_id)
                    && match response {
                        PermissionResponse::Tool(ToolDecision::Deny { message, .. })
                        | PermissionResponse::Dismiss { message } => {
                            message.len() <= MAX_CHAT_PREVIEW
                        }
                        PermissionResponse::Tool(
                            ToolDecision::Allow | ToolDecision::AllowAlways,
                        )
                        | PermissionResponse::Plan(PlanDecision::Approve { .. }) => true,
                        PermissionResponse::Questions(answers) => {
                            answers.len() <= MAX_CHAT_QUESTION_ENTRIES
                                && answers
                                    .iter()
                                    .all(|answer| answer.answers.len() <= MAX_CHAT_QUESTION_ENTRIES)
                                && answers
                                    .iter()
                                    .map(|answer| {
                                        answer.question.len()
                                            + answer.answers.iter().map(String::len).sum::<usize>()
                                    })
                                    .sum::<usize>()
                                    <= MAX_CHAT_QUESTIONS
                        }
                        PermissionResponse::Plan(PlanDecision::KeepPlanning { feedback }) => {
                            feedback.len() <= MAX_CHAT_TEXT
                        }
                        PermissionResponse::NativeChoice { choice } => {
                            !choice.is_empty() && name(choice)
                        }
                    }
            }
            Self::SetModel { model: text, .. } | Self::SetEffort { effort: text, .. } => name(text),
            Self::Subscribe { features, .. } => features.len() <= MAX_CHAT_FEATURES,
            Self::Discover {
                provider, features, ..
            } => provider.supported() && provider_features(*provider, features),
            Self::EnterProvider {
                selection,
                features,
                ..
            } => selection.valid() && provider_features(selection.provider, features),
            Self::Begin {
                selection,
                text,
                images,
                features,
                ..
            } => {
                selection.valid()
                    && provider_features(selection.provider, features)
                    && features.contains(&crate::Feature::CHAT_OPERATIONS)
                    && valid_prompt(text, images)
                    && (!text.is_empty() || !images.is_empty())
            }
            Self::OperationStatus { .. }
            | Self::PrepareSubmission { .. }
            | Self::LeaveProvider { .. }
            | Self::Leave { .. }
            | Self::Interrupt { .. }
            | Self::Unsubscribe { .. } => true,
        })
    }
}

impl ChatReply {
    pub fn validate(&self) -> Result<(), ErrorCode> {
        check(match self {
            Self::Left { detail: text, .. } | Self::Refused { detail: text, .. } => {
                detail(text.as_ref())
            }
            Self::Entered { .. } | Self::Ok | Self::Subscribed { .. } | Self::Prepared { .. } => {
                true
            }
            Self::Discovered {
                provider,
                capabilities,
                model,
                effort,
                threads,
                ..
            } => {
                provider.supported()
                    && capabilities.validate().is_ok()
                    && optional_name(model.as_ref())
                    && optional_name(effort.as_ref())
                    && threads.len() <= 200
                    && threads.iter().all(|thread| {
                        thread.title.len() <= MAX_CHAT_PREVIEW
                            && ChatSelection {
                                provider: *provider,
                                model: None,
                                effort: None,
                                target: Some(thread.target.clone()),
                                launch_context: None,
                            }
                            .valid()
                    })
            }
            Self::Submission(status) => {
                matches!(
                    status.outcome,
                    SubmissionOutcome::Pending
                        | SubmissionOutcome::Accepted
                        | SubmissionOutcome::Rejected
                        | SubmissionOutcome::Uncertain
                ) && detail(status.detail.as_ref())
            }
        })
    }
}

impl ChatBatch {
    /// The size of a batch's encoding is checked where it is encoded: an
    /// ordinary batch within `MAX_CHAT_BATCH_BYTES` and a snapshot within
    /// `MAX_CHAT_SNAPSHOT_BYTES`.
    pub fn validate(&self) -> Result<(), ErrorCode> {
        check(
            self.from_rev <= self.to_rev
                && self.events.len() <= MAX_CHAT_EVENTS
                && (self.snapshot.is_none() || self.events.is_empty()),
        )?;
        if let Some(snapshot) = &self.snapshot {
            snapshot.capabilities.validate()?;
            snapshot.state.validate()?;
            check(
                snapshot.items.len() <= MAX_CHAT_SNAPSHOT_ITEMS
                    && snapshot.pending.len() <= MAX_CHAT_PENDING_PROMPTS,
            )?;
            snapshot.items.iter().try_for_each(ChatItem::validate)?;
            let mut pending = 0;
            for prompt in &snapshot.pending {
                pending += prompt.validate()?;
            }
            check(pending <= MAX_CHAT_PENDING)?;
        }
        self.events.iter().try_for_each(ChatEvent::validate)
    }
}

impl ChatEvent {
    pub fn validate(&self) -> Result<(), ErrorCode> {
        match self {
            Self::Upsert(item) => item.validate(),
            Self::Delta { item_id, text } => check(name(item_id) && text.len() <= MAX_CHAT_TEXT),
            Self::State(state) => state.validate(),
            Self::PermissionAdded(prompt) => prompt.validate().map(drop),
            Self::PermissionRemoved { request_id, .. } => check(name(request_id)),
            Self::Error { text }
            | Self::Exited { reason: text }
            | Self::Blocked { reason: text } => check(text.len() <= MAX_CHAT_PREVIEW),
            Self::Resync => Ok(()),
        }
    }
}

impl ChatState {
    pub fn validate(&self) -> Result<(), ErrorCode> {
        check(
            optional_name(self.model.as_ref())
                && optional_name(self.effort.as_ref())
                && self.context_percent.is_none_or(|percent| percent <= 100)
                && self.provider.is_none_or(ChatProvider::supported)
                && self.target.as_ref().is_none_or(|target| {
                    ChatSelection {
                        provider: self.provider.unwrap_or_default(),
                        model: None,
                        effort: None,
                        target: Some(target.clone()),
                        launch_context: None,
                    }
                    .valid()
                }),
        )
    }
}

impl ChatCapabilities {
    pub fn validate(&self) -> Result<(), ErrorCode> {
        check(
            self.models.len() <= MAX_CHAT_MODELS
                && self.models.iter().all(|model| {
                    name(&model.alias)
                        && name(&model.label)
                        && model.efforts.len() <= MAX_CHAT_EFFORTS
                        && model.efforts.iter().all(|effort| name(effort))
                        && model
                            .default_effort
                            .as_ref()
                            .is_none_or(|effort| model.efforts.contains(effort))
                })
                && self.commands.len() <= MAX_CHAT_COMMANDS
                && self.commands.iter().all(|command| {
                    ChatCommand::valid_name(&command.name)
                        && command.description.len() <= MAX_CHAT_COMMAND_DESCRIPTION
                }),
        )
    }
}

impl ChatItem {
    pub fn validate(&self) -> Result<(), ErrorCode> {
        check(
            name(&self.id)
                && name(&self.turn_id)
                && match &self.kind {
                    ChatItemKind::User { text }
                    | ChatItemKind::Thinking { text }
                    | ChatItemKind::Notice { text }
                    | ChatItemKind::Assistant { markdown: text, .. } => text.len() <= MAX_CHAT_TEXT,
                    ChatItemKind::Tool {
                        name: tool,
                        summary,
                        input_preview,
                        result,
                        ..
                    } => {
                        name(tool)
                            && summary.len() <= MAX_CHAT_PREVIEW
                            && input_preview.len() <= MAX_CHAT_PREVIEW
                            && detail(result.as_ref())
                    }
                    ChatItemKind::TurnEnd { changed, .. } => {
                        changed.len() <= MAX_CHAT_FILE_CHANGES
                            && changed
                                .iter()
                                .all(|change| change.path.len() <= MAX_CHAT_PREVIEW)
                    }
                    ChatItemKind::Folded { .. } => true,
                },
        )
    }
}

impl PermissionPrompt {
    /// Returns the bytes the prompt counts against `MAX_CHAT_PENDING`.
    pub fn validate(&self) -> Result<usize, ErrorCode> {
        let (size, limit) = match &self.kind {
            PermissionKind::Tool {
                tool,
                title,
                detail,
                always,
            } => {
                check(
                    name(tool)
                        && always
                            .as_ref()
                            .is_none_or(|rule| rule.label.len() <= MAX_CHAT_PREVIEW),
                )?;
                (title.len() + detail.len(), MAX_CHAT_TOOL_PROMPT)
            }
            PermissionKind::Questions { questions } => {
                check(
                    questions.len() <= MAX_CHAT_QUESTION_ENTRIES
                        && questions.iter().all(|question| {
                            question.options.len() <= MAX_CHAT_QUESTION_ENTRIES
                                && question
                                    .options
                                    .iter()
                                    .all(|option| option.preview.len() <= MAX_CHAT_QUESTION_PREVIEW)
                        }),
                )?;
                (
                    questions
                        .iter()
                        .map(|question| {
                            question.question.len()
                                + question.header.len()
                                + question
                                    .options
                                    .iter()
                                    .map(|option| {
                                        option.label.len()
                                            + option.description.len()
                                            + option.preview.len()
                                    })
                                    .sum::<usize>()
                        })
                        .sum(),
                    MAX_CHAT_QUESTIONS,
                )
            }
            PermissionKind::Plan { markdown } => (markdown.len(), MAX_CHAT_PLAN),
            PermissionKind::Native {
                title,
                detail,
                choices,
            } => {
                check(
                    !choices.is_empty()
                        && choices.len() <= 8
                        && choices.iter().enumerate().all(|(at, choice)| {
                            !choice.id.is_empty()
                                && name(&choice.id)
                                && name(&choice.label)
                                && choice.scope.len() <= MAX_CHAT_PREVIEW
                                && choices[..at].iter().all(|other| other.id != choice.id)
                        }),
                )?;
                (
                    title.len()
                        + detail.len()
                        + choices
                            .iter()
                            .map(|choice| choice.id.len() + choice.label.len() + choice.scope.len())
                            .sum::<usize>(),
                    MAX_CHAT_TOOL_PROMPT,
                )
            }
        };
        check(name(&self.request_id) && optional_name(self.tool_use_id.as_ref()) && size <= limit)?;
        Ok(size)
    }
}

impl ChatProvider {
    pub fn supported(self) -> bool {
        matches!(self, Self::Claude | Self::Codex)
    }
}

fn provider_features(provider: ChatProvider, features: &[crate::Feature]) -> bool {
    features.len() <= MAX_CHAT_FEATURES
        && features.contains(&crate::Feature::CHAT_PROVIDERS)
        && (provider != ChatProvider::Codex
            || (features.contains(&crate::Feature::CHAT_NATIVE_APPROVALS)
                && features.contains(&crate::Feature::CHAT_QUESTIONS)))
}

impl ChatSelection {
    fn valid(&self) -> bool {
        self.provider.supported()
            && optional_name(self.model.as_ref())
            && optional_name(self.effort.as_ref())
            && self.target.as_ref().is_none_or(|target| {
                target.provider == self.provider
                    && uuid::Uuid::parse_str(&target.thread_id).is_ok()
                    && crate::validate_absolute_path(&target.home).is_ok()
                    && crate::validate_absolute_path(&target.executable).is_ok()
                    && target
                        .endpoint
                        .as_ref()
                        .is_none_or(|path| crate::validate_absolute_path(path).is_ok())
                    && target
                        .engine
                        .as_ref()
                        .is_none_or(|engine| engine.pid > 0 && engine.started_micros < 1_000_000)
            })
    }
}

fn valid_prompt(text: &str, images: &[ChatImage]) -> bool {
    text.len() <= MAX_CHAT_TEXT
        && images.len() <= MAX_CHAT_IMAGES
        && images.iter().map(|image| image.bytes.len()).sum::<usize>() <= MAX_CHAT_IMAGES_BYTES
        && images.iter().all(|image| {
            image.bytes.len() <= MAX_CHAT_IMAGE_BYTES && image.mime.matches(&image.bytes)
        })
}
