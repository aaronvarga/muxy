//! Fork: the session's input gate, which pauses terminal input while chat
//! holds the session.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SendError, Sender};
use std::sync::{Arc, Mutex, PoisonError};

use muxy_protocol::ErrorCode;

use crate::error::ServerError;

pub(super) struct InputWrite {
    pub(super) bytes: Vec<u8>,
    pub(super) reply: Option<Sender<Result<(), ServerError>>>,
    /// Written even while the gate is closed: the server's own recovery line, and fences.
    pub(super) authorized: bool,
}

impl From<Vec<u8>> for InputWrite {
    fn from(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            reply: None,
            authorized: false,
        }
    }
}

impl InputWrite {
    pub(super) fn refuse(self) {
        if let Some(reply) = self.reply {
            let _ = reply.send(Err(ServerError::new(
                ErrorCode::BadRequest,
                "this session is in Chat mode; terminal input is paused until it returns to Console",
            )));
        }
    }
}

/// Fork: the input gate. While it is closed the PTY takes no input from any
/// producer (typed, mouse, terminal replies, `WriteInput`), except the
/// server's own authorized writes. Resize and frames are unaffected.
///
/// Why closing is race-free: [`InputGate::send`] checks the flag and queues,
/// and [`InputGate::close`] sets the flag and queues a fence, under one lock,
/// so every input is either queued ahead of the fence or refused. The writer
/// takes items strictly in order, drops unauthorized items it pops after the
/// gate closed, and acknowledges the fence only once every earlier `write_all`
/// has returned. The fence's acknowledgement therefore means the writer is
/// idle: the queue ahead of it is empty and nothing is mid-write. Nobody waits
/// while holding the lock, so the owner thread never blocks on a waiter.
pub(crate) struct InputGate {
    queue: Mutex<Sender<InputWrite>>,
    closed: Arc<AtomicBool>,
}

impl std::fmt::Debug for InputGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InputGate")
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl InputGate {
    /// A gate over the writer's queue; the writer drops unauthorized input
    /// it pops while `closed` is set.
    pub(super) fn new(queue: Sender<InputWrite>, closed: Arc<AtomicBool>) -> Self {
        Self {
            queue: Mutex::new(queue),
            closed,
        }
    }

    /// Queues input, or refuses it while the gate is closed: a `WriteInput`
    /// reply gets an error, other input is dropped.
    pub(super) fn send(&self, input: InputWrite) -> Result<(), SendError<InputWrite>> {
        let queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if !input.authorized && self.closed.load(Ordering::Acquire) {
            drop(queue);
            input.refuse();
            return Ok(());
        }
        queue.send(input)
    }

    /// The one path that writes while the gate is closed: the server's own
    /// recovery line. `reply` is acknowledged after the bytes are written.
    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "used by the macOS chat host")
    )]
    pub(crate) fn send_authorized(
        &self,
        bytes: Vec<u8>,
        reply: Option<Sender<Result<(), ServerError>>>,
    ) -> Result<(), SendError<Vec<u8>>> {
        self.send(InputWrite {
            bytes,
            reply,
            authorized: true,
        })
        .map_err(|SendError(input)| SendError(input.bytes))
    }

    /// Closes the gate and returns the fence, acknowledged once the writer is
    /// idle. Disconnected means the writer has stopped.
    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "used by the macOS chat host")
    )]
    pub(crate) fn close(&self) -> Receiver<Result<(), ServerError>> {
        let (reply, fence) = mpsc::channel();
        let queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        self.closed.store(true, Ordering::Release);
        let _ = queue.send(InputWrite {
            bytes: Vec::new(),
            reply: Some(reply),
            authorized: true,
        });
        fence
    }

    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "used by the macOS chat host")
    )]
    pub(crate) fn open(&self) {
        let _queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        self.closed.store(false, Ordering::Release);
    }
}
