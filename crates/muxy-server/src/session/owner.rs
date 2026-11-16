#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::error::Error;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use muxy_protocol::{
    AttachSnapshot, ChannelId, ErrorCode, ExitReason, HistoryCursor, HistoryPage, InputModes,
    MetadataEvent, SavedScreen, ScreenFrame, SearchPage, SessionInfo, Size, TerminalColors,
};
use muxy_terminal::Terminal;
use muxy_terminal::pty::{ExitStatus, Pty, PtyEvent};

use crate::archive::{Archive, bound_history_page, history_range};
use crate::error::ServerError;
use crate::search::Search;
use crate::session::input_gate::{InputGate, InputWrite};
use crate::session::metadata::Metadata;
use crate::session::{
    AttachmentEvent, AttachmentId, AttachmentSink, SessionCommand, SessionHandle,
};

const TICK: Duration = Duration::from_millis(16);
const SYNC_TIMEOUT: Duration = Duration::from_secs(1);
const CHECKPOINT: Duration = Duration::from_secs(1);
/// CAN ends any unfinished escape sequence or character.
const CANCEL: &[u8] = b"\x18";
const METADATA_POLL: Duration = Duration::from_millis(100);

type Fault = Box<dyn Error + Send + Sync>;

#[derive(Debug)]
pub(crate) enum OwnerEvent {
    Command(SessionCommand),
    Pty(PtyEvent),
    WriteFailed(io::Error),
    /// Fork: supervisor and transition events for the chat host.
    #[cfg(target_os = "macos")]
    Chat(super::chat::Event),
}

impl From<PtyEvent> for OwnerEvent {
    fn from(event: PtyEvent) -> Self {
        Self::Pty(event)
    }
}

enum Wake {
    Event(OwnerEvent),
    Tick,
    Checkpoint,
    Metadata,
    Orphaned,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum OutputState {
    Open,
    Closed,
}

struct Attachment {
    sink: AttachmentSink,
    seq: u64,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "Pending work flags and the application's cursor mode are independent states"
)]
struct Owner {
    info: SessionInfo,
    pty: Pty,
    terminal: Terminal,
    metadata: Metadata,
    published: std::sync::Arc<super::Published>,
    activity: std::sync::Arc<crate::activity::Activity>,
    detector: crate::detection::Detector,
    detection_dirty: bool,
    next_detection: Instant,
    detection_screen: String,
    detection_progress: String,
    /// The foreground process group when the program status last changed.
    program_group: Option<u32>,
    program_reports: u64,
    seen_reports: u64,
    size: Size,
    events: Receiver<OwnerEvent>,
    input: std::sync::Arc<InputGate>,
    attachments: HashMap<AttachmentId, Attachment>,
    output_pending: bool,
    resize_pending: bool,
    compress_pending: bool,
    history_total: u64,
    history_generation: u64,
    input_modes: InputModes,
    cursor_blinking: bool,
    links: Vec<muxy_protocol::LinkRow>,
    graphics: muxy_protocol::Graphics,
    prompts: Vec<u16>,
    frame_state: Option<(muxy_protocol::Cursor, muxy_protocol::Modes)>,
    next_tick: Option<Instant>,
    tick_deferred: bool,
    synchronized_since: Option<Instant>,
    output_state: OutputState,
    archive: Archive,
    next_checkpoint: Option<Instant>,
    next_metadata: Instant,
    #[cfg(target_os = "macos")]
    chat: super::chat::Host,
}

