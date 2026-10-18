use super::*;

use muxy_terminal::pty::{Pty, PtyEvent, PtySize};
use std::error::Error;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct Shell {
    pty: Pty,
    events: Receiver<PtyEvent>,
    directory: PathBuf,
    terminal: muxy_terminal::Terminal,
}

impl Drop for Shell {
    fn drop(&mut self) {
        let _ = self.pty.kill();
        let _ = self.pty.wait();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

impl Shell {
    fn start(enabled: bool) -> TestResult<Self> {
        let directory = std::env::temp_dir().join(format!(
            "muxy-powershell-{}",
            muxy_protocol::OperationId::new()
        ));
        fs::create_dir(&directory)?;
        let hooks = ShellIntegration::install(&directory.join("hooks"))?;
        let system = std::env::var_os("SystemRoot").ok_or("SystemRoot is unset")?;
        let mut request = SpawnRequest {
            program: Path::new(&system).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
            args: vec!["-NoLogo".into()],
            cwd: directory.clone(),
            env: Vec::new(),
            size: PtySize {
                cols: 120,
                rows: 24,
            },
        };
        hooks.configure(&mut request, enabled);
        let pty = Pty::spawn(request)?;
        let (sender, events) = mpsc::channel();
        pty.start_reader(sender)?;
        Ok(Self {
            pty,
            events,
            directory,
            terminal: muxy_terminal::Terminal::new(
                muxy_terminal::Size {
                    cols: 120,
                    rows: 24,
                },
                1024 * 1024,
            )?,
        })
    }

    /// Output up to `expected`, answering the pseudoconsole's queries as the
    /// server does.
    fn until(&mut self, expected: &[u8]) -> TestResult<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut bytes = Vec::new();
        while !bytes
            .windows(expected.len())
            .any(|window| window == expected)
        {
            match self
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(PtyEvent::Output(output)) => {
                    self.terminal.feed(&output);
                    let reply = self.terminal.take_pty_output();
                    if !reply.is_empty() {
                        self.pty.write(&reply)?;
                    }
                    bytes.extend(output);
                }
                other => {
                    return Err(format!(
                        "waiting for {expected:?}: {other:?}; output={:?}",
                        String::from_utf8_lossy(&bytes)
                    )
                    .into());
                }
            }
        }
        Ok(bytes)
    }
}

fn contains(bytes: &[u8], expected: &[u8]) -> bool {
    bytes
        .windows(expected.len())
        .any(|window| window == expected)
}

#[test]
fn powershell_marks_prompts_commands_failures_and_drive_folders() -> TestResult {
    let mut shell = Shell::start(true)?;
    shell.until(b"\x1b]133;B\x07")?;
    shell.pty.write(b"Set-Location C:\\Windows\r")?;
    let moved = shell.until(b"\x1b]133;B\x07")?;
    assert!(contains(&moved, b"\x1b]133;C\x07"), "{moved:?}");
    assert!(contains(&moved, b"\x1b]133;D;0\x07"), "{moved:?}");
    assert!(
        contains(&moved, b"\x1b]7;file:///C:/Windows\x07"),
        "{:?}",
        String::from_utf8_lossy(&moved)
    );
    shell.pty.write(b"cmd /c exit 3\r")?;
    let failed = shell.until(b"\x1b]133;B\x07")?;
    assert!(contains(&failed, b"\x1b]133;D;3\x07"), "{failed:?}");
    shell.pty.write(b"Set-Location HKCU:\\\r")?;
    let registry = shell.until(b"\x1b]133;B\x07")?;
    assert!(
        !contains(&registry, b"\x1b]7;"),
        "only file-system folders are reported"
    );
    assert!(shell.terminal.screen_prompts()?.len() >= 3);
    Ok(())
}

#[test]
fn disabled_integration_starts_powershell_without_marks() -> TestResult {
    let mut shell = Shell::start(false)?;
    let output = shell.until(b">")?;
    shell.pty.write(b"'muxy-' + 'plain'\r")?;
    let output = [output, shell.until(b"muxy-plain")?].concat();
    assert!(!contains(&output, b"\x1b]133;"));
    Ok(())
}
