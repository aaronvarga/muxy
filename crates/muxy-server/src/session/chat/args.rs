//! Allowlist parser over the Claude TUI's user arguments (runtime and script already stripped).
//!
//! It yields the arguments for the headless child and for the console recovery line, and the
//! permission mode the child runs in. Anything it does not know is refused: the arguments are
//! retyped into a terminal, and may carry credentials or change what Claude persists.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use muxy_protocol::PermissionMode;

const BYPASS_NOTICE: &str = "Chat doesn't skip permissions; approvals will be asked";
const UNKNOWN_SETTINGS_MODE_NOTICE: &str =
    "Chat doesn't know the permission mode in Claude's settings; approvals will be asked";
/// Claude Code's managed settings, the highest settings layer.
const MANAGED_SETTINGS: &str = "/Library/Application Support/ClaudeCode/managed-settings.json";
/// Bigger settings files are skipped, never parsed.
const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;

/// A refused argument. Names the flag only, never a value (inline JSON can hold credentials).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ArgsError {
    pub(crate) flag: String,
    pub(crate) reason: &'static str,
}

impl fmt::Display for ArgsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Chat can't use `{}`: {}", self.flag, self.reason)
    }
}

/// Which picker-chosen settings replace the ones the TUI was started with.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Overrides {
    pub(crate) model: bool,
    pub(crate) effort: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedArgs {
    /// Kept flags plus `--permission-mode <m>`; the supervisor adds `headless_flags`.
    pub(crate) headless: Vec<String>,
    /// Kept flags plus the permission flags as the user typed them; for the recovery line.
    /// Keeps `--model`/`--effort` even when the picker overrides them for the headless child.
    pub(crate) console: Vec<String>,
    pub(crate) permission_mode: PermissionMode,
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<String>,
    pub(crate) notices: Vec<String>,
}

/// The fixed flags every headless child gets. `--include-partial-messages`,
/// `--replay-user-messages` and `--thinking-display` are needed for the stream the host reads.
/// `fresh`: start conversation `uuid` instead of resuming it.
pub(crate) fn headless_flags(uuid: &str, fresh: bool) -> Vec<String> {
    [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--replay-user-messages",
        "--thinking-display",
        "summarized",
        "--permission-prompt-tool",
        "stdio",
        if fresh { "--session-id" } else { "--resume" },
        uuid,
    ]
    .map(str::to_owned)
    .to_vec()
}

enum Kind {
    Bool,
    One,
    Variadic,
    /// Needs every value to be an existing regular file.
    VariadicFile,
    OneFile,
}

fn kept(name: &str) -> Option<Kind> {
    Some(match name {
        "--strict-mcp-config" | "--verbose" | "--enable-auto-mode" => Kind::Bool,
        "--model" | "--effort" | "--setting-sources" | "--agent" => Kind::One,
        "--add-dir" | "--allowedTools" | "--allowed-tools" | "--disallowedTools"
        | "--disallowed-tools" | "--plugin-dir" => Kind::Variadic,
        "--mcp-config" => Kind::VariadicFile,
        "--settings" => Kind::OneFile,
        _ => return None,
    })
}

/// The settings files the TUI read `permissions.defaultMode` from, highest precedence first.
// shortcut: ignores `--settings <file>` (a layer above local) and `--setting-sources` (drops
// layers), both allowlisted; read them here if a TUI started with them shows a different mode.
pub(crate) fn settings_layers(cwd: &Path, config_dir: &Path) -> [PathBuf; 4] {
    [
        PathBuf::from(MANAGED_SETTINGS),
        cwd.join(".claude/settings.local.json"),
        cwd.join(".claude/settings.json"),
        config_dir.join("settings.json"),
    ]
}

/// `permissions.defaultMode` of the first layer that sets it. Missing, unreadable, oversized
/// or invalid files are skipped, as Claude skips them.
pub(crate) fn settings_default_mode(layers: &[PathBuf]) -> Option<String> {
    layers.iter().find_map(|path| {
        let file = std::fs::File::open(path).ok()?;
        let mut bytes = Vec::new();
        file.take(MAX_SETTINGS_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return None;
        }
        let settings: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        settings
            .pointer("/permissions/defaultMode")?
            .as_str()
            .map(str::to_owned)
    })
}

