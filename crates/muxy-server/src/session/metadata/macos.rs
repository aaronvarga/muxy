use std::path::PathBuf;

use libproc::bsd_info::BSDInfo;
use libproc::file_info::{ListFDs, PIDFDInfo, PIDFDInfoFlavor, ProcFDType, pidfdinfo};
use libproc::net_info::VInfoStat;
use libproc::proc_pid::{self, PIDInfo, PidInfoFlavor};
use libproc::processes::{self, ProcFilter};
use muxy_protocol::ServerPath;

pub(super) fn foreground_member(group: u32) -> Option<(i32, String)> {
    let named = |pid| {
        let pid = i32::try_from(pid).ok()?;
        proc_pid::name(pid).ok().map(|name| (pid, name))
    };
    if let Some(leader) = named(group) {
        return Some(leader);
    }
    let mut members = processes::pids_by_type(ProcFilter::ByProgramGroup { pgrpid: group }).ok()?;
    members.sort_unstable();
    members.into_iter().find_map(named)
}

pub(in crate::session) fn process_directory(pid: i32) -> Option<ServerPath> {
    let info = proc_pid::pidinfo::<VnodePathInfo>(pid, 0).ok()?;
    let path = &info.current.path;
    let length = path.iter().position(|byte| *byte == 0)?;
    (path.first() == Some(&b'/')).then(|| ServerPath(path[..length].to_vec()))
}

#[repr(C)]
struct VnodeInfo {
    stat: VInfoStat,
    kind: i32,
    padding: i32,
    filesystem: [i32; 2],
}

#[repr(C)]
struct VnodePath {
    info: VnodeInfo,
    path: [u8; 1024],
}

#[repr(C)]
struct VnodePathInfo {
    current: VnodePath,
    root: VnodePath,
}

impl PIDInfo for VnodePathInfo {
    fn flavor() -> PidInfoFlavor {
        PidInfoFlavor::VNodePathInfo
    }
}

/// Darwin's `vnode_fdinfowithpath`: a `proc_fileinfo` followed by the vnode and its path.
#[repr(C)]
struct VnodeFdPathInfo {
    file: [u64; 3],
    vnode: VnodePath,
}

impl Default for VnodeFdPathInfo {
    fn default() -> Self {
        Self {
            file: [0; 3],
            vnode: VnodePath {
                info: VnodeInfo {
                    stat: VInfoStat::default(),
                    kind: 0,
                    padding: 0,
                    filesystem: [0; 2],
                },
                path: [0; 1024],
            },
        }
    }
}

impl PIDFDInfo for VnodeFdPathInfo {
    fn flavor() -> PIDFDInfoFlavor {
        PIDFDInfoFlavor::VNodePathInfo
    }
}

/// The foreground group's processes, by pid, with their names.
pub(in crate::session) fn group_members(group: u32) -> Vec<(i32, String)> {
    let mut pids =
        processes::pids_by_type(ProcFilter::ByProgramGroup { pgrpid: group }).unwrap_or_default();
    pids.sort_unstable();
    pids.into_iter()
        .take(128)
        .filter_map(|pid| {
            let pid = i32::try_from(pid).ok()?;
            Some((pid, proc_pid::name(pid).ok()?))
        })
        .collect()
}

/// Paths of the regular files `pid` has open.
pub(super) fn open_files(pid: i32) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let Ok(info) = proc_pid::pidinfo::<BSDInfo>(pid, 0) else {
        return Vec::new();
    };
    let Ok(fds) = proc_pid::listpidinfo::<ListFDs>(pid, info.pbi_nfiles as usize) else {
        return Vec::new();
    };
    fds.into_iter()
        .filter(|fd| matches!(ProcFDType::from(fd.proc_fdtype), ProcFDType::VNode))
        .filter_map(|fd| {
            let info = pidfdinfo::<VnodeFdPathInfo>(pid, fd.proc_fd).ok()?;
            let path = &info.vnode.path;
            let length = path.iter().position(|byte| *byte == 0)?;
            Some(PathBuf::from(std::ffi::OsStr::from_bytes(&path[..length])))
        })
        .collect()
}

pub(super) fn agent_provider(group: u32) -> Option<muxy_protocol::AgentProvider> {
    group_members(group).into_iter().find_map(|(pid, name)| {
        let leader = u32::try_from(pid).is_ok_and(|pid| pid == group);
        crate::detection::identify_member(&name, &process_arguments(pid), leader)
    })
}

/// What a process was started with: its executable, arguments and environment.
pub(in crate::session) struct ProcessLaunch {
    pub(in crate::session) executable: std::ffi::OsString,
    pub(in crate::session) argv: Vec<std::ffi::OsString>,
    pub(in crate::session) env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
}

