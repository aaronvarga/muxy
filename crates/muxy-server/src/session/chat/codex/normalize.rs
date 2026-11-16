//! Codex's native items and requests. No Claude wire messages are constructed here.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use muxy_protocol::{
    ChatAnswer, ChatApprovalChoice, ChatItemKind, ChatProvider, ChatQuestion, ChatQuestionOption,
    ChatState, MAX_CHAT_NAME, MAX_CHAT_PENDING, MAX_CHAT_PREVIEW, MAX_CHAT_TEXT, PermissionKind,
    PermissionPrompt, PermissionRemoved, PermissionResponse,
};
use ring::digest::{SHA256, digest};
use serde_json::{Value, json};

use super::super::boundary::Change;
use super::super::normalize::{clip, str_at};

struct Pending {
    native_id: Value,
    kind: RequestKind,
    answered: bool,
    bytes: usize,
}

enum RequestKind {
    Choices(Vec<Value>),
    Questions(Vec<(String, String, Vec<String>, bool)>),
}

pub(crate) struct Parser {
    thread: String,
    epoch: u64,
    state: ChatState,
    pending: HashMap<String, Pending>,
    pending_bytes: usize,
    unsupported: HashSet<String>,
    files: HashMap<String, String>,
    active_turn: Option<String>,
}

impl Parser {
    pub(crate) fn new(thread: String, epoch: u64) -> Self {
        Self {
            thread,
            epoch,
            state: ChatState {
                provider: Some(ChatProvider::Codex),
                quiescent: true,
                ..ChatState::default()
            },
            pending: HashMap::new(),
            pending_bytes: 0,
            unsupported: HashSet::new(),
            files: HashMap::new(),
            active_turn: None,
        }
    }

    pub(crate) fn thread(&self) -> &str {
        &self.thread
    }

    pub(crate) fn state(&self) -> &ChatState {
        &self.state
    }

    pub(crate) fn target(&mut self, target: muxy_protocol::ChatTarget, shared: bool) {
        self.state.target = Some(target);
        self.state.shared_engine = shared;
    }

    pub(crate) fn configure(
        &mut self,
        model: Option<String>,
        effort: Option<String>,
        busy: bool,
    ) -> Change {
        self.state.model = model;
        self.state.effort = effort;
        self.state.busy = busy;
        self.state.quiescent = !busy;
        Change::State(self.state.clone())
    }