/// `settings_mode` is the TUI's `permissions.defaultMode`, read only when argv sets no mode.
pub(crate) fn parse(
    args: &[String],
    cwd: &Path,
    overrides: Overrides,
    settings_mode: impl FnOnce() -> Option<String>,
) -> Result<ParsedArgs, ArgsError> {
    let refuse = |flag: &str, reason| ArgsError {
        flag: flag.to_owned(),
        reason,
    };
    // Control characters could act as keystrokes when retyped into a terminal.
    if args.iter().any(|a| a.chars().any(char::is_control)) {
        return Err(refuse("(argument)", "contains a control character"));
    }
    let mut out = ParsedArgs {
        headless: Vec::new(),
        console: Vec::new(),
        permission_mode: PermissionMode::Default,
        model: None,
        effort: None,
        notices: Vec::new(),
    };
    let (mut mode, mut skip, mut allow_skip, mut auto) = (None::<String>, false, false, false);
    let mut console_only: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if arg == "--" {
            break; // the rest is prompt text
        }
        if !arg.starts_with('-') || arg == "-" {
            continue; // prompt text
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if arg.starts_with("--") => (n, Some(v.to_owned())),
            _ => (arg.as_str(), None),
        };
        match name {
            // Session selectors, print and format flags: the host chooses these itself.
            "--continue"
            | "-c"
            | "--fork-session"
            | "-p"
            | "--print"
            | "--include-partial-messages"
            | "--replay-user-messages"
                if inline.is_none() => {}
            "--session-id" | "--output-format" | "--input-format" => {
                take(args, &mut i, inline.as_deref(), Some(1));
            }
            "--resume" | "-r" => {
                // The value is optional; a following word is taken as the session, as Claude does.
                if inline.is_some() || args.get(i).is_some_and(|a| !a.starts_with('-')) {
                    take(args, &mut i, inline.as_deref(), Some(1));
                }
            }
            "--dangerously-skip-permissions" if inline.is_none() => {
                skip = true;
                console_only.push(arg.clone());
            }
            "--allow-dangerously-skip-permissions" if inline.is_none() => {
                allow_skip = true;
                console_only.push(arg.clone());
            }
            "--permission-mode" => {
                let value = take(args, &mut i, inline.as_deref(), Some(1));
                let [value] = value.as_slice() else {
                    return Err(refuse(name, "needs a value"));
                };
                mode = Some(value.clone());
                console_only.extend(typed(arg, inline.is_some(), std::slice::from_ref(value)));
            }
            _ => {
                let values = kept_values(name, inline.as_deref(), args, &mut i, cwd)?;
                match name {
                    "--enable-auto-mode" => auto = true,
                    "--model" => out.model = values.first().cloned(),
                    "--effort" => out.effort = values.first().cloned(),
                    _ => {}
                }
                let tokens = typed(arg, inline.is_some(), &values);
                let overridden = (name == "--model" && overrides.model)
                    || (name == "--effort" && overrides.effort);
                if !overridden {
                    out.headless.extend(tokens.iter().cloned());
                }
                out.console.extend(tokens);
            }
        }
    }
    out.permission_mode = resolve_mode(
        skip,
        allow_skip,
        mode.as_deref(),
        auto,
        settings_mode,
        &mut out.notices,
    )?;
    out.headless.extend([
        "--permission-mode".to_owned(),
        mode_arg(out.permission_mode).to_owned(),
    ]);
    out.console.extend(console_only);
    Ok(out)
}

/// The values of an allowlisted flag, checked.
fn kept_values(
    name: &str,
    inline: Option<&str>,
    args: &[String],
    i: &mut usize,
    cwd: &Path,
) -> Result<Vec<String>, ArgsError> {
    let refuse = |reason| ArgsError {
        flag: name.to_owned(),
        reason,
    };
    let Some(kind) = kept(name) else {
        return Err(refuse("isn't supported in Chat"));
    };
    let values = match kind {
        Kind::Bool if inline.is_some() => return Err(refuse("takes no value")),
        Kind::Bool => return Ok(Vec::new()),
        Kind::One | Kind::OneFile => take(args, i, inline, Some(1)),
        Kind::Variadic | Kind::VariadicFile => take(args, i, inline, None),
    };
    if values.is_empty() {
        return Err(refuse("needs a value"));
    }
    if matches!(kind, Kind::OneFile | Kind::VariadicFile)
        && !values.iter().all(|v| cwd.join(v).is_file())
    {
        return Err(refuse(
            "only a path to an existing file is supported, not inline content",
        ));
    }
    Ok(values)
}