#[allow(
    clippy::too_many_arguments,
    reason = "Session startup receives owned terminal resources and shared server services"
)]
pub(crate) fn start(
    info: SessionInfo,
    mut pty: Pty,
    size: Size,
    history_budget_bytes: usize,
    archive: Archive,
    colors: Option<TerminalColors>,
    activity: std::sync::Arc<crate::activity::Activity>,
    notifier: std::sync::Arc<crate::notifier::Notifier>,
    chat: super::ChatServices,
    on_exit: impl FnOnce(ExitReason) + Send + 'static,
) -> Result<SessionHandle, ServerError> {
    let id = info.id.get();
    let (sender, receiver) = mpsc::channel();
    let (ready_sender, ready) = mpsc::channel();
    pty.start_reader(sender.clone())
        .map_err(ServerError::spawn_failed)?;

    let published = std::sync::Arc::new(super::Published::new(&info, notifier));
    let owner_published = std::sync::Arc::clone(&published);
    let session = info.clone();
    let failed = sender.clone();
    #[cfg(target_os = "macos")]
    let chat_events = sender.clone();
    #[cfg(not(target_os = "macos"))]
    let _ = chat;
    thread::Builder::new()
        .name(format!("session-{id}"))
        .spawn(move || {
            let terminal =
                match Terminal::new(size, history_budget_bytes).and_then(|mut terminal| {
                    if let Some(colors) = colors {
                        set_colors(&mut terminal, &colors)?;
                    }
                    Ok(terminal)
                }) {
                    Ok(terminal) => terminal,
                    Err(error) => {
                        let _ = pty.kill();
                        let _ = pty.wait();
                        let _ = ready_sender.send(Err(ServerError::spawn_failed(error)));
                        return;
                    }
                };
            let input = match start_input(&mut pty, failed) {
                Ok(input) => input,
                Err(error) => {
                    let _ = pty.kill();
                    let _ = pty.wait();
                    let _ = ready_sender.send(Err(error));
                    return;
                }
            };
            let _ = ready_sender.send(Ok(()));
            #[cfg(target_os = "macos")]
            let chat = super::chat::Host::new(session.clone(), chat, chat_events, input.clone());
            let owner = Owner {
                metadata: Metadata::new(session.directory.clone()),
                published: owner_published,
                activity,
                detector: crate::detection::Detector::default(),
                detection_dirty: true,
                next_detection: Instant::now(),
                detection_screen: String::new(),
                detection_progress: String::new(),
                program_group: None,
                program_reports: 0,
                seen_reports: 0,
                info: session,
                pty,
                terminal,
                size,
                events: receiver,
                input,
                attachments: HashMap::new(),
                output_pending: false,
                resize_pending: false,
                compress_pending: false,
                history_total: 0,
                history_generation: 0,
                input_modes: InputModes::default(),
                cursor_blinking: true,
                links: Vec::new(),
                graphics: muxy_protocol::Graphics::default(),
                prompts: Vec::new(),
                frame_state: None,
                next_tick: None,
                tick_deferred: false,
                synchronized_since: None,
                output_state: OutputState::Open,
                archive,
                next_checkpoint: Some(Instant::now() + CHECKPOINT),
                next_metadata: Instant::now(),
                #[cfg(target_os = "macos")]
                chat,
            };
            on_exit(owner.run());
        })
        .map_err(ServerError::spawn_failed)?;

    ready
        .recv()
        .map_err(|_| ServerError::spawn_failed("session thread stopped before it was ready"))??;
    Ok(SessionHandle::new(info, sender, published))
}

fn set_colors(
    terminal: &mut Terminal,
    colors: &TerminalColors,
) -> Result<(), muxy_terminal::TerminalError> {
    terminal.set_defaults(colors)
}

fn start_input(
    pty: &mut Pty,
    events: Sender<OwnerEvent>,
) -> Result<std::sync::Arc<InputGate>, ServerError> {
    let writer = pty.take_writer().map_err(ServerError::spawn_failed)?;
    spawn_input_writer(writer, format!("session-{}-input", pty.child_pid()), events)
}

fn spawn_input_writer(
    mut writer: impl Write + Send + 'static,
    name: String,
    events: Sender<OwnerEvent>,
) -> Result<std::sync::Arc<InputGate>, ServerError> {
    let (input, pending) = mpsc::channel::<InputWrite>();
    let closed = std::sync::Arc::new(AtomicBool::new(false));
    let gate = std::sync::Arc::new(InputGate::new(input, closed.clone()));
    thread::Builder::new()
        .name(name)
        .spawn(move || {
            for input in pending {
                if !input.authorized && closed.load(Ordering::Acquire) {
                    input.refuse();
                    continue;
                }
                let result = writer.write_all(&input.bytes);
                if let Some(reply) = input.reply {
                    let acknowledgement = result.as_ref().copied().map_err(|error| {
                        ServerError::new(
                            ErrorCode::BadRequest,
                            format!("terminal input write failed: {error}"),
                        )
                    });
                    let _ = reply.send(acknowledgement);
                }
                if let Err(error) = result {
                    let _ = events.send(OwnerEvent::WriteFailed(error));
                    break;
                }
            }
        })
        .map_err(ServerError::spawn_failed)?;
    Ok(gate)
}

impl Owner {
    fn run(mut self) -> ExitReason {
        let reason = self.serve().unwrap_or_else(|_| {
            let _ = self.pty.kill();
            self.pty.wait().map_or(ExitReason::Ended, exit_reason)
        });
        self.drain_output();
        self.update_metadata();
        self.activity.remove(self.info.id);
        self.checkpoint(Some(reason));
        for attachment in self.attachments.values_mut() {
            attachment.sink.send(AttachmentEvent::Ended(reason));
        }
        reason
    }