    fn remember_file(&mut self, item: &Value) {
        if item["type"] == "fileChange"
            && let Some(id) = str_at(item, "id")
        {
            let detail = item["changes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|change| {
                    format!(
                        "{}\n{}",
                        str_at(change, "path").unwrap_or_default(),
                        str_at(change, "diff").unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            if self.files.len() < 32
                && self.files.values().map(String::len).sum::<usize>() + detail.len()
                    <= MAX_CHAT_PENDING
            {
                self.files.insert(id.to_owned(), detail);
            }
        }
    }

    pub(crate) fn history(&mut self, turns: &[Value]) -> Vec<Change> {
        let mut changes = Vec::new();
        for turn in turns {
            let Some(id) = str_at(turn, "id").filter(|id| valid_id(id)) else {
                continue;
            };
            if turn["status"] == "inProgress" {
                self.active_turn = Some(id.into());
            }
            for item in turn["items"].as_array().into_iter().flatten() {
                self.remember_file(item);
                if let Some(change) = item_change(id, item) {
                    changes.push(change);
                }
            }
            if str_at(turn, "status") != Some("inProgress") {
                changes.push(end(id, turn));
            }
        }
        changes
    }

    pub(crate) fn event(&mut self, message: &Value) -> Vec<Change> {
        let params = &message["params"];
        if str_at(params, "threadId") != Some(self.thread.as_str()) {
            return Vec::new();
        }
        let method = str_at(message, "method").unwrap_or_default();
        if message.get("id").is_some() {
            return self.request(method, &message["id"], params);
        }
        let turn = str_at(params, "turnId").unwrap_or_default();
        match method {
            "item/started" | "item/completed" if valid_id(turn) => {
                let item = &params["item"];
                self.remember_file(item);
                item_change(turn, item).into_iter().collect()
            }
            "item/agentMessage/delta"
            | "item/reasoning/summaryTextDelta"
            | "item/reasoning/textDelta" => {
                let Some(id) = str_at(params, "itemId").filter(|id| valid_id(id) && valid_id(turn))
                else {
                    return Vec::new();
                };
                vec![Change::Delta {
                    turn: turn.to_owned(),
                    id: id.to_owned(),
                    text: clip(str_at(params, "delta").unwrap_or_default(), MAX_CHAT_TEXT),
                    thinking: method != "item/agentMessage/delta",
                }]
            }
            "turn/started" => {
                let id = str_at(&params["turn"], "id");
                if self.active_turn.as_deref() != id {
                    self.files.clear();
                    self.active_turn = id.map(str::to_owned);
                }
                self.state.busy = true;
                self.state.quiescent = false;
                vec![Change::State(self.state.clone())]
            }
            "turn/completed" => {
                self.state.busy = false;
                self.state.quiescent = true;
                let mut changes = Vec::new();
                if let Some(id) = str_at(&params["turn"], "id").filter(|id| valid_id(id)) {
                    changes.push(end(id, &params["turn"]));
                }
                changes.push(Change::State(self.state.clone()));
                changes
            }
            "thread/status/changed" => {
                let status = str_at(&params["status"], "type").unwrap_or_default();
                if matches!(status, "idle" | "active") {
                    self.state.busy = status == "active";
                    self.state.quiescent = status == "idle";
                    vec![Change::State(self.state.clone())]
                } else {
                    Vec::new()
                }
            }
            "thread/settings/updated" => {
                let settings = &params["threadSettings"];
                if let Some(model) = str_at(settings, "model") {
                    self.state.model = Some(clip(model, MAX_CHAT_NAME));
                }
                if let Some(effort) = str_at(settings, "effort") {
                    self.state.effort = Some(clip(effort, MAX_CHAT_NAME));
                }
                vec![Change::State(self.state.clone())]
            }
            "serverRequest/resolved" => {
                let key = self
                    .pending
                    .iter()
                    .find(|(_, pending)| pending.native_id == params["requestId"])
                    .map(|(key, _)| key.clone());
                if let Some(key) = key {
                    if let Some(pending) = self.pending.remove(&key) {
                        self.pending_bytes -= pending.bytes;
                    }
                    vec![Change::Resolved {
                        id: key,
                        reason: PermissionRemoved::Answered,
                    }]
                } else {
                    Vec::new()
                }
            }
            "error" if params["willRetry"] != true => vec![Change::Notice(
                "Codex reported an error. Review its Console for details.".into(),
            )],
            _ => Vec::new(),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Approval variants preserve native choices and validate the complete prompt before insertion"
    )]
    fn request(&mut self, method: &str, native_id: &Value, params: &Value) -> Vec<Change> {
        let turn = str_at(params, "turnId").unwrap_or_default();
        if !(native_id.is_string() || native_id.is_i64() || native_id.is_u64())
            || native_id.to_string().len() > MAX_CHAT_NAME
            || !valid_id(turn)
        {
            return self.unsupported(method);
        }
        let key = request_key(self.epoch, &self.thread, turn, native_id);
        if self.pending.contains_key(&key) {
            return Vec::new();
        }
        let (kind, encoding) = match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                let file = method.contains("fileChange");
                let decisions = params["availableDecisions"].as_array().cloned().unwrap_or_else(||{
                    let mut decisions=vec![json!("accept")];
                    if params["kind"]!="humanCommand"{decisions.push(json!("acceptForSession"));}
                    if !file && let Some(rule)=params["proposedExecpolicyAmendment"].as_array().filter(|rule|!rule.is_empty()){
                        decisions.push(json!({"acceptWithExecpolicyAmendment":{"execpolicy_amendment":rule}}));
                    }
                    if !file {for amendment in params["proposedNetworkPolicyAmendments"].as_array().into_iter().flatten(){
                        decisions.push(json!({"applyNetworkPolicyAmendment":{"network_policy_amendment":amendment}}));
                    }}
                    decisions.extend([json!("decline"),json!("cancel")]);decisions
                });
                let Some(choices) = decisions
                    .iter()
                    .enumerate()
                    .map(|(at, decision)| choice(at, decision))
                    .collect::<Option<Vec<_>>>()
                else {
                    return self.unsupported(method);
                };
                let mut detail = [
                    "command",
                    "cwd",
                    "reason",
                    "additionalPermissions",
                    "networkApprovalContext",
                    "environmentId",
                    "grantRoot",
                ]
                .into_iter()
                .filter_map(|field| {
                    let value = params.get(field).filter(|value| !value.is_null())?;
                    Some(format!(
                        "{field}: {}",
                        value
                            .as_str()
                            .map_or_else(|| value.to_string(), str::to_owned)
                    ))
                })
                .collect::<Vec<_>>()
                .join("\n\n");
                if file {
                    let Some(changes) = str_at(params, "itemId").and_then(|id| self.files.get(id))
                    else {
                        return self.unsupported(method);
                    };
                    detail = format!("{detail}\n\n{changes}");
                } else if !params["command"].is_string() {
                    return self.unsupported(method);
                }
                let mut choices = choices;
                if file {
                    for choice in &mut choices {
                        if choice.label == "Allow for session" {
                            choice.scope = "These files for this native session".into();
                        }
                    }
                }
                (
                    PermissionKind::Native {
                        title: if file {
                            "Codex file approval"
                        } else {
                            "Codex command approval"
                        }
                        .into(),
                        detail,
                        choices,
                    },
                    RequestKind::Choices(decisions),
                )
            }
            "item/tool/requestUserInput" => {
                let Some(native) = params["questions"].as_array() else {
                    return self.unsupported(method);
                };
                let mut questions = Vec::new();
                let mut mapping = Vec::new();
                for question in native {
                    if question["isSecret"] == true {
                        return self.unsupported(method);
                    }
                    let (Some(id), Some(text), Some(header)) = (
                        str_at(question, "id"),
                        str_at(question, "question"),
                        str_at(question, "header"),
                    ) else {
                        return self.unsupported(method);
                    };
                    if mapping.iter().any(|(old, _, _, _)| old == text) {
                        return self.unsupported(method);
                    }
                    let options: Vec<_> = question["options"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|option| {
                            Some(ChatQuestionOption {
                                label: str_at(option, "label")?.to_owned(),
                                description: str_at(option, "description")?.to_owned(),
                                preview: String::new(),
                            })
                        })
                        .collect();
                    mapping.push((
                        text.to_owned(),
                        id.to_owned(),
                        options.iter().map(|option| option.label.clone()).collect(),
                        question["isOther"] == true || options.is_empty(),
                    ));
                    let allow_other = question["isOther"] == true || options.is_empty();
                    questions.push(ChatQuestion {
                        question: text.to_owned(),
                        header: header.to_owned(),
                        options,
                        multi_select: false,
                        allow_other: Some(allow_other),
                    });
                }
                (
                    PermissionKind::Questions { questions },
                    RequestKind::Questions(mapping),
                )
            }
            _ => return self.unsupported(method),
        };
        let prompt = PermissionPrompt {
            request_id: key.clone(),
            tool_use_id: str_at(params, "itemId").map(str::to_owned),
            kind,
        };
        let Ok(bytes) = prompt.validate() else {
            return self.unsupported(method);
        };
        if self.pending.len() >= 32 || self.pending_bytes + bytes > MAX_CHAT_PENDING {
            return self.unsupported(method);
        }
        self.pending_bytes += bytes;
        self.pending.insert(
            key,
            Pending {
                native_id: native_id.clone(),
                kind: encoding,
                answered: false,
                bytes,
            },
        );
        vec![Change::Pending(prompt)]
    }

