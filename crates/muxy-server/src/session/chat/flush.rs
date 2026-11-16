//! Clearing typed input before Claude changes hands.

use std::error::Error;
use std::io;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use muxy_terminal::pty::Pty;

use crate::session::input_gate::InputGate;

/// Why typed input could not be cleared.
#[derive(Debug)]
pub(crate) enum FlushError {
    /// The writer did not go idle before the deadline.
    WriterBusy,
    FlushFailed(io::Error),
    PlatformUnsupported,
}

impl std::fmt::Display for FlushError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WriterBusy => formatter.write_str("terminal input writer is still busy"),
            Self::FlushFailed(error) => {
                write!(formatter, "could not clear terminal input: {error}")
            }
            Self::PlatformUnsupported => {
                formatter.write_str("clearing terminal input is not supported on this platform")
            }
        }
    }
}

impl Error for FlushError {}

/// The session's terminal device, held so a supervisor thread can clear its
/// unread input. The duplicated master keeps the device allocated, so its
/// name can't be handed to another terminal while this exists.
#[derive(Debug)]
pub(crate) struct FlushTarget {
    path: std::path::PathBuf,
    master: std::os::fd::OwnedFd,
}

impl FlushTarget {
    pub(crate) fn new(pty: &Pty) -> Result<Self, FlushError> {
        let path = pty
            .tty_path()
            .ok_or(FlushError::PlatformUnsupported)?
            .to_path_buf();
        let master = pty.clone_master().map_err(FlushError::FlushFailed)?;
        Ok(Self { path, master })
    }

    /// The master side, for `tcgetpgrp`.
    pub(crate) fn master(&self) -> std::os::fd::BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.master.as_fd()
    }

    /// Discards input the terminal holds that no program has read yet.
    #[allow(
        unsafe_code,
        reason = "Darwin names a master's terminal and flushes it only through ioctl and tcflush"
    )]
    fn flush_input(&self) -> Result<(), FlushError> {
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;
        const TIOCPTYGNAME: libc::c_ulong = 0x4080_7453; // _IOR('t', 83, char[128])
        let failed = |error| FlushError::FlushFailed(error);
        let mut name = [0u8; 128];
        // SAFETY: the master is open and `name` is the 128 bytes TIOCPTYGNAME writes.
        if unsafe { libc::ioctl(self.master.as_raw_fd(), TIOCPTYGNAME, name.as_mut_ptr()) } != 0 {
            return Err(failed(io::Error::last_os_error()));
        }
        let length = name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(name.len());
        if &name[..length] != self.path.as_os_str().as_bytes() {
            return Err(failed(io::Error::other(
                "terminal device path no longer names this session's terminal",
            )));
        }
        let device = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(&self.path)
            .map_err(failed)?;
        // SAFETY: `device` is an open terminal descriptor for the duration of the call.
        if unsafe { libc::tcflush(device.as_raw_fd(), libc::TCIFLUSH) } != 0 {
            return Err(failed(io::Error::last_os_error()));
        }
        Ok(())
    }
}

/// Clears typed input before Claude changes hands: closes the gate, waits up
/// to `timeout` for the writer to go idle, then discards unread terminal
/// input. Blocks, so it runs on a supervisor thread, never the owner. The gate
/// stays closed; reopen it with [`InputGate::open`].
pub(crate) fn gate_and_flush(
    gate: &InputGate,
    target: &FlushTarget,
    timeout: Duration,
) -> Result<(), FlushError> {
    match gate.close().recv_timeout(timeout) {
        Ok(_) => target.flush_input(),
        Err(RecvTimeoutError::Timeout) => Err(FlushError::WriterBusy),
        Err(RecvTimeoutError::Disconnected) => Err(FlushError::FlushFailed(io::Error::other(
            "terminal input writer stopped",
        ))),
    }
}
