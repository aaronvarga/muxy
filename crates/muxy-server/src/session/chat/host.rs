//! The session owner's chat state machine: `Console`, `Transitioning`,
//! `Chat{generation}` and `Blocked{generation}`.
//!
//! It runs on the owner thread and never blocks there. Each transition (Enter,
//! Leave, the rollback inside Enter, the Leave after an unexpected exit) runs
//! as one job on its own thread, under a new transition token, and reports an
//! [`Outcome`]; a completion or supervisor event with another token is
//! ignored. Every job step has a deadline inside the transition's 30 s (and
//! Enter's rollback 10 s more of its own), so a session always leaves
//! `Transitioning`. While the mode is anything but
//! `Console`, the input gate stays closed: only the server's own recovery
//! line reaches the shell.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime};

use muxy_protocol::{
    AgentState, ChatEvent, ChatImage, ChatMode, ChatProvider, ChatRefusal, ChatReply, ChatRequest,
    ChatSelection, ChatSubmission, Feature, MAX_CHAT_NAME, MAX_CHAT_PREVIEW,
    MAX_CHAT_SNAPSHOT_BYTES, MetadataEvent, OperationId, SessionInfo, SubmissionOutcome,
};
use muxy_terminal::pty::Pty;
use serde_json::{Value, json};

use super::context::{self, ClaudeContext, ContextError};
use super::env::ChildEnv;
use super::flush::{FlushError, FlushTarget, gate_and_flush};
use super::normalize::clip;
use super::reserve::{Reservation, Reservations, ReserveError};
use super::state::{Action, ChatLog, RespondError};
use super::submissions::{Start, Submissions, fingerprint};
use super::supervisor::{
    Completion, HandshakeError, INITIALIZE_TIMEOUT, Launch, StdinError, StdoutItem, Supervisor,
    SupervisorEvent, TERM_GIVE_UP, TERM_GRACE, Termination, run_version, spawn_job,
    terminate_group, wait_foreground_is,
};
use super::{args, codex, env, recovery, transcript};
use crate::connection::Connection;
use crate::session::OwnerEvent;
use crate::session::input_gate::InputGate;
use crate::session::{ChatCommand, ChatReplyTo};

/// The Claude Code version Milestone 0 tested; others get a notice.
const TESTED_VERSION: &str = "2.1.319";
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the input writer may take to go idle.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
const TYPE_TIMEOUT: Duration = Duration::from_secs(2);
const FOREGROUND_TIMEOUT: Duration = Duration::from_secs(1);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
/// Rollback's own budget for stopping the child and typing recovery, apart
/// from Enter's deadline, which a late initialize timeout may have spent.
const ROLLBACK_BUDGET: Duration = Duration::from_secs(10);
/// How long a shell gets, once Claude's TUI is gone, to show it would run on
/// into another command.
const SETTLE: Duration = Duration::from_millis(150);
/// How long a shell gets to take the ^C that clears its line before the
/// recovery line is typed.
const CLEAR_SETTLE: Duration = Duration::from_millis(150);
/// Stdout items handled per wake, so the PTY isn't starved.
const DRAIN: usize = 256;
/// The most stderr an `Exited` reason carries.
const STDERR_SHOWN: usize = 2048;

/// Finds the Claude TUI in a session (shell, foreground group, time left);
/// injectable so tests need no real Claude.
pub(crate) type Resolve = Arc<
    dyn Fn(
            u32,
            Option<u32>,
            Duration,
            &[(std::ffi::OsString, std::ffi::OsString)],
        ) -> Result<ClaudeContext, ContextError>
        + Send
        + Sync,
>;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Timeouts {
    pub(crate) initialize: Duration,
    pub(crate) term_grace: Duration,
    pub(crate) term_give_up: Duration,
    pub(crate) transition: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            initialize: INITIALIZE_TIMEOUT,
            term_grace: TERM_GRACE,
            term_give_up: TERM_GIVE_UP,
            transition: Duration::from_secs(30),
        }
    }
}

/// What every session's chat host shares, from the registry.
#[derive(Clone)]
pub(crate) struct Services {
    pub(crate) reservations: Arc<Reservations>,
    pub(crate) resolve: Resolve,
    /// Reads an idle shell for a new conversation (Start).
    pub(crate) resolve_shell: Resolve,
    pub(crate) timeouts: Timeouts,
}

impl Default for Services {
    fn default() -> Self {
        Self {
            reservations: Arc::default(),
            resolve: Arc::new(context::resolve),
            resolve_shell: Arc::new(context::resolve_shell),
            timeouts: Timeouts::default(),
        }
    }
}

impl std::fmt::Debug for Services {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Services")
            .field("reservations", &self.reservations)
            .field("timeouts", &self.timeouts)
            .finish_non_exhaustive()
    }
}

/// An owner event for the chat host.
#[derive(Debug)]
pub(crate) enum Event {
    Supervisor(SupervisorEvent),
    Done(Completion<Outcome>),
    Codex(Box<codex::Event>),
    Submission {
        operation: OperationId,
        outcome: SubmissionOutcome,
        detail: Option<String>,
    },
    BeginReady {
        operation: OperationId,
        result: Box<ChatReply>,
        text: String,
        images: Vec<ChatImage>,
    },
}

impl From<SupervisorEvent> for OwnerEvent {
    fn from(event: SupervisorEvent) -> Self {
        Self::Chat(Event::Supervisor(event))
    }
}

impl From<Completion<Outcome>> for OwnerEvent {
    fn from(completion: Completion<Outcome>) -> Self {
        Self::Chat(Event::Done(completion))
    }
}

/// How a transition ended.
#[derive(Debug)]
pub(crate) enum Outcome {
    Entered(Box<Entered>),
    /// Back in the terminal; the child, if any, is confirmed gone.
    Console(ChatReply),
    /// The headless child could not be confirmed stopped.
    Blocked {
        child: Box<Child>,
        reason: String,
        reply: ChatReply,
    },
}

#[derive(Debug)]
pub(crate) struct Entered {
    child: Child,
    initialize: Value,
    skipped: Vec<StdoutItem>,
    history: Vec<Value>,
    notices: Vec<String>,
    model: Option<String>,
    effort: Option<String>,
    /// The model alias the child started with, for checking efforts.
    alias: String,
}

/// What brings Claude's TUI back in the terminal.
#[derive(Debug)]
struct Resume {
    config_dir: PathBuf,
    shell: u32,
    cwd: PathBuf,
    env: ChildEnv,
    prefix: Vec<String>,
    console_args: Vec<String>,
    uuid: String,
    /// When Enter began stopping the TUI; shell children started since then
    /// are a command the shell ran on into.
    since: SystemTime,
    /// The config directory of a new conversation, which is resumed only
    /// once it has a transcript.
    fresh: Option<PathBuf>,
    /// Enter replaced a TUI that had saved nothing: with nothing sent,
    /// recovery relaunches it as it was, without `--resume`.
    relaunch: bool,
}

/// The headless child and the conversation it holds. Dropped while the
/// child may run, it stops the child's group on a thread of its own.
#[derive(Debug)]
pub(crate) struct Child {
    supervisor: Option<Supervisor>,
    reservation: Option<Reservation>,
    resume: Resume,
    timeouts: Timeouts,
}

impl Drop for Child {
    fn drop(&mut self) {
        let Some(supervisor) = self.supervisor.take() else {
            return;
        };
        let timeouts = self.timeouts;
        let stop = move |(mut supervisor, reservation): (Supervisor, Option<Reservation>)| {
            supervisor.close_stdin();
            let stopped =
                terminate_group(supervisor.pid(), timeouts.term_grace, timeouts.term_give_up);
            supervisor.shutdown(Instant::now() + SHUTDOWN_TIMEOUT);
            if stopped != Termination::Confirmed {
                // It may still use the conversation, so it stays reserved.
                std::mem::forget(reservation);
            }
        };
        // The thread takes its work from here, so a failed spawn leaves it to us.
        let work = Arc::new(Mutex::new(Some((supervisor, self.reservation.take()))));
        let taken = work.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("chat-cleanup".into())
            .spawn(move || take(&taken).map(stop))
        {
            log::error!("could not start stopping Claude Code, stopping it here: {error}");
            // Blocks this thread for up to `term_give_up`; a last resort.
            take(&work).map(stop);
        }
    }
}

/// Takes what a thread was handed, if it is still there.
fn take<T>(slot: &Mutex<Option<T>>) -> Option<T> {
    slot.lock().unwrap_or_else(PoisonError::into_inner).take()
}

struct Live {
    child: Child,
    next_request: u64,
    controls: HashMap<String, Control>,
    alias: String,
}

struct Control {
    kind: ControlKind,
    reply: ChatReplyTo,
}

enum ControlKind {
    Model(String),
    Effort(String),
    Other,
}

enum Mode {
    Console,
    Transitioning { reply: ChatReplyTo },
    Chat(Box<Live>),
    Blocked { child: Box<Child> },
}