    #[allow(
        clippy::too_many_lines,
        reason = "One exhaustive dispatch preserves ordering between terminal input and PTY output"
    )]
    fn serve(&mut self) -> Result<ExitReason, Fault> {
        loop {
            match self.next_wake() {
                Wake::Event(OwnerEvent::Pty(PtyEvent::Output(bytes))) => self.feed(&bytes)?,
                Wake::Event(OwnerEvent::Pty(PtyEvent::Closed)) => {
                    self.output_state = OutputState::Closed;
                }
                Wake::Event(OwnerEvent::WriteFailed(error)) => return Err(error.into()),
                Wake::Event(OwnerEvent::Command(SessionCommand::Input(bytes))) => {
                    self.input.send(bytes.into())?;
                }
                Wake::Event(OwnerEvent::Command(SessionCommand::TerminalInput(input))) => {
                    self.terminal_input(input)?;
                }
                Wake::Event(OwnerEvent::Command(SessionCommand::WriteInput { bytes, reply })) => {
                    self.input.send(InputWrite {
                        bytes,
                        reply: Some(reply),
                        authorized: false,
                    })?;
                }
                Wake::Event(OwnerEvent::Command(SessionCommand::ClearScreen { reply })) => {
                    let result = self.clear_screen().map_err(|error| {
                        ServerError::new(ErrorCode::BadRequest, format!("terminal clear: {error}"))
                    });
                    let _ = reply.send(result);
                }
                Wake::Event(OwnerEvent::Command(SessionCommand::CellSize(cell))) => {
                    self.pty.set_cell_size(cell)?;
                    self.terminal.set_cell_size(cell)?;
                    self.output_pending = true;
                }
                Wake::Event(OwnerEvent::Command(SessionCommand::Mouse(event))) => {
                    let bytes = self.terminal.encode_mouse(&event)?;
                    if !bytes.is_empty() {
                        self.input.send(bytes.into())?;
                    }
                }
                Wake::Event(OwnerEvent::Command(SessionCommand::SetColors(colors))) => {
                    set_colors(&mut self.terminal, &colors)?;
                    let answers = self.terminal.take_pty_output();
                    if !answers.is_empty() {
                        self.input.send(answers.into())?;
                    }
                    self.output_pending = true;
                }
                Wake::Event(OwnerEvent::Command(SessionCommand::Resize(size))) => {
                    self.resize(size)?;
                }
                Wake::Event(OwnerEvent::Command(SessionCommand::ResizeAttachment { id, size })) => {
                    self.resize(size)?;
                    self.broadcast_frame(Some(id))?;
                    self.output_pending = false;
                    self.resize_pending = false;
                }
                Wake::Event(OwnerEvent::Command(SessionCommand::Attach {
                    id,
                    channel,
                    size,
                    sink,
                })) => self.attach(id, channel, size, sink)?,
                Wake::Event(OwnerEvent::Command(SessionCommand::AttachWithoutResize {
                    id,
                    channel,
                    sink,
                })) => self.attach(id, channel, self.size, sink)?,
                Wake::Event(OwnerEvent::Command(SessionCommand::HistoryPage {
                    before,
                    max_rows,
                    reply,
                })) => {
                    let _ = reply.send(self.history_page(before, max_rows));
                }
                Wake::Event(OwnerEvent::Command(SessionCommand::Detach(id))) => {
                    self.attachments.remove(&id);
                }
                Wake::Event(OwnerEvent::Command(SessionCommand::ImageFrames {
                    image,
                    keys,
                    reply,
                })) => {
                    let frames = self.terminal.image_frames(image, &keys).map_err(|error| {
                        ServerError::new(ErrorCode::BadRequest, format!("image frames: {error}"))
                    });
                    let _ = reply.send(frames);
                }
                Wake::Event(OwnerEvent::Command(SessionCommand::Search {
                    query,
                    ignore_case,
                    before,
                    max_results,
                    reply,
                })) => {
                    let _ = reply.send(self.search(&query, ignore_case, before, max_results));
                }
                Wake::Event(OwnerEvent::Command(SessionCommand::End)) => {
                    return self.terminate(ExitReason::Ended);
                }
                Wake::Event(OwnerEvent::Command(SessionCommand::Stop)) => {
                    return self.terminate(ExitReason::ServerStopped);
                }
                #[cfg(target_os = "macos")]
                Wake::Event(OwnerEvent::Command(SessionCommand::Chat(command))) => {
                    self.chat.command(command, &self.pty);
                    self.chat_changed();
                }
                #[cfg(not(target_os = "macos"))]
                Wake::Event(OwnerEvent::Command(SessionCommand::Chat(command))) => {
                    command.reply.send(muxy_protocol::ChatReply::Refused {
                        reason: muxy_protocol::ChatRefusal::PlatformUnsupported,
                        detail: None,
                    });
                }
                #[cfg(target_os = "macos")]
                Wake::Event(OwnerEvent::Chat(event)) => {
                    self.chat.event(event);
                    self.chat_changed();
                }
                Wake::Tick => self.tick()?,
                Wake::Checkpoint => self.checkpoint(None),
                Wake::Metadata => self.update_metadata(),
                Wake::Orphaned => return self.terminate(ExitReason::ServerStopped),
            }
            if self.output_state == OutputState::Closed
                && let Some(status) = self.pty.try_wait()
            {
                return Ok(exit_reason(status));
            }
            if self.next_tick.is_none() && self.has_pending_work() {
                self.next_tick = Some(Instant::now() + TICK);
            }
        }
    }

    fn next_wake(&self) -> Wake {
        let now = Instant::now();
        if self.next_tick.is_some_and(|tick| tick <= now) {
            return Wake::Tick;
        }
        if self
            .next_checkpoint
            .is_some_and(|checkpoint| checkpoint <= now)
        {
            return Wake::Checkpoint;
        }
        if self.next_metadata <= now {
            return Wake::Metadata;
        }
        let deadline = self
            .next_tick
            .unwrap_or(self.next_metadata)
            .min(self.next_checkpoint.unwrap_or(self.next_metadata))
            .min(self.next_metadata);
        let received = self
            .events
            .recv_timeout(deadline.saturating_duration_since(now));
        match received {
            Ok(event) => Wake::Event(event),
            Err(RecvTimeoutError::Timeout) if Some(deadline) == self.next_checkpoint => {
                Wake::Checkpoint
            }
            Err(RecvTimeoutError::Timeout) if deadline == self.next_metadata => Wake::Metadata,
            Err(RecvTimeoutError::Timeout) => Wake::Tick,
            Err(RecvTimeoutError::Disconnected) => Wake::Orphaned,
        }
    }

    fn has_pending_work(&self) -> bool {
        self.output_pending
            || self.resize_pending
            || self.compress_pending
            || self.output_state == OutputState::Closed
    }

    fn feed(&mut self, bytes: &[u8]) -> Result<(), Fault> {
        self.detection_dirty = true;
        self.terminal.feed(bytes);
        if self.terminal.program_reports() != self.program_reports {
            self.program_reports = self.terminal.program_reports();
            self.program_group = self.pty.foreground_pid();
        }
        if self.terminal.synchronized_output()? {
            self.synchronized_since.get_or_insert_with(Instant::now);
        } else {
            self.synchronized_since = None;
            if self.tick_deferred {
                self.next_tick = Some(Instant::now());
            }
        }
        let answers = self.terminal.take_pty_output();
        if !answers.is_empty() {
            self.input.send(answers.into())?;
        }
        self.output_pending = true;
        self.compress_pending = true;
        self.next_checkpoint
            .get_or_insert_with(|| Instant::now() + CHECKPOINT);
        Ok(())
    }

    fn terminal_input(&mut self, input: muxy_protocol::TerminalInput) -> Result<(), Fault> {
        use muxy_protocol::TerminalInput;
        let bytes = match input {
            TerminalInput::Key(event) => self.terminal.encode_key(event)?,
            TerminalInput::Paste(bytes) => self.terminal.encode_paste(bytes)?,
            TerminalInput::Focus(focused) => self.terminal.encode_focus(focused)?,
            TerminalInput::ClearScreen => return self.clear_screen(),
        };
        if !bytes.is_empty() {
            self.input.send(bytes.into())?;
        }
        Ok(())
    }

    fn clear_screen(&mut self) -> Result<(), Fault> {
        if self.terminal.clear_screen()? {
            self.resize_pending = true;
            self.output_pending = true;
            self.detection_dirty = true;
            self.next_checkpoint = Some(Instant::now());
            let redraw = self.terminal.take_pty_output();
            if !redraw.is_empty() {
                self.input.send(redraw.into())?;
            }
        }
        Ok(())
    }

    fn resize(&mut self, size: Size) -> Result<(), Fault> {
        self.pty.resize(size.into())?;
        self.terminal.resize(size)?;
        self.size = size;
        self.resize_pending = true;
        self.next_checkpoint
            .get_or_insert_with(|| Instant::now() + CHECKPOINT);
        Ok(())
    }

    fn attach(
        &mut self,
        id: AttachmentId,
        channel: ChannelId,
        size: Size,
        mut sink: AttachmentSink,
    ) -> Result<(), Fault> {
        if self.attachments.is_empty() && size != self.size {
            self.resize(size)?;
        }
        self.update_metadata();
        self.update_cursor_blinking()?;
        // Establish one common baseline before adding an attachment. Existing
        // clients receive pending changes before the new client's snapshot.
        if self.attachments.is_empty() {
            self.terminal.take_changed_rows()?;
            self.links = self.terminal.screen_links();
            self.prompts = self.terminal.screen_prompts()?;
            self.frame_state = Some((self.terminal.cursor()?, self.terminal.modes()?));
        } else {
            self.broadcast_frame(None)?;
        }
        self.output_pending = false;
        self.resize_pending = false;
        let history = self.history_page(HistoryCursor(0), 200)?;
        let screen = history
            .screen
            .ok_or_else(|| io::Error::other("fresh history has no screen"))?;
        let snapshot = AttachSnapshot {
            links: history.links,
            graphics: screen.graphics,
            prompts: history.prompts,
            channel,
            size: self.size,
            rows: screen.rows,
            cursor: screen.cursor,
            modes: self.terminal.modes()?,
            title: self.metadata.title.clone(),
            directory: self.metadata.directory.clone(),
            history: history.rows,
            history_cursor: history.next,
            history_total: history.total_rows,
        };
        if sink.send(AttachmentEvent::Snapshot {
            snapshot,
            process: self.metadata.process.clone(),
        }) {
            sink.send(AttachmentEvent::Metadata(MetadataEvent::InputModes(
                self.terminal.input_modes()?,
            )));
            sink.send(AttachmentEvent::Metadata(MetadataEvent::CursorBlinking(
                self.cursor_blinking,
            )));
            sink.send(AttachmentEvent::Metadata(MetadataEvent::Links {
                seq: 0,
                rows: self.links.clone(),
            }));
            if let Some(session) = self.metadata.agent_session.clone() {
                sink.send(AttachmentEvent::Metadata(session.event()));
            }
            #[cfg(target_os = "macos")]
            if let Some(mode) = self.chat.mode_event() {
                sink.send(AttachmentEvent::Metadata(mode));
            }
            self.attachments.insert(id, Attachment { sink, seq: 1 });
        }
        Ok(())
    }

    fn history_page(
        &mut self,
        before: HistoryCursor,
        max_rows: u16,
    ) -> Result<HistoryPage, ServerError> {
        let terminal_error = |error| {
            ServerError::new(
                ErrorCode::HistoryUnavailable,
                format!("terminal history: {error}"),
            )
        };
        let generation = self.terminal.history_generation().map_err(terminal_error)?;
        let total = self.terminal.history_rows().map_err(terminal_error)?;
        let (range, mut next) = history_range(self.info.id, generation, before, max_rows, total)?;
        let requested = range.len();
        self.compress_pending = true;
        let muxy_terminal::TerminalHistory {
            rows: history,
            mut prompts,
            mut links,
        } = self
            .terminal
            .history_with_links(range)
            .map_err(terminal_error)?;
        if history.len() < requested {
            let kept = u16::try_from(history.len()).unwrap_or(max_rows);
            next = history_range(self.info.id, generation, before, kept, total)?.1;
        }
        let screen = if before.0 == 0 {
            links.extend(
                self.terminal
                    .read_screen_links(muxy_protocol::MAX_LINK_SPANS)
                    .map_err(terminal_error)?
                    .into_iter()
                    .filter_map(|mut row| {
                        row.row = u16::try_from(history.len() + usize::from(row.row)).ok()?;
                        Some(row)
                    }),
            );
            prompts.extend(
                self.terminal
                    .screen_prompts()
                    .map_err(terminal_error)?
                    .into_iter()
                    .filter_map(|row| u16::try_from(history.len() + usize::from(row)).ok()),
            );
            Some(SavedScreen {
                graphics: self.terminal.graphics().map_err(terminal_error)?,
                size: self.size,
                rows: self.terminal.screen().map_err(terminal_error)?,
                cursor: self.terminal.cursor().map_err(terminal_error)?,
                reason: None,
            })
        } else {
            None
        };
        bound_history_page(
            self.info.id,
            generation,
            before,
            HistoryPage {
                links: Some(links),
                prompts,
                rows: history,
                next,
                total_rows: total as u64,
                screen,
            },
        )
    }

    fn update_cursor_blinking(&mut self) -> Result<(), Fault> {
        let blinking = self.terminal.cursor_blinking()?;
        if blinking != self.cursor_blinking {
            self.cursor_blinking = blinking;
            self.attachments.retain(|_, attachment| {
                attachment
                    .sink
                    .send(AttachmentEvent::Metadata(MetadataEvent::CursorBlinking(
                        blinking,
                    )))
            });
        }
        Ok(())
    }

    fn tick(&mut self) -> Result<(), Fault> {
        let now = Instant::now();
        if self.terminal.synchronized_output()? {
            let since = self.synchronized_since.get_or_insert(now);
            if now.duration_since(*since) < SYNC_TIMEOUT {
                self.tick_deferred = true;
                self.next_tick = Some(*since + SYNC_TIMEOUT);
                return Ok(());
            }
            self.terminal.end_synchronized_output()?;
        }
        self.tick_deferred = false;
        self.synchronized_since = None;
        self.next_tick = Some(now + TICK);
        self.update_metadata();
        let modes = self.terminal.input_modes()?;
        if modes != self.input_modes {
            self.input_modes = modes;
            self.attachments.retain(|_, attachment| {
                attachment
                    .sink
                    .send(AttachmentEvent::Metadata(MetadataEvent::InputModes(modes)))
            });
        }
        self.update_cursor_blinking()?;
        if self.output_pending || self.resize_pending {
            self.broadcast_frame(None)?;
            self.output_pending = false;
            self.resize_pending = false;
        } else if self.compress_pending {
            self.terminal.compress_idle()?;
            self.compress_pending = false;
        }
        if !self.has_pending_work() {
            self.next_tick = None;
        }
        Ok(())
    }

    fn search(
        &mut self,
        query: &str,
        ignore_case: bool,
        before: HistoryCursor,
        max_results: u16,
    ) -> Result<SearchPage, ServerError> {
        let failed =
            |error| ServerError::new(ErrorCode::HistoryUnavailable, format!("search: {error}"));
        let history_rows = self.terminal.history_rows().map_err(failed)?;
        let search = Search {
            session: self.info.id,
            generation: self.terminal.history_generation().map_err(failed)?,
            query,
            ignore_case,
            before,
            max_results,
            history_rows,
            screen_rows: usize::from(self.size.rows),
        };
        let range = search.range()?;
        self.compress_pending = true;
        let history = self
            .terminal
            .history(range.start.min(history_rows)..range.end.min(history_rows))
            .map_err(failed)?;
        let screen = if range.end > history_rows {
            self.terminal.screen().map_err(failed)?
        } else {
            Vec::new()
        };
        search.scan(|index| {
            Ok(if index < history_rows {
                &history[index - range.start].runs
            } else {
                &screen[index - history_rows].runs
            })
        })
    }

    fn update_metadata(&mut self) {
        let terminal = self.terminal.take_events();
        for event in &terminal {
            if let muxy_terminal::TerminalEvent::Progress(progress) = event {
                use muxy_protocol::ProgressState;
                self.detection_progress = progress.progress.map_or_else(
                    || "4;0;0".into(),
                    |p| match p.state {
                        ProgressState::Running => {
                            format!("4;1;{}", p.percent.map_or(-1, i16::from))
                        }
                        ProgressState::Indeterminate | ProgressState::Unrecognized(_) => {
                            "4;1;-1".into()
                        }
                        ProgressState::Error => "4;2".into(),
                        ProgressState::Paused => "4;4".into(),
                    },
                );
                self.published.set_progress(*progress);
            }
        }
        let events = self.metadata.update(&self.pty, terminal);
        if events.iter().any(|event| {
            matches!(
                event,
                MetadataEvent::Title(_)
                    | MetadataEvent::Directory(_)
                    | MetadataEvent::ForegroundProcess { .. }
            )
        }) {
            self.published.set_metadata(muxy_protocol::SessionMetadata {
                title: self.metadata.title.clone(),
                directory: self.metadata.directory.clone(),
                process: self.metadata.process.clone(),
            });
        }
        for event in events {
            self.attachments.retain(|_, attachment| {
                attachment
                    .sink
                    .send(AttachmentEvent::Metadata(event.clone()))
            });
        }
        if self.report_chat_activity() {
            // Chat state, not the terminal (nor Claude's hooks), is the activity source.
            self.next_metadata = Instant::now() + Duration::from_secs(1);
            return;
        }
        let (agent, status) = crate::detection::reporting_agent(
            self.metadata.agent,
            self.detector.provider,
            self.reported_status(),
        );
        if agent != self.detector.provider {
            self.detection_dirty = true;
            self.detection_progress.clear();
        }
        let now = Instant::now();
        if now >= self.next_detection {
            self.next_detection = now + METADATA_POLL;
            self.seen_reports = self.terminal.program_reports();
            if let Some(agent) = agent {
                let completed = if let Some(status) = status {
                    self.detector.report(agent, status.state)
                } else {
                    self.detect_on_screen(agent, now)
                };
                if let Some(provider) = self.detector.provider {
                    self.activity.update(
                        muxy_protocol::AgentActivity {
                            session: self.info.id,
                            project: self.published.project(),
                            provider,
                            state: self.detector.state,
                        },
                        completed,
                    );
                }
            } else {
                self.detector = crate::detection::Detector::default();
                self.detection_screen.clear();
                self.activity.clear_agent(self.info.id);
            }
        }
        self.next_metadata = Instant::now()
            + if agent.is_some() {
                METADATA_POLL
            } else {
                Duration::from_secs(1)
            };
    }

    /// Fork: broadcasts a chat mode change and refreshes chat activity.
    #[cfg(target_os = "macos")]
    fn chat_changed(&mut self) {
        if let Some(event) = self.chat.take_mode_change() {
            self.attachments.retain(|_, attachment| {
                attachment
                    .sink
                    .send(AttachmentEvent::Metadata(event.clone()))
            });
        }
        self.report_chat_activity();
    }

    /// Reports activity from chat state while the session isn't in Console.
    /// Returns whether it did.
    #[cfg_attr(
        not(target_os = "macos"),
        allow(clippy::unused_self, reason = "chat is macOS only")
    )]
    fn report_chat_activity(&mut self) -> bool {
        #[cfg(target_os = "macos")]
        if let Some((state, completed)) = self.chat.activity() {
            self.activity.update(
                muxy_protocol::AgentActivity {
                    session: self.info.id,
                    project: self.info.project,
                    provider: self.chat.provider(),
                    state,
                },
                completed,
            );
            return true;
        }
        false
    }

    /// The status the foreground program reported itself (OSC 7501). A report
    /// the detector hasn't seen counts once after its program has gone, so a
    /// final `done` still finishes the agent's work.
    fn reported_status(&mut self) -> Option<muxy_terminal::ProgramStatus> {
        let group = self.pty.foreground_pid();
        let shell = self.metadata.shell_foreground;
        if shell || group != self.program_group {
            self.terminal.end_program_status();
        }
        let unseen = self.terminal.program_reports() != self.seen_reports;
        let reporter_running = !shell && self.program_group == group;
        self.terminal
            .program_status()
            .filter(|_| group.is_some() && (unseen || reporter_running))
    }

    fn detect_on_screen(&mut self, agent: muxy_protocol::AgentProvider, now: Instant) -> bool {
        if self.detection_dirty && !self.terminal.synchronized_output().unwrap_or(true) {
            if let Ok(screen) = self.terminal.detection_text() {
                self.detection_screen = screen;
            }
            self.detection_dirty = false;
        }
        self.detector.update(
            Some(agent),
            self.detection_screen.clone(),
            &self.metadata.title,
            &self.detection_progress,
            now,
        )
    }

    fn broadcast_frame(&mut self, resized: Option<AttachmentId>) -> Result<(), Fault> {
        if self.attachments.is_empty() {
            return Ok(());
        }
        let total_rows = self.terminal.history_rows()? as u64;
        let generation = self.terminal.history_generation()?;
        if total_rows != self.history_total || generation != self.history_generation {
            self.history_total = total_rows;
            self.history_generation = generation;
            self.attachments.retain(|_, attachment| {
                attachment
                    .sink
                    .send(AttachmentEvent::Metadata(MetadataEvent::History {
                        total_rows,
                    }))
            });
        }
        let graphics = self.terminal.graphics()?;
        let graphics_changed = graphics != self.graphics || self.resize_pending;
        self.graphics = graphics;
        let frame = ScreenFrame {
            size: self.size,
            graphics: graphics_changed.then(|| self.graphics.clone()),
            seq: 0,
            reset: self.resize_pending,
            rows: self.terminal.take_changed_rows()?,
            cursor: self.terminal.cursor()?,
            modes: self.terminal.modes()?,
        };
        let links = self.terminal.screen_links();
        let prompts = self.terminal.screen_prompts()?;
        let prompts_changed = self.prompts != prompts;
        self.prompts = prompts;
        let links_changed = self.links != links;
        self.links = links;
        let state = (frame.cursor, frame.modes);
        if !graphics_changed
            && !links_changed
            && !prompts_changed
            && !frame.reset
            && frame.rows.is_empty()
            && self.frame_state == Some(state)
        {
            return Ok(());
        }
        self.frame_state = Some(state);
        self.attachments.retain(|id, attachment| {
            let frame = ScreenFrame {
                seq: attachment.seq,
                ..frame.clone()
            };
            if prompts_changed || frame.reset {
                attachment
                    .sink
                    .send(AttachmentEvent::Metadata(MetadataEvent::ScreenPrompts {
                        seq: frame.seq,
                        rows: self.prompts.clone(),
                    }));
            }
            if links_changed || frame.reset {
                attachment
                    .sink
                    .send(AttachmentEvent::Metadata(MetadataEvent::Links {
                        seq: frame.seq,
                        rows: self.links.clone(),
                    }));
            }
            attachment.seq += 1;
            let event = if resized == Some(*id) {
                AttachmentEvent::Resized(frame)
            } else {
                AttachmentEvent::Frame(frame)
            };
            attachment.sink.send(event)
        });
        Ok(())
    }

    fn terminate(&mut self, reason: ExitReason) -> Result<ExitReason, Fault> {
        self.pty.kill()?;
        self.pty.wait()?;
        Ok(reason)
    }

    fn drain_output(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while self.output_state == OutputState::Open {
            match self
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(OwnerEvent::Pty(PtyEvent::Output(bytes))) => {
                    self.terminal.feed(&bytes);
                    self.terminal.take_pty_output();
                }
                Ok(OwnerEvent::Pty(PtyEvent::Closed)) => self.output_state = OutputState::Closed,
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }

    fn checkpoint(&mut self, reason: Option<ExitReason>) {
        self.next_checkpoint = None;
        self.compress_pending = true;
        let mut snapshot = self.terminal.snapshot();
        if reason.is_some() && matches!(snapshot, Ok(None)) {
            // The program ended inside a sequence; cancel it so its last screen is kept.
            self.terminal.feed(CANCEL);
            snapshot = self.terminal.snapshot();
        }
        let result = match snapshot {
            Ok(Some(snapshot)) => self.archive.save(self.info.id, snapshot, reason),
            // Output stopped inside a sequence; the output that completes it schedules the save.
            Ok(None) if reason.is_none() => return,
            Ok(None) => Err(io::Error::other("output ended inside a sequence")),
            Err(error) => Err(io::Error::other(error)),
        };
        if let Err(error) = result {
            log::error!(
                "session {} could not save terminal content: {error}",
                self.info.id.get()
            );
        }
    }
}

