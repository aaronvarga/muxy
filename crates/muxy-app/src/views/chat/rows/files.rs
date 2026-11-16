//! Fork: what chat chips show about files: a file's kind, a turn's line
//! totals, and the @mentions and attached paths in a prompt.

use std::ops::Range;

use muxy_protocol::FileChange;

/// What a file's icon and its color say it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FileKind {
    Image,
    React,
    Style,
    Rust,
    Code,
    Other,
}

pub(super) fn kind(path: &str) -> FileKind {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((_, ext)) = name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()) else {
        return FileKind::Other;
    };
    match ext.to_ascii_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "bmp" | "tiff" | "heic"
        | "avif" => FileKind::Image,
        "tsx" | "jsx" => FileKind::React,
        "css" | "scss" | "sass" | "less" => FileKind::Style,
        "rs" => FileKind::Rust,
        "md" | "txt" | "json" | "toml" | "yaml" | "yml" | "lock" | "csv" | "log" => FileKind::Other,
        _ => FileKind::Code,
    }
}

/// The last part of a path.
pub(super) fn name(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
}

/// Lines added and removed across a turn's files.
pub(super) fn totals(changed: &[FileChange]) -> (u64, u64) {
    changed.iter().fold((0, 0), |(added, removed), change| {
        (
            added + u64::from(change.added),
            removed + u64::from(change.removed),
        )
    })
}

/// Whether code like `src/main.rs:12:3` may name a file: it has a `/` or a
/// file extension. Pure, so drawing never asks; whether it exists is looked
/// up on the opener worker.
pub(crate) fn path_candidate(code: &str) -> bool {
    if code.is_empty()
        || code.len() > 1024
        || code.starts_with('-')
        || code.contains("://")
        || code.contains(|c: char| {
            c.is_whitespace() || c.is_control() || "*?<>|;&$`\"'(){}[]=,".contains(c)
        })
    {
        return false;
    }
    // Up to two `:digits` suffixes are the line and column.
    let mut path = code;
    for _ in 0..2 {
        match path.rsplit_once(':') {
            Some((base, number))
                if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) =>
            {
                path = base;
            }
            _ => break,
        }
    }
    if path.contains(':') {
        return false;
    }
    path.contains('/')
        || path.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty()
                && (1..=10).contains(&ext.len())
                && ext.bytes().all(|b| b.is_ascii_alphanumeric())
                && ext.bytes().any(|b| b.is_ascii_alphabetic())
        })
}

/// A prompt split for drawing: its text without attached paths, the @mention
/// ranges in that text, and the attached paths.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Prompt {
    pub(crate) text: gpui::SharedString,
    pub(crate) mentions: Vec<Range<usize>>,
    pub(crate) attachments: Vec<String>,
}

/// Splits a prompt, or `None` when it has neither mentions nor attachments.
/// Attachments are the `'/absolute/path'` the composer inserts for files;
/// the shell's other quoting forms stay text.
pub(crate) fn prompt(source: &str) -> Option<Prompt> {
    if !source.contains("'/") && !source.contains('@') {
        return None;
    }
    let mut out = Prompt::default();
    let mut text = String::new();
    let mut at = 0;
    while at < source.len() {
        let rest = &source[at..];
        let word_start = at == 0 || source[..at].ends_with(char::is_whitespace);
        if word_start && let Some(path) = attachment(rest) {
            out.attachments.push(path.to_owned());
            at += path.len() + 2;
            continue;
        }
        if word_start && let Some(len) = mention(rest) {
            let start = text.len();
            text.push_str(&rest[..len]);
            out.mentions.push(start..text.len());
            at += len;
            continue;
        }
        let ch = rest.chars().next().unwrap_or(' ');
        text.push(ch);
        at += ch.len_utf8();
    }
    if out.attachments.is_empty() && out.mentions.is_empty() {
        return None;
    }
    // Attachments leave their separating spaces behind.
    let trimmed = text.trim_end().len();
    text.truncate(trimmed);
    let lead = text.len() - text.trim_start().len();
    if lead > 0 {
        text.drain(..lead);
        for range in &mut out.mentions {
            *range = range.start - lead..range.end - lead;
        }
    }
    out.text = text.into();
    Some(out)
}