fn resolve_mode(
    skip: bool,
    allow_skip: bool,
    mode: Option<&str>,
    auto: bool,
    settings_mode: impl FnOnce() -> Option<String>,
    notices: &mut Vec<String>,
) -> Result<PermissionMode, ArgsError> {
    let mut bypass = skip || allow_skip;
    let resolved = match mode {
        _ if skip => PermissionMode::Default,
        Some(mode) => {
            bypass |= mode == "bypassPermissions";
            known_mode(mode).ok_or_else(|| ArgsError {
                flag: "--permission-mode".to_owned(),
                reason: "unknown mode",
            })?
        }
        None if auto => PermissionMode::Auto,
        // The mode the TUI started in from its settings.
        None => settings_mode().map_or(PermissionMode::Default, |mode| {
            bypass |= mode == "bypassPermissions";
            known_mode(&mode).unwrap_or_else(|| {
                notices.push(UNKNOWN_SETTINGS_MODE_NOTICE.to_owned());
                PermissionMode::Default
            })
        }),
    };
    if bypass {
        notices.push(BYPASS_NOTICE.to_owned());
    }
    Ok(resolved)
}

fn known_mode(mode: &str) -> Option<PermissionMode> {
    Some(match mode {
        "bypassPermissions" | "default" => PermissionMode::Default,
        "acceptEdits" => PermissionMode::AcceptEdits,
        "plan" => PermissionMode::Plan,
        "auto" => PermissionMode::Auto,
        _ => return None,
    })
}

fn mode_arg(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::AcceptEdits => "acceptEdits",
        PermissionMode::Plan => "plan",
        PermissionMode::Auto => "auto",
        _ => "default",
    }
}

/// The tokens as the user typed them: `--x=v` stays one token.
fn typed(arg: &str, inline: bool, values: &[String]) -> Vec<String> {
    if inline {
        return vec![arg.to_owned()];
    }
    std::iter::once(arg.to_owned())
        .chain(values.iter().cloned())
        .collect()
}