fn exit_reason(status: ExitStatus) -> ExitReason {
    match (status.code, status.signal) {
        (Some(code), _) => ExitReason::Exited(code),
        (None, Some(signal)) => ExitReason::Signaled(signal),
        (None, None) => ExitReason::Ended,
    }
}

#[cfg(all(test, target_os = "macos"))]
mod input_tests {
    use super::*;
    use crate::session::chat::flush::{FlushTarget, gate_and_flush};
    use muxy_terminal::pty::{PtySize, SpawnRequest};

    type TestResult = Result<(), Box<dyn Error>>;
    const WAIT: Duration = Duration::from_secs(2);

    /// Bytes of complete lines the terminal holds that no program has read.
    #[allow(unsafe_code, reason = "FIONREAD is only reachable through ioctl")]
    fn unread(path: &std::path::Path) -> io::Result<i32> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        let device = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(path)?;
        let mut count: libc::c_int = 0;
        // SAFETY: `device` is open and FIONREAD writes one int to `count`.
        if unsafe { libc::ioctl(device.as_raw_fd(), libc::FIONREAD, &raw mut count) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(count)
    }

    fn write(bytes: &[u8]) -> (InputWrite, Receiver<Result<(), ServerError>>) {
        let (reply, written) = mpsc::channel();
        let input = InputWrite {
            bytes: bytes.to_vec(),
            reply: Some(reply),
            authorized: false,
        };
        (input, written)
    }