/// `'/path'` at the start of `rest`, ended by whitespace or the end: the path.
fn attachment(rest: &str) -> Option<&str> {
    let body = rest.strip_prefix("'/")?;
    let end = body.find('\'')?;
    let after = &body[end + 1..];
    (after.is_empty() || after.starts_with(char::is_whitespace)).then(|| &rest[1..end + 2])
}

/// `@path/like.this` at the start of `rest`: its length, without trailing punctuation.
fn mention(rest: &str) -> Option<usize> {
    let body = rest.strip_prefix('@')?;
    let word = &body[..body.find(char::is_whitespace).unwrap_or(body.len())];
    let word = word.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '"', '\'']);
    // A path, not a handle: "@aaron" stays text.
    word.contains(['/', '.']).then_some(1 + word.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_follow_the_extension_not_the_folder() {
        assert_eq!(kind("public/hero.WEBP"), FileKind::Image);
        assert_eq!(kind("app/layout.tsx"), FileKind::React);
        assert_eq!(kind("app/globals.css"), FileKind::Style);
        assert_eq!(kind("/src/main.rs"), FileKind::Rust);
        assert_eq!(kind("src/model.ts"), FileKind::Code);
        assert_eq!(kind("Cargo.lock"), FileKind::Other);
        assert_eq!(kind("a.rs/Makefile"), FileKind::Other);
        assert_eq!(kind(".gitignore"), FileKind::Other);
    }

    #[test]
    fn totals_sum_without_overflow() {
        let change = |added, removed| FileChange {
            path: String::new(),
            added,
            removed,
        };
        let changed = [change(u32::MAX, 1), change(2, 7)];
        assert_eq!(totals(&changed), (u64::from(u32::MAX) + 2, 8));
        assert_eq!(totals(&[]), (0, 0));
    }

    #[test]
    fn attachments_leave_the_text_and_mentions_keep_their_place() {
        let split = prompt("make this the hero '/Users/a/Shot 1.png'").expect("split");
        assert_eq!(split.text.as_ref(), "make this the hero");
        assert_eq!(split.attachments, ["/Users/a/Shot 1.png"]);

        let split = prompt("use @src/command.tsx, in @Dialog.tsx.").expect("split");
        assert_eq!(&split.text[split.mentions[0].clone()], "@src/command.tsx");
        assert_eq!(&split.text[split.mentions[1].clone()], "@Dialog.tsx");

        let split = prompt("'/a/b.rs' then @x/y").expect("split");
        assert_eq!(split.text.as_ref(), "then @x/y");
        assert_eq!(&split.text[split.mentions[0].clone()], "@x/y");
    }

    #[test]
    fn path_candidates_are_paths_with_optional_positions_not_code() {
        for path in [
            "src/main.rs",
            "src/main.rs:12",
            "src/main.rs:12:3",
            "/etc/hosts",
            "~/notes.md",
            "Cargo.toml",
            "crates/muxy-app/",
        ] {
            assert!(path_candidate(path), "{path}");
        }
        for code in [
            "",
            "3.14",
            "foo()",
            "--flag",
            "cargo test",
            "Vec<T>",
            "src/**/*.rs",
            "https://example.com/a.rs",
            "a:b:c.rs",
            "self.text_longer_than_ten",
        ] {
            assert!(!path_candidate(code), "{code}");
        }

        // The position is parsed where the file is looked up, against the session's directory.
        let target = muxy_app_core::opener::Target::file(
            "src/main.rs:12:3",
            std::path::Path::new("/work"),
            None,
            |path| path == std::path::Path::new("/work/src/main.rs"),
        );
        assert_eq!(
            target,
            Some(muxy_app_core::opener::Target::File(
                muxy_app_core::opener::FileLocation {
                    path: "/work/src/main.rs".into(),
                    line: Some(12),
                    column: Some(3),
                }
            ))
        );
    }

    #[test]
    fn plain_quotes_emails_and_handles_stay_text() {
        assert_eq!(prompt("it's '/not closed"), None);
        assert_eq!(prompt("say 'hi' to a@b.com or @aaron"), None);
        assert_eq!(prompt("'/a'b' stays"), None);
    }
}
