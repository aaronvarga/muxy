//! A conversation's history from Claude Code's transcript, `<config>/projects/*/<id>.jsonl`.
//!
//! Only the file's tail is read, aligned to a whole line, and only message lines are kept; they
//! go through the same normalizer as the live stream (see `ChatLog::seed`). The parsing is
//! deliberately separate from muxy-app-core's, which the server can't depend on.
//!
//! shortcut: reads one file. The plan allows following up to 4 `continued-in` files, but no
//! continuation marker exists in the recorded fixtures or in the transcripts of the installed
//! CLI (2.1.319); add the visited-set walk once a version shows how continuation is written.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::normalize::str_at;

pub(crate) const MAX_TRANSCRIPT_BYTES: u64 = 2 * 1024 * 1024;

/// The transcript of conversation `uuid`: a regular file under `<config>/projects`, after
/// resolving links.
pub(crate) fn find(config: &Path, uuid: &str) -> io::Result<PathBuf> {
    let valid = !uuid.is_empty()
        && uuid.len() <= 64
        && uuid
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-');
    if !valid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a conversation id",
        ));
    }
    let projects = fs::canonicalize(config.join("projects"))?;
    let name = format!("{uuid}.jsonl");
    for entry in fs::read_dir(&projects)? {
        let Ok(path) = fs::canonicalize(entry?.path().join(&name)) else {
            continue;
        };
        if path.starts_with(&projects) && fs::metadata(&path).is_ok_and(|meta| meta.is_file()) {
            return Ok(path);
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "no transcript"))
}

/// The message lines (`user`, `assistant`) of the transcript's last `MAX_TRANSCRIPT_BYTES`.
pub(crate) fn read(config: &Path, uuid: &str) -> io::Result<Vec<Value>> {
    let mut file = File::open(find(config, uuid)?)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(MAX_TRANSCRIPT_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(MAX_TRANSCRIPT_BYTES).read_to_end(&mut bytes)?;
    let mut text = bytes.as_slice();
    if start > 0 {
        // Starts mid-line: drop up to the first full line.
        let first = text
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(text.len(), |at| at + 1);
        text = &text[first..];
    }
    Ok(text
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .filter(|line| matches!(str_at(line, "type"), Some("user" | "assistant")))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn config(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("muxy-transcript-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("projects/-repo")).unwrap();
        dir
    }

    const ID: &str = "1dc658c0-bc8f-4fb3-b0dc-f47a565d5f73";

    #[test]
    fn tail_starts_on_a_whole_line_and_keeps_only_messages() {
        let dir = config("tail");
        let mut file = File::create(dir.join(format!("projects/-repo/{ID}.jsonl"))).unwrap();
        let filler = format!(
            "{{\"type\":\"user\",\"uuid\":\"old\",\"message\":{{\"content\":\"{}\"}}}}\n",
            "x".repeat(1000)
        );
        for _ in 0..(MAX_TRANSCRIPT_BYTES / 1000 + 10) {
            file.write_all(filler.as_bytes()).unwrap();
        }
        writeln!(file, r#"{{"type":"attachment","uuid":"a"}}"#).unwrap();
        writeln!(file, r#"{{"type":"last-prompt","uuid":"b"}}"#).unwrap();
        writeln!(
            file,
            r#"{{"type":"user","uuid":"new","message":{{"content":"hi"}}}}"#
        )
        .unwrap();
        drop(file);

        let lines = read(&dir, ID).unwrap();
        assert!(
            lines
                .iter()
                .all(|line| matches!(str_at(line, "type"), Some("user" | "assistant")))
        );
        assert_eq!(str_at(lines.last().unwrap(), "uuid"), Some("new"));
        let size: usize = lines.iter().map(|line| line.to_string().len() + 1).sum();
        assert!(size as u64 <= MAX_TRANSCRIPT_BYTES);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_paths_outside_projects_and_bad_ids() {
        let dir = config("escape");
        let outside = dir.join(format!("{ID}.jsonl"));
        fs::write(&outside, "{}\n").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join(format!("projects/-repo/{ID}.jsonl")))
            .unwrap();
        assert!(find(&dir, ID).is_err());
        assert_eq!(
            find(&dir, "../x").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        fs::create_dir(dir.join("projects/-dir")).unwrap();
        fs::create_dir(dir.join(format!("projects/-dir/{ID}.jsonl"))).unwrap();
        assert!(find(&dir, ID).is_err(), "a directory is not a transcript");
        let _ = fs::remove_dir_all(&dir);
    }
}
