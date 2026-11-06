mod agent_session;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
use platform::{agent_provider, foreground_member, process_directory};

use agent_session::AgentSession;

use muxy_protocol::{ForegroundProcess, MetadataEvent, ServerPath};
use muxy_terminal::TerminalEvent;
use muxy_terminal::pty::Pty;

pub(super) struct Metadata {
    pub(super) title: String,
    pub(super) directory: ServerPath,
    pub(super) process: Option<ForegroundProcess>,
    pub(super) agent: Option<muxy_protocol::AgentProvider>,
    /// The session's own shell is in the foreground, so no program it ran is.
    pub(super) shell_foreground: bool,
    /// The last agent session seen, kept after the agent exits so it can be resumed.
    pub(super) agent_session: Option<AgentSession>,
    next_agent: std::time::Instant,
    next_agent_session: std::time::Instant,
    agent_group: Option<u32>,
    observed_directory: Option<ServerPath>,
}

impl Metadata {
    pub(super) fn new(directory: ServerPath) -> Self {
        Self {
            title: String::new(),
            directory,
            process: None,
            agent: None,
            shell_foreground: false,
            agent_session: None,
            next_agent: std::time::Instant::now(),
            next_agent_session: std::time::Instant::now(),
            agent_group: None,
            observed_directory: None,
        }
    }

    pub(super) fn update(&mut self, pty: &Pty, terminal: Vec<TerminalEvent>) -> Vec<MetadataEvent> {
        let mut events = Vec::new();
        let previous_directory = self.directory.clone();
        let group = pty.foreground_pid();
        let member = group.and_then(foreground_member);
        self.shell_foreground = group == Some(pty.child_pid())
            && member.as_ref().is_some_and(|(_, name)| {
                matches!(
                    name.trim_start_matches('-'),
                    "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh"
                )
            });
        if self.next_agent <= std::time::Instant::now() || self.agent_group != group {
            let changed = self.agent_group != group;
            self.next_agent = std::time::Instant::now() + std::time::Duration::from_millis(500);
            self.agent_group = group;
            self.agent = if self.shell_foreground {
                None
            } else {
                group.and_then(agent_provider)
            };
            if let Some(event) = self.find_agent_session(group, changed) {
                events.push(event);
            }
        }
        if let Some(group) = group {
            let process = ForegroundProcess {
                name: member
                    .as_ref()
                    .map_or_else(String::new, |(_, name)| name.clone()),
                is_shell: group == pty.child_pid(),
            };
            if self.process.as_ref() != Some(&process) {
                events.push(MetadataEvent::ForegroundProcess {
                    name: process.name.clone(),
                    is_shell: process.is_shell,
                });
                self.process = Some(process);
            }
            if let Some(directory) = member.and_then(|(pid, _)| process_directory(pid))
                && self.observed_directory.as_ref() != Some(&directory)
            {
                self.directory.clone_from(&directory);
                self.observed_directory = Some(directory);
            }
        }
        for event in terminal {
            match event {
                TerminalEvent::Title(title) if title != self.title => {
                    self.title.clone_from(&title);
                    events.push(MetadataEvent::Title(title));
                }
                TerminalEvent::Directory(directory) => {
                    if let Some(directory) = terminal_directory(&directory) {
                        self.directory = directory;
                    }
                }
                TerminalEvent::ClipboardWrite(text) => {
                    events.push(MetadataEvent::ClipboardWrite(text));
                }
                TerminalEvent::Bell => events.push(MetadataEvent::Bell),
                TerminalEvent::Progress(_) | TerminalEvent::Title(_) => {}
            }
        }
        if self.directory != previous_directory {
            events.push(MetadataEvent::Directory(self.directory.clone()));
        }
        events
    }
}

impl Metadata {
    /// Rechecked every few seconds, since `/clear` or `/new` starts another session.
    fn find_agent_session(&mut self, group: Option<u32>, changed: bool) -> Option<MetadataEvent> {
        let now = std::time::Instant::now();
        let (Some(group), Some(provider)) = (group, self.agent) else {
            return None;
        };
        if !changed && now < self.next_agent_session {
            return None;
        }
        self.next_agent_session = now + std::time::Duration::from_secs(3);
        let session = agent_session::find(group, provider)?;
        if self.agent_session.as_ref() == Some(&session) {
            return None;
        }
        self.agent_session = Some(session.clone());
        Some(session.event())
    }
}

impl AgentSession {
    pub(crate) fn event(self) -> MetadataEvent {
        MetadataEvent::AgentSession {
            provider: self.provider,
            id: self.id,
            arguments: self.arguments,
        }
    }
}

fn terminal_directory(value: &[u8]) -> Option<ServerPath> {
    if value.starts_with(b"/") {
        return (!value.contains(&0)).then(|| ServerPath(value.to_vec()));
    }
    let rest = value.strip_prefix(b"file://")?;
    let path = &rest[rest.iter().position(|byte| *byte == b'/')? + 1..];
    let mut bytes = path.iter().copied();
    let mut path = vec![b'/'];
    while let Some(byte) = bytes.next() {
        let byte = if byte == b'%' {
            let high = char::from(bytes.next()?).to_digit(16)?;
            let low = char::from(bytes.next()?).to_digit(16)?;
            u8::try_from(high * 16 + low).ok()?
        } else {
            byte
        };
        if byte == 0 {
            return None;
        }
        path.push(byte);
    }
    Some(ServerPath(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_directories_keep_raw_and_percent_encoded_bytes() {
        assert_eq!(
            terminal_directory(b"file://host/tmp/caf\xe9"),
            Some(ServerPath(b"/tmp/caf\xe9".to_vec()))
        );
        assert_eq!(
            terminal_directory(b"file://host/tmp/caf%E9"),
            Some(ServerPath(b"/tmp/caf\xe9".to_vec()))
        );
        assert_eq!(
            terminal_directory(b"/plain/path"),
            Some(ServerPath(b"/plain/path".to_vec()))
        );
        assert_eq!(terminal_directory(b"file://host/a%00b"), None);
        assert_eq!(terminal_directory(b"relative"), None);
    }
}