struct Subscriber {
    outbox: Weak<Connection>,
    /// Approval kinds this subscriber can answer; one no subscriber lists is denied.
    features: Vec<Feature>,
}

pub(crate) struct Host {
    info: SessionInfo,
    services: Services,
    events: Sender<OwnerEvent>,
    gate: Arc<InputGate>,
    target: Option<Arc<FlushTarget>>,
    token: u64,
    generation: u64,
    mode: Mode,
    /// The current generation's chat, kept while it leaves or is blocked.
    log: Option<ChatLog>,
    subscribers: Vec<Subscriber>,
    mode_changed: bool,
    activity: Option<AgentState>,
    /// The headless child's exit, seen while Enter was still running.
    early_exit: Option<SupervisorEvent>,
    native: Option<codex::Host>,
    submissions: Submissions,
    epoch: u64,
    pending_prompt: Option<(OperationId, String, Vec<ChatImage>)>,
    config_error: Option<String>,
    launch_context: Option<u64>,
    probes: Arc<AtomicUsize>,
}

fn refused(reason: ChatRefusal, detail: impl AsRef<str>) -> ChatReply {
    let detail = detail.as_ref();
    ChatReply::Refused {
        reason,
        detail: (!detail.is_empty()).then(|| clip(detail, MAX_CHAT_PREVIEW)),
    }
}