    fn unsupported(&mut self, method: &str) -> Vec<Change> {
        // Shared-daemon requests must remain answerable by another capable client.
        if self.unsupported.len() < 16 && self.unsupported.insert(method.to_owned()) {
            vec![Change::Notice("This Codex request needs Console or another capable client. Muxy has left it pending.".into())]
        } else {
            Vec::new()
        }
    }

    pub(crate) fn response(
        &mut self,
        key: &str,
        response: PermissionResponse,
    ) -> Result<Value, &'static str> {
        let pending = self
            .pending
            .get_mut(key)
            .ok_or("This approval has already been resolved")?;
        if pending.answered {
            return Err("An answer is already pending");
        }
        let result = match (&pending.kind, response) {
            (RequestKind::Choices(decisions), PermissionResponse::NativeChoice { choice }) => {
                let at = choice
                    .parse::<usize>()
                    .map_err(|_| "Unknown approval choice")?;
                json!({"decision": decisions.get(at).ok_or("Unknown approval choice")?})
            }
            (RequestKind::Choices(decisions), PermissionResponse::Dismiss { .. }) => {
                let decline = decisions
                    .iter()
                    .find(|decision| decision.as_str() == Some("decline"))
                    .ok_or("This request must be answered in Console")?;
                json!({"decision": decline})
            }
            (RequestKind::Questions(mapping), PermissionResponse::Questions(answers)) => {
                question_response(mapping, &answers)?
            }
            _ => return Err("The answer does not match this native request"),
        };
        pending.answered = true;
        Ok(json!({"id": pending.native_id, "result": result}))
    }
}

