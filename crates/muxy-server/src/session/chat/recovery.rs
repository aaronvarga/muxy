//! The one typed console line that returns the terminal to Claude's TUI after Chat.
//!
//! It restates the cwd, `CLAUDE_CONFIG_DIR` and the forwarded `CLAUDE_CODE_*` settings, since a
//! user may have set those inline for the original launch; everything else the child saw
//! (PATH, proxies, locale...) is already the shell's own. It holds no secret: `env.rs` retypes
//! only `CLAUDE_CODE_*` settings that don't look secret.
//!
//! shortcut: the line is POSIX sh syntax (`cd --`, `&&`, `env`, single quotes), which zsh, bash
//! and sh accept; detect the shell and refuse or adapt if fish/nushell users switch to Chat.

use std::path::Path;

use super::env::ChildEnv;

/// POSIX single-quoting: the value is always a literal string.
pub(crate) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The line to type (without a trailing newline), resuming `uuid` or with `None` relaunching
/// Claude as it was, or `None` when it can't be typed safely: the
/// cwd is gone, the session id is malformed, or a word is not UTF-8 or holds a control character
/// (which would act as a keystroke). The caller then shows `plain_command`.
pub(crate) fn command(
    cwd: &Path,
    env: &ChildEnv,
    prefix: &[String],
    console_args: &[String],
    uuid: Option<&str>,
) -> Option<String> {
    if !cwd.is_dir() || prefix.is_empty() || !uuid.is_none_or(valid_id) {
        return None;
    }
    let mut words = vec![match env.config_dir() {
        Some(dir) => format!("CLAUDE_CONFIG_DIR={}", quote(dir.to_str()?)),
        None => "-u CLAUDE_CONFIG_DIR".to_owned(),
    }];
    for (name, value) in env.code_settings() {
        words.push(format!("{}={}", name.to_str()?, quote(value.to_str()?)));
    }
    let program = prefix.iter().chain(console_args).map(|w| quote(w));
    let resume = uuid.map(|uuid| ["--resume".to_owned(), quote(uuid)]);
    let tail = program
        .chain(resume.into_iter().flatten())
        .collect::<Vec<_>>();
    let line = format!(
        "cd -- {} && env {} {}",
        quote(cwd.to_str()?),
        words.join(" "),
        tail.join(" ")
    );
    (!line.chars().any(char::is_control)).then_some(line)
}

/// A session id that is safe to type unquoted (copied from muxy-app-core's resume command).
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

/// What to show for the user to copy when `command` can't be typed. Carries no flags or env.
pub(crate) fn plain_command(uuid: &str) -> Option<String> {
    valid_id(uuid).then(|| format!("claude --resume {uuid}"))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::session::chat::env::build;

    fn env(pairs: &[(&str, &str)]) -> ChildEnv {
        let captured: Vec<(OsString, OsString)> = pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        build(&captured).unwrap()
    }

    #[test]
    fn quoting_neutralizes_shell_metacharacters() {
        assert_eq!(quote("it's"), r"'it'\''s'");
        for hostile in [
            "$(touch x)",
            "`touch x`",
            "a b",
            "a\nb",
            "; rm -rf ~",
            "\"q\"",
            "''",
        ] {
            let q = quote(hostile);
            // The only way out of single quotes is the escaped-quote idiom.
            let inner = &q[1..q.len() - 1];
            assert_eq!(inner.replace(r"'\''", ""), hostile.replace('\'', ""));
            assert!(q.starts_with('\'') && q.ends_with('\''));
        }
    }

    #[test]
    fn line_restates_cwd_config_dir_and_settings_quoted() {
        let cwd = std::env::temp_dir();
        let uuid = "01a1b0fb-84a9-7741-bc07-b4efd1aaba4e";
        let line = command(
            &cwd,
            &env(&[
                ("CLAUDE_CONFIG_DIR", "/c d/it's"),
                ("CLAUDE_CODE_X", "$(y)"),
                ("PATH", "/bin"),
            ]),
            &["/usr/bin/node".into(), "/s/cli.js".into()],
            &["--model".into(), "a b".into()],
            Some(uuid),
        )
        .unwrap();
        assert_eq!(
            line,
            format!(
                "cd -- {} && env CLAUDE_CONFIG_DIR='/c d/it'\\''s' CLAUDE_CODE_X='$(y)' \
                 '/usr/bin/node' '/s/cli.js' '--model' 'a b' --resume '{uuid}'",
                quote(cwd.to_str().unwrap())
            )
        );
        let unset = command(&cwd, &env(&[]), &["claude".into()], &[], Some(uuid)).unwrap();
        assert!(unset.contains("env -u CLAUDE_CONFIG_DIR 'claude' --resume"));
    }

    #[test]
    fn untypeable_cases_fall_back_to_the_plain_command() {
        let uuid = "01a1b0fb-84a9-7741-bc07-b4efd1aaba4e";
        let prefix = ["claude".to_owned()];
        assert!(
            command(
                Path::new("/definitely/not/here"),
                &env(&[]),
                &prefix,
                &[],
                Some(uuid)
            )
            .is_none()
        );
        let cwd = std::env::temp_dir();
        assert!(command(&cwd, &env(&[]), &prefix, &["a\nb".into()], Some(uuid)).is_none());
        assert!(command(&cwd, &env(&[]), &prefix, &[], Some("x'; rm -rf ~")).is_none());
        assert_eq!(
            plain_command(uuid).unwrap(),
            format!("claude --resume {uuid}")
        );
        assert!(plain_command("x y").is_none());
    }
}
