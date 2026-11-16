//! Finds the Claude Code TUI in a session's terminal and everything needed to
//! relaunch it headless: the launch prefix, arguments, cwd, environment,
//! config directory and conversation. Shares process reading with metadata
//! detection. Blocks on process and file reads, so it runs on a supervisor
//! thread.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use muxy_protocol::ChatRefusal;

pub(crate) fn launch_stamp(paths: &[&Path], env: &[(OsString, OsString)]) -> u64 {
    use std::hash::{Hash, Hasher};
    use std::os::unix::fs::MetadataExt;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    paths.hash(&mut hash);
    for path in paths {
        if let Ok(metadata) = path.metadata() {
            (
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.mtime(),
                metadata.mtime_nsec(),
            )
                .hash(&mut hash);
        }
    }
    // Environment values stay server-side; only this opaque context stamp leaves the process.
    let environment: std::collections::BTreeMap<_, _> = env
        .iter()
        // Oh My Posh generates a new prompt-session ID for each login-shell probe.
        .filter(|(name, _)| name != "POSH_SESSION_ID")
        .map(|(name, value)| (name, value))
        .collect();
    environment.hash(&mut hash);
    hash.finish()
}

impl ClaudeContext {
    pub(crate) fn launch_stamp(&self) -> u64 {
        let executable = self.launch_prefix.first().map_or(Path::new(""), Path::new);
        launch_stamp(
            &[
                executable,
                &self.cwd,
                &self.config_dir.join("settings.json"),
                &self.cwd.join(".claude/settings.json"),
            ],
            &self.env,
        )
    }
}

use crate::session::metadata::agent_session::uuid_shaped;

/// The running Claude TUI. `Debug` names environment variables but never
/// prints their values.
#[derive(Clone)]
pub(crate) struct ClaudeContext {
    pub(crate) pid: i32,
    /// Claude's process group, which owns the terminal's foreground.
    pub(crate) group: u32,
    /// The PTY's direct child, normally the shell.
    pub(crate) shell: u32,
    /// The native executable, or the runtime and Claude Code's script.
    pub(crate) launch_prefix: Vec<OsString>,
    /// Everything after the launch prefix, unfiltered.
    pub(crate) user_args: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) env: Vec<(OsString, OsString)>,
    /// `CLAUDE_CONFIG_DIR`, else `~/.claude`, as Claude saw it (not canonical).
    pub(crate) config_dir: PathBuf,
    /// The conversation UUID from `<config>/sessions/<pid>.json`.
    pub(crate) session_id: String,
}

impl std::fmt::Debug for ClaudeContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClaudeContext")
            .field("pid", &self.pid)
            .field("group", &self.group)
            .field("shell", &self.shell)
            .field("launch_prefix", &self.launch_prefix)
            .field("user_args", &self.user_args)
            .field("cwd", &self.cwd)
            .field(
                "env",
                &self.env.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .field("config_dir", &self.config_dir)
            .field("session_id", &self.session_id)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ContextError {
    /// No Claude in the foreground, or something about it can't be read reliably.
    NoClaudeSession,
    /// The shell is gone, or Claude replaced it (`exec claude`).
    NoShell,
    /// Start: the terminal's own process isn't a known interactive shell, or is gone.
    NotAtPrompt,
    /// The login shell didn't report its environment.
    EnvironmentUnread,
    /// No `claude` on the shell's `PATH`.
    ClaudeNotFound,
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NoClaudeSession => "no resumable Claude Code session in this terminal",
            Self::NoShell => "Claude Code is not running under a shell",
            Self::NotAtPrompt => "this terminal isn't at a shell prompt",
            Self::EnvironmentUnread => "your login shell didn't report its environment",
            Self::ClaudeNotFound => "`claude` isn't on your shell's PATH",
        })
    }
}

impl std::error::Error for ContextError {}