fn question_response(
    mapping: &[(String, String, Vec<String>, bool)],
    answers: &[ChatAnswer],
) -> Result<Value, &'static str> {
    if answers.len() != mapping.len() {
        return Err("Answer every question");
    }
    let mut result = serde_json::Map::new();
    for (text, id, options, other) in mapping {
        let matched: Vec<_> = answers
            .iter()
            .filter(|answer| answer.question == *text)
            .collect();
        if matched.len() != 1
            || matched[0].answers.is_empty()
            || (!other
                && matched[0]
                    .answers
                    .iter()
                    .any(|answer| !options.contains(answer)))
        {
            return Err("The answer does not match the offered options");
        }
        result.insert(id.clone(), json!({"answers": matched[0].answers}));
    }
    Ok(json!({"answers": result}))
}

fn request_key(epoch: u64, thread: &str, turn: &str, id: &Value) -> String {
    let input = json!([epoch, thread, turn, id]).to_string();
    let hash = digest(&SHA256, input.as_bytes());
    let mut key = String::from("codex:");
    for byte in hash.as_ref() {
        let _ = write!(key, "{byte:02x}");
    }
    key
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_CHAT_NAME
}

fn choice(at: usize, value: &Value) -> Option<ChatApprovalChoice> {
    let (label, scope) = match value.as_str() {
        Some("accept") => ("Allow once", "This command only".into()),
        Some("acceptForSession") => (
            "Allow for session",
            "The provider's session-scoped approval cache".into(),
        ),
        Some("decline") => ("Deny", "Continue without this command".into()),
        Some("cancel") => ("Deny and interrupt", "Stop the current turn".into()),
        _ if value.get("acceptWithExecpolicyAmendment").is_some() => (
            "Allow and save rule",
            value["acceptWithExecpolicyAmendment"].to_string(),
        ),
        _ if value.get("applyNetworkPolicyAmendment").is_some() => (
            "Save network rule",
            value["applyNetworkPolicyAmendment"].to_string(),
        ),
        _ => return None,
    };
    Some(ChatApprovalChoice {
        id: at.to_string(),
        label: label.into(),
        scope,
    })
}

fn end(id: &str, turn: &Value) -> Change {
    Change::End {
        turn: id.to_owned(),
        duration_ms: turn["completedAtMs"]
            .as_u64()
            .zip(turn["startedAtMs"].as_u64())
            .map_or(0, |(end, start)| end.saturating_sub(start)),
        is_error: turn["status"] == "failed",
    }
}

