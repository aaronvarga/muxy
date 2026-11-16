//! Bounded native JSON-RPC over owned stdio or a verified daemon Unix WebSocket.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ring::digest::{SHA1_FOR_LEGACY_USE_ONLY, digest};
use serde_json::{Value, json};

use super::super::host::base64;
use super::super::supervisor::{StdoutItem, Supervisor, SupervisorEvent, terminate_group};
use super::context::{Context, Identity};

const MAX_MESSAGE: usize = 16 << 20;
pub(crate) const MAX_BUFFER: usize = 4 << 20;
const POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Error {
    Io,
    Timeout,
    Protocol,
    Refused,
    WrongHome,
    WrongEngine,
    Overflow,
    Closed,
}

impl std::fmt::Display for Error {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(match self {
            Self::Io => "Cannot communicate with the Codex engine",
            Self::Timeout => "The Codex engine did not acknowledge the request in time",
            Self::Protocol => "This Codex engine uses an unsupported protocol",
            Self::Refused => "Codex refused the request; review its Console configuration",
            Self::WrongHome => "The Codex engine belongs to another CODEX_HOME",
            Self::WrongEngine => "The Codex engine identity changed",
            Self::Overflow => "Codex events exceeded the buffer; resynchronize before continuing",
            Self::Closed => "The Codex connection closed",
        })
    }
}

enum Wire {
    Stdio {
        supervisor: Option<Supervisor>,
        events: mpsc::Receiver<SupervisorEvent>,
    },
    Socket(WebSocket),
}

pub(crate) struct Client {
    wire: Wire,
    next: u64,
    buffered: VecDeque<Value>,
    bytes: usize,
    pub(crate) engine: Identity,
    pub(crate) endpoint: Option<PathBuf>,
    socket_identity: Option<(u64, u64)>,
}

impl Client {
    pub(crate) fn shutdown_owned(&mut self, deadline: Instant) -> Result<(), Error> {
        let Wire::Stdio { supervisor, .. } = &mut self.wire else {
            return Err(Error::Protocol);
        };
        let Some(mut child) = supervisor.take() else {
            return Ok(());
        };
        child.close_stdin();
        let graceful = (Instant::now() + Duration::from_secs(2)).min(deadline);
        while !child.exited() && Instant::now() < graceful {
            std::thread::sleep(Duration::from_millis(20));
        }
        if !child.exited()
            && terminate_group(
                child.pid(),
                Duration::from_secs(1),
                deadline.saturating_duration_since(Instant::now()),
            ) != super::super::supervisor::Termination::Confirmed
        {
            *supervisor = Some(child);
            return Err(Error::Timeout);
        }
        child.shutdown(deadline);
        Ok(())
    }
    pub(crate) fn connect(context: &Context, deadline: Instant) -> Result<Self, Error> {
        let (wire, engine, endpoint, socket_identity) = if let Some(endpoint) = &context.endpoint {
            let endpoint = endpoint.canonicalize().map_err(|_| Error::Io)?;
            let metadata = endpoint.metadata().map_err(|_| Error::Io)?;
            let socket = UnixStream::connect(&endpoint).map_err(|_| Error::Io)?;
            let engine = Identity::read(peer_pid(&socket)?).map_err(|_| Error::WrongEngine)?;
            let socket = WebSocket::connect(socket, deadline)?;
            (
                Wire::Socket(socket),
                engine,
                Some(endpoint),
                Some((metadata.dev(), metadata.ino())),
            )
        } else {
            let (send, events) = mpsc::channel();
            let supervisor =
                Supervisor::spawn(&context.launch(), 0, send).map_err(|_| Error::Io)?;
            let engine =
                Identity::read(i32::try_from(supervisor.pid()).map_err(|_| Error::Protocol)?)
                    .map_err(|_| Error::WrongEngine)?;
            (
                Wire::Stdio {
                    supervisor: Some(supervisor),
                    events,
                },
                engine,
                None,
                None,
            )
        };
        let mut client = Self {
            wire,
            next: 0,
            buffered: VecDeque::new(),
            bytes: 0,
            engine,
            endpoint,
            socket_identity,
        };
        let initialized = client.request(
            "initialize",
            json!({"clientInfo":{"name":"muxy","version":env!("CARGO_PKG_VERSION")},
            "capabilities":{"experimentalApi":true,"requiredHumanApproval":true}}),
            deadline,
        )?;
        let home = initialized["codexHome"].as_str().ok_or(Error::Protocol)?;
        if PathBuf::from(home).canonicalize().ok().as_ref() != Some(&context.home) {
            return Err(Error::WrongHome);
        }
        client.send(&json!({"method":"initialized"}))?;
        if client.endpoint.is_none() {
            client.engine = Identity::read(client.engine.pid).map_err(|_| Error::WrongEngine)?;
        }
        context
            .verify_engine(&client.engine)
            .map_err(|_| Error::WrongEngine)?;
        Ok(client)
    }