    #[test]
    fn gated_input_never_reaches_the_terminal() -> TestResult {
        let mut pty = Pty::spawn(SpawnRequest {
            program: "/bin/sleep".into(),
            args: vec!["30".into()],
            cwd: "/".into(),
            env: Vec::new(),
            size: PtySize { cols: 80, rows: 24 },
        })?;
        let (events, _events) = mpsc::channel();
        let gate = start_input(&mut pty, events)?;
        let target = FlushTarget::new(&pty)?;
        let path = pty.tty_path().ok_or("no terminal path")?.to_path_buf();

        let (typed, written) = write(b"typed\n");
        gate.send(typed)?;
        written.recv_timeout(WAIT)??;
        gate.send(b"queued\n".to_vec().into())?;
        let before = unread(&path)?;
        gate_and_flush(&gate, &target, WAIT)?;
        let after = unread(&path)?;

        let (late, refused) = write(b"late\n");
        gate.send(late)?;
        let refused = refused.recv_timeout(WAIT)?;
        gate.send(b"mouse\n".to_vec().into())?;
        let recovery = b"claude --resume x\n";
        let (reply, recovered) = mpsc::channel();
        gate.send_authorized(recovery.to_vec(), Some(reply))?;
        recovered.recv_timeout(WAIT)??;
        let delivered = unread(&path)?;
        pty.kill()?;
        pty.wait()?;

        assert!(before > 0, "the typed line was pending before the flush");
        assert_eq!(after, 0, "the flush discarded every unread byte");
        assert!(refused.is_err(), "WriteInput is refused while gated");
        assert_eq!(
            usize::try_from(delivered)?,
            recovery.len(),
            "only the authorized recovery line reached the terminal"
        );
        Ok(())
    }

    struct Stalled {
        started: Sender<Vec<u8>>,
        release: Receiver<()>,
    }

    impl Write for Stalled {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.started.send(bytes.to_vec());
            let _ = self.release.recv();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn idle_waits_for_the_write_in_progress() -> TestResult {
        let (started, writes) = mpsc::channel();
        let (release, stalled) = mpsc::channel();
        let (events, _events) = mpsc::channel();
        let gate = spawn_input_writer(
            Stalled {
                started,
                release: stalled,
            },
            "test-input".into(),
            events,
        )?;
        gate.send(b"first".to_vec().into())?;
        let first = writes.recv_timeout(WAIT)?;
        let (second, refused) = write(b"second");
        gate.send(second)?;

        let fence = gate.close();
        let busy = fence.recv_timeout(Duration::from_millis(100));
        release.send(())?;
        fence.recv_timeout(WAIT)??;

        assert_eq!(first, b"first");
        assert!(
            matches!(busy, Err(RecvTimeoutError::Timeout)),
            "not idle while a write is in progress"
        );
        assert!(
            refused.recv_timeout(WAIT)?.is_err(),
            "input queued before the gate closed is dropped, not written"
        );
        assert!(writes.try_recv().is_err());
        Ok(())
    }
}
