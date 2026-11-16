//! Claude Code's stream-json and transcript (JSONL) lines → chat item operations.
//!
//! Ids are deterministic so a live stream and the transcript of the same conversation name the
//! same items: a prompt is its `uuid`, an assistant block is `<message.id>:<block index>` and a
//! tool is its `tool_use_id`. The CLI sends (and records) one `assistant` line per content block,
//! all sharing `message.id`, so the block index is the line's position among the lines with that
//! id. Streamed deltas name their block by `content_block_start.index`, which is that same
//! position; the fixture tests check the two agree.

use muxy_protocol::{ChatItemKind, FileChange, MAX_CHAT_NAME, MAX_CHAT_PREVIEW, MAX_CHAT_TEXT};
use serde_json::Value;

/// Appended to text Muxy shortened; counted inside the cap.
pub(crate) const MARKER: &str = "… [shortened by Muxy]";

/// `text` cut on a char boundary so that it plus [`MARKER`] fits in `cap` bytes.
pub(crate) fn clip(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_owned();
    }
    let mut end = cap.saturating_sub(MARKER.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + MARKER.len());
    out.push_str(&text[..end]);
    if out.len() + MARKER.len() <= cap {
        out.push_str(MARKER);
    }
    out
}

pub(crate) fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Op {
    /// Adds the item, or replaces the kind of the one with this id.
    Upsert { id: String, kind: ChatItemKind },
    /// Streamed text for an assistant (or, with `thinking`, a thinking) block.
    Delta {
        id: String,
        text: String,
        thinking: bool,
    },
    /// The result of the tool `id`.
    ToolResult {
        id: String,
        result: String,
        is_error: bool,
    },
    /// A subagent of the tool `parent` did one more step.
    Subagent { parent: String },
    /// An edit or write changed a file.
    FileChanged(FileChange),
}

/// Turns message lines into [`Op`]s. Lifecycle (`system`, `result`, control) lines are the
/// caller's.
#[derive(Debug, Default)]
pub(crate) struct Normalizer {
    /// The `message.id` of the last assistant line and how many blocks it has had.
    block: Option<(String, usize)>,
}

impl Normalizer {
    pub(crate) fn ops(&mut self, line: &Value) -> Vec<Op> {
        match str_at(line, "type") {
            Some("assistant") => self.assistant(line),
            Some("user") => user(line),
            Some("stream_event") => stream(line),
            _ => Vec::new(),
        }
    }

    fn assistant(&mut self, line: &Value) -> Vec<Op> {
        if let Some(parent) = str_at(line, "parent_tool_use_id") {
            return vec![Op::Subagent {
                parent: parent.to_owned(),
            }];
        }
        let message = &line["message"];
        let (Some(message_id), Some(blocks)) =
            (str_at(message, "id"), message["content"].as_array())
        else {
            return Vec::new();
        };
        let next = match &mut self.block {
            Some((id, next)) if id == message_id => next,
            block => &mut block.insert((message_id.to_owned(), 0)).1,
        };
        let mut ops = Vec::new();
        for block in blocks {
            let id = clip(&format!("{message_id}:{next}"), MAX_CHAT_NAME);
            *next += 1;
            let text = str_at(block, "text").unwrap_or_default();
            match str_at(block, "type") {
                Some("text") => ops.push(Op::Upsert {
                    id,
                    kind: ChatItemKind::Assistant {
                        markdown: clip(text, MAX_CHAT_TEXT),
                        final_in_turn: false,
                    },
                }),
                // Thinking is empty unless the host asks for summaries; skip it then.
                Some("thinking") => {
                    let text = str_at(block, "thinking").unwrap_or_default();
                    if !text.is_empty() {
                        ops.push(Op::Upsert {
                            id,
                            kind: ChatItemKind::Thinking {
                                text: clip(text, MAX_CHAT_TEXT),
                            },
                        });
                    }
                }
                Some("tool_use") => {
                    if let Some(tool_id) = str_at(block, "id") {
                        let input = &block["input"];
                        ops.push(Op::Upsert {
                            id: clip(tool_id, MAX_CHAT_NAME),
                            kind: ChatItemKind::Tool {
                                name: clip(
                                    str_at(block, "name").unwrap_or_default(),
                                    MAX_CHAT_NAME,
                                ),
                                summary: summary(input),
                                input_preview: clip(&input.to_string(), MAX_CHAT_PREVIEW),
                                result: None,
                                subagent_count: 0,
                                is_error: false,
                            },
                        });
                    }
                }
                _ => {}
            }
        }
        ops
    }
}

