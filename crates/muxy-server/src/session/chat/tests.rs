//! Owner-level chat transitions against a fake Claude in a real PTY. The fake
//! is a shell script; the real `claude` never runs.

use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::{Duration, Instant};

use muxy_protocol::{
    ChannelId, ChatBatch, ChatMode, ChatRefusal, ChatReply, ChatRequest, Message, MetadataEvent,
    ProjectId, ServerPath, SessionId, SessionInfo, Size,
};
use muxy_terminal::pty::{Pty, PtySize, SpawnRequest};

use super::context::{ClaudeContext, ContextError};
use super::host::{Outcome, Timeouts};
use super::reserve::ReserveError;
use super::supervisor::{Completion, SupervisorEvent};
use super::{Event, Services};
use crate::archive::Archive;
use crate::connection::Connection;
use crate::session::{
    AttachmentEvent, AttachmentId, ChatCommand, ChatReplyTo, OwnerEvent, SessionCommand,
    SessionHandle,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const UUID: &str = "bde3d35b-d3c0-49d7-93c1-9a0391781702";
/// The conversation Start begins.
const NEW_UUID: &str = "5f0c2a44-8d1e-4b7a-9c3f-2e6d1a0b9c88";
const WAIT: Duration = Duration::from_secs(10);

/// Answers `--version`, plays headless Claude for `-p`, and otherwise
/// stands in for the TUI: it logs its arguments and sleeps.
const FAKE: &str = r#"#!/bin/sh
dir=$(dirname "$0")
case "$1" in
--version) echo '2.1.319 (Claude Code)'; exit 0 ;;
-p) ;;
*) echo $$ >>"$dir/tui.pids"; printf '%s\n' "$*" >>"$dir/tui.log"
  # Saves its first prompt to the file named there as it is stopped.
  trap '[ -e "$dir/save-on-exit" ] && printf "{\"type\":\"user\"}\n" >>"$(cat "$dir/save-on-exit")"; exit 0' TERM
  sleep 600 & wait; exit 0 ;;
esac
echo $$ >"$dir/headless.pid"
printf '%s\n' "$*" >"$dir/headless.args"
session= prev=
for arg; do [ "$prev" = --session-id ] && session=$arg; prev=$arg; done
while IFS= read -r line; do
  printf '%s\n' "$line" >>"$dir/wire.log"
  [ -e "$dir/silent" ] && continue
  id=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
  case "$line" in
  *'"subtype":"initialize"'*)
    [ -e "$dir/slow" ] && sleep 1
    printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"models":[{"value":"default","displayName":"Default","supportedEffortLevels":["low","high"]}]}}}\n' "$id" ;;
  *'"type":"control_request"'*)
    printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$id" ;;
  *'exit please'*) printf 'bye now' >&2; exit 3 ;;
  *'"type":"user"'*)
    # Like Claude, a new conversation's transcript appears with its first prompt.
    [ -n "$session" ] && printf '{"type":"user"}\n' >>"$dir/config/projects/-repo/$session.jsonl"
    printf '%s\n' "$line"
    printf '{"type":"assistant","message":{"id":"msg_1","role":"assistant","content":[{"type":"text","text":"pong"}]},"parent_tool_use_id":null}\n'
    printf '{"type":"result","subtype":"success","is_error":false,"result":"pong"}\n' ;;
  esac
done
"#;

struct Fixture {
    root: PathBuf,
    config: PathBuf,
    services: Services,
    session: SessionHandle,
    modes: Receiver<AttachmentEvent>,
}

impl Fixture {
    /// A session running the fake TUI.
    fn new(timeouts: Timeouts) -> Result<Self, Box<dyn std::error::Error>> {
        let fixture = Self::idle(timeouts)?;
        // Start the "TUI" the way a user would, by typing it.
        fixture.type_line(&format!(
            "'{}' first",
            fixture.root.join("claude").display()
        ))?;
        fixture.wait_tui_launches(1)?;
        Ok(fixture)
    }

    /// A session at its shell prompt.
    fn idle(timeouts: Timeouts) -> Result<Self, Box<dyn std::error::Error>> {
        Self::idle_in(timeouts, &["/bin/sh", "-i"])
    }