/// Values of a flag: the inline one, or the following tokens per its arity (`None`: until the
/// next flag).
fn take(args: &[String], i: &mut usize, inline: Option<&str>, count: Option<usize>) -> Vec<String> {
    if let Some(v) = inline {
        return vec![v.to_owned()];
    }
    let mut values = Vec::new();
    while *i < args.len() && count.is_none_or(|c| values.len() < c) {
        if count.is_none() && args[*i].starts_with('-') {
            break;
        }
        values.push(args[*i].clone());
        *i += 1;
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn parsed(items: &[&str]) -> Result<ParsedArgs, ArgsError> {
        parse(&v(items), Path::new("/"), Overrides::default(), || None)
    }

    #[test]
    fn inline_json_configs_are_refused_without_echoing_them() {
        for flag in ["--settings", "--mcp-config"] {
            let json = r#"{"mcpServers":{"x":{"env":{"TOK":"hunter2"}}}}"#;
            for args in [v(&[flag, json]), v(&[&format!("{flag}={json}")])] {
                let err = parse(&args, Path::new("/"), Overrides::default(), || None).unwrap_err();
                assert_eq!(err.flag, flag);
                assert!(!err.to_string().contains("hunter2"));
            }
        }
    }

    #[test]
    fn config_files_are_kept_when_they_exist_relative_to_the_cwd() {
        let dir = std::env::temp_dir().join(format!("muxy-chat-args-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("s.json"), "{}").unwrap();
        let args = v(&[
            "--settings",
            "s.json",
            "--mcp-config",
            "s.json",
            "nope.json",
        ]);
        assert!(parse(&args, &dir, Overrides::default(), || None).is_err());
        let args = v(&["--settings", "s.json", "--mcp-config=s.json"]);
        let out = parse(&args, &dir, Overrides::default(), || None).unwrap();
        assert_eq!(
            out.headless[..3],
            v(&["--settings", "s.json", "--mcp-config=s.json"])[..]
        );
        // A directory is not a config file.
        assert!(
            parse(&v(&["--settings", "."]), &dir, Overrides::default(), || {
                None
            })
            .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bypass_becomes_default_with_a_notice() {
        for args in [
            &["--dangerously-skip-permissions"][..],
            &["--allow-dangerously-skip-permissions"],
            &["--permission-mode", "bypassPermissions"],
            &["--permission-mode=bypassPermissions"],
            &[
                "--permission-mode",
                "plan",
                "--dangerously-skip-permissions",
            ],
        ] {
            let out = parsed(args).unwrap();
            assert_eq!(out.permission_mode, PermissionMode::Default, "{args:?}");
            assert_eq!(out.notices.len(), 1);
            assert!(
                out.headless
                    .ends_with(&v(&["--permission-mode", "default"]))
            );
            assert!(args.iter().all(|a| out.console.contains(&(*a).to_owned())));
        }
        assert!(parsed(&["--permission-mode", "dontAsk"]).is_err());
        assert!(parsed(&["--permission-mode"]).is_err());
    }

    #[test]
    fn explicit_modes_and_auto_mode() {
        for (arg, mode) in [
            ("plan", PermissionMode::Plan),
            ("acceptEdits", PermissionMode::AcceptEdits),
        ] {
            let out = parsed(&[&format!("--permission-mode={arg}")]).unwrap();
            assert_eq!(out.permission_mode, mode);
            assert!(out.notices.is_empty());
        }
        let out = parsed(&["--enable-auto-mode"]).unwrap();
        assert_eq!(out.permission_mode, PermissionMode::Auto);
        assert!(out.headless.ends_with(&v(&["--permission-mode", "auto"])));
        let both = parsed(&["--enable-auto-mode", "--permission-mode", "plan"]).unwrap();
        assert_eq!(both.permission_mode, PermissionMode::Plan);
        assert_eq!(
            parsed(&[]).unwrap().permission_mode,
            PermissionMode::Default
        );
    }

    #[test]
    fn without_a_mode_flag_the_tuis_settings_mode_applies_by_precedence() {
        let dir = std::env::temp_dir().join(format!("muxy-chat-settings-{}", std::process::id()));
        let (cwd, config) = (dir.join("project"), dir.join("config"));
        std::fs::create_dir_all(cwd.join(".claude")).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        let managed = dir.join("managed.json");
        let layers = [
            managed.clone(),
            cwd.join(".claude/settings.local.json"),
            cwd.join(".claude/settings.json"),
            config.join("settings.json"),
        ];
        let write = |path: &Path, mode: &str| {
            std::fs::write(
                path,
                format!(r#"{{"permissions":{{"defaultMode":"{mode}"}}}}"#),
            )
            .unwrap();
        };
        let resolved = |args: &[&str]| {
            parse(&v(args), &cwd, Overrides::default(), || {
                settings_default_mode(&layers)
            })
            .unwrap()
        };
        assert_eq!(resolved(&[]).permission_mode, PermissionMode::Default);
        write(&layers[3], "plan");
        assert_eq!(resolved(&[]).permission_mode, PermissionMode::Plan);
        write(&layers[2], "acceptEdits");
        assert_eq!(resolved(&[]).permission_mode, PermissionMode::AcceptEdits);
        write(&layers[1], "auto");
        let out = resolved(&[]);
        assert_eq!(out.permission_mode, PermissionMode::Auto);
        assert!(out.headless.ends_with(&v(&["--permission-mode", "auto"])));
        assert!(out.console.is_empty(), "the TUI reads its settings itself");
        // Explicit flags still win.
        assert_eq!(
            resolved(&["--permission-mode", "plan"]).permission_mode,
            PermissionMode::Plan
        );
        // An invalid or oversized layer is skipped, a layer without the key falls through.
        std::fs::write(&layers[1], "{not json").unwrap();
        assert_eq!(resolved(&[]).permission_mode, PermissionMode::AcceptEdits);
        std::fs::write(&managed, r#"{"permissions":{}}"#).unwrap();
        assert_eq!(resolved(&[]).permission_mode, PermissionMode::AcceptEdits);
        let mut big = r#"{"permissions":{"defaultMode":"plan"},"x":""#.to_owned();
        big.push_str(&" ".repeat(usize::try_from(MAX_SETTINGS_BYTES).unwrap()));
        big.push_str(r#""}"#);
        std::fs::write(&managed, big).unwrap();
        assert_eq!(resolved(&[]).permission_mode, PermissionMode::AcceptEdits);
        // Bypass and unknown modes become default, each with one notice.
        for mode in ["bypassPermissions", "dontAsk"] {
            write(&managed, mode);
            let out = resolved(&[]);
            assert_eq!(out.permission_mode, PermissionMode::Default, "{mode}");
            assert_eq!(out.notices.len(), 1, "{mode}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn selectors_prompt_and_print_flags_are_stripped() {
        let out = parsed(&[
            "--resume",
            "abc",
            "-c",
            "--session-id",
            "u",
            "--fork-session",
            "-p",
            "--print",
            "--output-format=json",
            "--input-format",
            "text",
            "--include-partial-messages",
            "--replay-user-messages",
            "--model",
            "opus",
            "fix the bug",
            "--",
            "--not-a-flag",
        ])
        .unwrap();
        assert_eq!(
            out.headless,
            v(&["--model", "opus", "--permission-mode", "default"])
        );
        assert_eq!(out.console, v(&["--model", "opus"]));
        // An optional resume value doesn't swallow the next flag.
        assert_eq!(
            parsed(&["-r", "--verbose"]).unwrap().headless[0],
            "--verbose"
        );
    }

    #[test]
    fn unknown_and_persistence_changing_flags_are_refused() {
        for flag in [
            "--no-session-persistence",
            "--append-system-prompt",
            "--system-prompt",
            "--made-up",
            "-x",
            "--dangerously-skip-permissions=1",
            "--print=1",
        ] {
            let err = parsed(&[flag, "text"]).unwrap_err();
            assert_eq!(err.flag, flag.split('=').next().unwrap());
        }
        assert!(parsed(&["--append-system-prompt=secret"]).is_err());
        assert!(parsed(&["--model", "a\nb"]).is_err());
    }

    #[test]
    fn value_flags_accept_the_equals_form_and_variadics_stop_at_the_next_flag() {
        let out = parsed(&[
            "--model=opus",
            "--effort=high",
            "--setting-sources=user",
            "--agent=x",
            "--add-dir=/a",
            "--allowedTools=Bash",
            "--allowed-tools=Read",
            "--disallowedTools=Edit",
            "--disallowed-tools=Write",
            "--plugin-dir=/p",
        ])
        .unwrap();
        assert_eq!(out.model.as_deref(), Some("opus"));
        assert_eq!(out.effort.as_deref(), Some("high"));
        assert_eq!(out.headless.len(), 10 + 2);
        let out = parsed(&[
            "--add-dir",
            "/a",
            "/b",
            "--allowedTools",
            "Bash(ls)",
            "Read",
            "--strict-mcp-config",
        ])
        .unwrap();
        assert_eq!(
            out.headless[..7],
            v(&[
                "--add-dir",
                "/a",
                "/b",
                "--allowedTools",
                "Bash(ls)",
                "Read",
                "--strict-mcp-config"
            ])[..]
        );
        assert!(parsed(&["--model"]).is_err());
    }

    #[test]
    fn picker_overrides_drop_model_and_effort_from_the_headless_child_only() {
        let out = parse(
            &v(&["--model", "opus", "--effort=low", "--verbose"]),
            Path::new("/"),
            Overrides {
                model: true,
                effort: true,
            },
            || None,
        )
        .unwrap();
        assert_eq!(
            out.headless,
            v(&["--verbose", "--permission-mode", "default"])
        );
        assert_eq!(
            out.console,
            v(&["--model", "opus", "--effort=low", "--verbose"])
        );
    }

    #[test]
    fn headless_flags_are_the_tested_set() {
        assert_eq!(
            headless_flags("U", false).join(" "),
            "-p --input-format stream-json --output-format stream-json --verbose \
             --include-partial-messages --replay-user-messages --thinking-display summarized \
             --permission-prompt-tool stdio --resume U"
        );
        assert!(
            headless_flags("U", true)
                .join(" ")
                .ends_with("stdio --session-id U")
        );
    }
}