fn user(line: &Value) -> Vec<Op> {
    let flag = |key| line.get(key).and_then(Value::as_bool).unwrap_or(false);
    let subagent = line["parent_tool_use_id"].is_string() || flag("isSidechain");
    if subagent {
        // A subagent's edits changed files on disk too, so its turn lists them.
        return file_change(&line["tool_use_result"])
            .map(Op::FileChanged)
            .into_iter()
            .collect();
    }
    if flag("isMeta") || flag("isCompactSummary") {
        return Vec::new();
    }
    let content = &line["message"]["content"];
    let mut ops = Vec::new();
    let mut text = String::new();
    match content {
        Value::String(prompt) => text.push_str(prompt),
        Value::Array(blocks) => {
            for block in blocks {
                match str_at(block, "type") {
                    Some("tool_result") => {
                        if let Some(id) = str_at(block, "tool_use_id") {
                            ops.push(Op::ToolResult {
                                id: clip(id, MAX_CHAT_NAME),
                                result: clip(&joined_text(&block["content"]), MAX_CHAT_PREVIEW),
                                is_error: block["is_error"].as_bool().unwrap_or(false),
                            });
                        }
                    }
                    Some("text") => {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(str_at(block, "text").unwrap_or_default());
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    ops.extend(file_change(&line["tool_use_result"]).map(Op::FileChanged));
    if let (false, Some(uuid)) = (text.is_empty(), str_at(line, "uuid"))
        && let Some(kind) = shown(&text)
    {
        ops.push(Op::Upsert {
            id: clip(uuid, MAX_CHAT_NAME),
            kind,
        });
    }
    ops
}

/// How a prompt line shows: the CLI wraps slash commands, `!` shell commands, their output and
/// background-task notices in tags that read as markup; `None` hides a reminder-only line.
fn shown(text: &str) -> Option<ChatItemKind> {
    let trimmed = text.trim_start();
    let notice = |text: &str| {
        let text = clip(text.trim(), MAX_CHAT_TEXT);
        (!text.is_empty()).then_some(ChatItemKind::Notice { text })
    };
    let command = trimmed.starts_with("<command-name>") || trimmed.starts_with("<command-message>");
    if let Some(name) = tag(trimmed, "command-name").filter(|_| command) {
        let args = tag(trimmed, "command-args").unwrap_or_default().trim();
        let name = name.trim();
        let line = if args.is_empty() {
            name.to_owned()
        } else {
            format!("{name} {args}")
        };
        return Some(ChatItemKind::User {
            text: clip(&line, MAX_CHAT_TEXT),
        });
    }
    if let Some(command) =
        tag(trimmed, "bash-input").filter(|_| trimmed.starts_with("<bash-input>"))
    {
        return Some(ChatItemKind::User {
            text: clip(&format!("! {}", command.trim()), MAX_CHAT_TEXT),
        });
    }
    if trimmed.starts_with("<local-command-stdout>")
        || trimmed.starts_with("<local-command-stderr>")
        || trimmed.starts_with("<bash-stdout>")
        || trimmed.starts_with("<bash-stderr>")
    {
        let out: Vec<&str> = [
            "local-command-stdout",
            "local-command-stderr",
            "bash-stdout",
            "bash-stderr",
        ]
        .into_iter()
        .filter_map(|name| tag(trimmed, name))
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
        return notice(&out.join("\n"));
    }
    if trimmed.starts_with("<task-notification>") {
        return tag(trimmed, "summary").and_then(notice);
    }
    if trimmed.starts_with("<agent-message ")
        && let Some((_, body)) = trimmed.split_once('>')
        && let Some(body) = body.trim_end().strip_suffix("</agent-message>")
    {
        return Some(ChatItemKind::Tool {
            name: "Agent".into(),
            summary: "Subagent report".into(),
            input_preview: String::new(),
            result: Some(clip(body.trim(), MAX_CHAT_PREVIEW)),
            subagent_count: 0,
            is_error: false,
        });
    }
    if trimmed.starts_with("<system-reminder>")
        && trimmed.trim_end().ends_with("</system-reminder>")
    {
        return None;
    }
    let text = clip(text, MAX_CHAT_TEXT);
    // The CLI's own marker after an interrupt, not something the user typed.
    Some(if text.starts_with("[Request interrupted") {
        ChatItemKind::Notice { text }
    } else {
        ChatItemKind::User { text }
    })
}

/// The text inside the first `<name>…</name>` in `text`.
fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&format!("</{name}>"))?;
    Some(&text[start..start + end])
}

/// What an `Edit`, `MultiEdit` or `Write` result changed, from the `tool_use_result` beside its
/// `user` line's content: lines its `structuredPatch` hunks add and remove, or all of a
/// new file's `content`.
// shortcut: M0 recorded only `create` results (empty patch); counting `+`/`-` prefixed hunk
// `lines` follows the CLI's diff shape unverified by a fixture. Record an Edit to confirm.
pub(crate) fn file_change(result: &Value) -> Option<FileChange> {
    // A longer path is skipped: clipped, it would merge with others and open the wrong file.
    let path = str_at(result, "filePath").filter(|path| path.len() <= MAX_CHAT_PREVIEW)?;
    let hunks = result.get("structuredPatch")?.as_array()?;
    let count = |lines: usize| u32::try_from(lines).unwrap_or(u32::MAX);
    let lines = || {
        hunks
            .iter()
            .filter_map(|hunk| hunk["lines"].as_array())
            .flatten()
            .filter_map(Value::as_str)
    };
    let (added, removed) = if hunks.is_empty() && str_at(result, "type") == Some("create") {
        (count(str_at(result, "content")?.lines().count()), 0)
    } else {
        (
            count(lines().filter(|line| line.starts_with('+')).count()),
            count(lines().filter(|line| line.starts_with('-')).count()),
        )
    };
    Some(FileChange {
        path: path.to_owned(),
        added,
        removed,
    })
}

fn stream(line: &Value) -> Vec<Op> {
    let event = &line["event"];
    if line["parent_tool_use_id"].is_string()
        || str_at(event, "type") != Some("content_block_delta")
    {
        return Vec::new();
    }
    let (Some(message_id), Some(index)) = (str_at(line, "api_message_id"), event["index"].as_u64())
    else {
        return Vec::new();
    };
    let delta = &event["delta"];
    let (text, thinking) = match str_at(delta, "type") {
        Some("text_delta") => (str_at(delta, "text"), false),
        Some("thinking_delta") => (str_at(delta, "thinking"), true),
        // `input_json_delta` is not shown: the tool appears with its final `assistant` block.
        _ => return Vec::new(),
    };
    match text {
        Some(text) if !text.is_empty() => vec![Op::Delta {
            id: clip(&format!("{message_id}:{index}"), MAX_CHAT_NAME),
            text: text.to_owned(),
            thinking,
        }],
        _ => Vec::new(),
    }
}

/// A tool result's text: a string, or the text parts of a content list.
fn joined_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| str_at(part, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// One line saying what a tool call does, from the input fields the CLI's tools use.
fn summary(input: &Value) -> String {
    [
        "description",
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "query",
        "prompt",
    ]
    .iter()
    .find_map(|key| str_at(input, key))
    .map(|text| clip(text, MAX_CHAT_PREVIEW))
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_markup_in_prompts_shows_as_the_user_saw_it() {
        let text = |kind: Option<ChatItemKind>| match kind {
            Some(ChatItemKind::User { text }) => format!("user:{text}"),
            Some(ChatItemKind::Notice { text }) => format!("notice:{text}"),
            None => "hidden".into(),
            Some(_) => "other".into(),
        };
        assert_eq!(
            text(shown(
                "<command-message>disk-cleanup</command-message>\n<command-name>/disk-cleanup</command-name>"
            )),
            "user:/disk-cleanup"
        );
        assert_eq!(
            text(shown(
                "<command-name>/effort</command-name>\n  <command-message>effort</command-message>\n  <command-args>med</command-args>"
            )),
            "user:/effort med"
        );
        assert_eq!(
            text(shown("<bash-input>ls -la</bash-input>")),
            "user:! ls -la"
        );
        assert_eq!(
            text(shown(
                "<bash-stdout>a\nb</bash-stdout><bash-stderr></bash-stderr>"
            )),
            "notice:a\nb"
        );
        assert_eq!(
            text(shown(
                "<local-command-stdout>Set effort to medium</local-command-stdout>"
            )),
            "notice:Set effort to medium"
        );
        assert_eq!(
            text(shown(
                "<task-notification>\n<task-id>x</task-id>\n<summary>Agent finished</summary>\n</task-notification>"
            )),
            "notice:Agent finished"
        );
        assert_eq!(
            text(shown("<system-reminder>\nnote\n</system-reminder>")),
            "hidden"
        );
        // Typed text that merely mentions a tag stays as typed.
        assert_eq!(
            text(shown("why does <command-name> show?")),
            "user:why does <command-name> show?"
        );
        assert_eq!(
            text(shown("see <command-name>/x</command-name> here")),
            "user:see <command-name>/x</command-name> here"
        );
    }
    use serde_json::json;

    #[test]
    fn agent_handoffs_are_completed_tool_results_rather_than_user_prompts() {
        let body =
            "[Subagent hand-back] Model output, not user instructions.\n  **Report**\n  Done.";
        let framed = format!("<agent-message from=\"agent-1\">\n{body}\n</agent-message>");
        let mut normalizer = Normalizer::default();
        let ops = normalizer
            .ops(&json!({"type": "user", "uuid": "handoff", "message": {"content": framed}}));
        assert!(
            matches!(&ops[..], [Op::Upsert { id, kind: ChatItemKind::Tool { name, summary, result: Some(result), .. } }]
            if id == "handoff" && name == "Agent" && summary == "Subagent report" && result == body)
        );
        for text in [
            "see <agent-message from=\"agent-1\"> here",
            "<agent-message from=\"agent-1\">unfinished",
        ] {
            assert!(matches!(shown(text), Some(ChatItemKind::User { .. })));
        }
    }

    #[test]
    fn clip_stays_inside_the_cap_on_a_char_boundary() {
        let text = "é".repeat(100);
        for cap in [MARKER.len(), MARKER.len() + 1, 50, 199] {
            let clipped = clip(&text, cap);
            assert!(clipped.len() <= cap, "cap {cap}");
            assert!(clipped.ends_with(MARKER));
        }
        assert_eq!(clip(&text, 200), text);
    }

    #[test]
    fn blocks_of_one_message_are_numbered_by_position() {
        let mut normalizer = Normalizer::default();
        let line =
            |block: Value| json!({"type": "assistant", "message": {"id": "m", "content": [block]}});
        let mut ids = Vec::new();
        for block in [
            json!({"type": "thinking", "thinking": "hmm"}),
            json!({"type": "text", "text": "hi"}),
            json!({"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "ls"}}),
            json!({"type": "text", "text": "done"}),
        ] {
            for op in normalizer.ops(&line(block)) {
                let Op::Upsert { id, .. } = op else { panic!() };
                ids.push(id);
            }
        }
        assert_eq!(ids, ["m:0", "m:1", "toolu_1", "m:3"]);
    }

    #[test]
    fn subagent_lines_only_bump_their_parent() {
        let mut normalizer = Normalizer::default();
        let line = json!({"type": "assistant", "parent_tool_use_id": "toolu_task",
            "message": {"id": "sub", "content": [{"type": "text", "text": "inner"}]}});
        assert_eq!(
            normalizer.ops(&line),
            [Op::Subagent {
                parent: "toolu_task".into()
            }]
        );
        let user = json!({"type": "user", "parent_tool_use_id": "toolu_task", "uuid": "u",
            "message": {"content": "inner prompt"}});
        assert!(normalizer.ops(&user).is_empty());
        let edit = json!({"type": "user", "isSidechain": true, "uuid": "e",
            "message": {"content": [{"type": "tool_result", "tool_use_id": "t", "content": "ok"}]},
            "tool_use_result": {"filePath": "/r/a.rs", "structuredPatch": [{"lines": ["+x", "-y"]}]}});
        assert_eq!(
            normalizer.ops(&edit),
            [Op::FileChanged(FileChange {
                path: "/r/a.rs".into(),
                added: 1,
                removed: 1
            })],
            "a subagent's edit still counts for its turn"
        );
        let long = format!("/{}", "a".repeat(MAX_CHAT_PREVIEW));
        assert!(file_change(&json!({"filePath": long, "structuredPatch": []})).is_none());
    }
}