    pub(crate) fn revalidate(&self) -> Result<(), Error> {
        self.engine.revalidate().map_err(|_| Error::WrongEngine)?;
        if let (Some(endpoint), Some(identity)) = (&self.endpoint, self.socket_identity) {
            let metadata = endpoint.metadata().map_err(|_| Error::WrongEngine)?;
            if (metadata.dev(), metadata.ino()) != identity {
                return Err(Error::WrongEngine);
            }
        }
        Ok(())
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "RPC owns its JSON payload across transport admission"
    )]
    pub(crate) fn request(
        &mut self,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, Error> {
        self.next += 1;
        let id = format!("muxy:{}", self.next);
        self.send(&json!({"id":id,"method":method,"params":params}))?;
        loop {
            if Instant::now() >= deadline {
                return Err(Error::Timeout);
            }
            let Some(message) = self.poll(deadline)? else {
                continue;
            };
            if message["id"] == id && message.get("method").is_none() {
                if message.get("error").is_some() {
                    return Err(Error::Refused);
                }
                return message.get("result").cloned().ok_or(Error::Protocol);
            }
            let size = message.to_string().len();
            if self.bytes + size > MAX_BUFFER || self.buffered.len() >= 256 {
                return Err(Error::Overflow);
            }
            self.bytes += size;
            self.buffered.push_back(message);
        }
    }

    pub(crate) fn take_buffered(&mut self) -> Vec<Value> {
        self.bytes = 0;
        self.buffered.drain(..).collect()
    }

    pub(crate) fn send(&mut self, message: &Value) -> Result<(), Error> {
        match &mut self.wire {
            Wire::Socket(socket) => socket.send(message.to_string().as_bytes(), 1),
            Wire::Stdio { supervisor, .. } => supervisor
                .as_ref()
                .ok_or(Error::Closed)?
                .send(message)
                .map_err(|_| Error::Io),
        }
    }

    pub(crate) fn poll(&mut self, deadline: Instant) -> Result<Option<Value>, Error> {
        match &mut self.wire {
            Wire::Socket(socket) => socket.poll(deadline),
            Wire::Stdio { supervisor, events } => {
                let supervisor = supervisor.as_ref().ok_or(Error::Closed)?;
                for event in events.try_iter() {
                    if matches!(event, SupervisorEvent::WriteFailed { .. }) {
                        return Err(Error::Io);
                    }
                }
                if let Some(item) = supervisor.drain(1).pop() {
                    return match item {
                        StdoutItem::Message(value) => Ok(Some(value)),
                        StdoutItem::Closed(_) => Err(Error::Closed),
                        StdoutItem::Malformed | StdoutItem::Overlong { .. } => Err(Error::Protocol),
                    };
                }
                if supervisor.exited() {
                    return Err(Error::Closed);
                }
                std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
                Ok(None)
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Wire::Stdio { supervisor, .. } = &mut self.wire
            && let Some(mut child) = supervisor.take()
        {
            child.close_stdin();
            if !child.exited() {
                terminate_group(child.pid(), Duration::from_secs(2), Duration::from_secs(4));
            }
            child.shutdown(Instant::now() + Duration::from_secs(1));
        }
        // Dropping a daemon connection only detaches; its engine is never signalled.
    }
}

#[allow(
    unsafe_code,
    reason = "Darwin LOCAL_PEERPID returns the kernel-verified Unix socket peer PID"
)]
fn peer_pid(socket: &UnixStream) -> Result<i32, Error> {
    let mut pid = 0_i32;
    let mut length = u32::try_from(size_of_val(&pid)).map_err(|_| Error::Protocol)?;
    // SAFETY: both output pointers name live correctly-sized scalar storage.
    if unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            0,
            2,
            std::ptr::from_mut(&mut pid).cast(),
            &raw mut length,
        )
    } != 0
        || length as usize != size_of_val(&pid)
        || pid <= 0
    {
        return Err(Error::WrongEngine);
    }
    Ok(pid)
}

struct WebSocket {
    socket: UnixStream,
    bytes: Vec<u8>,
    fragments: Vec<u8>,
    fragmented: bool,
}