    /// A session at the prompt of `shell` (program and arguments).
    fn idle_in(timeouts: Timeouts, shell: &[&str]) -> Result<Self, Box<dyn std::error::Error>> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "muxy-chat-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        let repo = root.join("repo");
        let config = root.join("config");
        fs::create_dir_all(&repo)?;
        fs::create_dir_all(config.join("projects/-repo"))?;
        fs::write(
            config.join(format!("projects/-repo/{UUID}.jsonl")),
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        )?;
        let fake = root.join("claude");
        fs::write(&fake, FAKE)?;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755))?;

        let resolve_root = root.clone();
        let shell_root = root.clone();
        let services = Services {
            reservations: Arc::default(),
            resolve: Arc::new(move |shell, foreground, _, _| {
                let group = foreground
                    .filter(|group| *group != shell)
                    .ok_or(ContextError::NoClaudeSession)?;
                let pid = i32::try_from(group).map_err(|_| ContextError::NoClaudeSession)?;
                Ok(fake_context(&resolve_root, shell, group, Some(pid)))
            }),
            resolve_shell: Arc::new(move |shell, _, _, _| {
                Ok(fake_context(&shell_root, shell, shell, None))
            }),
            timeouts,
        };
        let pty = Pty::spawn(SpawnRequest {
            program: shell[0].into(),
            args: shell[1..].iter().map(|arg| (*arg).into()).collect(),
            cwd: repo,
            env: vec![
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("HOME".into(), root.clone().into()),
                ("PS1".into(), "$ ".into()),
            ],
            size: PtySize { cols: 80, rows: 24 },
        })?;
        let directory = ServerPath(root.as_os_str().as_encoded_bytes().to_vec());
        let size = Size { cols: 80, rows: 24 };
        let session = crate::session::start(
            SessionInfo {
                project: ProjectId::from_u128(1),
                id: SessionId::from(std::num::NonZeroU64::MIN),
                directory,
            },
            pty,
            size,
            1 << 20,
            Archive::memory(0),
            None,
            Arc::default(),
            Arc::default(),
            services.clone(),
            |_| {},
        )?;
        let (sink, modes) = mpsc::channel();
        session.send(SessionCommand::Attach {
            id: AttachmentId(1),
            channel: ChannelId(1),
            size,
            sink: sink.into(),
        })?;
        Ok(Self {
            root,
            config,
            services,
            session,
            modes,
        })
    }

    fn type_line(&self, line: &str) -> TestResult {
        self.session
            .send(SessionCommand::Input(format!("{line}\r").into_bytes()))?;
        Ok(())
    }

    fn chat(&self, request: ChatRequest) -> Result<ChatReply, Box<dyn std::error::Error>> {
        self.chat_from(request, Weak::new())
    }

    fn chat_from(
        &self,
        request: ChatRequest,
        from: Weak<Connection>,
    ) -> Result<ChatReply, Box<dyn std::error::Error>> {
        let (reply, replied) = mpsc::channel();
        self.session.send(SessionCommand::Chat(ChatCommand {
            request,
            from,
            reply: ChatReplyTo::Channel(reply),
        }))?;
        Ok(replied.recv_timeout(WAIT)?)
    }

    fn id(&self) -> SessionId {
        self.session.id()
    }

    fn enter(&self) -> Result<ChatReply, Box<dyn std::error::Error>> {
        self.chat(ChatRequest::Enter {
            session: self.id(),
            model: None,
            effort: Some("high".into()),
        })
    }

    fn leave(&self, generation: u64) -> Result<ChatReply, Box<dyn std::error::Error>> {
        self.chat(ChatRequest::Leave {
            session: self.id(),
            generation,
        })
    }

    fn tui_launches(&self) -> Vec<String> {
        fs::read_to_string(self.root.join("tui.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn wait_tui_launches(&self, count: usize) -> TestResult {
        let deadline = Instant::now() + WAIT;
        while self.tui_launches().len() < count {
            if Instant::now() > deadline {
                return Err(format!("TUI launches: {:?}", self.tui_launches()).into());
            }
            thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    /// The chat modes broadcast so far, in order.
    fn modes(&self) -> Vec<(ChatMode, u64)> {
        self.modes
            .try_iter()
            .filter_map(|event| match event {
                AttachmentEvent::Metadata(MetadataEvent::ChatMode { mode, generation }) => {
                    Some((mode, generation))
                }
                _ => None,
            })
            .collect()
    }

    /// Waits until `mode` is the latest one broadcast; returns every mode seen.
    fn wait_mode(
        &self,
        mode: ChatMode,
    ) -> Result<Vec<(ChatMode, u64)>, Box<dyn std::error::Error>> {
        let deadline = Instant::now() + WAIT;
        let mut modes = Vec::new();
        while modes.last().map(|(seen, _)| *seen) != Some(mode) {
            if Instant::now() > deadline {
                return Err(format!("modes: {modes:?}").into());
            }
            thread::sleep(Duration::from_millis(20));
            modes.extend(self.modes());
        }
        Ok(modes)
    }

    fn reserved(&self) -> bool {
        self.holds(UUID)
    }

    fn holds(&self, id: &str) -> bool {
        matches!(
            self.services.reservations.try_reserve(&self.config, id, 0),
            Err(ReserveError::ConversationInUse)
        )
    }

    fn start(&self) -> Result<ChatReply, Box<dyn std::error::Error>> {
        self.chat(ChatRequest::Start {
            session: self.id(),
            model: None,
            effort: None,
        })
    }

    fn headless_pid(&self) -> Result<i32, Box<dyn std::error::Error>> {
        Ok(fs::read_to_string(self.root.join("headless.pid"))?
            .trim()
            .parse()?)
    }
}

/// What the fake resolvers find: the TUI `tui`'s conversation, or with none
/// Start's new one.
fn fake_context(root: &Path, shell: u32, group: u32, tui: Option<i32>) -> ClaudeContext {
    let mut env = vec![
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("HOME".into(), root.into()),
        ("CLAUDE_CONFIG_DIR".into(), root.join("config").into()),
        (
            "POSH_SESSION_ID".into(),
            muxy_protocol::OperationId::new().to_string().into(),
        ),
    ];
    if root.join("api-key").exists() {
        env.push(("ANTHROPIC_API_KEY".into(), "sk-test".into()));
    }
    ClaudeContext {
        pid: tui.unwrap_or(0),
        group,
        shell,
        launch_prefix: vec![root.join("claude").into()],
        user_args: if tui.is_some() {
            vec!["first".into()]
        } else {
            Vec::new()
        },
        cwd: root.join("repo"),
        env,
        config_dir: root.join("config"),
        session_id: if tui.is_some() { UUID } else { NEW_UUID }.into(),
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.session.send(SessionCommand::Stop);
        let pids = fs::read_to_string(self.root.join("tui.pids")).unwrap_or_default();
        for pid in pids
            .lines()
            .filter_map(|pid| pid.trim().parse::<i32>().ok())
        {
            // SAFETY: kill only sends a signal.
            #[allow(unsafe_code, reason = "stopping the fake TUIs the test started")]
            unsafe {
                // Each is its own job, so its group holds its `sleep` too.
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn short() -> Timeouts {
    Timeouts {
        initialize: Duration::from_secs(5),
        term_grace: Duration::from_millis(300),
        term_give_up: Duration::from_secs(2),
        transition: Duration::from_secs(15),
    }
}

fn refusal(reply: &ChatReply) -> Option<ChatRefusal> {
    match reply {
        ChatReply::Refused { reason, .. } => Some(*reason),
        _ => None,
    }
}

/// Every chat batch the outbox sends, until it closes.
fn batches(outbox: &Arc<Connection>) -> Receiver<ChatBatch> {
    let (sender, batches) = mpsc::channel();
    let outbox = outbox.clone();
    thread::spawn(move || {
        while let Some((_, message)) = outbox.next() {
            if let Message::Chat(batch) = message
                && sender.send(batch).is_err()
            {
                return;
            }
        }
    });
    batches
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one Enter-to-Leave session, checked step by step"
)]
fn enter_chat_then_leave_restores_the_tui_and_drops_typed_input() -> TestResult {
    let fixture = Fixture::new(short())?;
    assert_eq!(fixture.enter()?, ChatReply::Entered { generation: 1 });
    assert!(fixture.reserved(), "Chat holds the conversation");

    // Terminal input is refused or dropped while Chat runs.
    let leak = fixture.root.join("leak");
    let (written, refused) = mpsc::channel();
    fixture.session.send(SessionCommand::WriteInput {
        bytes: format!("touch '{}-write'\r", leak.display()).into_bytes(),
        reply: written,
    })?;
    let refused = refused.recv_timeout(WAIT)?;
    fixture.type_line(&format!("touch '{}-typed'", leak.display()))?;

    let outbox = Arc::new(Connection::new(Arc::default()));
    let received = batches(&outbox);
    let subscribed = fixture.chat_from(
        ChatRequest::Subscribe {
            session: fixture.id(),
            subscription: 7,
            features: Vec::new(),
        },
        Arc::downgrade(&outbox),
    )?;
    let sent = fixture.chat(ChatRequest::Send {
        session: fixture.id(),
        generation: 1,
        text: "ping".into(),
        images: Vec::new(),
    })?;
    let stale = fixture.chat(ChatRequest::Send {
        session: fixture.id(),
        generation: 0,
        text: "old".into(),
        images: Vec::new(),
    })?;
    let bogus = fixture.chat(ChatRequest::SetEffort {
        session: fixture.id(),
        generation: 1,
        effort: "bogus".into(),
    })?;

    // Completions from an earlier transition are ignored.
    fixture
        .session
        .inject(OwnerEvent::Chat(Event::Done(Completion {
            transition: 0,
            result: Outcome::Console(ChatReply::Ok),
        })));
    fixture.session.inject(OwnerEvent::Chat(Event::Supervisor(
        SupervisorEvent::Exited {
            transition: 0,
            status: Err(std::io::Error::other("stale")),
            stderr_tail: String::new(),
        },
    )));
    let effort = fixture.chat(ChatRequest::SetEffort {
        session: fixture.id(),
        generation: 1,
        effort: "low".into(),
    })?;

    // The reply arrives after the result; give the batch its flush window.
    let deadline = Instant::now() + WAIT;
    let mut seen = Vec::new();
    while !format!("{seen:?}").contains("pong") && Instant::now() < deadline {
        if let Ok(batch) = received.recv_timeout(Duration::from_millis(100)) {
            seen.push(batch);
        }
    }

    let left = fixture.leave(1)?;
    fixture.wait_tui_launches(2)?;
    let modes = fixture.wait_mode(ChatMode::Console)?;
    let launches = fixture.tui_launches();
    outbox.close();

    assert!(refused.is_err_and(|error| error.to_string().contains("Chat mode")));
    assert_eq!(
        subscribed,
        ChatReply::Subscribed {
            generation: 1,
            subscription: 7
        }
    );
    assert_eq!(sent, ChatReply::Ok);
    assert_eq!(refusal(&stale), Some(ChatRefusal::StaleGeneration));
    assert_eq!(refusal(&bogus), Some(ChatRefusal::UnsupportedArguments));
    assert_eq!(effort, ChatReply::Ok, "still in Chat after stale events");
    let (snapshot, rest) = seen.split_first().ok_or("no snapshot")?;
    assert!(snapshot.snapshot.is_some(), "the snapshot comes first");
    assert!(format!("{seen:?}").contains("pong"), "{seen:?}");
    let mut revision = snapshot.to_rev;
    for batch in rest {
        assert!(batch.snapshot.is_none());
        assert_eq!(batch.from_rev, revision, "batches are contiguous");
        revision = batch.to_rev;
    }
    assert_eq!(
        left,
        ChatReply::Left {
            resumed: true,
            detail: None
        }
    );
    assert_eq!(launches.len(), 2, "exactly one recovery line: {launches:?}");
    assert_eq!(
        launches[1],
        format!("--resume {UUID}"),
        "the first prompt is not sent again"
    );
    assert!(!fixture.root.join("leak-write").exists());
    assert!(!fixture.root.join("leak-typed").exists());
    assert!(!fixture.reserved(), "Leave released the conversation");
    assert!(modes.contains(&(ChatMode::Chat, 1)), "{modes:?}");
    assert_eq!(modes.last(), Some(&(ChatMode::Console, 1)), "{modes:?}");
    Ok(())
}

#[test]
fn an_initialize_timeout_rolls_back_to_the_tui() -> TestResult {
    let fixture = Fixture::new(Timeouts {
        initialize: Duration::from_millis(500),
        ..short()
    })?;
    fs::write(fixture.root.join("silent"), "")?;
    let reply = fixture.enter()?;
    fixture.wait_tui_launches(2)?;
    let (written, accepted) = mpsc::channel();
    fixture.session.send(SessionCommand::WriteInput {
        bytes: Vec::new(),
        reply: written,
    })?;

    assert_eq!(
        refusal(&reply),
        Some(ChatRefusal::StartupTimeout),
        "{reply:?}"
    );
    assert_eq!(
        fixture.tui_launches()[1],
        format!("--resume {UUID}"),
        "the first prompt is not sent again"
    );
    assert!(
        accepted.recv_timeout(WAIT)?.is_ok(),
        "Console takes input again"
    );
    assert!(!fixture.reserved());
    assert_eq!(fixture.modes().last(), Some(&(ChatMode::Console, 0)));
    Ok(())
}

#[test]
fn an_unconfirmed_stop_blocks_and_keeps_the_reservation() -> TestResult {
    let fixture = Fixture::new(Timeouts {
        term_give_up: Duration::from_millis(800),
        ..short()
    })?;
    assert_eq!(fixture.enter()?, ChatReply::Entered { generation: 1 });
    // An unreaped child in Claude's group: killed, it stays a zombie member.
    let mut straggler = std::process::Command::new("/bin/sleep")
        .arg("30")
        .process_group(fixture.headless_pid()?)
        .spawn()?;

    let blocked = fixture.leave(1)?;
    let reserved = fixture.reserved();
    let send = fixture.chat(ChatRequest::Send {
        session: fixture.id(),
        generation: 1,
        text: "hi".into(),
        images: Vec::new(),
    })?;
    let _ = straggler.kill();
    straggler.wait()?;
    let left = fixture.leave(1)?;
    fixture.wait_tui_launches(2)?;

    assert_eq!(
        refusal(&blocked),
        Some(ChatRefusal::SpawnFailed),
        "{blocked:?}"
    );
    assert!(reserved, "Blocked keeps the conversation reserved");
    assert_eq!(refusal(&send), Some(ChatRefusal::NotChat));
    assert!(
        matches!(left, ChatReply::Left { resumed: true, .. }),
        "{left:?}"
    );
    assert!(!fixture.reserved());
    let modes = fixture.modes();
    assert!(modes.contains(&(ChatMode::Blocked, 1)), "{modes:?}");
    assert_eq!(fixture.tui_launches().len(), 2);
    Ok(())
}

#[test]
fn claude_exiting_on_its_own_returns_to_the_tui() -> TestResult {
    let fixture = Fixture::new(short())?;
    assert_eq!(fixture.enter()?, ChatReply::Entered { generation: 1 });
    let outbox = Arc::new(Connection::new(Arc::default()));
    let received = batches(&outbox);
    fixture.chat_from(
        ChatRequest::Subscribe {
            session: fixture.id(),
            subscription: 1,
            features: Vec::new(),
        },
        Arc::downgrade(&outbox),
    )?;
    fixture.chat(ChatRequest::Send {
        session: fixture.id(),
        generation: 1,
        text: "exit please".into(),
        images: Vec::new(),
    })?;
    fixture.wait_tui_launches(2)?;
    let deadline = Instant::now() + WAIT;
    let mut seen = String::new();
    while !seen.contains("bye now") && Instant::now() < deadline {
        if let Ok(batch) = received.recv_timeout(Duration::from_millis(100)) {
            let _ = write!(seen, "{batch:?}");
        }
    }
    outbox.close();

    assert!(seen.contains("bye now"), "subscribers see why: {seen}");
    assert!(!fixture.reserved());
    assert_eq!(fixture.modes().last(), Some(&(ChatMode::Console, 1)));
    Ok(())
}

#[test]
fn leaving_blocked_again_succeeds_once_claude_itself_exited() -> TestResult {
    let fixture = Fixture::new(Timeouts {
        term_give_up: Duration::from_millis(800),
        ..short()
    })?;
    assert_eq!(fixture.enter()?, ChatReply::Entered { generation: 1 });
    // An unreaped member that never leaves the group, like a stuck tool.
    let mut straggler = std::process::Command::new("/bin/sleep")
        .arg("30")
        .process_group(fixture.headless_pid()?)
        .spawn()?;

    let blocked = fixture.leave(1)?;
    let left = fixture.leave(1)?;
    fixture.wait_tui_launches(2)?;
    let reserved = fixture.reserved();
    let _ = straggler.kill();
    straggler.wait()?;

    assert_eq!(refusal(&blocked), Some(ChatRefusal::SpawnFailed));
    match &left {
        ChatReply::Left {
            resumed: true,
            detail: Some(detail),
        } => assert!(detail.starts_with("1 process "), "{detail}"),
        other => panic!("{other:?}"),
    }
    assert!(!reserved, "Leave released the conversation");
    assert_eq!(fixture.tui_launches()[1], format!("--resume {UUID}"));
    Ok(())
}

#[test]
fn an_exit_during_enter_still_returns_to_the_tui() -> TestResult {
    let fixture = Fixture::new(short())?;
    // The fake answers initialize after a second.
    fs::write(fixture.root.join("slow"), "")?;
    let (reply, replied) = mpsc::channel();
    fixture.session.send(SessionCommand::Chat(ChatCommand {
        request: ChatRequest::Enter {
            session: fixture.id(),
            model: None,
            effort: None,
        },
        from: Weak::new(),
        reply: ChatReplyTo::Channel(reply),
    }))?;
    // The child's exit, ordered before Enter completes (a real exit would race
    // the completion); the first Enter is transition 1.
    thread::sleep(Duration::from_millis(300));
    fixture.session.inject(OwnerEvent::Chat(Event::Supervisor(
        SupervisorEvent::Exited {
            transition: 1,
            status: Err(std::io::Error::other("gone")),
            stderr_tail: String::new(),
        },
    )));
    let entered = replied.recv_timeout(WAIT)?;
    fixture.wait_tui_launches(2)?;
    let modes = fixture.wait_mode(ChatMode::Console)?;

    assert_eq!(entered, ChatReply::Entered { generation: 1 });
    assert_eq!(fixture.tui_launches()[1], format!("--resume {UUID}"));
    assert!(!fixture.reserved());
    assert_eq!(modes.last(), Some(&(ChatMode::Console, 1)), "{modes:?}");
    Ok(())
}

#[test]
fn start_from_the_prompt_then_leave_resumes_the_new_conversation() -> TestResult {
    start_then_leave_in(&["/bin/sh", "-i"])
}

#[test]
fn start_then_leave_drops_a_draft_in_zsh_and_bash() -> TestResult {
    for shell in [
        &["/bin/zsh", "-f", "-i"][..],
        &["/bin/bash", "--norc", "--noprofile", "-i"],
    ] {
        if Path::new(shell[0]).exists() {
            start_then_leave_in(shell).map_err(|error| format!("{shell:?}: {error}"))?;
        }
    }
    Ok(())
}

/// Start from `shell`'s prompt with a draft typed, chat, then Leave.
fn start_then_leave_in(shell: &[&str]) -> TestResult {
    let fixture = Fixture::idle_in(short(), shell)?;
    // A draft left at the prompt; glued to the resume line it would still run.
    let draft = fixture.root.join("draft-ran");
    fixture.session.send(SessionCommand::Input(
        format!("touch '{}'; ", draft.display()).into_bytes(),
    ))?;
    // Read by the line editor, so it isn't in the input Start flushes.
    thread::sleep(Duration::from_millis(300));
    assert_eq!(fixture.start()?, ChatReply::Entered { generation: 1 });
    let reserved = fixture.holds(NEW_UUID);
    let sent = fixture.chat(ChatRequest::Send {
        session: fixture.id(),
        generation: 1,
        text: "ping".into(),
        images: Vec::new(),
    })?;
    let transcript = fixture
        .config
        .join(format!("projects/-repo/{NEW_UUID}.jsonl"));
    let deadline = Instant::now() + WAIT;
    while !transcript.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let left = fixture.leave(1)?;
    fixture.wait_tui_launches(1)?;
    let args = fs::read_to_string(fixture.root.join("headless.args"))?;

    assert!(reserved, "Chat holds the new conversation");
    assert_eq!(sent, ChatReply::Ok);
    assert!(args.contains(&format!("--session-id {NEW_UUID}")), "{args}");
    assert!(!args.contains("--resume"), "{args}");
    assert_eq!(
        left,
        ChatReply::Left {
            resumed: true,
            detail: None
        }
    );
    assert_eq!(fixture.tui_launches(), vec![format!("--resume {NEW_UUID}")]);
    assert!(!fixture.holds(NEW_UUID), "Leave released the conversation");
    assert!(!draft.exists(), "the draft at the prompt ran");
    Ok(())
}

#[test]
fn leaving_a_started_chat_with_nothing_sent_types_nothing() -> TestResult {
    let fixture = Fixture::idle(short())?;
    assert_eq!(fixture.start()?, ChatReply::Entered { generation: 1 });
    let left = fixture.leave(1)?;
    fixture.wait_mode(ChatMode::Console)?;
    // Whatever Leave typed would have run by now.
    thread::sleep(Duration::from_millis(500));

    assert_eq!(
        left,
        ChatReply::Left {
            resumed: false,
            detail: None
        }
    );
    assert!(fixture.tui_launches().is_empty());
    assert!(!fixture.holds(NEW_UUID));
    Ok(())
}

#[test]
fn start_is_refused_while_a_program_runs() -> TestResult {
    let fixture = Fixture::new(short())?;
    let reply = fixture.start()?;

    assert_eq!(refusal(&reply), Some(ChatRefusal::Busy), "{reply:?}");
    assert_eq!(fixture.tui_launches().len(), 1, "the TUI was left alone");
    assert!(!fixture.root.join("headless.pid").exists());
    Ok(())
}

#[test]
fn start_refuses_an_api_key_in_the_shell_environment() -> TestResult {
    let fixture = Fixture::idle(short())?;
    fs::write(fixture.root.join("api-key"), "")?;
    let reply = fixture.start()?;

    assert_eq!(
        refusal(&reply),
        Some(ChatRefusal::UnsupportedEnvironment),
        "{reply:?}"
    );
    assert!(!fixture.root.join("headless.pid").exists());
    assert!(!fixture.holds(NEW_UUID));
    Ok(())
}

/// The `--session-id` the headless child was started with.
fn new_session(args: &str) -> Option<&str> {
    let (_, rest) = args.split_once("--session-id ")?;
    rest.split_whitespace().next()
}

#[test]
fn entering_a_tui_that_saved_nothing_yet_starts_a_new_conversation() -> TestResult {
    let fixture = Fixture::new(short())?;
    // The TUI hasn't had its first prompt, so it has no transcript to resume.
    fs::remove_file(fixture.config.join(format!("projects/-repo/{UUID}.jsonl")))?;
    assert_eq!(fixture.enter()?, ChatReply::Entered { generation: 1 });
    let args = fs::read_to_string(fixture.root.join("headless.args"))?;
    let id = new_session(&args).ok_or("no --session-id")?.to_owned();
    let held = fixture.holds(&id);
    let left = fixture.leave(1)?;
    fixture.wait_tui_launches(2)?;

    assert!(held, "Chat holds the new conversation");
    assert_ne!(id, UUID);
    assert!(!args.contains("--resume"), "{args}");
    // The permission mode still comes from the TUI's flags and settings.
    assert!(args.contains("--permission-mode default"), "{args}");
    assert!(
        matches!(left, ChatReply::Left { resumed: true, .. }),
        "{left:?}"
    );
    // Nothing was sent, so the TUI comes back as launched, not resumed.
    assert_eq!(fixture.tui_launches()[1], "");
    assert!(!fixture.holds(&id), "Leave released the conversation");
    Ok(())
}

#[test]
fn a_tui_that_saves_its_first_prompt_as_it_stops_is_resumed() -> TestResult {
    let fixture = Fixture::new(short())?;
    let transcript = fixture.config.join(format!("projects/-repo/{UUID}.jsonl"));
    fs::remove_file(&transcript)?;
    // Unsaved at preflight, saved by the time it has stopped.
    fs::write(
        fixture.root.join("save-on-exit"),
        transcript.as_os_str().as_encoded_bytes(),
    )?;
    let entered = fixture.enter()?;
    fs::remove_file(fixture.root.join("save-on-exit"))?;
    let args = fs::read_to_string(fixture.root.join("headless.args"))?;
    let held = fixture.reserved();
    let left = fixture.leave(1)?;
    fixture.wait_tui_launches(2)?;

    assert_eq!(entered, ChatReply::Entered { generation: 1 });
    assert!(transcript.exists());
    assert!(args.contains(&format!("--resume {UUID}")), "{args}");
    assert!(!args.contains("--session-id"), "{args}");
    assert!(held, "Chat holds the TUI's conversation");
    assert!(
        matches!(left, ChatReply::Left { resumed: true, .. }),
        "{left:?}"
    );
    assert_eq!(fixture.tui_launches()[1], format!("--resume {UUID}"));
    assert!(!fixture.reserved());
    Ok(())
}

#[test]
fn first_submission_ignores_prompt_sessions_and_a_lost_reply_retry_does_not_repeat_it() -> TestResult
{
    use muxy_protocol::{ChatProvider, ChatSelection, Feature, SubmissionOutcome};
    let fixture = Fixture::idle(short())?;
    let ChatReply::Prepared {
        host_epoch,
        operation,
    } = fixture.chat(ChatRequest::PrepareSubmission {
        session: fixture.id(),
    })?
    else {
        panic!("prepared ticket")
    };
    let request = ChatRequest::Begin {
        session: fixture.id(),
        host_epoch,
        operation,
        selection: Box::new(ChatSelection {
            provider: ChatProvider::Claude,
            model: Some("default".into()),
            effort: Some("high".into()),
            target: None,
            launch_context: Some(fake_context(&fixture.root, 0, 0, None).launch_stamp()),
        }),
        text: "one accepted prompt".into(),
        images: Vec::new(),
        features: vec![Feature::CHAT_PROVIDERS, Feature::CHAT_OPERATIONS],
    };
    assert!(matches!(
        fixture.chat(request.clone())?,
        ChatReply::Submission(_)
    ));
    let deadline = Instant::now() + WAIT;
    loop {
        let ChatReply::Submission(status) = fixture.chat(ChatRequest::OperationStatus {
            session: fixture.id(),
            operation,
        })?
        else {
            panic!("operation status")
        };
        if status.outcome == SubmissionOutcome::Accepted {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "provider did not acknowledge: {status:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        matches!(fixture.chat(request)?,ChatReply::Submission(status) if status.outcome==SubmissionOutcome::Accepted)
    );
    let wire = fs::read_to_string(fixture.root.join("wire.log"))?;
    let lines: Vec<serde_json::Value> = wire
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(
        lines.iter().filter(|line| line["type"] == "user").count(),
        1
    );
    let user = lines
        .iter()
        .position(|line| line["type"] == "user")
        .unwrap();
    assert!(
        lines[..user]
            .iter()
            .any(|line| line["request"]["subtype"] == "apply_flag_settings"),
        "configuration precedes first prompt: {wire}"
    );
    fixture.leave(1)?;
    Ok(())
}

#[test]
#[ignore = "run only through scripts/check-codex-chat.py --rust-native-test with an isolated engine"]
#[allow(
    clippy::too_many_lines,
    reason = "Exercises an isolated real engine through the session owner and terminal gate"
)]
fn native_owner_resume_console_and_terminal_takeover() -> TestResult {
    use muxy_protocol::{ChatEngine, ChatProvider, ChatSelection, ChatTarget, Feature};
    let path = |key| {
        PathBuf::from(std::env::var_os(key).expect("isolated native probe context"))
            .canonicalize()
            .expect("canonical isolated path")
    };
    let home = path("MUXY_CODEX_PROBE_HOME");
    let project = path("MUXY_CODEX_PROBE_PROJECT");
    let engine = super::codex::context::Identity::read(
        std::env::var("MUXY_CODEX_PROBE_ENGINE_PID")?.parse()?,
    )?;
    let root = std::env::temp_dir().join(format!("mx-owner-{}", std::process::id()));
    fs::create_dir(&root)?;
    let mut environment = vec![
        ("HOME".into(), home.parent().unwrap().as_os_str().to_owned()),
        ("CODEX_HOME".into(), home.as_os_str().to_owned()),
        ("PS1".into(), "$ ".into()),
    ];
    for key in [
        "OPENAI_API_KEY",
        "CODEX_API_KEY",
        "OPENAI_BASE_URL",
        "OPENAI_API_BASE",
    ] {
        environment.push((key.into(), "".into()));
    }
    for key in ["PATH", "TMPDIR", "LANG"] {
        if let Some(value) = std::env::var_os(key) {
            environment.push((key.into(), value));
        }
    }
    let pty = Pty::spawn(SpawnRequest {
        program: "/bin/zsh".into(),
        args: vec!["-f".into(), "-i".into()],
        cwd: project.clone(),
        env: environment,
        size: PtySize {
            cols: 100,
            rows: 30,
        },
    })?;
    let size = Size {
        cols: 100,
        rows: 30,
    };
    let services = Services::default();
    let session = crate::session::start(
        SessionInfo {
            project: ProjectId::from_u128(1),
            id: SessionId::new(1).unwrap(),
            directory: ServerPath::from(project),
        },
        pty,
        size,
        1 << 20,
        Archive::memory(0),
        None,
        Arc::default(),
        Arc::default(),
        services.clone(),
        |_| {},
    )?;
    let (sink, modes) = mpsc::channel();
    session.send(SessionCommand::Attach {
        id: AttachmentId(1),
        channel: ChannelId(1),
        size,
        sink: sink.into(),
    })?;
    let fixture = Fixture {
        root,
        config: home.clone(),
        services,
        session,
        modes,
    };
    let features = vec![
        Feature::CHAT_PROVIDERS,
        Feature::CHAT_OPERATIONS,
        Feature::CHAT_NATIVE_APPROVALS,
        Feature::CHAT_QUESTIONS,
    ];
    let target = ChatTarget {
        provider: ChatProvider::Codex,
        thread_id: std::env::var("MUXY_CODEX_PROBE_THREAD")?,
        home: ServerPath::from(home),
        endpoint: Some(ServerPath::from(path("MUXY_CODEX_PROBE_ENDPOINT"))),
        executable: ServerPath::from(engine.executable.clone()),
        engine: Some(ChatEngine {
            pid: u32::try_from(engine.pid)?,
            started_seconds: engine.started.0,
            started_micros: engine.started.1,
        }),
    };
    let enter = || ChatRequest::EnterProvider {
        session: fixture.id(),
        selection: Box::new(ChatSelection {
            provider: ChatProvider::Codex,
            model: None,
            effort: None,
            target: Some(target.clone()),
            launch_context: None,
        }),
        features: features.clone(),
    };
    let entered = fixture.chat(enter())?;
    let ChatReply::Entered { generation } = entered else {
        panic!("direct native resume: {entered:?}")
    };
    let outbox = Arc::new(Connection::new(Arc::default()));
    let received = batches(&outbox);
    fixture.chat_from(
        ChatRequest::Subscribe {
            session: fixture.id(),
            subscription: 7,
            features: features.clone(),
        },
        Arc::downgrade(&outbox),
    )?;
    let snapshot = received
        .recv_timeout(WAIT)?
        .snapshot
        .expect("native snapshot");
    assert_eq!(snapshot.state.target.as_ref(), Some(&target));
    assert!(snapshot.items.iter().any(|item|matches!(&item.kind,muxy_protocol::ChatItemKind::User {text} if text.contains("MUXY_TARGET_legacy"))));
    let blocked = fixture.root.join("blocked-input-ran");
    fixture.type_line(&format!("touch '{}'", blocked.display()))?;
    let left = fixture.chat_from(
        ChatRequest::LeaveProvider {
            session: fixture.id(),
            generation,
            interrupt: false,
        },
        Arc::downgrade(&outbox),
    )?;
    assert!(
        matches!(left, ChatReply::Left { resumed: true, .. }),
        "{left:?}"
    );
    assert!(!blocked.exists(), "Chat leaked terminal input");
    // Rejoin through the native TUI, validating socket ownership before detaching it.
    assert!(matches!(fixture.chat(enter())?, ChatReply::Entered { .. }));
    engine.revalidate()?;
    outbox.close();
    Ok(())
}