fn item_change(turn: &str, item: &Value) -> Option<Change> {
    let id = str_at(item, "id").filter(|id| valid_id(id))?;
    let text = |field| str_at(item, field).unwrap_or_default();
    let kind = match text("type") {
        "userMessage" => ChatItemKind::User {
            text: clip(
                &item["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|part| match str_at(part, "type") {
                        Some("text") => str_at(part, "text").map(str::to_owned),
                        Some("image" | "localImage") => Some("[Image]".into()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                MAX_CHAT_TEXT,
            ),
        },
        "agentMessage" => ChatItemKind::Assistant {
            markdown: clip(text("text"), MAX_CHAT_TEXT),
            final_in_turn: text("phase") == "final_answer",
        },
        "reasoning" => ChatItemKind::Thinking {
            text: clip(
                &["summary", "content"]
                    .iter()
                    .flat_map(|field| item[*field].as_array().into_iter().flatten())
                    .filter_map(|part| part.as_str().or_else(|| str_at(part, "text")))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                MAX_CHAT_TEXT,
            ),
        },
        "commandExecution" => ChatItemKind::Tool {
            name: "shell".into(),
            summary: clip(text("command"), MAX_CHAT_PREVIEW),
            input_preview: clip(text("cwd"), MAX_CHAT_PREVIEW),
            result: (text("status") != "inProgress")
                .then(|| clip(text("aggregatedOutput"), MAX_CHAT_PREVIEW)),
            subagent_count: 0,
            is_error: matches!(text("status"), "failed" | "declined")
                || item["exitCode"].as_i64().is_some_and(|code| code != 0),
        },
        "mcpToolCall" | "dynamicToolCall" => ChatItemKind::Tool {
            name: clip(text("tool"), MAX_CHAT_NAME),
            summary: clip(text("server"), MAX_CHAT_PREVIEW),
            input_preview: clip(&item["arguments"].to_string(), MAX_CHAT_PREVIEW),
            result: (text("status") != "inProgress")
                .then(|| clip(&item["result"].to_string(), MAX_CHAT_PREVIEW)),
            subagent_count: 0,
            is_error: text("status") == "failed"
                || !item["error"].is_null()
                || item["result"]["isError"] == true,
        },
        "fileChange" => ChatItemKind::Tool {
            name: "apply_patch".into(),
            summary: "File changes".into(),
            input_preview: clip(&item["changes"].to_string(), MAX_CHAT_PREVIEW),
            result: (text("status") != "inProgress")
                .then(|| clip(text("status"), MAX_CHAT_PREVIEW)),
            subagent_count: 0,
            is_error: matches!(text("status"), "failed" | "declined"),
        },
        other => ChatItemKind::Tool {
            name: clip(other, MAX_CHAT_NAME),
            summary: clip(text("query"), MAX_CHAT_PREVIEW),
            input_preview: String::new(),
            result: Some(clip(text("status"), MAX_CHAT_PREVIEW)),
            subagent_count: 0,
            is_error: text("status") == "failed",
        },
    };
    Some(Change::Item {
        turn: turn.to_owned(),
        id: id.to_owned(),
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approvals_are_typed_scoped_replayed_and_resolved_by_other_clients() {
        let mut parser = Parser::new("thread".into(), 7);
        let request = |id| {
            json!({"id": id, "method": "item/commandExecution/requestApproval", "params": {
            "threadId":"thread", "turnId":"turn", "itemId":"tool", "command":"echo hello",
            "availableDecisions":["accept","acceptForSession","decline"] }})
        };
        let numeric = parser.event(&request(json!(9)));
        let Change::Pending(prompt) = &numeric[0] else {
            panic!("missing prompt")
        };
        let key = prompt.request_id.clone();
        assert!(parser.event(&request(json!(9))).is_empty());
        assert_eq!(parser.event(&request(json!("9"))).len(), 1);
        let answer = parser
            .response(
                &key,
                PermissionResponse::NativeChoice { choice: "1".into() },
            )
            .unwrap();
        assert_eq!(answer["id"], 9);
        assert_eq!(answer["result"]["decision"], "acceptForSession");
        assert!(
            parser
                .response(
                    &key,
                    PermissionResponse::NativeChoice { choice: "0".into() }
                )
                .is_err()
        );
        let resolved = parser.event(&json!({"method":"serverRequest/resolved", "params":{"threadId":"thread","requestId":9}}));
        assert!(matches!(&resolved[0], Change::Resolved { id, .. } if id == &key));
        assert!(
            parser
                .response(
                    &key,
                    PermissionResponse::NativeChoice { choice: "0".into() }
                )
                .is_err()
        );
        assert_ne!(request_key(8, "thread", "turn", &json!(9)), key);
        assert_ne!(request_key(7, "thread", "other", &json!(9)), key);
    }

    #[test]
    fn unsupported_shared_requests_are_never_denied_or_answered() {
        let mut parser = Parser::new("thread".into(), 1);
        let changes = parser.event(&json!({"id":1,"method":"future/approval", "params":{"threadId":"thread","turnId":"turn"}}));
        assert!(matches!(changes.as_slice(), [Change::Notice(_)]));
        assert!(parser.pending.is_empty());
    }
}