impl std::fmt::Debug for ProcessLaunch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessLaunch")
            .field("executable", &self.executable)
            .field("argv", &self.argv)
            .field(
                "env",
                &self.env.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// The raw `KERN_PROCARGS2` record: argc, the executable path, padding, then
/// argv, the environment, and the kernel's own strings, all NUL-terminated.
#[allow(
    unsafe_code,
    reason = "Darwin exposes process arguments only through sysctl"
)]
fn procargs(pid: i32) -> Option<Vec<u8>> {
    unsafe extern "C" {
        fn sysctl(
            name: *mut std::ffi::c_int,
            namelen: u32,
            oldp: *mut std::ffi::c_void,
            oldlenp: *mut usize,
            newp: *mut std::ffi::c_void,
            newlen: usize,
        ) -> std::ffi::c_int;
    }
    let mut mib = [1, 49, pid]; // CTL_KERN, KERN_PROCARGS2
    let mut length = 0;
    // SAFETY: a null buffer asks only for the record's size, written to `length`.
    if unsafe {
        sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &raw mut length,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    let mut bytes = vec![0u8; length];
    // SAFETY: all pointers reference initialized, writable buffers of the supplied lengths.
    if unsafe {
        sysctl(
            mib.as_mut_ptr(),
            3,
            bytes.as_mut_ptr().cast(),
            &raw mut length,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    bytes.truncate(length);
    Some(bytes)
}

/// The executable, argv and environment entries of a `KERN_PROCARGS2` record.
type Procargs<'a> = (&'a [u8], Vec<&'a [u8]>, Vec<&'a [u8]>);

/// Splits a `KERN_PROCARGS2` record into the executable, argv and environment.
fn parse_procargs(bytes: &[u8]) -> Option<Procargs<'_>> {
    let count = usize::try_from(i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?)).ok()?;
    let rest = bytes.get(4..)?;
    let executable_end = rest.iter().position(|b| *b == 0)?;
    let strings = &rest[executable_end..];
    let start = strings.iter().position(|b| *b != 0)?;
    let mut strings = strings[start..].split(|b| *b == 0);
    let argv: Vec<_> = strings.by_ref().take(count).collect();
    // The environment ends at the first empty string; the kernel's own strings follow.
    let env = strings.take_while(|string| !string.is_empty()).collect();
    Some((&rest[..executable_end], argv, env))
}

pub(in crate::session) fn process_arguments(pid: i32) -> Vec<String> {
    procargs(pid)
        .as_deref()
        .and_then(parse_procargs)
        .map(|(_, argv, _)| {
            argv.into_iter()
                .take(65)
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The complete launch record of `pid`, or `None` if any part is unreadable.
/// macOS hides the environment of platform binaries, so it may be empty.
pub(in crate::session) fn process_launch(pid: i32) -> Option<ProcessLaunch> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let bytes = procargs(pid)?;
    let (executable, argv, env) = parse_procargs(&bytes)?;
    let count = usize::try_from(i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?)).ok()?;
    (argv.len() == count && !executable.is_empty()).then_some(())?;
    let owned = |bytes: &[u8]| OsStr::from_bytes(bytes).to_owned();
    Some(ProcessLaunch {
        executable: owned(executable),
        argv: argv.into_iter().map(owned).collect(),
        env: env
            .into_iter()
            .filter_map(|entry| {
                let split = entry.iter().position(|b| *b == b'=')?;
                Some((owned(&entry[..split]), owned(&entry[split + 1..])))
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vnode_layout_matches_the_darwin_abi() {
        assert_eq!(size_of::<VnodePathInfo>(), 2352);
        assert_eq!(std::mem::offset_of!(VnodePath, path), 152);
        assert_eq!(size_of::<VnodeFdPathInfo>(), 1200);
    }

    #[test]
    fn open_files_lists_this_process_files() -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!(
            "muxy-open-files-{}",
            muxy_protocol::OperationId::new()
        ));
        let file = std::fs::File::create(&path)?;
        let listed = open_files(i32::try_from(std::process::id())?);
        let expected = path.canonicalize()?;
        drop(file);
        std::fs::remove_file(&path)?;
        assert!(listed.contains(&expected), "{listed:?}");
        Ok(())
    }

    fn agent_of_group_running_claude(
        label: &str,
        args: &[String],
    ) -> Option<muxy_protocol::AgentProvider> {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::process::CommandExt;

        let directory =
            std::env::temp_dir().join(format!("muxy-group-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("directory");
        let claude = directory.join("claude");
        std::fs::write(&claude, "#!/bin/sh\nsleep 30\n").expect("script");
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755))
            .expect("permissions");
        let mut leader = std::process::Command::new("/bin/sh")
            .args(["-c", "\"$0\" \"$@\"; :"])
            .arg(&claude)
            .args(args)
            .process_group(0)
            .spawn()
            .expect("spawn");
        let group = leader.id();
        let started = (0..500).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            processes::pids_by_type(ProcFilter::ByProgramGroup { pgrpid: group })
                .is_ok_and(|members| members.len() >= 3)
        });
        let agent = agent_provider(group);
        let _ = std::process::Command::new("/bin/kill")
            .args(["-9", "--", &format!("-{group}")])
            .status();
        let _ = leader.wait();
        let _ = std::fs::remove_dir_all(&directory);
        assert!(started, "the group's members never started");
        agent
    }

    #[test]
    fn a_one_shot_agent_a_program_started_is_not_the_terminals_agent() {
        assert_eq!(
            agent_of_group_running_claude("session", &[]),
            Some(muxy_protocol::AgentProvider::Claude)
        );
        assert_eq!(
            agent_of_group_running_claude("one-shot", &["-p".into(), "prompt".into()]),
            None
        );
        assert_eq!(
            agent_of_group_running_claude("long-prompt", &["-p".into(), "x".repeat(100 * 1024)]),
            None
        );
    }

    #[test]
    fn process_launch_reads_this_process_environment() -> Result<(), Box<dyn std::error::Error>> {
        let launch = process_launch(i32::try_from(std::process::id())?).ok_or("unreadable")?;
        let current = std::env::current_exe()?;
        assert_eq!(
            std::path::Path::new(&launch.executable).canonicalize()?,
            current.canonicalize()?
        );
        assert_eq!(launch.argv.len(), std::env::args_os().len());
        for name in ["PATH", "HOME", "CARGO_PKG_NAME"] {
            let value = launch
                .env
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value);
            assert_eq!(value, std::env::var_os(name).as_ref(), "{name}");
        }
        Ok(())
    }
}