impl ContextError {
    pub(crate) const fn refusal(self) -> ChatRefusal {
        match self {
            Self::NoClaudeSession => ChatRefusal::NoClaudeSession,
            Self::NoShell | Self::NotAtPrompt => ChatRefusal::NoShell,
            Self::EnvironmentUnread => ChatRefusal::UnsupportedEnvironment,
            Self::ClaudeNotFound => ChatRefusal::CliIncompatible,
        }
    }
}

/// Resolves the Claude process in the terminal's `foreground` group
/// (`tcgetpgrp` on the master) of the session whose PTY child is `shell`.
pub(crate) fn resolve(
    shell: u32,
    foreground: Option<u32>,
    _: std::time::Duration,
    _: &[(OsString, OsString)],
) -> Result<ClaudeContext, ContextError> {
    use muxy_protocol::AgentProvider;
    use std::os::unix::ffi::OsStringExt;

    use crate::session::metadata::agent_session::{claude_config_dir, claude_session};
    use crate::session::metadata::macos::{group_members, process_directory, process_launch};

    let missing = ContextError::NoClaudeSession;
    let group = foreground.ok_or(missing)?;
    let (pid, launch) = group_members(group)
        .into_iter()
        .find_map(|(pid, name)| {
            let launch = process_launch(pid)?;
            let argv = lossy(&launch.argv);
            (crate::detection::identify(&name, &argv) == Some(AgentProvider::Claude))
                .then_some((pid, launch))
        })
        .ok_or(missing)?;
    let cwd = process_directory(pid)
        .map(|directory| PathBuf::from(OsString::from_vec(directory.0)))
        .ok_or(missing)?;
    // Platform binaries hide their environment; Claude never is one, so empty means unreadable.
    if launch.env.is_empty() {
        return Err(missing);
    }
    let (launch_prefix, user_args) = split_launch(&launch.executable, &launch.argv, &cwd)?;
    let config_dir = claude_config_dir(&launch.env, Some(&cwd)).ok_or(missing)?;
    let session_id = claude_session(&config_dir.join("sessions"), pid).ok_or(missing)?;
    check_shell(shell, Some(group))?;
    Ok(ClaudeContext {
        pid,
        group,
        shell,
        launch_prefix,
        user_args,
        cwd,
        env: launch.env,
        config_dir,
        session_id,
    })
}

/// Everything a new conversation needs from the idle shell `shell`: its cwd,
/// the environment its login shell sets up, `claude` on that `PATH`, and a
/// fresh conversation UUID, reading it within `budget`. The caller checks the
/// shell owns the terminal.
///
/// The live shell's environment can't be read: macOS hides it for platform
/// binaries such as `/bin/zsh`, and for others it is the one the shell
/// started with, before its startup files. So a new login shell reports it.
// shortcut: variables exported only in this terminal (`export`, a sourced
// venv) are missed; have shell integration report them if users need that.
pub(crate) fn resolve_shell(
    shell: u32,
    _: Option<u32>,
    budget: std::time::Duration,
    base: &[(OsString, OsString)],
) -> Result<ClaudeContext, ContextError> {
    use crate::session::metadata::agent_session::claude_config_dir;

    let ShellContext { group, cwd, env } = capture_shell(shell, budget, base)?;
    let claude = on_path("claude", &env).ok_or(ContextError::ClaudeNotFound)?;
    let config_dir = claude_config_dir(&env, Some(&cwd)).ok_or(ContextError::NoClaudeSession)?;
    Ok(ClaudeContext {
        pid: 0,
        group,
        shell,
        launch_prefix: vec![claude.into_os_string()],
        user_args: Vec::new(),
        cwd,
        env,
        config_dir,
        session_id: new_uuid().ok_or(ContextError::NoClaudeSession)?,
    })
}

/// The launch context shared by promptless provider probes and first submission.
pub(crate) struct ShellContext {
    pub(crate) group: u32,
    pub(crate) cwd: PathBuf,
    pub(crate) env: Vec<(OsString, OsString)>,
}

