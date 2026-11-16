//! Captured launch and process identities. Error messages never contain environment values.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use libproc::{bsd_info::BSDInfo, proc_pid};
use muxy_protocol::{AgentProvider, ChatTarget};

use super::super::{context, supervisor::Launch};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Identity {
    pub(crate) pid: i32,
    pub(crate) started: (u64, u64),
    pub(crate) executable: PathBuf,
    pub(crate) group: u32,
    file: (u64, u64, u64, i64, i64),
}

impl Identity {
    pub(crate) fn read(pid: i32) -> Result<Self, &'static str> {
        let info =
            proc_pid::pidinfo::<BSDInfo>(pid, 0).map_err(|_| "Cannot verify the Codex process")?;
        if info.pbi_uid != rustix::process::getuid().as_raw() {
            return Err("Codex runs under another user");
        }
        let executable = PathBuf::from(
            proc_pid::pidpath(pid).map_err(|_| "Cannot resolve the Codex executable")?,
        )
        .canonicalize()
        .map_err(|_| "The Codex executable is unavailable")?;
        let metadata = executable
            .metadata()
            .map_err(|_| "Cannot verify the Codex executable file")?;
        Ok(Self {
            pid,
            started: (info.pbi_start_tvsec, info.pbi_start_tvusec),
            executable,
            group: info.pbi_pgid,
            file: (
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.mtime(),
                metadata.mtime_nsec(),
            ),
        })
    }
    pub(crate) fn revalidate(&self) -> Result<(), &'static str> {
        if Self::read(self.pid).as_ref() != Ok(self) {
            return Err("The Codex process changed; retry from Console");
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct Context {
    pub(crate) shell: u32,
    pub(crate) group: u32,
    pub(crate) cwd: PathBuf,
    pub(crate) home: PathBuf,
    pub(crate) executable: PathBuf,
    pub(crate) env: Vec<(OsString, OsString)>,
    pub(crate) terminal: Option<Identity>,
    pub(crate) endpoint: Option<PathBuf>,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("CodexContext")
            .field("shell", &self.shell)
            .field("cwd", &self.cwd)
            .field("home", &self.home)
            .field("executable", &self.executable)
            .field("terminal", &self.terminal)
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl Context {
    pub(crate) fn launch_stamp(&self) -> u64 {
        context::launch_stamp(
            &[
                &self.executable,
                &self.cwd,
                &self.home.join("config.toml"),
                &self.home.join("auth.json"),
                &self.cwd.join(".codex/config.toml"),
                self.endpoint.as_deref().unwrap_or(Path::new("")),
            ],
            &self.env,
        )
    }
    pub(crate) fn standalone_recovered(
        &self,
        foreground: Option<u32>,
        thread: &str,
        old: &Identity,
    ) -> bool {
        use crate::session::metadata::macos::{group_members, process_directory, process_launch};
        let Some(group) = foreground.filter(|group| *group != self.shell) else {
            return false;
        };
        group_members(group).into_iter().any(|(pid, _)| {
            let Ok(identity) = Identity::read(pid) else {
                return false;
            };
            if identity.executable != old.executable || identity.file != old.file {
                return false;
            }
            let Some(launch) = process_launch(pid) else {
                return false;
            };
            let arguments: Vec<_> = launch.argv.iter().filter_map(|arg| arg.to_str()).collect();
            if arguments.len() != 4 || arguments[1..] != ["--no-daemon", "resume", thread] {
                return false;
            }
            let Some(directory) = process_directory(pid) else {
                return false;
            };
            let directory = PathBuf::from(OsString::from_vec(directory.0));
            home(&launch.env, &directory).ok().as_ref() == Some(&self.home)
        })
    }
    pub(crate) fn for_target(mut self, target: &ChatTarget) -> Result<Self, &'static str> {
        let home = PathBuf::from(OsString::from_vec(target.home.0.clone()))
            .canonicalize()
            .map_err(|_| "The selected CODEX_HOME is unavailable")?;
        let endpoint = target
            .endpoint
            .as_ref()
            .ok_or("This stored standalone thread has no verified engine; keep it in Console")?;
        let endpoint = PathBuf::from(OsString::from_vec(endpoint.0.clone()))
            .canonicalize()
            .map_err(|_| "The selected Codex engine is unavailable")?;
        let executable = PathBuf::from(OsString::from_vec(target.executable.0.clone()))
            .canonicalize()
            .map_err(|_| "The captured Codex executable is unavailable")?;
        self.env.retain(|(name, _)| name != "CODEX_HOME");
        self.env
            .push(("CODEX_HOME".into(), home.as_os_str().to_owned()));
        self.home = home;
        self.endpoint = Some(endpoint);
        self.executable = executable;
        Ok(self)
    }
    pub(crate) fn verify_engine(&self, engine: &Identity) -> Result<(), &'static str> {
        engine.revalidate()?;
        if let Some(terminal) = &self.terminal {
            terminal.revalidate()?;
            if engine.group == self.group || !connected_to(terminal.pid, engine.pid)? {
                return Err(
                    "Cannot bind this terminal to the selected Codex engine; keep it in Console",
                );
            }
        }
        Ok(())
    }
    pub(crate) fn shell(
        shell: u32,
        budget: Duration,
        base: &[(OsString, OsString)],
    ) -> Result<Self, &'static str> {
        let captured = context::capture_shell(shell, budget, base)
            .map_err(|_| "This terminal must be at an idle shell prompt")?;
        let executable = context::on_path("codex", &captured.env)
            .ok_or("Codex is not on your login shell's PATH")?
            .canonicalize()
            .map_err(|_| "Cannot resolve the Codex executable")?;
        let home = home(&captured.env, &captured.cwd)?;
        check_backend_overrides(&captured.env)?;
        let endpoint = home.join("app-server-control/app-server-control.sock");
        Ok(Self {
            shell,
            group: captured.group,
            cwd: captured.cwd,
            home,
            executable,
            env: captured.env,
            terminal: None,
            endpoint: endpoint.exists().then_some(endpoint),
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Native launch argument and socket verification are one preflight"
    )]
    pub(crate) fn terminal(
        shell: u32,
        foreground: Option<u32>,
        target: Option<&ChatTarget>,
    ) -> Result<Self, &'static str> {
        use crate::session::metadata::macos::{group_members, process_directory, process_launch};
        use std::os::unix::ffi::OsStrExt;
        let group = foreground.ok_or("No Codex terminal is active")?;
        let (pid, launch) = group_members(group)
            .into_iter()
            .find_map(|(pid, name)| {
                let launch = process_launch(pid)?;
                let argv = launch
                    .argv
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                (crate::detection::identify(&name, &argv) == Some(AgentProvider::Codex))
                    .then_some((pid, launch))
            })
            .ok_or("No Codex terminal is active")?;
        if launch.env.is_empty() {
            return Err("Cannot verify this Codex launch environment");
        }
        check_backend_overrides(&launch.env)?;
        context::check_shell(shell, Some(group))
            .map_err(|_| "Codex must be running under an interactive shell")?;
        let cwd = PathBuf::from(OsString::from_vec(
            process_directory(pid)
                .ok_or("Cannot read Codex's working directory")?
                .0,
        ));
        let home = home(&launch.env, &cwd)?;
        if target.is_some_and(|target| target.home.0 != home.as_os_str().as_bytes()) {
            return Err("The selected thread belongs to another CODEX_HOME");
        }
        let mut endpoint = home.join("app-server-control/app-server-control.sock");
        let arguments = launch
            .argv
            .iter()
            .skip(1)
            .map(|arg| arg.to_str().ok_or("Unsupported Codex arguments"))
            .collect::<Result<Vec<_>, _>>()?;
        let mut at = 0;
        while at < arguments.len() {
            let argument = arguments[at];
            match argument {
                "--remote" => {
                    at += 1;
                    let remote = *arguments
                        .get(at)
                        .ok_or("Codex remote endpoint is missing")?;
                    let path = remote.strip_prefix("unix://").ok_or("This Codex endpoint needs Console; only verified local Unix engines are supported")?;
                    if !path.is_empty() {
                        endpoint = PathBuf::from(path);
                    }
                }
                "resume" => {
                    // argv is not proof of the current thread: /new can change it. The user
                    // selects an exact target from this verified engine before takeover.
                    if arguments.get(at + 1).is_some_and(|id| !id.starts_with('-')) {
                        at += 1;
                    }
                }
                "--no-alt-screen" => {}
                "--no-daemon" => {
                    return Err(
                        "This standalone Codex TUI has no verified API endpoint; keep it in Console",
                    );
                }
                _ => {
                    return Err(
                        "This Codex profile or launch override cannot be preserved safely; keep it in Console",
                    );
                }
            }
            at += 1;
        }
        let endpoint = endpoint
            .canonicalize()
            .map_err(|_| "The Codex engine endpoint is unavailable")?;
        if let Some(target) = target {
            let selected = target
                .endpoint
                .as_ref()
                .ok_or("A running daemon thread requires its verified endpoint")?;
            if selected.0 != endpoint.as_os_str().as_bytes() {
                return Err("The selected thread belongs to another Codex engine");
            }
        }
        let terminal = Identity::read(pid)?;
        Ok(Self {
            shell,
            group,
            cwd,
            home,
            executable: terminal.executable.clone(),
            env: launch.env,
            terminal: Some(terminal),
            endpoint: Some(endpoint),
        })
    }

    pub(crate) fn launch(&self) -> Launch {
        Launch {
            prefix: vec![self.executable.as_os_str().to_owned()],
            args: vec!["app-server".into(), "--stdio".into()],
            env: self.env.clone(),
            cwd: self.cwd.clone(),
        }
    }

    pub(crate) fn recovery(
        &self,
        thread: &str,
        endpoint: Option<&Path>,
    ) -> Result<String, &'static str> {
        use super::super::recovery::quote;
        if !crate::session::metadata::agent_session::uuid_shaped(thread) {
            return Err("Invalid native Codex thread ID");
        }
        let text = |path: &Path| {
            path.to_str()
                .filter(|text| !text.chars().any(char::is_control))
                .map(quote)
                .ok_or("This Codex launch path cannot be typed safely")
        };
        let remote = match endpoint {
            Some(endpoint) => format!(
                "--remote {}",
                text(&PathBuf::from(format!(
                    "unix://{}",
                    endpoint.to_str().ok_or("Invalid engine endpoint")?
                )))?
            ),
            None => "--no-daemon".to_owned(),
        };
        Ok(format!(
            "cd -- {} && env CODEX_HOME={} {} {} resume {}",
            text(&self.cwd)?,
            text(&self.home)?,
            text(&self.executable)?,
            remote,
            quote(thread)
        ))
    }
}

