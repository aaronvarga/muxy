//! The headless child's environment: the Claude TUI's captured one, passed through whole so
//! tools, logins and shell snapshots work as in the Console.
//!
//! Only Muxy's own variables and a parent Claude's session markers are dropped, and settings that
//! would switch Claude's own login are refused: Chat supports subscription login only. No value
//! is ever logged, put in an error, printed by `Debug`, or typed (recovery retypes only
//! non-secret `CLAUDE_CODE_*` settings).

use std::ffi::{OsStr, OsString};
use std::fmt;

use muxy_protocol::ChatRefusal;

/// A captured variable Chat can't carry over. Holds the NAME only.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UnsupportedEnv {
    pub(crate) name: String,
}

impl UnsupportedEnv {
    pub(crate) const fn refusal() -> ChatRefusal {
        ChatRefusal::UnsupportedEnvironment
    }
}

impl fmt::Display for UnsupportedEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Chat supports subscription login only; the environment variable {} isn't supported",
            self.name
        )
    }
}

/// `CLAUDE_CODE_SKIP_PROMPT_HISTORY` turns session persistence off, so there is nothing to resume.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PersistenceDisabled;

impl PersistenceDisabled {
    pub(crate) const fn refusal() -> ChatRefusal {
        ChatRefusal::PersistenceDisabled
    }
}

impl fmt::Display for PersistenceDisabled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CLAUDE_CODE_SKIP_PROMPT_HISTORY is set, so Claude doesn't save this session")
    }
}

pub(crate) fn check_persistence(
    captured: &[(OsString, OsString)],
) -> Result<(), PersistenceDisabled> {
    let disabled = captured
        .iter()
        .any(|(k, v)| k == "CLAUDE_CODE_SKIP_PROMPT_HISTORY" && !v.is_empty());
    if disabled {
        Err(PersistenceDisabled)
    } else {
        Ok(())
    }
}

/// The variables to give the child (and `CLAUDE_CONFIG_DIR`/`CLAUDE_CODE_*` for recovery).
#[derive(Clone)]
pub(crate) struct ChildEnv {
    vars: Vec<(OsString, OsString)>,
}

// Values may be sensitive paths or proxy hosts; show the count only.
impl fmt::Debug for ChildEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChildEnv({} variables)", self.vars.len())
    }
}

impl ChildEnv {
    /// For `Command::env_clear().envs(..)`.
    pub(crate) fn vars(&self) -> &[(OsString, OsString)] {
        &self.vars
    }

    pub(crate) fn config_dir(&self) -> Option<&OsStr> {
        self.get("CLAUDE_CONFIG_DIR")
    }

    /// The `CLAUDE_CODE_*` settings the recovery line restates: plain names (retyped unquoted)
    /// that don't look secret, since the line lands in shell history.
    pub(crate) fn code_settings(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.vars
            .iter()
            .filter(|(k, _)| {
                k.to_str().is_some_and(|k| {
                    k.starts_with("CLAUDE_CODE_")
                        && !looks_secret(k)
                        && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                })
            })
            .map(|(k, v)| (k.as_os_str(), v.as_os_str()))
    }

    fn get(&self, name: &str) -> Option<&OsStr> {
        self.vars
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_os_str())
    }
}

/// Set by a parent Claude for its own session; forwarding them could make the child believe it is
/// nested, or attach it to the parent's session.
fn is_session_marker(name: &str) -> bool {
    matches!(
        name,
        "CLAUDECODE" | "CLAUDE_PID" | "CLAUDE_CODE_ENTRYPOINT" | "CLAUDE_CODE_EXECPATH"
    ) || name.strip_prefix("CLAUDE_CODE_").is_some_and(|rest| {
        rest.split('_')
            .any(|part| matches!(part, "SESSION" | "SOCKET" | "PID" | "CHILD" | "MESSAGING"))
    })
}

/// Settings that would move Claude off the subscription login: an API key or token, another
/// endpoint, or a cloud provider. Model choices (`ANTHROPIC_*_MODEL`) are only preferences.
fn is_refused(name: &str) -> bool {
    (name.starts_with("ANTHROPIC_") && !name.ends_with("_MODEL"))
        || name.starts_with("CLAUDE_CODE_USE_")
        || name == "CLAUDE_CODE_OAUTH_TOKEN"
}

/// A `CLAUDE_CODE_*` setting that may hold a secret is passed to the child but never retyped.
fn looks_secret(name: &str) -> bool {
    ["KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL", "AUTH"]
        .iter()
        .any(|p| name.contains(p))
}

fn is_proxy(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY"
    )
}

fn has_userinfo(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#'])
        .next()
        .is_some_and(|authority| authority.contains('@'))
}

