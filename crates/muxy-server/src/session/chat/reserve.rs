//! Lets one Chat at a time hold a conversation, across every session of the
//! server, and refuses a conversation another live Claude process is using.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use muxy_protocol::ChatRefusal;

use crate::session::metadata::agent_session::claude_session;

/// The registry-wide table of reserved `(canonical config dir, conversation)`.
#[derive(Debug, Default)]
pub(crate) struct Reservations {
    held: Mutex<HashSet<(PathBuf, String)>>,
}

/// Holds a conversation until dropped or [`Reservation::release`]d.
#[derive(Debug)]
pub(crate) struct Reservation {
    table: Arc<Reservations>,
    key: (PathBuf, String),
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.table
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.key);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReserveError {
    ConversationInUse,
    /// The config directory doesn't exist or can't be resolved.
    NoConfig,
}

impl ReserveError {
    pub(crate) const fn refusal(self) -> ChatRefusal {
        match self {
            Self::ConversationInUse => ChatRefusal::ConversationInUse,
            Self::NoConfig => ChatRefusal::NoClaudeSession,
        }
    }
}

impl Reservations {
    /// Atomically reserves the conversation, then checks that no live Claude
    /// other than `replacing` (the TUI being stopped) records it in
    /// `<config>/sessions/*.json`.
    pub(crate) fn try_reserve(
        self: &Arc<Self>,
        config_dir: &Path,
        session_id: &str,
        replacing: i32,
    ) -> Result<Reservation, ReserveError> {
        let config_dir = config_dir
            .canonicalize()
            .map_err(|_| ReserveError::NoConfig)?;
        let key = (config_dir, session_id.to_owned());
        if !self
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.clone())
        {
            return Err(ReserveError::ConversationInUse);
        }
        let reservation = Reservation {
            table: self.clone(),
            key,
        };
        if used_elsewhere(&reservation.key.0.join("sessions"), session_id, replacing) {
            return Err(ReserveError::ConversationInUse);
        }
        Ok(reservation)
    }
}

fn used_elsewhere(sessions: &Path, session_id: &str, replacing: i32) -> bool {
    let Ok(entries) = std::fs::read_dir(sessions) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|pid| pid.parse::<i32>().ok())
        else {
            return false;
        };
        pid != replacing
            && claude_session(sessions, pid).as_deref() == Some(session_id)
            && claude_running(pid)
    })
}

/// A stale record's pid may since belong to another program, so on macOS
/// only Claude counts.
fn claude_running(pid: i32) -> bool {
    use crate::session::metadata::macos::process_arguments;
    let Some(process) = rustix::process::Pid::from_raw(pid) else {
        return false;
    };
    match rustix::process::test_kill_process(process) {
        Ok(()) | Err(rustix::io::Errno::PERM) => {}
        Err(_) => return false,
    }
    let name = libproc::proc_pid::name(pid).unwrap_or_default();
    crate::detection::identify(&name, &process_arguments(pid))
        == Some(muxy_protocol::AgentProvider::Claude)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    const ID: &str = "bde3d35b-d3c0-49d7-93c1-9a0391781702";

    fn record(sessions: &Path, pid: u32) -> std::io::Result<()> {
        std::fs::write(
            sessions.join(format!("{pid}.json")),
            format!(r#"{{"pid":{pid},"sessionId":"{ID}"}}"#),
        )
    }

    #[test]
    fn a_conversation_is_reserved_once() -> TestResult {
        let config = std::env::temp_dir().join(format!(
            "muxy-chat-reserve-{}",
            muxy_protocol::OperationId::new()
        ));
        let sessions = config.join("sessions");
        std::fs::create_dir_all(&sessions)?;
        let table = Arc::new(Reservations::default());

        let first = table.try_reserve(&config, ID, 1);
        // The same directory by another spelling is the same conversation.
        let again = table.try_reserve(&config.join("sessions/.."), ID, 1);
        drop(first);
        let after_release = table.try_reserve(&config, ID, 1).map(drop);
        // A record whose pid is gone, or is the TUI being replaced, doesn't count.
        record(&sessions, 999_999)?;
        let stale = table.try_reserve(&config, ID, 1).map(drop);
        let own = std::process::id();
        record(&sessions, own)?;
        let replaced = table
            .try_reserve(&config, ID, i32::try_from(own)?)
            .map(drop);
        #[cfg(target_os = "macos")]
        let live = {
            use std::os::unix::process::CommandExt;
            let mut claude = std::process::Command::new("/bin/sleep")
                .arg0("claude")
                .arg("30")
                .spawn()?;
            record(&sessions, claude.id())?;
            // Let exec finish so its arguments are readable.
            std::thread::sleep(std::time::Duration::from_millis(200));
            let live = table.try_reserve(&config, ID, 1).err();
            claude.kill()?;
            claude.wait()?;
            live
        };
        std::fs::remove_dir_all(&config)?;

        assert_eq!(again.err(), Some(ReserveError::ConversationInUse));
        assert_eq!(after_release, Ok(()));
        assert_eq!(stale, Ok(()));
        assert_eq!(replaced, Ok(()));
        #[cfg(target_os = "macos")]
        assert_eq!(live, Some(ReserveError::ConversationInUse));
        Ok(())
    }
}
