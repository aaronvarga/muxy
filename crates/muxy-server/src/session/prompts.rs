//! Finds the rows a prompt rail jumps to: shell prompts the terminal marked,
//! and messages sent to coding agents, which mark nothing.

use muxy_protocol::{MAX_PROMPT_TEXT, PromptMark};

/// Marks for `rows` of `(row, text, shell prompt)`, oldest first. `under_rule`
/// says whether the row before them was a rule; the result says the same of
/// their last row, so a later scan can carry on.
pub(crate) fn marks(
    rows: impl IntoIterator<Item = (usize, String, bool)>,
    mut under_rule: bool,
) -> (Vec<PromptMark>, bool) {
    let mut marks = Vec::new();
    for (row, text, prompt) in rows {
        let found = if prompt {
            Some(text.trim())
        } else if under_rule {
            // Claude Code's input box sits between two rules; it isn't sent yet.
            None
        } else {
            sent_message(&text)
        };
        if let Some(found) = found {
            marks.push(PromptMark {
                row: row as u64,
                text: fit(found),
            });
        }
        under_rule = is_rule(&text);
    }
    (marks, under_rule)
}

/// A message Claude Code (`❯`) or Codex (`›`) shows as sent. Menus that use
/// the same arrow are indented or boxed, so only the first column counts.
fn sent_message(text: &str) -> Option<&str> {
    let message = text
        .strip_prefix("❯ ")
        .or_else(|| text.strip_prefix("› "))?
        .trim();
    (!message.is_empty()).then_some(message)
}

fn is_rule(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty() && text.chars().all(|ch| ch == '─')
}

fn fit(text: &str) -> String {
    let mut end = text.len().min(MAX_PROMPT_TEXT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(lines: &[(&str, bool)]) -> Vec<(u64, String)> {
        marks(
            lines
                .iter()
                .enumerate()
                .map(|(row, (text, prompt))| (row, (*text).to_owned(), *prompt)),
            false,
        )
        .0
        .into_iter()
        .map(|mark| (mark.row, mark.text))
        .collect()
    }

    #[test]
    fn sent_agent_messages_are_marked_but_the_input_box_and_menus_are_not() {
        let screen = [
            ("aaron@mac muxy % claude", true),
            ("❯ why does the tile flicker?", false),
            ("", false),
            ("⏺ The pulse repaints the grid.", false),
            ("  ❯ 1. Yes", false),
            ("│ ❯ Approve │", false),
            ("› add a test for it", false),
            ("────────────────────", false),
            ("❯ draft I haven't sent", false),
            ("────────────────────", false),
        ];
        assert_eq!(
            found(&screen),
            [
                (0, "aaron@mac muxy % claude".to_owned()),
                (1, "why does the tile flicker?".to_owned()),
                (6, "add a test for it".to_owned()),
            ]
        );
    }

    #[test]
    fn a_rule_at_the_end_of_one_scan_hides_the_input_box_at_the_start_of_the_next() {
        let (first, under_rule) = marks([(0, "────".to_owned(), false)], false);
        assert!(first.is_empty() && under_rule);
        let (next, _) = marks([(1, "❯ draft".to_owned(), false)], under_rule);
        assert!(next.is_empty());
    }

    #[test]
    fn long_text_is_cut_on_a_character_boundary() {
        let long = format!("❯ {}", "é".repeat(MAX_PROMPT_TEXT));
        let marks = found(&[(&long, false)]);
        assert!(marks[0].1.len() <= MAX_PROMPT_TEXT);
        assert!(marks[0].1.chars().all(|ch| ch == 'é'));
    }
}