struct ProbePermit(Arc<AtomicUsize>);
impl Drop for ProbePermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn discover_claude(
    shell: u32,
    epoch: u64,
    base: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Result<ChatReply, String> {
    let context = context::resolve_shell(shell, Some(shell), Duration::from_secs(5), base)
        .map_err(|error| error.to_string())?;
    let stamp = context.launch_stamp();
    let env = env::build(&context.env).map_err(|_| "This Claude environment is unsupported")?;
    env::check_persistence(&context.env)
        .map_err(|_| "Claude conversation persistence is disabled")?;
    let launch = Launch {
        prefix: context.launch_prefix,
        args: args::headless_flags(&context.session_id, true)
            .into_iter()
            .map(Into::into)
            .collect(),
        env: env.vars().to_vec(),
        cwd: context.cwd,
    };
    let (events, _received) = mpsc::channel::<SupervisorEvent>();
    let mut supervisor = Supervisor::spawn(&launch, 0, events)
        .map_err(|_| "Could not start the Claude model probe")?;
    let result = supervisor.initialize("muxy_probe", Duration::from_secs(15));
    supervisor.close_stdin();
    terminate_group(
        supervisor.pid(),
        Duration::from_secs(2),
        Duration::from_secs(4),
    );
    supervisor.shutdown(Instant::now() + Duration::from_secs(1));
    let (initialize, _) =
        result.map_err(|_| "Claude did not acknowledge the promptless model probe")?;
    let mut log = ChatLog::default();
    log.apply_cli_line(&json!({"type":"control_response","response":{"response":initialize}}));
    Ok(ChatReply::Discovered {
        host_epoch: epoch,
        provider: ChatProvider::Claude,
        capabilities: log.capabilities().clone(),
        model: None,
        effort: None,
        threads: Vec::new(),
        launch_context: Some(stamp),
    })
}

impl Host {
    pub(crate) fn new(
        info: SessionInfo,
        services: Services,
        events: Sender<OwnerEvent>,
        gate: Arc<InputGate>,
    ) -> Self {
        Self {
            info,
            services,
            events,
            gate,
            target: None,
            token: 0,
            generation: 0,
            mode: Mode::Console,
            log: None,
            subscribers: Vec::new(),
            mode_changed: false,
            activity: None,
            early_exit: None,
            native: None,
            submissions: Submissions::default(),
            epoch: {
                let mut bytes = [0_u8; 8];
                if getrandom::fill(&mut bytes).is_ok() {
                    u64::from_ne_bytes(bytes)
                } else {
                    0
                }
            },
            pending_prompt: None,
            config_error: None,
            launch_context: None,
            probes: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn chat_mode(&self) -> ChatMode {
        if let Some(native) = &self.native {
            return native.mode();
        }
        match self.mode {
            Mode::Console => ChatMode::Console,
            Mode::Transitioning { .. } => ChatMode::Transitioning,
            Mode::Chat(_) => ChatMode::Chat,
            Mode::Blocked { .. } => ChatMode::Blocked,
        }
    }

    /// The mode to replay to a new attachment; nothing in plain Console.
    pub(crate) fn mode_event(&self) -> Option<MetadataEvent> {
        if let Some(native) = &self.native {
            return Some(native.mode_event());
        }
        (!matches!(self.mode, Mode::Console)).then(|| MetadataEvent::ChatMode {
            mode: self.chat_mode(),
            generation: self.generation,
        })
    }

    /// The mode to broadcast, once after each change.
    pub(crate) fn take_mode_change(&mut self) -> Option<MetadataEvent> {
        if let Some(native) = self.native.as_mut() {
            return native.take_mode_change();
        }
        std::mem::take(&mut self.mode_changed).then(|| MetadataEvent::ChatMode {
            mode: self.chat_mode(),
            generation: self.generation,
        })
    }

    /// The agent state chat reports while it, not terminal detection, is the
    /// activity source, and whether work just completed. `None` in Console.
    pub(crate) fn activity(&mut self) -> Option<(AgentState, bool)> {
        if let Some(native) = self.native.as_mut() {
            return native.activity();
        }
        let state = match &self.mode {
            Mode::Console => {
                self.activity = None;
                return None;
            }
            Mode::Transitioning { .. } => AgentState::Working,
            Mode::Blocked { .. } => AgentState::Blocked,
            Mode::Chat(_) => match &self.log {
                Some(log) if log.pending().next().is_some() => AgentState::Blocked,
                Some(log) if !log.is_quiet() => AgentState::Working,
                _ => AgentState::Idle,
            },
        };
        let completed = state == AgentState::Idle
            && matches!(
                self.activity,
                Some(AgentState::Working | AgentState::Blocked)
            );
        self.activity = Some(state);
        Some((state, completed))
    }

    fn set_mode(&mut self, mode: Mode) {
        let console = matches!(mode, Mode::Console);
        self.mode = mode;
        self.mode_changed = true;
        if console {
            self.log = None;
            self.subscribers.clear();
            self.gate.open();
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Submission admission and legacy request dispatch share the owner transaction"
    )]
    pub(crate) fn command(&mut self, command: ChatCommand, pty: &Pty) {
        let ChatCommand {
            request,
            from,
            reply,
        } = command;
        match request {
            ChatRequest::PrepareSubmission { .. } => {
                reply.send(if self.epoch == 0 {
                    refused(ChatRefusal::SpawnFailed, "Submission IDs are unavailable")
                } else if let Some(operation) = self.submissions.prepare(self.generation) {
                    ChatReply::Prepared {
                        host_epoch: self.epoch,
                        operation,
                    }
                } else {
                    refused(
                        ChatRefusal::Busy,
                        "Resolve pending submissions before sending another message",
                    )
                });
            }
            ChatRequest::OperationStatus { operation, .. } => {
                let status = self.submissions.status(operation, self.generation);
                if status.outcome == SubmissionOutcome::Uncertain
                    && let Some(native) = self.native.as_mut()
                {
                    native.reconcile(operation);
                } else if status.outcome == SubmissionOutcome::Uncertain {
                    let resume = match &self.mode {
                        Mode::Chat(live) => Some(&live.child.resume),
                        Mode::Blocked { child } => Some(&child.resume),
                        _ => None,
                    };
                    if let Some(resume) = resume
                        && self
                            .probes
                            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                                (count < 2).then_some(count + 1)
                            })
                            .is_ok()
                    {
                        let permit = ProbePermit(self.probes.clone());
                        let config = resume.config_dir.clone();
                        let thread = resume.uuid.clone();
                        let events = self.events.clone();
                        let _ = std::thread::Builder::new()
                            .name("chat-reconcile".into())
                            .spawn(move || {
                                let _permit = permit;
                                let id = super::submissions::native_id(operation);
                                if transcript::read(&config, &thread).is_ok_and(|lines| {
                                    lines
                                        .iter()
                                        .any(|line| line["type"] == "user" && line["uuid"] == id)
                                }) {
                                    let _ = events.send(OwnerEvent::Chat(Event::Submission {
                                        operation,
                                        outcome: SubmissionOutcome::Accepted,
                                        detail: None,
                                    }));
                                }
                            });
                    }
                }
                reply.send(ChatReply::Submission(status));
            }
            ChatRequest::Begin {
                host_epoch,
                operation,
                selection,
                text,
                images,
                features,
                ..
            } => {
                if host_epoch != self.epoch {
                    return reply.send(refused(
                        ChatRefusal::StaleGeneration,
                        "The launch context changed; check the operation before sending again",
                    ));
                }
                let hash = fingerprint(Some(&selection), 0, &text, &images);
                match self.start_operation(operation, hash) {
                    Ok(None) => {}
                    Ok(Some(status)) => return reply.send(ChatReply::Submission(status)),
                    Err(error) => {
                        return reply.send(refused(ChatRefusal::UnsupportedArguments, error));
                    }
                }
                if !matches!(self.mode, Mode::Console) || self.native.is_some() {
                    self.submissions.update(
                        operation,
                        self.generation,
                        SubmissionOutcome::Rejected,
                        Some("This terminal is already owned by a conversation".into()),
                    );
                    return reply.send(ChatReply::Submission(
                        self.submissions.status(operation, self.generation),
                    ));
                }
                reply.send(ChatReply::Submission(
                    self.submissions.status(operation, self.generation),
                ));
                let (finished, received) = mpsc::channel();
                let events = self.events.clone();
                if std::thread::Builder::new()
                    .name("chat-first-submit".into())
                    .spawn(move || {
                        let result = received
                            .recv_timeout(Duration::from_secs(45))
                            .unwrap_or_else(|_| {
                                refused(ChatRefusal::Busy, "Startup acknowledgment is uncertain")
                            });
                        let _ = events.send(OwnerEvent::Chat(Event::BeginReady {
                            operation,
                            result: Box::new(result),
                            text,
                            images,
                        }));
                    })
                    .is_err()
                {
                    self.submissions.update(
                        operation,
                        self.generation,
                        SubmissionOutcome::Rejected,
                        Some("Could not start the submission worker".into()),
                    );
                    return;
                }
                self.open_provider(
                    *selection,
                    true,
                    &features,
                    ChatReplyTo::Channel(finished),
                    pty,
                );
            }
            ChatRequest::Submit {
                generation,
                operation,
                text,
                images,
                ..
            } => {
                let hash = fingerprint(None, generation, &text, &images);
                match self.start_operation(operation, hash) {
                    Ok(None) => {}
                    Ok(Some(status)) => return reply.send(ChatReply::Submission(status)),
                    Err(error) => {
                        return reply.send(refused(ChatRefusal::UnsupportedArguments, error));
                    }
                }
                if generation != self.generation {
                    self.submissions.update(
                        operation,
                        self.generation,
                        SubmissionOutcome::Rejected,
                        Some("The conversation changed before submission".into()),
                    );
                } else if let Some(native) = self.native.as_mut() {
                    if native.authorized(&from) {
                        native.submit(operation, &text, images);
                    } else {
                        self.submissions.update(
                            operation,
                            self.generation,
                            SubmissionOutcome::Rejected,
                            Some("Subscribe with Codex capabilities before submitting".into()),
                        );
                    }
                } else {
                    self.send_operation(operation, text, images);
                }
                reply.send(ChatReply::Submission(
                    self.submissions.status(operation, self.generation),
                ));
            }
            ChatRequest::Discover { provider, .. } => {
                if self
                    .probes
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                        (count < 2).then_some(count + 1)
                    })
                    .is_err()
                {
                    return reply.send(refused(
                        ChatRefusal::Busy,
                        "Provider discovery is already running; retry the picker shortly",
                    ));
                }
                let permit = ProbePermit(self.probes.clone());
                let base = pty.environment().to_vec();
                let shell = pty.child_pid();
                let foreground = pty.foreground_pid();
                let epoch = self.epoch;
                let (sender, receiver) = mpsc::channel::<ChatReplyTo>();
                if std::thread::Builder::new()
                    .name("chat-model-probe".into())
                    .spawn(move || {
                        let _permit = permit;
                        let result = match provider {
                            ChatProvider::Codex => codex::discover(shell, foreground, epoch, &base),
                            ChatProvider::Claude => discover_claude(shell, epoch, &base),
                            ChatProvider::Unrecognized(_) => Err("Unsupported provider".into()),
                        };
                        if let Ok(reply) = receiver.recv() {
                            reply.send(result.unwrap_or_else(|error| {
                                refused(ChatRefusal::CliIncompatible, error)
                            }));
                        }
                    })
                    .is_err()
                {
                    return reply.send(refused(
                        ChatRefusal::SpawnFailed,
                        "Could not start provider discovery",
                    ));
                }
                let _ = sender.send(reply);
            }
            ChatRequest::EnterProvider {
                selection,
                features,
                ..
            } => {
                self.open_provider(*selection, false, &features, reply, pty);
            }
            request => {
                if let Some(native) = self.native.as_mut() {
                    return native.command(ChatCommand {
                        request,
                        from,
                        reply,
                    });
                }
                self.legacy_command(request, from, reply, pty);
            }
        }
    }

    fn legacy_command(
        &mut self,
        request: ChatRequest,
        from: Weak<Connection>,
        reply: ChatReplyTo,
        pty: &Pty,
    ) {
        match request {
            ChatRequest::Enter { model, effort, .. } => {
                self.enter(model, effort, false, reply, pty);
            }
            ChatRequest::Start { model, effort, .. } => self.enter(model, effort, true, reply, pty),
            ChatRequest::Leave { generation, .. } => self.leave(generation, reply),
            ChatRequest::Subscribe {
                subscription,
                features,
                ..
            } => self.subscribe(from, subscription, features, reply),
            ChatRequest::Unsubscribe { session } => {
                self.subscribers
                    .retain(|subscriber| !subscriber.outbox.ptr_eq(&from));
                if let Some(outbox) = from.upgrade() {
                    outbox.chat_unsubscribe(session);
                }
                reply.send(ChatReply::Ok);
            }
            request => self.live_request(request, reply),
        }
    }

    pub(crate) fn event(&mut self, event: Event) {
        match event {
            Event::Supervisor(event) => self.supervisor(event),
            Event::Done(completion) => self.done(completion),
            Event::Codex(event) => {
                if let Some(native) = self.native.as_mut() {
                    native.event(*event);
                    if native.mode() == ChatMode::Chat {
                        self.generation = native.generation();
                    }
                    if native.mode() == ChatMode::Console {
                        self.native = None;
                        self.mode_changed = true;
                    }
                }
            }
            Event::Submission {
                operation,
                outcome,
                detail,
            } => self
                .submissions
                .update(operation, self.generation, outcome, detail),
            Event::BeginReady {
                operation,
                result,
                text,
                images,
            } => match *result {
                ChatReply::Entered { generation } if generation == self.generation => {
                    self.submissions.update(
                        operation,
                        generation,
                        SubmissionOutcome::Pending,
                        None,
                    );
                    if let Some(native) = self.native.as_mut() {
                        native.submit(operation, &text, images);
                    } else {
                        self.send_operation(operation, text, images);
                    }
                }
                ChatReply::Refused { detail, .. } => self.submissions.update(
                    operation,
                    self.generation,
                    SubmissionOutcome::Rejected,
                    detail,
                ),
                _ => self.submissions.update(
                    operation,
                    self.generation,
                    SubmissionOutcome::Uncertain,
                    Some("Startup acceptance is uncertain; no prompt was resent".into()),
                ),
            },
        }
    }

    pub(crate) fn provider(&self) -> muxy_protocol::AgentProvider {
        if self.native.is_some() {
            muxy_protocol::AgentProvider::Codex
        } else {
            muxy_protocol::AgentProvider::Claude
        }
    }

    fn start_operation(
        &mut self,
        operation: OperationId,
        hash: [u8; 32],
    ) -> Result<Option<ChatSubmission>, &'static str> {
        match self.submissions.start(operation, hash, self.generation) {
            Ok(Start::New) => Ok(None),
            Ok(Start::Existing(status)) => Ok(Some(status)),
            Err(error) => Err(error),
        }
    }

    fn open_provider(
        &mut self,
        selection: ChatSelection,
        fresh: bool,
        features: &[Feature],
        reply: ChatReplyTo,
        pty: &Pty,
    ) {
        if self.native.is_some() || !matches!(self.mode, Mode::Console) {
            return reply.send(refused(
                ChatRefusal::Busy,
                "A conversation already owns this terminal",
            ));
        }
        if !features.contains(&Feature::CHAT_PROVIDERS) {
            return reply.send(refused(
                ChatRefusal::UnsupportedArguments,
                "Update Muxy to select a chat provider",
            ));
        }
        match selection.provider {
            ChatProvider::Claude => {
                if selection.target.is_some() {
                    return reply.send(refused(
                        ChatRefusal::UnsupportedArguments,
                        "Use the existing Claude resume flow for this target",
                    ));
                }
                self.launch_context = selection.launch_context;
                self.enter(selection.model, selection.effort, fresh, reply, pty);
            }
            ChatProvider::Codex => {
                if !features.contains(&Feature::CHAT_NATIVE_APPROVALS)
                    || !features.contains(&Feature::CHAT_QUESTIONS)
                {
                    return reply.send(refused(
                        ChatRefusal::UnsupportedArguments,
                        "Update Muxy to handle native Codex approvals",
                    ));
                }
                self.token += 1;
                match codex::Host::open(
                    self.info.clone(),
                    self.events.clone(),
                    self.gate.clone(),
                    pty,
                    selection,
                    fresh,
                    self.token,
                    self.generation + 1,
                    reply,
                ) {
                    Ok(native) => self.native = Some(native),
                    Err(reply) => reply.send(refused(
                        ChatRefusal::SpawnFailed,
                        "Could not start Codex preflight",
                    )),
                }
            }
            ChatProvider::Unrecognized(_) => reply.send(refused(
                ChatRefusal::UnsupportedArguments,
                "Unsupported provider",
            )),
        }
    }

    fn send_operation(&mut self, operation: OperationId, text: String, images: Vec<ChatImage>) {
        if self.config_error.is_some() {
            self.submissions.update(
                operation,
                self.generation,
                SubmissionOutcome::Rejected,
                self.config_error.clone(),
            );
            return;
        }
        let Mode::Chat(live) = &self.mode else {
            self.submissions.update(
                operation,
                self.generation,
                SubmissionOutcome::Rejected,
                Some("Chat is unavailable".into()),
            );
            return;
        };
        if !live.controls.is_empty() {
            if self.pending_prompt.is_some() {
                self.submissions.update(
                    operation,
                    self.generation,
                    SubmissionOutcome::Rejected,
                    Some("Another submission is waiting for configuration".into()),
                );
            } else {
                self.pending_prompt = Some((operation, text, images));
            }
            return;
        }
        let mut content = Vec::new();
        if !text.is_empty() {
            content.push(json!({"type":"text","text":text}));
        }
        for image in images {
            content.push(json!({"type":"image","source":{"type":"base64","media_type":image.mime.mime(),"data":base64(&image.bytes)}}));
        }
        let prompt = json!({"type":"user","uuid":super::submissions::native_id(operation),"message":{"role":"user","content":content},"parent_tool_use_id":null,"session_id":""});
        match self.write(&prompt) {
            Ok(()) => self.publish_with(ChatLog::prompt_sent),
            Err(error) => {
                self.submissions.update(
                    operation,
                    self.generation,
                    if error == StdinError::Full {
                        SubmissionOutcome::Rejected
                    } else {
                        SubmissionOutcome::Uncertain
                    },
                    Some(
                        "Claude did not acknowledge this submission; it has not been resent".into(),
                    ),
                );
            }
        }
    }

    fn job(&self) -> Option<Job> {
        Some(Job {
            gate: self.gate.clone(),
            target: self.target.clone()?,
            timeouts: self.services.timeouts,
            deadline: Instant::now() + self.services.timeouts.transition,
        })
    }

    /// `fresh`: Start, a new conversation from the idle shell.
    fn enter(
        &mut self,
        model: Option<String>,
        effort: Option<String>,
        fresh: bool,
        reply: ChatReplyTo,
        pty: &Pty,
    ) {
        if !matches!(self.mode, Mode::Console) {
            return reply.send(refused(ChatRefusal::Busy, "a switch is already running"));
        }
        if self.target.is_none() {
            match FlushTarget::new(pty) {
                Ok(target) => self.target = Some(Arc::new(target)),
                Err(error) => {
                    return reply
                        .send(refused(ChatRefusal::PlatformUnsupported, error.to_string()));
                }
            }
        }
        let Some(job) = self.job() else {
            return reply.send(refused(ChatRefusal::PlatformUnsupported, ""));
        };
        self.token += 1;
        let enter = EnterJob {
            base: pty.environment().to_vec(),
            launch_context: self.launch_context.take(),
            job,
            resolve: if fresh {
                self.services.resolve_shell.clone()
            } else {
                self.services.resolve.clone()
            },
            fresh,
            reservations: self.services.reservations.clone(),
            shell: pty.child_pid(),
            foreground: pty.foreground_pid(),
            model,
            effort,
            events: self.events.clone(),
            token: self.token,
        };
        match spawn_job(self.token, move || enter.run(), self.events.clone()) {
            Ok(_) => self.set_mode(Mode::Transitioning { reply }),
            Err(error) => reply.send(refused(ChatRefusal::SpawnFailed, error.to_string())),
        }
    }

    fn leave(&mut self, generation: u64, reply: ChatReplyTo) {
        match &self.mode {
            Mode::Console => return reply.send(refused(ChatRefusal::NotChat, "")),
            Mode::Transitioning { .. } => {
                return reply.send(refused(ChatRefusal::Busy, "a switch is already running"));
            }
            Mode::Chat(_) | Mode::Blocked { .. } if generation != self.generation => {
                return reply.send(refused(ChatRefusal::StaleGeneration, ""));
            }
            Mode::Chat(_) | Mode::Blocked { .. } => {}
        }
        let quiet = self.log.as_ref().is_none_or(ChatLog::is_quiet);
        match std::mem::replace(&mut self.mode, Mode::Console) {
            Mode::Chat(mut live) => {
                if !quiet {
                    // The app asked the user to confirm stopping the work.
                    let tasks: Vec<String> = self
                        .log
                        .as_ref()
                        .map(|log| log.tasks().map(str::to_owned).collect())
                        .unwrap_or_default();
                    let _ = send_control(&mut live, &json!({"subtype": "interrupt"}));
                    for task in tasks {
                        // UNVERIFIED in M0: the CLI answers an unknown subtype with an error.
                        let _ = send_control(
                            &mut live,
                            &json!({"subtype": "stop_task", "task_id": task}),
                        );
                    }
                }
                refuse_controls(&mut live, "Claude chat is ending");
                self.start_leave(live.child, reply, !quiet, false);
            }
            Mode::Blocked { child } => self.start_leave(*child, reply, false, true),
            Mode::Console | Mode::Transitioning { .. } => {}
        }
    }

    /// `retry`: leaving `Blocked`, where processes left in the group once
    /// Claude itself has exited no longer block.
    fn start_leave(&mut self, child: Child, reply: ChatReplyTo, interrupted: bool, retry: bool) {
        let Some(job) = self.job() else {
            // Unreachable: Chat and Blocked come after Enter set the target.
            self.set_mode(Mode::Blocked {
                child: Box::new(child),
            });
            return reply.send(refused(ChatRefusal::PlatformUnsupported, ""));
        };
        self.token += 1;
        // The job takes the child from here, so a failed spawn leaves it to us.
        let work = Arc::new(Mutex::new(Some(child)));
        let taken = work.clone();
        let leave = move || match take(&taken) {
            Some(child) => job.leave(child, interrupted, retry),
            None => Outcome::Console(refused(ChatRefusal::SpawnFailed, "")),
        };
        match spawn_job(self.token, leave, self.events.clone()) {
            Ok(_) => self.set_mode(Mode::Transitioning { reply }),
            Err(error) => {
                log::error!("could not start leaving chat: {error}");
                let Some(child) = take(&work) else {
                    return;
                };
                // Unstopped, it keeps the conversation and the gate stays closed.
                let Outcome::Blocked {
                    child,
                    reason,
                    reply: result,
                } = blocked(child)
                else {
                    return;
                };
                self.publish_with(|log| log.blocked(&reason));
                self.set_mode(Mode::Blocked { child });
                reply.send(result);
            }
        }
    }

    fn done(&mut self, completion: Completion<Outcome>) {
        if completion.transition != self.token {
            log::debug!(
                "session {} ignored a stale chat transition",
                self.info.id.get()
            );
            // Dropping a stale outcome stops any child it holds.
            return;
        }
        if !matches!(self.mode, Mode::Transitioning { .. }) {
            log::error!(
                "session {} finished a chat transition it wasn't in",
                self.info.id.get()
            );
            return;
        }
        let Mode::Transitioning { reply } = std::mem::replace(&mut self.mode, Mode::Console) else {
            return;
        };
        let early_exit = self.early_exit.take();
        match completion.result {
            Outcome::Entered(entered) => self.entered(*entered, reply, early_exit),
            Outcome::Console(result) => {
                let detail = match &result {
                    ChatReply::Left { detail, .. } => detail.clone(),
                    _ => None,
                };
                if let (Some(detail), Some(log)) = (detail, self.log.as_mut()) {
                    let events = log.host_notice(&detail);
                    self.publish(&events);
                }
                self.set_mode(Mode::Console);
                reply.send(result);
            }
            Outcome::Blocked {
                child,
                reason,
                reply: result,
            } => {
                // Blocked during Enter's rollback has no log yet; Subscribe needs one.
                let events = self
                    .log
                    .get_or_insert_with(ChatLog::default)
                    .blocked(&reason);
                self.publish(&events);
                self.set_mode(Mode::Blocked { child });
                reply.send(result);
            }
        }
    }

    fn entered(
        &mut self,
        entered: Entered,
        reply: ChatReplyTo,
        early_exit: Option<SupervisorEvent>,
    ) {
        let Entered {
            child,
            initialize,
            skipped,
            history,
            notices,
            model,
            effort,
            alias,
        } = entered;
        self.generation += 1;
        self.config_error = None;
        let mut log = ChatLog::default();
        log.seed(&history);
        log.apply_cli_line(&json!({
            "type": "control_response",
            "response": {"subtype": "success", "response": initialize},
        }));
        for notice in &notices {
            log.host_notice(notice);
        }
        // Nobody subscribes to a new generation before this; its snapshot has all of it.
        self.log = Some(log);
        self.subscribers.clear();
        self.set_mode(Mode::Chat(Box::new(Live {
            child,
            next_request: 0,
            controls: HashMap::new(),
            alias: alias.clone(),
        })));
        for item in skipped {
            self.stdout_item(item);
        }
        // The handshake may have consumed the one wake-up for queued output.
        self.drain();
        if let Some(event) = early_exit {
            // Before any control write, so the exit's own reason is the one shown.
            reply.send(ChatReply::Entered {
                generation: self.generation,
            });
            return self.supervisor(event);
        }
        let effort_model = model.clone().unwrap_or(alias);
        if let Some(model) = model {
            self.set_model(model, ChatReplyTo::Nobody);
        }
        if let Some(effort) = effort {
            self.set_effort(effort, Some(&effort_model), ChatReplyTo::Nobody);
        }
        reply.send(ChatReply::Entered {
            generation: self.generation,
        });
    }

    fn subscribe(
        &mut self,
        from: Weak<Connection>,
        subscription: u32,
        features: Vec<Feature>,
        reply: ChatReplyTo,
    ) {
        let Some(log) = self.log.as_ref() else {
            let reason = if matches!(self.mode, Mode::Console) {
                ChatRefusal::NotChat
            } else {
                ChatRefusal::Busy
            };
            return reply.send(refused(reason, ""));
        };
        let Some(outbox) = from.upgrade() else {
            return;
        };
        let snapshot = log.snapshot(
            self.info.id,
            self.generation,
            subscription,
            MAX_CHAT_SNAPSHOT_BYTES,
        );
        self.subscribers
            .retain(|subscriber| !subscriber.outbox.ptr_eq(&from));
        if outbox.chat_subscribe(snapshot) {
            self.subscribers.push(Subscriber {
                outbox: from,
                features,
            });
        }
        reply.send(ChatReply::Subscribed {
            generation: self.generation,
            subscription,
        });
    }

    /// Sends events `revision - len .. revision` to every subscriber.
    fn publish(&mut self, events: &[ChatEvent]) {
        let Some(log) = &self.log else {
            return;
        };
        if events.is_empty() {
            return;
        }
        let to = log.revision();
        let from = to - events.len() as u64;
        let (session, generation) = (self.info.id, self.generation);
        self.subscribers.retain(|subscriber| {
            subscriber.outbox.upgrade().is_some_and(|outbox| {
                outbox.push_chat(session, generation, (from, to), events.to_vec())
            })
        });
    }

    fn live_request(&mut self, request: ChatRequest, reply: ChatReplyTo) {
        let generation = match &request {
            ChatRequest::Send { generation, .. }
            | ChatRequest::Interrupt { generation, .. }
            | ChatRequest::Respond { generation, .. }
            | ChatRequest::SetModel { generation, .. }
            | ChatRequest::SetEffort { generation, .. } => *generation,
            _ => return reply.send(refused(ChatRefusal::NotChat, "")),
        };
        match &self.mode {
            Mode::Console => return reply.send(refused(ChatRefusal::NotChat, "")),
            Mode::Transitioning { .. } => {
                return reply.send(refused(ChatRefusal::Busy, "a switch is running"));
            }
            Mode::Blocked { .. } if generation == self.generation => {
                return reply.send(refused(
                    ChatRefusal::NotChat,
                    "Claude Code did not stop; switch to Console to try again",
                ));
            }
            _ if generation != self.generation => {
                return reply.send(refused(ChatRefusal::StaleGeneration, ""));
            }
            _ => {}
        }
        match request {
            ChatRequest::Send { text, images, .. } => self.send_prompt(&text, &images, reply),
            ChatRequest::Interrupt { .. } => {
                if self.control(
                    &json!({"subtype": "interrupt"}),
                    ControlKind::Other,
                    ChatReplyTo::Nobody,
                ) {
                    self.publish_with(ChatLog::interrupted);
                    reply.send(ChatReply::Ok);
                } else {
                    reply.send(refused(
                        ChatRefusal::Busy,
                        "Claude Code isn't reading its input",
                    ));
                }
            }
            ChatRequest::Respond {
                request_id,
                response,
                ..
            } => self.respond(&request_id, response, reply),
            ChatRequest::SetModel { model, .. } => self.set_model(model, reply),
            ChatRequest::SetEffort { effort, .. } => self.set_effort(effort, None, reply),
            _ => reply.send(refused(ChatRefusal::NotChat, "")),
        }
    }

    fn publish_with(&mut self, change: impl FnOnce(&mut ChatLog) -> Vec<ChatEvent>) {
        if let Some(log) = self.log.as_mut() {
            let events = change(log);
            self.publish(&events);
        }
    }

    fn send_prompt(&mut self, text: &str, images: &[ChatImage], reply: ChatReplyTo) {
        let mut content = Vec::with_capacity(images.len() + 1);
        if !text.is_empty() {
            content.push(json!({"type": "text", "text": text}));
        }
        for image in images {
            content.push(json!({
                "type": "image",
                "source": {"type": "base64", "media_type": image.mime.mime(), "data": base64(&image.bytes)},
            }));
        }
        if content.is_empty() {
            return reply.send(refused(
                ChatRefusal::UnsupportedArguments,
                "the message is empty",
            ));
        }
        let message = json!({
            "type": "user",
            "message": {"role": "user", "content": content},
            "parent_tool_use_id": null,
            "session_id": "",
        });
        match self.write(&message) {
            Ok(()) => {
                self.publish_with(ChatLog::prompt_sent);
                reply.send(ChatReply::Ok);
            }
            Err(StdinError::Full) => {
                reply.send(refused(
                    ChatRefusal::Busy,
                    "Claude Code isn't reading its input",
                ));
            }
            Err(StdinError::Closed) => {
                reply.send(refused(ChatRefusal::NotChat, "Claude Code stopped"));
            }
        }
    }

    fn respond(
        &mut self,
        request_id: &str,
        response: muxy_protocol::PermissionResponse,
        reply: ChatReplyTo,
    ) {
        let Some(log) = self.log.as_mut() else {
            return reply.send(refused(ChatRefusal::NotChat, ""));
        };
        let responded = match log.respond(request_id, response) {
            Ok(responded) => responded,
            Err(RespondError::Gone) => return reply.send(refused(ChatRefusal::ApprovalGone, "")),
            Err(RespondError::Mismatch) => {
                return reply.send(refused(
                    ChatRefusal::UnsupportedArguments,
                    "the answer doesn't fit this approval",
                ));
            }
        };
        let mut actions = responded.actions.into_iter();
        let written = match actions.next() {
            Some(Action::Write(line)) => self.send_line(&line),
            _ => Err(StdinError::Full),
        };
        // Before any exit events, so subscribers see no revision gap.
        self.publish(&responded.events);
        match written {
            Ok(()) => {}
            Err(StdinError::Closed) => {
                // The approval expires with the chat; nothing to restore.
                self.unexpected_exit("Claude Code stopped reading its input", "");
                return reply.send(refused(ChatRefusal::NotChat, "Claude Code stopped"));
            }
            Err(StdinError::Full) => {
                self.publish_with(|log| log.restore(responded.taken));
                return reply.send(refused(ChatRefusal::Busy, "the answer could not be sent"));
            }
        }
        for action in actions {
            self.act(action);
        }
        reply.send(ChatReply::Ok);
    }

    fn set_model(&mut self, model: String, reply: ChatReplyTo) {
        let known = self.log.as_ref().is_some_and(|log| {
            let models = &log.capabilities().models;
            models.is_empty() || models.iter().any(|offered| offered.alias == model)
        });
        if !known {
            let text = format!("Claude Code doesn't offer the model {model}");
            if matches!(reply, ChatReplyTo::Nobody) {
                self.config_error = Some(text.clone());
                self.publish_with(|log| log.host_notice(&text));
            }
            return reply.send(refused(ChatRefusal::UnsupportedArguments, text));
        }
        let request = json!({"subtype": "set_model", "model": model});
        if !self.control(&request, ControlKind::Model(model), reply) {
            self.publish_with(|log| log.error("Claude Code isn't reading its input"));
        }
    }

    /// Checks `effort` against the model's levels first: the CLI accepts any.
    fn set_effort(&mut self, effort: String, model: Option<&str>, reply: ChatReplyTo) {
        let Mode::Chat(live) = &self.mode else {
            return reply.send(refused(ChatRefusal::NotChat, ""));
        };
        let model = model.unwrap_or(&live.alias).to_owned();
        let offered = self.log.as_ref().and_then(|log| {
            log.capabilities()
                .models
                .iter()
                .find(|offered| offered.alias == model)
                .map(|offered| offered.efforts.contains(&effort))
        });
        if offered != Some(true) {
            let text = if offered.is_none() {
                format!("Muxy can't check effort levels for the model {model}")
            } else {
                format!("The model {model} has no effort level {effort}")
            };
            if matches!(reply, ChatReplyTo::Nobody) {
                self.config_error = Some(text.clone());
                self.publish_with(|log| log.host_notice(&text));
            }
            return reply.send(refused(ChatRefusal::UnsupportedArguments, text));
        }
        let request =
            json!({"subtype": "apply_flag_settings", "settings": {"effortLevel": effort}});
        if !self.control(&request, ControlKind::Effort(effort), reply) {
            self.publish_with(|log| log.error("Claude Code isn't reading its input"));
        }
    }

    /// Sends a control request and tracks its response. False if it couldn't
    /// be queued; then `reply` has been refused.
    fn control(&mut self, request: &Value, kind: ControlKind, reply: ChatReplyTo) -> bool {
        let Mode::Chat(live) = &mut self.mode else {
            reply.send(refused(ChatRefusal::NotChat, ""));
            return false;
        };
        match send_control(live, request) {
            Ok(id) => {
                live.controls.insert(id, Control { kind, reply });
                true
            }
            Err(error) => {
                reply.send(refused(
                    ChatRefusal::Busy,
                    "Claude Code isn't reading its input",
                ));
                if error == StdinError::Closed {
                    self.unexpected_exit("Claude Code stopped reading its input", "");
                }
                false
            }
        }
    }

    /// Queues a line for the child, without acting on a failure.
    fn send_line(&self, line: &Value) -> Result<(), StdinError> {
        let Mode::Chat(active) = &self.mode else {
            return Err(StdinError::Closed);
        };
        active
            .child
            .supervisor
            .as_ref()
            .ok_or(StdinError::Closed)
            .and_then(|supervisor| supervisor.send(line))
    }

    fn write(&mut self, line: &Value) -> Result<(), StdinError> {
        let result = self.send_line(line);
        if result == Err(StdinError::Closed) {
            self.unexpected_exit("Claude Code stopped reading its input", "");
        }
        result
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::Write(line) => {
                if self.write(&line) == Err(StdinError::Full) {
                    self.publish_with(|log| log.error("Claude Code isn't reading its input"));
                }
            }
            Action::SetPermissionMode(mode) => {
                self.control(
                    &json!({"subtype": "set_permission_mode", "mode": mode}),
                    ControlKind::Other,
                    ChatReplyTo::Nobody,
                );
            }
        }
    }

    fn supervisor(&mut self, event: SupervisorEvent) {
        let transition = match &event {
            SupervisorEvent::Output { transition }
            | SupervisorEvent::WriteFailed { transition, .. }
            | SupervisorEvent::Exited { transition, .. } => *transition,
        };
        if transition != self.token {
            return;
        }
        if matches!(self.mode, Mode::Transitioning { .. }) {
            // Enter is still handing over this child; `entered` replays it.
            if !matches!(event, SupervisorEvent::Output { .. }) && self.early_exit.is_none() {
                self.early_exit = Some(event);
            }
            return;
        }
        if !matches!(self.mode, Mode::Chat(_)) {
            return;
        }
        match event {
            SupervisorEvent::Output { .. } => self.drain(),
            SupervisorEvent::WriteFailed { error, .. } => {
                self.unexpected_exit(&format!("Writing to Claude Code failed: {error}"), "");
            }
            SupervisorEvent::Exited {
                status,
                stderr_tail,
                ..
            } => {
                // Its last lines may still be queued.
                self.drain();
                let reason = match status {
                    Ok(status) => format!("Claude Code exited ({status})"),
                    Err(error) => format!("Claude Code exited: {error}"),
                };
                self.unexpected_exit(&reason, &stderr_tail);
            }
        }
    }

    fn drain(&mut self) {
        let Mode::Chat(live) = &self.mode else {
            return;
        };
        let Some(supervisor) = live.child.supervisor.as_ref() else {
            return;
        };
        let items = supervisor.drain(DRAIN);
        let more = items.len() == DRAIN;
        for item in items {
            if !matches!(self.mode, Mode::Chat(_)) {
                return;
            }
            self.stdout_item(item);
        }
        if more {
            let _ = self.events.send(
                SupervisorEvent::Output {
                    transition: self.token,
                }
                .into(),
            );
        }
    }

    fn stdout_item(&mut self, item: StdoutItem) {
        match item {
            StdoutItem::Message(line) => self.cli_line(&line),
            StdoutItem::Overlong { discarded } => self.publish_with(|log| {
                log.host_notice(&format!(
                    "Muxy skipped a {discarded}-byte line from Claude Code"
                ))
            }),
            StdoutItem::Malformed => log::debug!("Claude Code wrote a line that isn't JSON"),
            // `Exited`, with the stderr tail, follows; a child that closes
            // stdout and lives on is left for the user to Leave.
            StdoutItem::Closed(error) => {
                log::debug!("Claude Code closed its output: {error:?}");
            }
        }
    }

    fn cli_line(&mut self, line: &Value) {
        if line["type"] == "user"
            && let Some(operation) = line["uuid"].as_str().and_then(|id| id.parse().ok())
        {
            self.submissions.update(
                operation,
                self.generation,
                SubmissionOutcome::Accepted,
                None,
            );
        }
        if line["type"] == "control_response"
            && let Some(id) = line["response"]["request_id"].as_str()
            && let Mode::Chat(live) = &mut self.mode
            && let Some(control) = live.controls.remove(id)
        {
            self.control_done(control, &line["response"]);
        }
        let shown = (line["type"] == "control_request").then(|| self.shown_features());
        let Some(log) = self.log.as_mut() else {
            return;
        };
        if let Some(shown) = shown {
            log.set_shown(shown);
        }
        let applied = log.apply_cli_line(line);
        self.publish(&applied.events);
        for action in applied.actions {
            self.act(action);
        }
    }

    /// What the live subscribers can show together; `None` when nobody is subscribed.
    fn shown_features(&self) -> Option<Vec<Feature>> {
        let mut live = self
            .subscribers
            .iter()
            .filter(|subscriber| subscriber.outbox.strong_count() > 0)
            .peekable();
        live.peek()?;
        Some(
            live.flat_map(|subscriber| subscriber.features.iter().copied())
                .collect(),
        )
    }

    fn control_done(&mut self, control: Control, response: &Value) {
        let ok = response["subtype"] == "success";
        if !ok
            && matches!(control.reply, ChatReplyTo::Nobody)
            && matches!(control.kind, ControlKind::Model(_) | ControlKind::Effort(_))
        {
            self.config_error = Some(
                "Claude rejected the selected model or effort; the prompt was not sent".into(),
            );
        }
        match control.kind {
            ControlKind::Model(alias) if ok => {
                let resolved = response["response"]["model"]
                    .as_str()
                    .unwrap_or(&alias)
                    .to_owned();
                self.publish_with(|log| {
                    log.update_state(|state| state.model = Some(clip(&resolved, MAX_CHAT_NAME)))
                });
                if let Mode::Chat(live) = &mut self.mode {
                    live.alias = alias;
                }
            }
            ControlKind::Effort(effort) if ok => {
                self.publish_with(|log| log.update_state(|state| state.effort = Some(effort)));
            }
            _ => {}
        }
        control.reply.send(if ok {
            ChatReply::Ok
        } else {
            refused(
                ChatRefusal::UnsupportedArguments,
                response["error"].as_str().unwrap_or("Claude Code refused"),
            )
        });
        if matches!(&self.mode,Mode::Chat(live) if live.controls.is_empty())
            && let Some((operation, text, images)) = self.pending_prompt.take()
        {
            self.send_operation(operation, text, images);
        }
    }

    /// The child ended, or stopped taking input: approvals expire, waiting
    /// requests are refused, subscribers get `Exited`, then Leave runs.
    fn unexpected_exit(&mut self, reason: &str, stderr: &str) {
        let Mode::Chat(mut live) = std::mem::replace(&mut self.mode, Mode::Console) else {
            return;
        };
        let stderr = tail(stderr.trim_end(), STDERR_SHOWN);
        let text = if stderr.is_empty() {
            reason.to_owned()
        } else {
            format!("{reason}: {stderr}")
        };
        self.publish_with(|log| log.exited(&text));
        refuse_controls(&mut live, "Claude Code exited");
        self.start_leave(live.child, ChatReplyTo::Nobody, false, false);
    }
}

