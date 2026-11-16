//! Runs Claude Code's headless child on its own threads, so the session owner
//! never blocks on it. Everything here that waits ([`terminate_group`],
//! [`wait_foreground_is`], [`run_version`], [`Supervisor::initialize`],
//! [`Supervisor::shutdown`]) must run off the owner thread, for example in
//! [`spawn_job`].

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::BorrowedFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
use serde_json::{Value, json};

/// A stdout line longer than this is discarded up to its newline.
pub(crate) const MAX_LINE: usize = 16 << 20;
/// Parsed stdout lines waiting for the owner; a full queue stops reading, so
/// a slow consumer pushes back on Claude. Also caps the items `initialize`
/// skips.
// shortcut: counts lines, not bytes, so this can hold up to 256 lines of up to
// MAX_LINE each; bound by bytes if memory spikes show up.
const OUTPUT_QUEUE: usize = 256;
const STDIN_QUEUE: usize = 64;
const STDERR_TAIL: usize = 64 << 10;
/// SIGTERM, then SIGKILL after this long.
pub(crate) const TERM_GRACE: Duration = Duration::from_secs(3);
/// Stop waiting for a group to exit after this long.
pub(crate) const TERM_GIVE_UP: Duration = Duration::from_secs(5);
pub(crate) const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(20);

/// How to start the child. `Debug` names the environment's variables but
/// never prints their values.
#[derive(Clone)]
pub(crate) struct Launch {
    /// The native executable, or a runtime and Claude Code's script.
    pub(crate) prefix: Vec<OsString>,
    pub(crate) args: Vec<OsString>,
    /// The complete environment; nothing is inherited from the server.
    pub(crate) env: Vec<(OsString, OsString)>,
    pub(crate) cwd: PathBuf,
}

impl std::fmt::Debug for Launch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Launch")
            .field("prefix", &self.prefix)
            .field("args", &self.args)
            .field(
                "env",
                &self.env.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .field("cwd", &self.cwd)
            .finish()
    }
}

impl Launch {
    fn command(&self) -> io::Result<Command> {
        let (program, prefix) = self
            .prefix
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty launch prefix"))?;
        let mut command = Command::new(program);
        command
            .args(prefix)
            .args(&self.args)
            .env_clear()
            .envs(self.env.iter().map(|(name, value)| (name, value)))
            .current_dir(&self.cwd)
            // Its own group, so terminating it reaches everything it started.
            .process_group(0);
        Ok(command)
    }
}

/// One item from the child's stdout, in order.
#[derive(Debug)]
pub(crate) enum StdoutItem {
    Message(Value),
    /// A line over [`MAX_LINE`] was dropped without being kept in memory.
    Overlong {
        discarded: u64,
    },
    /// A line that isn't JSON. Its text isn't kept.
    Malformed,
    /// Stdout ended; nothing follows.
    Closed(Option<io::Error>),
}