pub(crate) fn capture_shell(
    shell: u32,
    budget: std::time::Duration,
    base: &[(OsString, OsString)],
) -> Result<ShellContext, ContextError> {
    use crate::session::metadata::macos::process_directory;
    use std::os::unix::ffi::OsStringExt;
    let group = check_shell(shell, None).map_err(|_| ContextError::NotAtPrompt)?;
    let pid = i32::try_from(shell).map_err(|_| ContextError::NotAtPrompt)?;
    let program = libproc::proc_pid::pidpath(pid).map_err(|_| ContextError::NotAtPrompt)?;
    // Only a shell runs `-l -i -c` as a login shell; anything else could do anything with it.
    if !is_shell(Path::new(&program)) {
        return Err(ContextError::NotAtPrompt);
    }
    let cwd = process_directory(pid)
        .map(|directory| PathBuf::from(OsString::from_vec(directory.0)))
        .ok_or(ContextError::NotAtPrompt)?;
    let env = login_environment(
        Path::new(&program),
        base.to_vec(),
        &cwd,
        ENVIRONMENT_TIMEOUT.min(budget),
    )
    .ok_or(ContextError::EnvironmentUnread)?;
    Ok(ShellContext { group, cwd, env })
}

/// Whether `program` is an interactive shell Start knows how to probe.
fn is_shell(program: &Path) -> bool {
    program
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "zsh" | "bash" | "sh" | "dash" | "ksh" | "fish"))
}

const ENVIRONMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Printed before the environment, after anything the startup files print.
const ENVIRONMENT_MARK: &[u8] = b"\0muxy-environment\0";

/// The environment a login, interactive `shell` sets up in `cwd`, started
/// from `base`, as `env -0` prints it after the startup files ran.
fn login_environment(
    shell: &Path,
    base: Vec<(OsString, OsString)>,
    cwd: &Path,
    timeout: std::time::Duration,
) -> Option<Vec<(OsString, OsString)>> {
    use std::os::unix::ffi::OsStrExt;
    let launch = super::supervisor::Launch {
        prefix: vec![shell.into()],
        args: [
            "-l",
            "-i",
            "-c",
            "/usr/bin/printf '\\0%s\\0' muxy-environment; exec /usr/bin/env -0",
        ]
        .map(Into::into)
        .to_vec(),
        env: base,
        cwd: cwd.to_path_buf(),
    };
    let output = super::supervisor::run_output(&launch, timeout, 1 << 20).ok()?;
    let start = output
        .windows(ENVIRONMENT_MARK.len())
        .rposition(|window| window == ENVIRONMENT_MARK)?
        + ENVIRONMENT_MARK.len();
    let env: Vec<_> = output[start..]
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let at = entry.iter().position(|byte| *byte == b'=')?;
            // The child's cwd is set explicitly, and shells recompute these.
            if matches!(&entry[..at], b"PWD" | b"OLDPWD") {
                return None;
            }
            Some((
                std::ffi::OsStr::from_bytes(&entry[..at]).to_owned(),
                std::ffi::OsStr::from_bytes(&entry[at + 1..]).to_owned(),
            ))
        })
        .collect();
    (!env.is_empty()).then_some(env)
}

/// `program`'s executable file in the first `PATH` folder of `env` holding one.
pub(crate) fn on_path(program: &str, env: &[(OsString, OsString)]) -> Option<PathBuf> {
    let path = env.iter().find(|(name, _)| name == "PATH")?;
    std::env::split_paths(&path.1)
        .filter(|folder| folder.is_absolute())
        .map(|folder| folder.join(program))
        .find(|file| {
            file.is_file() && rustix::fs::access(file, rustix::fs::Access::EXEC_OK).is_ok()
        })
}