fn send_control(live: &mut Live, request: &Value) -> Result<String, StdinError> {
    live.next_request += 1;
    let id = format!("muxy_{}", live.next_request);
    let supervisor = live.child.supervisor.as_ref().ok_or(StdinError::Closed)?;
    supervisor.send(&json!({"type": "control_request", "request_id": id, "request": request}))?;
    Ok(id)
}

fn refuse_controls(live: &mut Live, why: &str) {
    for (_, control) in live.controls.drain() {
        control.reply.send(refused(ChatRefusal::NotChat, why));
    }
}

/// The last `max` bytes of `text`, on a char boundary.
fn tail(text: &str, max: usize) -> &str {
    let mut start = text.len().saturating_sub(max);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

pub(super) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |value, (index, byte)| {
                value | u32::from(*byte) << (16 - 8 * index)
            });
        for index in 0..4 {
            if index <= chunk.len() {
                encoded.push(char::from(
                    ALPHABET[(value >> (18 - 6 * index)) as usize & 63],
                ));
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

/// What every transition job needs from the session.
struct Job {
    gate: Arc<InputGate>,
    target: Arc<FlushTarget>,
    timeouts: Timeouts,
    deadline: Instant,
}

impl Job {
    /// `step`, cut short so the transition ends by its deadline.
    fn within(&self, step: Duration) -> Duration {
        step.min(self.deadline.saturating_duration_since(Instant::now()))
    }

    /// Stops the headless child's group. `None` when it can't be confirmed;
    /// else how many processes outlived it, which with `leftovers` may be
    /// more than none once Claude itself has exited: they can't write its
    /// conversation.
    fn stop(&self, child: &mut Child, leftovers: bool) -> Option<usize> {
        let Some(supervisor) = child.supervisor.as_mut() else {
            return Some(0);
        };
        supervisor.close_stdin();
        let stopped = terminate_group(
            supervisor.pid(),
            self.within(self.timeouts.term_grace),
            self.within(self.timeouts.term_give_up),
        );
        let left = match stopped {
            Termination::Confirmed => 0,
            // Not confirmed means someone is still in the group.
            Termination::NotConfirmed if leftovers && supervisor.exited() => {
                context::group_size(supervisor.pid()).max(1)
            }
            Termination::NotConfirmed => return None,
        };
        if let Some(supervisor) = child.supervisor.take() {
            supervisor.shutdown(Instant::now() + SHUTDOWN_TIMEOUT);
        }
        Some(left)
    }

    /// Types the recovery line into the shell once it owns the terminal
    /// again. Returns whether it was typed, and else why not with the command
    /// to copy.
    fn recover(&self, resume: &Resume) -> (bool, Option<String>) {
        // Nothing was sent, so there is nothing to resume.
        let unsaved = resume
            .fresh
            .as_ref()
            .is_some_and(|config| context::resumable_transcript(config, &resume.uuid).is_err());
        if unsaved && !resume.relaunch {
            return (false, None);
        }
        let uuid = (!unsaved).then_some(resume.uuid.as_str());
        let failed = |why: &str| (false, Some(not_restarted(why, uuid)));
        // The shell is alive, owns the terminal and runs nothing new, so a typed line is its input.
        let idle = || {
            let Ok(group) = context::check_shell(resume.shell, None) else {
                return Err("the shell is gone");
            };
            let foreground = Instant::now() + self.within(FOREGROUND_TIMEOUT);
            if !wait_foreground_is(self.target.master(), group, foreground) {
                return Err("the shell doesn't own the terminal");
            }
            // Typed into a command the shell ran on into, the line would be its input.
            if context::children_since(resume.shell, group, resume.since) > 0 {
                return Err("the shell is running another command");
            }
            Ok(())
        };
        let typed = |bytes: &[u8]| {
            let (written, acknowledged) = mpsc::channel();
            self.gate
                .send_authorized(bytes.to_vec(), Some(written))
                .is_ok()
                && matches!(
                    acknowledged.recv_timeout(self.within(TYPE_TIMEOUT)),
                    Ok(Ok(()))
                )
        };
        if let Err(why) = idle() {
            return failed(why);
        }
        if let Err(error) = gate_and_flush(&self.gate, &self.target, self.within(FLUSH_TIMEOUT)) {
            return failed(&error.to_string());
        }
        let Some(line) = recovery::command(
            &resume.cwd,
            &resume.env,
            &resume.prefix,
            &resume.console_args,
            uuid,
        ) else {
            return failed("its directory is gone");
        };
        // Start left the shell at its prompt, maybe with a draft in its line
        // editor (Enter's shell has a fresh one, as the TUI just exited there).
        // ^C discards it and redraws the prompt in zsh, bash and fish alike.
        if resume.fresh.is_some() {
            if !typed(b"\x03") {
                return failed("typing into the terminal failed");
            }
            // shortcut: a fixed pause for the shell to take the SIGINT, which
            // also flushes pending input; watch for the new prompt if it's short.
            std::thread::sleep(self.within(CLEAR_SETTLE));
            if let Err(why) = idle() {
                return failed(why);
            }
        }
        if typed(format!("{line}\r").as_bytes()) {
            (true, None)
        } else {
            failed("typing into the terminal failed")
        }
    }

    /// `retry`: leaving `Blocked`; see [`Job::stop`]'s `leftovers`.
    fn leave(self, mut child: Child, interrupted: bool, retry: bool) -> Outcome {
        if interrupted {
            let deadline = Instant::now() + self.within(Duration::from_secs(20));
            let mut completed = false;
            while Instant::now() < deadline {
                if let Some(supervisor) = &child.supervisor {
                    completed=supervisor.exited()||supervisor.drain(256).iter().any(|item|
                        matches!(item,StdoutItem::Message(value) if value["type"]=="result"));
                }
                if completed {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            if !completed {
                return blocked(child);
            }
        }
        let Some(left) = self.stop(&mut child, retry) else {
            return blocked(child);
        };
        let (resumed, mut detail) = self.recover(&child.resume);
        // Releases the reservation: Claude is confirmed gone.
        drop(child);
        if left > 0 {
            let note = format!(
                "{left} process{} Claude Code started did not stop",
                if left == 1 { "" } else { "es" }
            );
            detail = Some(clip(
                &detail.map_or(note.clone(), |detail| format!("{note}; {detail}")),
                MAX_CHAT_PREVIEW,
            ));
        }
        Outcome::Console(ChatReply::Left { resumed, detail })
    }

    /// Enter's step 4 failed: stop the child, then bring the TUI back, on a
    /// budget of its own.
    fn rollback(&self, mut child: Child, reason: ChatRefusal, detail: &str) -> Outcome {
        let job = Job {
            gate: self.gate.clone(),
            target: self.target.clone(),
            timeouts: self.timeouts,
            deadline: Instant::now() + ROLLBACK_BUDGET,
        };
        if job.stop(&mut child, false).is_none() {
            return blocked(child);
        }
        let (resumed, why) = job.recover(&child.resume);
        drop(child);
        let detail = match why {
            Some(why) if !resumed => format!("{detail}; not resumed: {why}"),
            _ => detail.to_owned(),
        };
        Outcome::Console(refused(reason, detail))
    }
}

/// Why the TUI wasn't brought back, with the command to copy.
fn not_restarted(why: &str, uuid: Option<&str>) -> String {
    let copy = uuid
        .and_then(recovery::plain_command)
        .map_or_else(String::new, |command| {
            format!("; run `{command}` to resume it (context may differ)")
        });
    clip(
        &format!("Claude Code was not restarted: {why}{copy}"),
        MAX_CHAT_PREVIEW,
    )
}

fn blocked(child: Child) -> Outcome {
    let reason = "Claude Code did not stop; Muxy keeps its conversation reserved. Switch to Console to try again.";
    Outcome::Blocked {
        child: Box::new(child),
        reason: reason.to_owned(),
        reply: refused(ChatRefusal::SpawnFailed, reason),
    }
}

struct EnterJob {
    base: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    launch_context: Option<u64>,
    job: Job,
    resolve: Resolve,
    /// Start: a new conversation from the idle shell, with no TUI to stop.
    fresh: bool,
    reservations: Arc<Reservations>,
    shell: u32,
    foreground: Option<u32>,
    model: Option<String>,
    effort: Option<String>,
    events: Sender<OwnerEvent>,
    token: u64,
}

impl EnterJob {
    #[allow(
        clippy::too_many_lines,
        reason = "The preflight table and handoff steps read best in order"
    )]
    fn run(self) -> Outcome {
        let refuse = |reason, detail: &str| Outcome::Console(refused(reason, detail));
        let idle = "a program is running in this terminal; start Chat from an idle shell prompt";
        // Before preflight, so Start sees a command run meanwhile.
        let started = SystemTime::now();
        if self.fresh {
            match context::check_shell(self.shell, None) {
                Ok(group) if self.foreground == Some(group) => {}
                Ok(_) => return refuse(ChatRefusal::Busy, idle),
                Err(error) => return refuse(error.refusal(), "the shell is gone"),
            }
        }
        // Preflight: nothing is stopped yet.
        let mut ctx = match (self.resolve)(
            self.shell,
            self.foreground,
            self.job.within(Duration::MAX),
            &self.base,
        ) {
            Ok(ctx) => ctx,
            Err(error) => return refuse(error.refusal(), &error.to_string()),
        };
        if self
            .launch_context
            .is_some_and(|stamp| stamp != ctx.launch_stamp())
        {
            return refuse(
                ChatRefusal::StaleGeneration,
                "The launch environment changed; reopen the provider picker",
            );
        }
        // A TUI that hasn't saved its conversation has nothing to resume: Chat
        // starts a new one in the TUI's own context, unless the TUI saves it
        // before it is stopped (`orphan` keeps its id for that check).
        let orphan = (!self.fresh
            && context::resumable_transcript(&ctx.config_dir, &ctx.session_id).is_err())
        .then(|| ctx.session_id.clone());
        if orphan.is_some() {
            let Some(id) = context::new_uuid() else {
                return refuse(ChatRefusal::NoClaudeSession, "no conversation id");
            };
            ctx.session_id = id;
        }
        let mut fresh = self.fresh || orphan.is_some();
        let overrides = args::Overrides {
            model: self.model.is_some(),
            effort: self.effort.is_some(),
        };
        let parsed = match args::parse(&ctx.user_args, &ctx.cwd, overrides, || {
            args::settings_default_mode(&args::settings_layers(&ctx.cwd, &ctx.config_dir))
        }) {
            Ok(parsed) => parsed,
            Err(error) => return refuse(ChatRefusal::UnsupportedArguments, &error.to_string()),
        };
        let child_env = match env::build(&ctx.env) {
            Ok(child_env) => child_env,
            Err(error) => return refuse(env::UnsupportedEnv::refusal(), &error.to_string()),
        };
        if let Err(error) = env::check_persistence(&ctx.env) {
            return refuse(
                env::PersistenceDisabled::refusal(),
                &format!("{error}; switching back would lose the conversation"),
            );
        }
        let Some(prefix) = ctx
            .launch_prefix
            .iter()
            .map(|word| word.to_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
        else {
            return refuse(
                ChatRefusal::UnsupportedArguments,
                "Claude Code's path isn't UTF-8",
            );
        };
        let version = match run_version(
            &ctx.launch_prefix,
            child_env.vars(),
            &ctx.cwd,
            self.job.within(VERSION_TIMEOUT),
        ) {
            Ok(version) => version,
            Err(error) => {
                return refuse(
                    ChatRefusal::CliIncompatible,
                    &format!("`claude --version` failed: {error}"),
                );
            }
        };
        let mut notices = parsed.notices.clone();
        if version.split_whitespace().next() != Some(TESTED_VERSION) {
            notices.push(clip(
                &format!(
                    "Untested Claude Code version {version}; Muxy was tested with {TESTED_VERSION}"
                ),
                MAX_CHAT_PREVIEW,
            ));
        }
        let mut reservation =
            match self
                .reservations
                .try_reserve(&ctx.config_dir, &ctx.session_id, ctx.pid)
            {
                Ok(reservation) => reservation,
                Err(error) => {
                    let detail = match error {
                        ReserveError::ConversationInUse => {
                            "another Claude Code or chat is using this conversation"
                        }
                        ReserveError::NoConfig => "Claude Code's config directory can't be found",
                    };
                    return refuse(error.refusal(), detail);
                }
            };

        // Step 1: clear typed input. The owner reopens the gate on refusal. Input written
        // between the Transitioning broadcast and this close reaches the still-running TUI,
        // never the shell (by design: preflight may refuse and the user stays in Console).
        if let Err(error) = gate_and_flush(
            &self.job.gate,
            &self.job.target,
            self.job.within(FLUSH_TIMEOUT),
        ) {
            let reason = match error {
                FlushError::WriterBusy => ChatRefusal::Busy,
                FlushError::FlushFailed(_) | FlushError::PlatformUnsupported => {
                    ChatRefusal::PlatformUnsupported
                }
            };
            return refuse(reason, &error.to_string());
        }
        let since = if self.fresh {
            // Step 2: nothing to stop; the shell must still be idle.
            let Ok(group) = context::check_shell(self.shell, None) else {
                return refuse(ChatRefusal::NoShell, "the shell is gone");
            };
            if !wait_foreground_is(self.job.target.master(), group, Instant::now())
                || context::children_since(self.shell, group, started) > 0
            {
                return refuse(ChatRefusal::Busy, idle);
            }
            started
        } else {
            // Step 2: stop the TUI and wait for the shell to own the terminal.
            let since = SystemTime::now();
            let stopped = terminate_group(
                ctx.group,
                self.job.within(self.job.timeouts.term_grace),
                self.job.within(self.job.timeouts.term_give_up),
            );
            if stopped != Termination::Confirmed {
                return refuse(ChatRefusal::SpawnFailed, "Claude did not exit");
            }
            // The TUI may have saved its first prompt since preflight; it's gone
            // now, so this check is final: resume that conversation instead.
            if let Some(old) = &orphan
                && context::resumable_transcript(&ctx.config_dir, old).is_ok()
            {
                match self.reservations.try_reserve(&ctx.config_dir, old, ctx.pid) {
                    // Releases the new conversation's reservation.
                    Ok(held) => reservation = held,
                    Err(error) => {
                        let why = not_restarted(
                            "another Claude Code or chat is using its conversation",
                            Some(old),
                        );
                        return refuse(error.refusal(), &why);
                    }
                }
                ctx.session_id.clone_from(old);
                fresh = false;
            }
            let resumable = (!fresh).then_some(ctx.session_id.as_str());
            let Ok(group) = context::check_shell(self.shell, None) else {
                let why = not_restarted("the shell is gone", resumable);
                return refuse(ChatRefusal::SpawnFailed, &why);
            };
            let foreground = Instant::now() + self.job.within(self.job.timeouts.term_give_up);
            if !wait_foreground_is(self.job.target.master(), group, foreground) {
                let why = not_restarted("the shell doesn't own the terminal", resumable);
                return refuse(ChatRefusal::SpawnFailed, &why);
            }
            // A shell that runs on after the TUI (`claude -c || claude`, a loop)
            // would start another Claude on this conversation or take the line
            // Leave types. shortcut: a builtin after `claude`, such as `read`,
            // starts no process and isn't detected.
            std::thread::sleep(self.job.within(SETTLE));
            if !wait_foreground_is(self.job.target.master(), group, Instant::now())
                || context::children_since(self.shell, group, since) > 0
            {
                return refuse(
                    ChatRefusal::SpawnFailed,
                    "Claude Code stopped, but the shell ran on into another command, so Chat did not start",
                );
            }
            since
        };

        // Step 3: start the headless child.
        let mut args = args::headless_flags(&ctx.session_id, fresh);
        args.extend(parsed.headless);
        let launch = Launch {
            prefix: ctx.launch_prefix.clone(),
            args: args.into_iter().map(Into::into).collect(),
            env: child_env.vars().to_vec(),
            cwd: ctx.cwd.clone(),
        };
        let mut child = Child {
            supervisor: None,
            reservation: Some(reservation),
            resume: Resume {
                config_dir: ctx.config_dir.clone(),
                shell: self.shell,
                cwd: ctx.cwd.clone(),
                env: child_env,
                prefix,
                console_args: parsed.console,
                uuid: ctx.session_id.clone(),
                since,
                fresh: fresh.then(|| ctx.config_dir.clone()),
                relaunch: fresh && orphan.is_some(),
            },
            timeouts: self.job.timeouts,
        };
        match Supervisor::spawn(&launch, self.token, self.events.clone()) {
            Ok(supervisor) => child.supervisor = Some(supervisor),
            Err(error) => {
                return self.job.rollback(
                    child,
                    ChatRefusal::SpawnFailed,
                    &format!("Claude Code didn't start: {error}"),
                );
            }
        }
        // Step 4: wait for the initialize response.
        let handshake = child.supervisor.as_ref().map(|supervisor| {
            supervisor.initialize(
                &format!("muxy_init_{}", self.token),
                self.job.within(self.job.timeouts.initialize),
            )
        });
        match handshake {
            Some(Ok((initialize, skipped))) => {
                let history =
                    transcript::read(&ctx.config_dir, &ctx.session_id).unwrap_or_default();
                Outcome::Entered(Box::new(Entered {
                    child,
                    initialize,
                    skipped,
                    history,
                    notices,
                    model: self.model,
                    effort: self.effort,
                    alias: parsed.model.unwrap_or_else(|| "default".to_owned()),
                }))
            }
            Some(Err(HandshakeError::Timeout)) => self.job.rollback(
                child,
                ChatRefusal::StartupTimeout,
                "Claude Code didn't start in time",
            ),
            Some(Err(error)) => {
                let why = match error {
                    HandshakeError::Refused(text) => format!("it refused: {text}"),
                    HandshakeError::Stdin(error) => format!("its input failed ({error:?})"),
                    HandshakeError::Closed => "it exited".to_owned(),
                    HandshakeError::Unexpected | HandshakeError::Timeout => {
                        "it wrote too much before answering".to_owned()
                    }
                };
                self.job.rollback(
                    child,
                    ChatRefusal::SpawnFailed,
                    &format!("Claude Code didn't start: {why}"),
                )
            }
            None => self
                .job
                .rollback(child, ChatRefusal::SpawnFailed, "Claude Code didn't start"),
        }
    }
}