/// Lifecycle events, each tagged with the transition that spawned the child.
#[derive(Debug)]
pub(crate) enum SupervisorEvent {
    /// Stdout items are waiting in [`Supervisor::drain`]. Sent once until the
    /// next drain, not per line.
    Output {
        transition: u64,
    },
    WriteFailed {
        transition: u64,
        error: io::Error,
    },
    Exited {
        transition: u64,
        status: io::Result<ExitStatus>,
        /// The last 64 KB of stderr, lossily decoded.
        stderr_tail: String,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum StdinError {
    /// 64 messages are already waiting; the child isn't reading.
    Full,
    /// The writer stopped: stdin was closed or a write failed.
    Closed,
}

#[derive(Debug)]
pub(crate) enum HandshakeError {
    Stdin(StdinError),
    Timeout,
    /// Stdout ended before the response.
    Closed,
    /// The child answered with an error.
    Refused(String),
    /// Too much other output arrived before the response.
    Unexpected,
}

/// The running headless child and its threads. Dropping it detaches the
/// threads and leaves the child running: stop the group with
/// [`terminate_group`], then call [`Supervisor::shutdown`].
#[derive(Debug)]
pub(crate) struct Supervisor {
    pid: u32,
    stdin: Option<SyncSender<Vec<u8>>>,
    output: Receiver<StdoutItem>,
    output_pending: Arc<AtomicBool>,
    /// Set once the `wait` thread has reaped the child.
    exited: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Supervisor {
    /// Starts the child with piped stdio in its own process group, plus a
    /// stdout reader, a stdin writer, a stderr drain and a `wait` thread.
    pub(crate) fn spawn<T>(launch: &Launch, transition: u64, events: Sender<T>) -> io::Result<Self>
    where
        T: From<SupervisorEvent> + Send + 'static,
    {
        let mut child = launch
            .command()?
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let pid = child.id();
        let pipes = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let (Some(stdin), Some(stdout), Some(stderr)) = pipes else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("child has no piped stdio"));
        };
        let started = (|| {
            let mut threads = Vec::with_capacity(4);
            let (output_sender, output) = mpsc::sync_channel(OUTPUT_QUEUE);
            let output_pending = Arc::new(AtomicBool::new(false));
            let pending = output_pending.clone();
            let wake = events.clone();
            threads.push(spawn_named("claude-stdout", move || {
                read_stdout(stdout, &output_sender, &pending, || {
                    let _ = wake.send(SupervisorEvent::Output { transition }.into());
                });
            })?);
            let (stdin_sender, queued) = mpsc::sync_channel::<Vec<u8>>(STDIN_QUEUE);
            let failed = events.clone();
            threads.push(spawn_named("claude-stdin", move || {
                let mut stdin = stdin;
                for bytes in queued {
                    if let Err(error) = stdin.write_all(&bytes) {
                        let _ =
                            failed.send(SupervisorEvent::WriteFailed { transition, error }.into());
                        return;
                    }
                }
            })?);
            let tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL)));
            let (drained, stderr_done) = mpsc::channel::<()>();
            let ring = tail.clone();
            threads.push(spawn_named("claude-stderr", move || {
                drain_stderr(stderr, &ring);
                let _ = drained.send(());
            })?);
            Ok::<_, io::Error>((
                threads,
                output,
                output_pending,
                stdin_sender,
                tail,
                stderr_done,
            ))
        })();
        let (mut threads, output, output_pending, stdin, tail, stderr_done) = match started {
            Ok(started) => started,
            Err(error) => {
                kill_group(pid);
                let _ = child.wait();
                return Err(error);
            }
        };
        let exited = Arc::new(AtomicBool::new(false));
        let reaped = exited.clone();
        let waited = spawn_named("claude-wait", move || {
            let status = child.wait();
            reaped.store(true, Ordering::Release);
            // Grandchildren may hold stderr open; don't wait on them for long.
            let _ = stderr_done.recv_timeout(Duration::from_millis(200));
            let stderr_tail = {
                let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
                String::from_utf8_lossy(tail.make_contiguous()).into_owned()
            };
            let _ = events.send(
                SupervisorEvent::Exited {
                    transition,
                    status,
                    stderr_tail,
                }
                .into(),
            );
        });
        match waited {
            Ok(handle) => threads.push(handle),
            Err(error) => {
                // shortcut: the closure owned the child, so it stays a zombie until the server exits.
                kill_group(pid);
                return Err(error);
            }
        }
        Ok(Self {
            pid,
            stdin: Some(stdin),
            output,
            output_pending,
            exited,
            threads,
        })
    }

    /// Whether the child itself has exited and been reaped; others in its
    /// group may remain.
    pub(crate) fn exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    /// The child's pid, which is also its process group.
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    /// Queues one JSON line for stdin without blocking.
    pub(crate) fn send(&self, message: &Value) -> Result<(), StdinError> {
        let mut line = message.to_string().into_bytes();
        line.push(b'\n');
        let stdin = self.stdin.as_ref().ok_or(StdinError::Closed)?;
        stdin.try_send(line).map_err(|error| match error {
            TrySendError::Full(_) => StdinError::Full,
            TrySendError::Disconnected(_) => StdinError::Closed,
        })
    }

    /// Ends stdin once queued messages are written, so the child sees EOF.
    pub(crate) fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Takes up to `max` stdout items without blocking. Call it once after
    /// taking over a supervisor, then on every [`SupervisorEvent::Output`];
    /// if it returns `max` items, call it again later.
    pub(crate) fn drain(&self, max: usize) -> Vec<StdoutItem> {
        // Clear before reading: an item queued after this sends a new wake.
        self.output_pending.store(false, Ordering::Release);
        self.output.try_iter().take(max).collect()
    }

    /// Sends `initialize` and waits for its `control_response`, returning the
    /// response payload and any items that arrived first. Blocks for up to
    /// `timeout`.
    pub(crate) fn initialize(
        &self,
        request_id: &str,
        timeout: Duration,
    ) -> Result<(Value, Vec<StdoutItem>), HandshakeError> {
        self.send(&json!({
            "type": "control_request",
            "request_id": request_id,
            "request": {"subtype": "initialize", "hooks": null},
        }))
        .map_err(HandshakeError::Stdin)?;
        let deadline = Instant::now() + timeout;
        let mut skipped = Vec::new();
        loop {
            let item = self
                .output
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|error| match error {
                    RecvTimeoutError::Timeout => HandshakeError::Timeout,
                    RecvTimeoutError::Disconnected => HandshakeError::Closed,
                })?;
            match item {
                StdoutItem::Message(mut value)
                    if value["type"] == "control_response"
                        && value["response"]["request_id"] == request_id =>
                {
                    let mut response = value["response"].take();
                    return if response["subtype"] == "success" {
                        Ok((response["response"].take(), skipped))
                    } else {
                        Err(HandshakeError::Refused(
                            response["error"].as_str().unwrap_or_default().to_owned(),
                        ))
                    };
                }
                StdoutItem::Closed(_) => return Err(HandshakeError::Closed),
                _ if skipped.len() >= OUTPUT_QUEUE => return Err(HandshakeError::Unexpected),
                other => skipped.push(other),
            }
        }
    }

    /// Closes stdin, stops taking stdout, and joins every thread that finishes
    /// by `deadline`. Returns whether all of them did; threads left behind are
    /// detached (a process that outlived termination may hold a pipe open).
    pub(crate) fn shutdown(self, deadline: Instant) -> bool {
        let Self {
            stdin,
            output,
            threads,
            ..
        } = self;
        drop(stdin);
        // Unblocks a reader waiting on a full queue.
        drop(output);
        loop {
            if threads.iter().all(JoinHandle::is_finished) {
                for thread in threads {
                    let _ = thread.join();
                }
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(POLL);
        }
    }
}