fn check_backend_overrides(env: &[(OsString, OsString)]) -> Result<(), &'static str> {
    if env.iter().any(|(name, value)| {
        !value.is_empty() && matches!(name.to_str(), Some("OPENAI_BASE_URL" | "OPENAI_API_BASE"))
    }) {
        return Err("This Codex context overrides the API backend; use Console");
    }
    Ok(())
}

fn connected_to(terminal: i32, engine: i32) -> Result<bool, &'static str> {
    let terminal = socket_edges(terminal)?;
    let engine = socket_edges(engine)?;
    Ok(terminal.iter().any(|(local, remote)| {
        *remote != 0
            && engine
                .iter()
                .any(|(peer, back)| peer == remote && back == local)
    }))
}

#[allow(
    unsafe_code,
    reason = "libproc tags the Unix socket union with soi_kind before it is read"
)]
fn socket_edges(pid: i32) -> Result<Vec<(u64, u64)>, &'static str> {
    use libproc::file_info::{ListFDs, ProcFDType, pidfdinfo};
    use libproc::net_info::{SocketFDInfo, SocketInfoKind};
    let info =
        proc_pid::pidinfo::<BSDInfo>(pid, 0).map_err(|_| "Cannot inspect the Codex connection")?;
    if info.pbi_nfiles > 4096 {
        return Err("Too many descriptors to verify the Codex connection safely");
    }
    let files = proc_pid::listpidinfo::<ListFDs>(pid, info.pbi_nfiles as usize)
        .map_err(|_| "Cannot inspect the Codex connection")?;
    Ok(files
        .into_iter()
        .filter(|file| matches!(ProcFDType::from(file.proc_fdtype), ProcFDType::Socket))
        .filter_map(|file| {
            let socket = pidfdinfo::<SocketFDInfo>(pid, file.proc_fd).ok()?.psi;
            if !matches!(SocketInfoKind::from(socket.soi_kind), SocketInfoKind::Un) {
                return None;
            }
            // SAFETY: SocketInfoKind::Un selects pri_un in the kernel-provided union.
            let peer = unsafe { socket.soi_proto.pri_un.unsi_conn_so };
            Some((socket.soi_so, peer))
        })
        .collect())
}