pub(crate) fn build(captured: &[(OsString, OsString)]) -> Result<ChildEnv, UnsupportedEnv> {
    let mut vars = Vec::new();
    for (key, value) in captured {
        let name = key.to_string_lossy();
        let upper = name.to_ascii_uppercase();
        // Muxy's own variables, and markers set by a parent Claude, are dropped silently.
        if upper.starts_with("MUXY_") || is_session_marker(&upper) {
            continue;
        }
        if is_refused(&upper) {
            return Err(UnsupportedEnv {
                name: name.into_owned(),
            });
        }
        if is_proxy(&name) && has_userinfo(&value.to_string_lossy()) {
            return Err(UnsupportedEnv {
                name: name.into_owned(),
            });
        }
        vars.push((key.clone(), value.clone()));
    }
    Ok(ChildEnv { vars })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect()
    }

    fn names(env: &ChildEnv) -> Vec<String> {
        env.vars()
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn credentials_are_refused_by_name_and_never_echoed() {
        for name in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ] {
            let err = build(&env(&[("PATH", "/bin"), (name, "hunter2")])).unwrap_err();
            assert_eq!(err.name, name);
            assert!(!err.to_string().contains("hunter2"), "{name}");
            assert_eq!(
                UnsupportedEnv::refusal(),
                ChatRefusal::UnsupportedEnvironment
            );
        }
    }

    #[test]
    fn the_rest_passes_through_but_only_safe_settings_are_retyped() {
        let child = build(&env(&[
            ("PATH", "/bin"),
            ("OPENAI_API_KEY", "hunter2"),
            ("GITHUB_TOKEN", "hunter2"),
            ("AWS_PROFILE", "work"),
            ("DB_PASSWORD", "hunter2"),
            ("EDITOR", "vi"),
            ("SSH_AUTH_SOCK", "/agent"),
            ("MUXY_SOCKET", "/m"),
            ("muxy_lower", "/m"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_SOME_TOKEN", "hunter2"),
            ("CLAUDE_CODE_DISABLE_TERMINAL_TITLE", "1"),
            ("ANTHROPIC_DEFAULT_HAIKU_MODEL", "claude-haiku-5-5"),
        ]))
        .unwrap();
        assert_eq!(
            names(&child),
            [
                "PATH",
                "OPENAI_API_KEY",
                "GITHUB_TOKEN",
                "AWS_PROFILE",
                "DB_PASSWORD",
                "EDITOR",
                "SSH_AUTH_SOCK",
                "CLAUDE_CODE_SOME_TOKEN",
                "CLAUDE_CODE_DISABLE_TERMINAL_TITLE",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL"
            ]
        );
        let retyped: Vec<_> = child.code_settings().map(|(name, _)| name).collect();
        assert_eq!(retyped, ["CLAUDE_CODE_DISABLE_TERMINAL_TITLE"]);
    }

    #[test]
    fn parent_session_markers_and_muxy_variables_are_dropped_not_refused() {
        let out = build(&env(&[
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_CHILD_SESSION", "1"),
            ("CLAUDE_CODE_SESSION_ID", "x"),
            ("CLAUDE_PID", "1"),
            ("CLAUDE_CODE_MESSAGING_SOCKET", "/s"),
            ("CLAUDE_CODE_MESSAGING_TOKEN", "t0ken"),
            ("CLAUDE_CODE_BRIDGE_SESSION_ID", "b"),
            ("CLAUDE_CODE_SESSION_ATTENDED", "1"),
            ("CLAUDE_CODE_ENTRYPOINT", "cli"),
            ("CLAUDE_CODE_EXECPATH", "/c"),
            ("MUXY_SOCKET", "/m"),
            ("MUXY_HOOK_TOKEN", "t"),
            ("CLAUDE_CODE_DISABLE_TERMINAL_TITLE", "1"),
            ("PATH", "/bin"),
        ]))
        .unwrap();
        assert_eq!(names(&out), ["CLAUDE_CODE_DISABLE_TERMINAL_TITLE", "PATH"]);
    }

    #[test]
    fn proxies_with_userinfo_are_refused_and_plain_ones_forwarded() {
        for url in [
            "http://user@proxy:8080",
            "http://user:pw@proxy:8080/",
            "user:pw@proxy:8080",
        ] {
            for name in ["HTTPS_PROXY", "http_proxy", "ALL_PROXY"] {
                let err = build(&env(&[(name, url)])).unwrap_err();
                assert_eq!(err.name, name);
                assert!(!err.to_string().contains("pw"));
            }
        }
        let out = build(&env(&[
            ("HTTPS_PROXY", "http://proxy:8080/path@x"),
            ("NO_PROXY", "localhost"),
            ("LC_ALL", "C"),
            ("CLAUDE_CONFIG_DIR", "/cfg"),
        ]))
        .unwrap();
        assert_eq!(
            names(&out),
            ["HTTPS_PROXY", "NO_PROXY", "LC_ALL", "CLAUDE_CONFIG_DIR"]
        );
        assert_eq!(out.config_dir(), Some(OsStr::new("/cfg")));
        assert!(!format!("{out:?}").contains("/cfg"));
    }

    #[test]
    fn disabled_persistence_is_detected_only_for_a_non_empty_value() {
        assert!(check_persistence(&env(&[("CLAUDE_CODE_SKIP_PROMPT_HISTORY", "1")])).is_err());
        assert!(check_persistence(&env(&[("CLAUDE_CODE_SKIP_PROMPT_HISTORY", "")])).is_ok());
        assert!(check_persistence(&env(&[])).is_ok());
    }
}