fn spawn_named(name: &str, body: impl FnOnce() + Send + 'static) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().name(name.into()).spawn(body)
}

fn read_stdout(
    stdout: impl Read,
    output: &SyncSender<StdoutItem>,
    pending: &AtomicBool,
    wake: impl Fn(),
) {
    let mut reader = BufReader::with_capacity(64 << 10, stdout);
    let mut line = Vec::new();
    loop {
        let item = match read_line_bounded(&mut reader, &mut line, MAX_LINE) {
            Ok(Line::Complete) if line.iter().all(u8::is_ascii_whitespace) => continue,
            Ok(Line::Complete) => {
                serde_json::from_slice(&line).map_or(StdoutItem::Malformed, StdoutItem::Message)
            }
            Ok(Line::Overlong(discarded)) => StdoutItem::Overlong { discarded },
            Ok(Line::Eof) => StdoutItem::Closed(None),
            Err(error) => StdoutItem::Closed(Some(error)),
        };
        if line.capacity() > 1 << 20 {
            line = Vec::new();
        }
        let closed = matches!(item, StdoutItem::Closed(_));
        if output.send(item).is_err() {
            return;
        }
        // Queue first, then flag: a drain that clears the flag sees this item or a new wake.
        if !pending.swap(true, Ordering::AcqRel) {
            wake();
        }
        if closed {
            return;
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Line {
    Complete,
    /// The line passed the limit; this many bytes were dropped in total.
    Overlong(u64),
    Eof,
}

/// Reads one line, without its newline, into `line`. Past `limit` bytes the
/// rest of the line is consumed without being stored.
fn read_line_bounded(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    limit: usize,
) -> io::Result<Line> {
    line.clear();
    let mut discarded: Option<u64> = None;
    loop {
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            return Ok(match discarded {
                Some(count) => Line::Overlong(count),
                None if line.is_empty() => Line::Eof,
                None => Line::Complete,
            });
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let chunk = &available[..newline.unwrap_or(available.len())];
        match &mut discarded {
            Some(count) => *count += chunk.len() as u64,
            None if line.len() + chunk.len() > limit => {
                discarded = Some((line.len() + chunk.len()) as u64);
                line.clear();
            }
            None => line.extend_from_slice(chunk),
        }
        let used = newline.map_or(available.len(), |index| index + 1);
        reader.consume(used);
        if newline.is_some() {
            return Ok(discarded.map_or(Line::Complete, Line::Overlong));
        }
    }
}

fn drain_stderr(mut stderr: impl Read, tail: &Mutex<VecDeque<u8>>) {
    let mut buffer = [0u8; 8192];
    loop {
        match stderr.read(&mut buffer) {
            Ok(0) => return,
            Ok(read) => {
                let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
                tail.extend(&buffer[..read]);
                let excess = tail.len().saturating_sub(STDERR_TAIL);
                tail.drain(..excess);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

fn group_pid(pgid: u32) -> Option<Pid> {
    // Never signal init, every process (0), or the server's own group.
    let own = rustix::process::getpgrp();
    i32::try_from(pgid)
        .ok()
        .filter(|pgid| *pgid > 1)
        .and_then(Pid::from_raw)
        .filter(|group| *group != own)
}

fn kill_group(pgid: u32) {
    if let Some(group) = group_pid(pgid) {
        let _ = kill_process_group(group, Signal::KILL);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Termination {
    /// The group has no members left.
    Confirmed,
    NotConfirmed,
}

/// Sends SIGTERM to process group `pgid`, SIGKILL after `kill_after`, and
/// waits until the group is empty or `give_up` has passed. A zombie still
/// counts as a member, so its parent must reap it (the supervisor's `wait`
/// thread does for the headless child; the shell does for the TUI).
pub(crate) fn terminate_group(pgid: u32, kill_after: Duration, give_up: Duration) -> Termination {
    let Some(group) = group_pid(pgid) else {
        return Termination::NotConfirmed;
    };
    let start = Instant::now();
    let _ = kill_process_group(group, Signal::TERM);
    let mut killed = false;
    loop {
        if test_kill_process_group(group) == Err(Errno::SRCH) {
            return Termination::Confirmed;
        }
        let elapsed = start.elapsed();
        if elapsed >= give_up {
            return Termination::NotConfirmed;
        }
        if !killed && elapsed >= kill_after {
            let _ = kill_process_group(group, Signal::KILL);
            killed = true;
        }
        thread::sleep(POLL);
    }
}

/// The terminal's foreground process group, from its master side.
#[allow(
    unsafe_code,
    reason = "rustix is built without termios here, so tcgetpgrp comes from libc"
)]
pub(crate) fn foreground_group(master: BorrowedFd<'_>) -> Option<u32> {
    use std::os::fd::AsRawFd;
    // SAFETY: `master` is a borrowed, open descriptor for the duration of the call.
    let group = unsafe { libc::tcgetpgrp(master.as_raw_fd()) };
    u32::try_from(group).ok().filter(|group| *group > 0)
}

/// Waits until `group` (the shell's) owns the terminal's foreground.
pub(crate) fn wait_foreground_is(master: BorrowedFd<'_>, group: u32, deadline: Instant) -> bool {
    loop {
        if foreground_group(master) == Some(group) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(POLL);
    }
}

/// Runs `<prefix> --version` with the given environment and returns its
/// trimmed stdout.
pub(crate) fn run_version(
    prefix: &[OsString],
    env: &[(OsString, OsString)],
    cwd: &Path,
    timeout: Duration,
) -> io::Result<String> {
    let launch = Launch {
        prefix: prefix.to_vec(),
        args: vec!["--version".into()],
        env: env.to_vec(),
        cwd: cwd.to_path_buf(),
    };
    let output = run_output(&launch, timeout, 4096)?;
    Ok(String::from_utf8_lossy(&output).trim().to_owned())
}

/// Runs `launch` with no input and returns up to `limit` bytes of its stdout
/// once it exits successfully within `timeout`. Stdout is read while it runs,
/// so it never stalls on a full pipe, and not waited on to close: anything
/// the child left behind, even outside its group, may hold it open.
pub(crate) fn run_output(launch: &Launch, timeout: Duration, limit: u64) -> io::Result<Vec<u8>> {
    let mut child = launch
        .command()?
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    let read = (|| {
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout"))?;
        rustix::io::ioctl_fionbio(&stdout, true)?;
        let pid = i32::try_from(child.id())
            .ok()
            .and_then(Pid::from_raw)
            .ok_or_else(|| io::Error::other("bad pid"))?;
        let mut chunk = [0_u8; 16 << 10];
        loop {
            // Checked before draining: once it exited, all it wrote is in the pipe.
            let exited = rustix::process::waitid(
                rustix::process::WaitId::Pid(pid),
                rustix::process::WaitIdOptions::EXITED
                    | rustix::process::WaitIdOptions::NOHANG
                    | rustix::process::WaitIdOptions::NOWAIT,
            )?
            .is_some();
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => {
                        let room = usize::try_from(limit)
                            .unwrap_or(usize::MAX)
                            .saturating_sub(output.len());
                        output.extend_from_slice(&chunk[..count.min(room)]);
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error),
                }
            }
            if exited {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"));
            }
            thread::sleep(POLL);
        }
    })();
    // Before reaping, while the unreaped child still holds its group's id: this
    // stops it on a timeout, and anything it left behind in its group.
    kill_group(child.id());
    let status = child.wait()?;
    read?;
    if !status.success() {
        return Err(io::Error::other(format!("failed: {status}")));
    }
    Ok(output)
}

/// A blocking job's result, tagged with its transition.
#[derive(Debug)]
pub(crate) struct Completion<T> {
    pub(crate) transition: u64,
    pub(crate) result: T,
}

/// Runs `job` on its own thread and delivers its [`Completion`].
pub(crate) fn spawn_job<T, E>(
    transition: u64,
    job: impl FnOnce() -> T + Send + 'static,
    events: Sender<E>,
) -> io::Result<JoinHandle<()>>
where
    T: Send + 'static,
    E: From<Completion<T>> + Send + 'static,
{
    spawn_named("chat-transition", move || {
        let result = job();
        let _ = events.send(Completion { transition, result }.into());
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::os::unix::process::ExitStatusExt;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    const WAIT: Duration = Duration::from_secs(5);

    fn shell(script: &str) -> Launch {
        Launch {
            prefix: vec!["/bin/sh".into(), "-c".into(), script.into()],
            args: Vec::new(),
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            cwd: "/".into(),
        }
    }

    fn exited(
        events: &Receiver<SupervisorEvent>,
    ) -> Result<(ExitStatus, String), Box<dyn std::error::Error>> {
        loop {
            if let SupervisorEvent::Exited {
                status,
                stderr_tail,
                ..
            } = events.recv_timeout(WAIT)?
            {
                return Ok((status?, stderr_tail));
            }
        }
    }

    #[test]
    fn run_output_returns_once_the_child_exits() -> TestResult {
        // A same-group job and a setsid'd one (out of the group's reach) hold stdout open.
        let script = "command -v /usr/bin/perl >/dev/null || exit 9; head -c 100000 /dev/zero | tr '\\0' a; sleep 8 & \
            /usr/bin/perl -MPOSIX -e 'fork and exit; POSIX::setsid() or die; sleep 8' & exit 0";
        let started = Instant::now();
        let output = run_output(&shell(script), Duration::from_secs(4), 1 << 20)?;
        let capped = run_output(&shell(script), Duration::from_secs(4), 10)?;
        let stuck = run_output(&shell("sleep 8"), Duration::from_millis(200), 10);
        assert_eq!(output.len(), 100_000, "more than a pipe holds, all read");
        assert_eq!(capped.len(), 10);
        assert_eq!(
            stuck.map_err(|error| error.kind()),
            Err(io::ErrorKind::TimedOut)
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        Ok(())
    }

    #[test]
    fn initialize_round_trip() -> TestResult {
        let script = r#"read line
id=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
printf '{"type":"system","subtype":"status"}\n'
printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"pid":7}}}\n' "$id"
cat >/dev/null"#;
        let (events, _events) = mpsc::channel::<SupervisorEvent>();
        let mut supervisor = Supervisor::spawn(&shell(script), 4, events)?;
        let (response, skipped) = supervisor
            .initialize("req_1", WAIT)
            .map_err(|error| format!("{error:?}"))?;
        supervisor.close_stdin();
        let stopped = terminate_group(supervisor.pid(), TERM_GRACE, TERM_GIVE_UP);
        assert!(supervisor.shutdown(Instant::now() + WAIT));
        assert_eq!(response, json!({"pid": 7}));
        assert!(matches!(skipped.as_slice(), [StdoutItem::Message(_)]));
        assert_eq!(stopped, Termination::Confirmed);
        Ok(())
    }

    #[test]
    fn overlong_lines_are_dropped_without_being_stored() -> TestResult {
        // Chunks well under the limit, so the line grows before it is dropped.
        let limit = 16 << 10;
        let mut input = io::repeat(b'a').take(8 << 20).chain(&b"\n{\"ok\":1}\n"[..]);
        let mut reader = BufReader::with_capacity(1024, &mut input);
        let mut line = Vec::new();
        let first = read_line_bounded(&mut reader, &mut line, limit)?;
        let capacity = line.capacity();
        let mut grown = Vec::new();
        let mut short = BufReader::with_capacity(1024, io::repeat(b'a').take(limit as u64 - 1));
        read_line_bounded(&mut short, &mut grown, limit)?;
        let second = read_line_bounded(&mut reader, &mut line, limit)?;
        assert_eq!(first, Line::Overlong(8 << 20));
        assert!(
            grown.capacity() >= limit - 1,
            "the reader stores lines up to the limit"
        );
        assert!(
            capacity <= 2 * limit,
            "kept {capacity} bytes of an 8 MiB line"
        );
        assert_eq!(
            (second, line.as_slice()),
            (Line::Complete, &b"{\"ok\":1}"[..])
        );

        // And through the child's real pipe at the real limit.
        let script = "head -c 17000000 /dev/zero | tr '\\0' a; printf '\\n{\"ok\":2}\\n'";
        let (events, events_received) = mpsc::channel::<SupervisorEvent>();
        let supervisor = Supervisor::spawn(&shell(script), 1, events)?;
        exited(&events_received)?;
        let mut items = Vec::new();
        let deadline = Instant::now() + WAIT;
        while !matches!(items.last(), Some(StdoutItem::Closed(_))) && Instant::now() < deadline {
            items.extend(supervisor.drain(16));
            thread::sleep(POLL);
        }
        assert!(supervisor.shutdown(Instant::now() + WAIT));
        assert!(
            matches!(
                items.as_slice(),
                [
                    StdoutItem::Overlong {
                        discarded: 17_000_000
                    },
                    StdoutItem::Message(_),
                    StdoutItem::Closed(None)
                ]
            ),
            "{items:?}"
        );
        Ok(())
    }

    #[test]
    fn a_child_ignoring_sigterm_is_killed_and_confirmed() -> TestResult {
        let (events, events_received) = mpsc::channel::<SupervisorEvent>();
        let supervisor =
            Supervisor::spawn(&shell("trap '' TERM; echo ready; sleep 60"), 2, events)?;
        // Wait for the trap to be installed.
        let mut ready = Vec::new();
        let deadline = Instant::now() + WAIT;
        while ready.is_empty() && Instant::now() < deadline {
            ready.extend(supervisor.drain(1));
            thread::sleep(POLL);
        }
        let start = Instant::now();
        let stopped = terminate_group(supervisor.pid(), Duration::from_millis(300), WAIT);
        let took = start.elapsed();
        let (status, _) = exited(&events_received)?;
        assert!(supervisor.shutdown(Instant::now() + WAIT));
        assert_eq!(stopped, Termination::Confirmed);
        assert!(
            took >= Duration::from_millis(300),
            "SIGTERM alone stopped it"
        );
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        Ok(())
    }

    #[test]
    fn exit_reports_the_stderr_tail() -> TestResult {
        let script = "command -v /usr/bin/perl >/dev/null || exit 9; head -c 100000 /dev/zero | tr '\\0' e >&2; printf 'last words' >&2; exit 3";
        let (events, events_received) = mpsc::channel::<SupervisorEvent>();
        let supervisor = Supervisor::spawn(&shell(script), 3, events)?;
        let (status, tail) = exited(&events_received)?;
        assert!(supervisor.shutdown(Instant::now() + WAIT));
        assert_eq!(status.code(), Some(3));
        assert!(tail.ends_with("last words"));
        assert_eq!(tail.len(), STDERR_TAIL);
        Ok(())
    }

    #[test]
    fn writing_after_exit_fails() -> TestResult {
        let (events, events_received) = mpsc::channel::<SupervisorEvent>();
        let supervisor = Supervisor::spawn(&shell("exec 0<&-; exit 0"), 9, events)?;
        exited(&events_received)?;
        let deadline = Instant::now() + WAIT;
        let failed = loop {
            if let Err(error) = supervisor.send(&json!({"type": "user"})) {
                break error == StdinError::Closed;
            }
            if let Ok(SupervisorEvent::WriteFailed { transition, .. }) =
                events_received.recv_timeout(POLL)
            {
                break transition == 9;
            }
            if Instant::now() >= deadline {
                break false;
            }
        };
        assert!(supervisor.shutdown(Instant::now() + WAIT));
        assert!(failed, "a write to an exited child was not reported");
        Ok(())
    }
}