fn home(env: &[(OsString, OsString)], cwd: &Path) -> Result<PathBuf, &'static str> {
    let var = |key| {
        env.iter()
            .find(|(name, value)| name == key && !value.is_empty())
            .map(|(_, value)| PathBuf::from(value))
    };
    let home = var("CODEX_HOME")
        .or_else(|| var("HOME").map(|home| home.join(".codex")))
        .ok_or("Cannot resolve CODEX_HOME")?;
    (if home.is_absolute() {
        home
    } else {
        cwd.join(home)
    })
    .canonicalize()
    .map_err(|_| "CODEX_HOME is unavailable; sign in with Codex in Console first")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_api_keys_are_checked_against_the_engine_account_instead_of_rejected_here() {
        let keys = vec![
            ("OPENAI_API_KEY".into(), "private-test-key".into()),
            ("CODEX_API_KEY".into(), "private-test-key".into()),
        ];
        assert!(check_backend_overrides(&keys).is_ok());
        for name in ["OPENAI_BASE_URL", "OPENAI_API_BASE"] {
            let error = check_backend_overrides(&[(name.into(), "private-test-endpoint".into())])
                .unwrap_err();
            assert!(!error.contains("private-test-endpoint"));
        }
    }

    #[test]
    fn darwin_socket_edges_verify_both_ends_of_a_real_connection() {
        let (left, right) = std::os::unix::net::UnixStream::pair().unwrap();
        let pid = i32::try_from(std::process::id()).unwrap();
        assert!(connected_to(pid, pid).unwrap());
        let edges = socket_edges(pid).unwrap();
        assert!(edges.iter().any(|(local, remote)| {
            local != remote
                && *remote != 0
                && edges
                    .iter()
                    .any(|(peer, back)| peer == remote && back == local)
        }));
        drop((left, right));
    }
}