/// A random (version 4) UUID, as Claude Code's `--session-id` takes it.
pub(crate) fn new_uuid() -> Option<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    bytes[6] = bytes[6] & 0x0f | 0x40;
    bytes[8] = bytes[8] & 0x3f | 0x80;
    let hex = bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
        hex
    });
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

fn lossy(argv: &[OsString]) -> Vec<String> {
    argv.iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect()
}

/// Splits argv into the launch prefix and the user's arguments. The prefix
/// starts with the kernel's executable path, which is absolute where argv[0]
/// may be a bare `claude`. When argv[0] isn't Claude itself, a runtime runs
/// Claude's script, which joins the prefix.
fn split_launch(
    executable: &std::ffi::OsStr,
    argv: &[OsString],
    cwd: &Path,
) -> Result<(Vec<OsString>, Vec<String>), ContextError> {
    let program = argv.first().ok_or(ContextError::NoClaudeSession)?;
    let name = program.to_string_lossy();
    let native = crate::detection::identify(&name, &[name.clone().into_owned()])
        == Some(muxy_protocol::AgentProvider::Claude);
    let mut prefix = vec![executable.to_owned()];
    let skip = if native {
        1
    } else {
        let script = argv.get(1).ok_or(ContextError::NoClaudeSession)?;
        prefix.push(cwd.join(script).into_os_string());
        2
    };
    // Arguments are retyped and parsed, so a lossy one would change the command.
    let user_args = argv[skip..]
        .iter()
        .map(|argument| argument.to_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()
        .ok_or(ContextError::NoClaudeSession)?;
    Ok((prefix, user_args))
}

/// Checks the PTY's direct child is alive and, given Claude's group, not in
/// it (`exec claude` replaced the shell). Returns the shell's process group,
/// which must own the foreground before anything is typed.
pub(crate) fn check_shell(shell: u32, claude_group: Option<u32>) -> Result<u32, ContextError> {
    use libproc::bsd_info::BSDInfo;
    use libproc::proc_pid;
    const ZOMBIE: u32 = 5; // SZOMB
    let pid = i32::try_from(shell).map_err(|_| ContextError::NoShell)?;
    let info = proc_pid::pidinfo::<BSDInfo>(pid, 0).map_err(|_| ContextError::NoShell)?;
    if info.pbi_status == ZOMBIE || claude_group == Some(info.pbi_pgid) {
        return Err(ContextError::NoShell);
    }
    Ok(info.pbi_pgid)
}

/// How many processes are in group `group`.
pub(crate) fn group_size(group: u32) -> usize {
    use libproc::processes::{ProcFilter, pids_by_type};
    pids_by_type(ProcFilter::ByProgramGroup { pgrpid: group }).map_or(0, |pids| pids.len())
}

/// How many jobs `shell` (in process group `group`) started at or after
/// `since`: a shell running on after Claude exited, as in `claude -c ||
/// claude`, puts the next command in a group of its own. Older children, such
/// as background jobs, and ones in the shell's group, such as prompt hooks,
/// don't count; one that can't be read does.
pub(crate) fn children_since(shell: u32, group: u32, since: std::time::SystemTime) -> usize {
    use libproc::bsd_info::BSDInfo;
    use libproc::processes::{ProcFilter, pids_by_type};
    use std::time::{Duration, UNIX_EPOCH};
    let Ok(children) = pids_by_type(ProcFilter::ByParentProcess { ppid: shell }) else {
        return 0;
    };
    children
        .into_iter()
        .filter(|pid| {
            let started = i32::try_from(*pid)
                .ok()
                .and_then(|pid| libproc::proc_pid::pidinfo::<BSDInfo>(pid, 0).ok())
                .map(|info| {
                    let start = UNIX_EPOCH
                        + Duration::from_secs(info.pbi_start_tvsec)
                        + Duration::from_micros(info.pbi_start_tvusec);
                    (info.pbi_pgid, start)
                });
            started.is_none_or(|(pgid, start)| pgid != group && start >= since)
        })
        .count()
}

/// Most bytes read looking for a user line; it is normally among the first.
const TRANSCRIPT_SCAN: u64 = 64 << 20;

/// Finds `<config>/projects/*/<id>.jsonl`, a regular file under
/// `<config>/projects` holding at least one `"type":"user"` line, which
/// `--resume` needs. Claude writes it only after the first prompt.
pub(crate) fn resumable_transcript(
    config_dir: &Path,
    session_id: &str,
) -> Result<PathBuf, ContextError> {
    let missing = ContextError::NoClaudeSession;
    if !uuid_shaped(session_id) {
        return Err(missing);
    }
    let projects = config_dir
        .join("projects")
        .canonicalize()
        .map_err(|_| missing)?;
    let name = format!("{session_id}.jsonl");
    std::fs::read_dir(&projects)
        .map_err(|_| missing)?
        .filter_map(|entry| {
            let path = entry.ok()?.path().join(&name);
            path.symlink_metadata().ok()?.is_file().then_some(())?;
            let path = path.canonicalize().ok()?;
            path.starts_with(&projects).then_some(path)
        })
        .find(|path| has_user_line(path))
        .ok_or(missing)
}

// shortcut: a single line is read whole, up to TRANSCRIPT_SCAN bytes; read it
// bounded if transcripts with huge first lines show up.
fn has_user_line(path: &Path) -> bool {
    #[derive(serde::Deserialize)]
    struct Line<'a> {
        #[serde(rename = "type", borrow)]
        kind: Option<std::borrow::Cow<'a, str>>,
    }
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut reader = BufReader::new(file.take(TRANSCRIPT_SCAN));
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return false,
            Ok(_) => {}
        }
        if serde_json::from_slice::<Line<'_>>(&line)
            .is_ok_and(|parsed| parsed.kind.as_deref() == Some("user"))
        {
            return true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    const ID: &str = "bde3d35b-d3c0-49d7-93c1-9a0391781702";

    #[test]
    fn launch_stamp_ignores_prompt_sessions_and_environment_order_but_keeps_provider_changes() {
        let environment = |session: &str| {
            vec![
                (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
                ("CLAUDE_CONFIG_DIR".into(), "/config/claude".into()),
                ("CODEX_HOME".into(), "/config/codex".into()),
                ("POSH_SESSION_ID".into(), session.into()),
            ]
        };
        let first = environment("discovery-shell");
        let mut next = environment("submission-shell");
        let expected = launch_stamp(&[], &first);
        assert_eq!(
            expected,
            launch_stamp(&[], &next),
            "prompt rendering does not change the provider context"
        );
        next.reverse();
        assert_eq!(
            expected,
            launch_stamp(&[], &next),
            "environment order is not significant"
        );
        for name in [
            "PATH",
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "HTTPS_PROXY",
        ] {
            let mut changed = first.clone();
            changed.retain(|(key, _)| key != name);
            changed.push((name.into(), "changed".into()));
            assert_ne!(
                expected,
                launch_stamp(&[], &changed),
                "{name} remains protected"
            );
        }
        next.push(("PATH".into(), "/other".into()));
        assert_ne!(
            expected,
            launch_stamp(&[], &next),
            "the last value wins, as Command::envs does"
        );
    }

    #[test]
    fn transcript_must_be_a_regular_file_with_a_user_line() -> TestResult {
        let config = std::env::temp_dir().join(format!(
            "muxy-chat-context-{}",
            muxy_protocol::OperationId::new()
        ));
        let project = config.join("projects").join("-repo");
        std::fs::create_dir_all(&project)?;
        let outside = config.join("outside.jsonl");
        std::fs::write(&outside, "{\"type\":\"user\"}\n")?;
        let transcript = project.join(format!("{ID}.jsonl"));

        std::os::unix::fs::symlink(&outside, &transcript)?;
        let linked = resumable_transcript(&config, ID);
        std::fs::remove_file(&transcript)?;
        std::fs::write(&transcript, "{\"type\":\"summary\"}\n")?;
        let no_prompt = resumable_transcript(&config, ID);
        std::fs::write(
            &transcript,
            "{\"type\":\"summary\",\"text\":\"\\\"type\\\":\\\"user\\\"\"}\n{\"type\":\"user\",\"message\":{}}\n",
        )?;
        let found = resumable_transcript(&config, ID);
        let traversal = resumable_transcript(&config, "../outside");
        let expected = transcript.canonicalize()?;
        std::fs::remove_dir_all(&config)?;

        assert_eq!(
            linked,
            Err(ContextError::NoClaudeSession),
            "symlink escapes projects"
        );
        assert_eq!(no_prompt, Err(ContextError::NoClaudeSession));
        assert_eq!(found, Ok(expected));
        assert_eq!(traversal, Err(ContextError::NoClaudeSession));
        Ok(())
    }

    #[test]
    fn launch_prefix_keeps_the_runtime_and_script() -> TestResult {
        let strings =
            |values: &[&str]| -> Vec<OsString> { values.iter().map(OsString::from).collect() };
        let cwd = Path::new("/work");
        let native = split_launch(
            "/Users/a/.local/bin/claude".as_ref(),
            &strings(&["claude", "--model", "opus"]),
            cwd,
        )?;
        let script = split_launch(
            "/opt/node/bin/node".as_ref(),
            &strings(&[
                "node",
                "/lib/node_modules/@anthropic-ai/claude-code/cli.js",
                "-c",
            ]),
            cwd,
        )?;
        assert_eq!(
            native,
            (
                strings(&["/Users/a/.local/bin/claude"]),
                vec!["--model".into(), "opus".into()]
            )
        );
        assert_eq!(
            script,
            (
                strings(&[
                    "/opt/node/bin/node",
                    "/lib/node_modules/@anthropic-ai/claude-code/cli.js"
                ]),
                vec!["-c".into()]
            )
        );
        Ok(())
    }

    #[test]
    fn login_environment_comes_after_whatever_startup_files_print() -> TestResult {
        let home = std::env::temp_dir().join(format!(
            "muxy-chat-login-{}",
            muxy_protocol::OperationId::new()
        ));
        std::fs::create_dir_all(&home)?;
        std::fs::write(
            home.join(".profile"),
            "echo 'welcome FAKE=1'\nexport FROM_PROFILE='a b'\n",
        )?;
        let base = vec![
            ("HOME".into(), home.clone().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("FROM_MUXY".into(), "yes".into()),
        ];
        let env = login_environment(
            Path::new("/bin/sh"),
            base,
            &home,
            std::time::Duration::from_secs(5),
        );
        std::fs::remove_dir_all(&home)?;
        let env = env.ok_or("no environment")?;
        let get = |name: &str| {
            env.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };

        assert_eq!(get("FROM_PROFILE"), Some("a b".into()));
        assert_eq!(get("PWD"), None, "the child's cwd is set, not inherited");
        assert_eq!(get("FROM_MUXY"), Some("yes".into()));
        assert_eq!(get("FAKE"), None, "startup output isn't environment");
        Ok(())
    }

    #[test]
    fn new_conversation_ids_are_uuids() {
        let id = new_uuid().unwrap_or_default();
        assert!(uuid_shaped(&id), "{id}");
        assert_eq!(id.as_bytes()[14], b'4');
        assert_ne!(new_uuid(), Some(id));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn exec_claude_has_no_shell() -> TestResult {
        let own = std::process::id();
        let group = u32::try_from(rustix::process::getpgrp().as_raw_nonzero().get())?;
        assert_eq!(check_shell(own, Some(group)), Err(ContextError::NoShell));
        assert_eq!(check_shell(own, None), Ok(group));
        Ok(())
    }
}
