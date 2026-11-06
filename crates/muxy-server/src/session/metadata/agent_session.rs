use std::path::{Path, PathBuf};

use muxy_protocol::AgentProvider;

use super::platform::{group_members, open_files, process_arguments};

/// A coding agent's own session, which its resume command reopens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentSession {
    pub(crate) provider: AgentProvider,
    pub(crate) id: String,
    pub(crate) arguments: Vec<String>,
}

/// Finds the session the agent in foreground `group` is writing.
pub(super) fn find(group: u32, provider: AgentProvider) -> Option<AgentSession> {
    let members = group_members(group);
    let (argv, id) = match provider {
        AgentProvider::Claude => {
            let sessions = claude_home()?.join("sessions");
            // Only Claude itself, so a stale `<pid>.json` can't match a reused pid of a child.
            members.iter().find_map(|(pid, name)| {
                let argv = process_arguments(*pid);
                (crate::detection::identify(name, &argv) == Some(AgentProvider::Claude))
                    .then_some(())?;
                Some((argv, claude_session(&sessions, *pid)?))
            })?
        }
        AgentProvider::Codex => members
            .iter()
            .filter(|(_, name)| name == "codex")
            .find_map(|(pid, _)| {
                let id = codex_session(&open_files(*pid))?;
                Some((process_arguments(*pid), id))
            })?,
        _ => return None,
    };
    Some(AgentSession {
        provider,
        id,
        arguments: user_arguments(argv),
    })
}

fn claude_home() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".claude")))
}

/// Claude Code records each running process's session in `sessions/<pid>.json`.
fn claude_session(sessions: &Path, pid: i32) -> Option<String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Record {
        pid: i32,
        session_id: String,
    }
    let bytes = std::fs::read(sessions.join(format!("{pid}.json"))).ok()?;
    let record: Record = serde_json::from_slice(&bytes).ok()?;
    // Clients drop the connection on an id they reject, so send only well-formed ones.
    (record.pid == pid && uuid_shaped(&record.session_id)).then_some(record.session_id)
}

/// Codex keeps its `rollout-<time>-<uuid>.jsonl` open while the session runs.
fn codex_session(files: &[PathBuf]) -> Option<String> {
    // shortcut: subagents open later rollouts in the same process, so the oldest is the
    // main session; revisit if Codex keeps a replaced session's rollout open after /new.
    files
        .iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
            let id = stem.get(stem.len().checked_sub(36)?..)?;
            uuid_shaped(id).then(|| (name.to_owned(), id.to_owned()))
        })
        .min()
        .map(|(_, id)| id)
}

fn uuid_shaped(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// Drops the program, and the script when a runtime such as node runs it.
fn user_arguments(mut argv: Vec<String>) -> Vec<String> {
    let runtime = argv.first().is_some_and(|program| {
        let name = Path::new(program)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(program);
        matches!(name, "node" | "bun" | "deno") || name.starts_with("python")
    });
    let skip = if runtime { 2 } else { 1 };
    argv.drain(..skip.min(argv.len()));
    // Dropping some words could pair a flag with the wrong value, so keep all or none.
    if argv.len() > muxy_protocol::MAX_AGENT_ARGUMENTS
        || argv
            .iter()
            .any(|argument| argument.len() > muxy_protocol::MAX_AGENT_ARGUMENT)
    {
        argv.clear();
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_session_is_the_oldest_open_rollout() {
        let files = vec![
            PathBuf::from("/x/.codex/log/codex-tui.log"),
            PathBuf::from(
                "/x/sessions/2026/11/06/rollout-2026-11-06T08-12-00-01a1b0fb-0000-7741-bc07-b4efd1aaba4f.jsonl",
            ),
            PathBuf::from(
                "/x/sessions/2026/11/06/rollout-2026-11-06T08-10-36-01a1b0fb-84a9-7741-bc07-b4efd1aaba4e.jsonl",
            ),
            PathBuf::from("/x/sessions/rollout-not-a-uuid.jsonl"),
        ];
        assert_eq!(
            codex_session(&files).as_deref(),
            Some("01a1b0fb-84a9-7741-bc07-b4efd1aaba4e")
        );
        assert_eq!(codex_session(&[]), None);
    }

    #[test]
    fn claude_sessions_with_malformed_ids_are_ignored() -> Result<(), Box<dyn std::error::Error>> {
        let directory = std::env::temp_dir().join(format!(
            "muxy-claude-sessions-{}",
            muxy_protocol::OperationId::new()
        ));
        std::fs::create_dir_all(&directory)?;
        std::fs::write(
            directory.join("42.json"),
            r#"{"pid":42,"sessionId":"bde3d35b-d3c0-49d7-93c1-9a0391781702","cwd":"/"}"#,
        )?;
        std::fs::write(directory.join("43.json"), r#"{"pid":43,"sessionId":""}"#)?;
        let found = (
            claude_session(&directory, 42),
            claude_session(&directory, 43),
        );
        std::fs::remove_dir_all(&directory)?;
        assert_eq!(
            found,
            (Some("bde3d35b-d3c0-49d7-93c1-9a0391781702".into()), None)
        );
        Ok(())
    }

    #[test]
    fn user_arguments_skip_the_program_and_script() {
        let strings = |values: &[&str]| values.iter().map(|&value| value.to_owned()).collect();
        assert_eq!(
            user_arguments(strings(&["claude", "--enable-auto-mode"])),
            vec!["--enable-auto-mode"]
        );
        assert_eq!(
            user_arguments(strings(&["/usr/bin/node", "/lib/codex.js", "--yolo"])),
            vec!["--yolo"]
        );
        assert!(user_arguments(Vec::new()).is_empty());
    }
}