impl WebSocket {
    fn connect(mut socket: UnixStream, deadline: Instant) -> Result<Self, Error> {
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| Error::Io)?;
        let key = base64(&nonce);
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|_| Error::Io)?;
        socket.write_all(format!("GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").as_bytes()).map_err(|_| Error::Io)?;
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            if header.len() >= 8192 || Instant::now() >= deadline {
                return Err(Error::Protocol);
            }
            socket
                .set_read_timeout(Some(deadline.saturating_duration_since(Instant::now())))
                .map_err(|_| Error::Io)?;
            let mut byte = [0];
            socket.read_exact(&mut byte).map_err(|_| Error::Io)?;
            header.push(byte[0]);
        }
        let header = std::str::from_utf8(&header).map_err(|_| Error::Protocol)?;
        let expected = base64(
            digest(
                &SHA1_FOR_LEGACY_USE_ONLY,
                format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
            )
            .as_ref(),
        );
        if !header.starts_with("HTTP/1.1 101 ")
            || !header.lines().any(|line| {
                line.split_once(':').is_some_and(|(name, value)| {
                    name.eq_ignore_ascii_case("sec-websocket-accept") && value.trim() == expected
                })
            })
        {
            return Err(Error::Protocol);
        }
        Ok(Self {
            socket,
            bytes: Vec::with_capacity(65536),
            fragments: Vec::new(),
            fragmented: false,
        })
    }

    fn send(&mut self, payload: &[u8], opcode: u8) -> Result<(), Error> {
        if payload.len() > 64 << 20 {
            return Err(Error::Overflow);
        }
        let mut bytes = Vec::with_capacity(payload.len() + 14);
        bytes.push(0x80 | opcode);
        if payload.len() < 126 {
            bytes.push(0x80 | u8::try_from(payload.len()).map_err(|_| Error::Protocol)?);
        } else if let Ok(length) = u16::try_from(payload.len()) {
            bytes.push(0xfe);
            bytes.extend_from_slice(&length.to_be_bytes());
        } else {
            bytes.push(0xff);
            bytes.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
        let mut mask = [0_u8; 4];
        getrandom::fill(&mut mask).map_err(|_| Error::Io)?;
        bytes.extend_from_slice(&mask);
        bytes.extend(
            payload
                .iter()
                .enumerate()
                .map(|(at, byte)| byte ^ mask[at % 4]),
        );
        self.socket.write_all(&bytes).map_err(|_| Error::Io)
    }

    fn poll(&mut self, deadline: Instant) -> Result<Option<Value>, Error> {
        loop {
            if let Some((opcode, final_frame, offset, length)) = frame_header(&self.bytes)? {
                let total = offset + length;
                if self.bytes.len() >= total {
                    let payload = self.bytes[offset..total].to_vec();
                    self.bytes.drain(..total);
                    match opcode {
                        8 => return Err(Error::Closed),
                        9 => {
                            self.send(&payload, 10)?;
                            continue;
                        }
                        10 => continue,
                        1 if !self.fragmented => self.fragments.clear(),
                        0 if self.fragmented => {}
                        _ => return Err(Error::Protocol),
                    }
                    if self.fragments.len() + payload.len() > MAX_MESSAGE {
                        return Err(Error::Overflow);
                    }
                    self.fragments.extend_from_slice(&payload);
                    self.fragmented = !final_frame;
                    if final_frame {
                        let value = serde_json::from_slice::<Value>(&self.fragments)
                            .map_err(|_| Error::Protocol)?;
                        self.fragments.clear();
                        if !value.is_object() {
                            return Err(Error::Protocol);
                        }
                        return Ok(Some(value));
                    }
                    continue;
                }
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            self.socket
                .set_read_timeout(Some(
                    POLL.min(deadline.saturating_duration_since(Instant::now())),
                ))
                .map_err(|_| Error::Io)?;
            let mut buffer = [0_u8; 16384];
            match self.socket.read(&mut buffer) {
                Ok(0) => return Err(Error::Closed),
                Ok(count) if self.bytes.len() + count <= MAX_MESSAGE + 65550 => {
                    self.bytes.extend_from_slice(&buffer[..count]);
                }
                Ok(_) => return Err(Error::Overflow),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                Err(_) => return Err(Error::Io),
            }
        }
    }
}

fn frame_header(bytes: &[u8]) -> Result<Option<(u8, bool, usize, usize)>, Error> {
    if bytes.len() < 2 {
        return Ok(None);
    }
    if bytes[0] & 0x70 != 0 || bytes[1] & 0x80 != 0 {
        return Err(Error::Protocol);
    }
    let (offset, length) = match bytes[1] & 0x7f {
        126 if bytes.len() < 4 => return Ok(None),
        126 => (4, usize::from(u16::from_be_bytes([bytes[2], bytes[3]]))),
        127 if bytes.len() < 10 => return Ok(None),
        127 => (
            10,
            usize::try_from(u64::from_be_bytes(
                bytes[2..10].try_into().map_err(|_| Error::Protocol)?,
            ))
            .map_err(|_| Error::Overflow)?,
        ),
        length => (2, usize::from(length)),
    };
    let opcode = bytes[0] & 15;
    if !matches!(opcode, 0 | 1 | 8 | 9 | 10) {
        return Err(Error::Protocol);
    }
    let final_frame = bytes[0] & 0x80 != 0;
    if length > MAX_MESSAGE || (opcode >= 8 && (!final_frame || length > 125)) {
        return Err(Error::Overflow);
    }
    Ok(Some((opcode, final_frame, offset, length)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_and_invalid_control_frames_are_rejected_before_allocating() {
        let mut large = vec![0x81, 127];
        large.extend_from_slice(&u64::MAX.to_be_bytes());
        assert!(frame_header(&large).is_err());
        assert!(frame_header(&[0x09, 1]).is_err());
        assert!(frame_header(&[0x81, 0x80]).is_err());
        assert!(frame_header(&[0x81, 127, 0]).unwrap().is_none());
    }
}
