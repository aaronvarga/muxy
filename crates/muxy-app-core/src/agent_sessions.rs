mod files;
mod parse;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use muxy_protocol::AgentProvider;

pub use files::{Catalog, Listing, Roots};

pub const PROVIDERS: [AgentProvider; 12] = [
    AgentProvider::Claude,
    AgentProvider::Codex,
    AgentProvider::Copilot,
    AgentProvider::Droid,
    AgentProvider::Pi,
    AgentProvider::OhMyPi,
    AgentProvider::Xal,
    AgentProvider::OpenCode,
    AgentProvider::Cursor,
    AgentProvider::Grok,
    AgentProvider::Kiro,
    AgentProvider::Antigravity,
];

pub fn supported(provider: AgentProvider) -> bool {
    matches!(
        provider,
        AgentProvider::Claude
            | AgentProvider::Codex
            | AgentProvider::Copilot
            | AgentProvider::Droid
            | AgentProvider::Pi
            | AgentProvider::OhMyPi
            | AgentProvider::Xal
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Session {
    pub provider: AgentProvider,
    pub id: String,
    pub title: String,
    pub directory: PathBuf,
    pub path: PathBuf,
    pub modified: SystemTime,
}

impl Session {
    pub fn matches(&self, query: &str) -> bool {
        query.split_whitespace().all(|word| {
            let word = word.to_lowercase();
            self.title.to_lowercase().contains(&word)
                || self.id.to_lowercase().contains(&word)
                || self.provider.name().to_lowercase().contains(&word)
        })
    }

    pub fn resume_command(&self) -> Option<String> {
        resume_command(self.provider, &self.id, &[])
    }
}

/// The shell command reopening `provider`'s session `id`, keeping the flags it ran with.
pub fn resume_command(provider: AgentProvider, id: &str, arguments: &[String]) -> Option<String> {
    if !valid_id(id) {
        return None;
    }
    let command = match provider {
        AgentProvider::Claude => "claude --resume",
        AgentProvider::Codex => "codex resume",
        AgentProvider::Copilot => "copilot --resume",
        AgentProvider::Droid => "droid --resume",
        AgentProvider::Pi => "pi --session",
        AgentProvider::OhMyPi => "omp --resume",
        AgentProvider::Xal => "xal resume",
        _ => return None,
    };
    let (program, resume) = command.split_once(' ')?;
    let flags = resume_flags(provider, arguments)
        .into_iter()
        .map(|flag| quoted(&flag))
        .collect::<Vec<_>>();
    // Flags go before a resume flag, and after a resume subcommand.
    let words: Vec<&str> = if resume.starts_with('-') {
        std::iter::once(program)
            .chain(flags.iter().map(String::as_str))
            .chain([resume, id])
            .collect()
    } else {
        [program, resume]
            .into_iter()
            .chain(flags.iter().map(String::as_str))
            .chain([id])
            .collect()
    };
    Some(words.join(" "))
}

/// Flags and their values, without positional words or ones choosing a session.
// shortcut: for agents without a list of flags taking values, a word after any flag is taken as
// its value, so `claude --flag "prompt"` keeps the prompt; add a list if that comes up.
fn resume_flags(provider: AgentProvider, arguments: &[String]) -> Vec<String> {
    // Codex reads a stray word as the session id, so only these flags keep the next word.
    const CODEX_VALUES: &[&str] = &[
        "-m",
        "--model",
        "-c",
        "--config",
        "-p",
        "--profile",
        "-s",
        "--sandbox",
        "-a",
        "--ask-for-approval",
        "-C",
        "--cd",
        "-i",
        "--image",
        "--add-dir",
        "--enable",
        "--disable",
        "--local-provider",
    ];
    let session_flags: &[&str] = match provider {
        AgentProvider::Claude => &[
            "--resume",
            "-r",
            "--continue",
            "-c",
            "--session-id",
            "--fork-session",
        ],
        AgentProvider::Codex => &["--last", "--all"],
        _ => &[],
    };
    let mut flags = Vec::new();
    let mut value = false;
    // Control characters could act as keystrokes when pasted into another terminal.
    if arguments
        .iter()
        .any(|argument| argument.chars().any(char::is_control))
    {
        return Vec::new();
    }
    for argument in arguments {
        if argument == "--" {
            break;
        }
        if argument.starts_with('-') {
            let name = argument.split('=').next().unwrap_or(argument);
            value = !session_flags.contains(&name);
            if value {
                flags.push(argument.clone());
                value = !argument.contains('=')
                    && (provider != AgentProvider::Codex || CODEX_VALUES.contains(&name));
            }
        } else if std::mem::take(&mut value) {
            flags.push(argument.clone());
        }
    }
    flags
}

fn quoted(word: &str) -> String {
    if !word.is_empty()
        && !word.starts_with('=')
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./=:,@%+".contains(&byte))
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

fn normalized(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests;
