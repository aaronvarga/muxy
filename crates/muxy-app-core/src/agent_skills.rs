//! Skills and commands a coding agent offers, read from the same folders the
//! agent reads, so Composer can complete them. The agents expose no listing
//! API, so this is a close approximation, not the agent's own list.
//!
//! shortcut: built-in commands are a hand-kept list (Claude Code 2.1, Codex
//! 0.166); refresh them when either agent adds or renames commands.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Skill {
    /// What Composer inserts, including the trigger: `/review`, `$imagegen`.
    pub text: String,
    pub description: String,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Agent {
    Claude,
    Codex,
}

const CLAUDE_BUILTINS: &[(&str, &str)] = &[
    ("/add-dir", "Add a new working directory"),
    ("/clear", "Start a new session with empty context"),
    (
        "/compact",
        "Free up context by summarizing the conversation so far",
    ),
    ("/config", "Open settings"),
    ("/context", "Visualize current context usage"),
    ("/copy", "Copy Claude's last response to clipboard"),
    (
        "/doctor",
        "Diagnose and verify your Claude Code installation",
    ),
    ("/effort", "Set effort level for model usage"),
    ("/exit", "Exit the REPL"),
    (
        "/export",
        "Export the current conversation to a file or clipboard",
    ),
    ("/fast", "Toggle fast mode"),
    ("/feedback", "Send feedback to Anthropic or report a bug"),
    ("/help", "Show help and available commands"),
    ("/hooks", "View hook configurations for tool events"),
    ("/ide", "Manage IDE integrations and show status"),
    (
        "/init",
        "Initialize a new CLAUDE.md file with codebase documentation",
    ),
    ("/login", "Sign in with your Anthropic account"),
    ("/logout", "Sign out from your Anthropic account"),
    ("/mcp", "Manage MCP servers"),
    ("/memory", "Edit CLAUDE.md files and memory settings"),
    ("/model", "Set the AI model"),
    ("/output-style", "List output styles or switch to one"),
    (
        "/permissions",
        "Manage allow and deny tool permission rules",
    ),
    ("/plan", "Enable plan mode or view the current session plan"),
    ("/plugin", "Manage Claude Code plugins"),
    ("/release-notes", "View release notes"),
    ("/rename", "Rename the current conversation"),
    ("/resume", "Resume a previous conversation"),
    ("/review", "Review a pull request"),
    (
        "/rewind",
        "Restore the code and/or conversation to a previous point",
    ),
    (
        "/security-review",
        "Complete a security review of the pending changes",
    ),
    ("/skills", "List available skills"),
    (
        "/status",
        "Show version, model, account and connectivity status",
    ),
    ("/statusline", "Set up the status line"),
    (
        "/tasks",
        "View and manage everything running in the background",
    ),
    ("/terminal-setup", "Set up terminal key bindings"),
    ("/theme", "Change the theme"),
    (
        "/usage",
        "Show session cost, plan usage, and activity stats",
    ),
];

const CODEX_BUILTINS: &[(&str, &str)] = &[
    ("/clear", "Clear the terminal and start a new chat"),
    (
        "/compact",
        "Summarize conversation to prevent hitting the context limit",
    ),
    ("/copy", "Copy the last response or part of it"),
    ("/diff", "Show git diff (including untracked files)"),
    ("/exit", "Exit Codex"),
    ("/export", "Export the conversation as markdown"),
    ("/feedback", "Send logs to maintainers"),
    ("/fork", "Fork the current chat"),
    (
        "/init",
        "Create an AGENTS.md file with instructions for Codex",
    ),
    ("/logout", "Log out of Codex"),
    ("/mcp", "List configured MCP tools"),
    ("/mention", "Mention a file"),
    ("/model", "Choose what model and reasoning effort to use"),
    ("/new", "Start a new chat during a conversation"),
    ("/permissions", "Choose what Codex is allowed to do"),
    ("/plan", "Switch to Plan mode"),
    ("/ps", "List background terminals"),
    ("/rename", "Rename the current thread"),
    ("/resume", "Resume a saved chat"),
    ("/review", "Review my current changes and find issues"),
    (
        "/skills",
        "Use skills to improve how Codex performs specific tasks",
    ),
    (
        "/status",
        "Show current session configuration and token usage",
    ),
    (
        "/statusline",
        "Configure which items appear in the status line",
    ),
    ("/theme", "Choose a syntax highlighting theme"),
    ("/usage", "View account usage"),
];

/// Every skill and command `agent` offers in `project`, sorted and unique.
pub fn discover(agent: Agent, home: &Path, project: Option<&Path>) -> Vec<Skill> {
    let mut found = BTreeMap::new();
    let mut add = |text: String, description: String| {
        found.entry(text).or_insert(description);
    };
    match agent {
        Agent::Claude => {
            let root = std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| home.join(".claude"));
            let mut roots = vec![root.clone()];
            roots.extend(project.map(|project| project.join(".claude")));
            for root in &roots {
                skills(&root.join("skills"), "/", "", &mut add);
                commands(&root.join("commands"), "/", "", &mut add);
            }
            for (plugin, path) in claude_plugins(&root, project) {
                let prefix = format!("{plugin}:");
                skills(&path.join("skills"), "/", &prefix, &mut add);
                commands(&path.join("commands"), "/", &prefix, &mut add);
            }
        }
        Agent::Codex => {
            let root = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| home.join(".codex"));
            let mut roots = vec![root.join("skills"), home.join(".agents/skills")];
            if let Some(project) = project {
                roots.push(project.join(".agents/skills"));
                roots.push(project.join(".codex/skills"));
            }
            for root in &roots {
                skills(root, "$", "", &mut add);
                skills(&root.join(".system"), "$", "", &mut add);
            }
            commands(&root.join("prompts"), "/", "prompts:", &mut add);
            for (plugin, path) in codex_plugins(&root) {
                skills(&path.join("skills"), "$", &format!("{plugin}:"), &mut add);
            }
        }
    }
    let builtins = match agent {
        Agent::Claude => CLAUDE_BUILTINS,
        Agent::Codex => CODEX_BUILTINS,
    };
    for (text, description) in builtins {
        add((*text).to_owned(), (*description).to_owned());
    }
    found
        .into_iter()
        .map(|(text, description)| Skill { text, description })
        .collect()
}

/// `<dir>/<skill>/SKILL.md`, named by its front matter or its folder.
fn skills(dir: &Path, trigger: &str, prefix: &str, add: &mut impl FnMut(String, String)) {
    for entry in entries(dir) {
        let Ok(text) = std::fs::read_to_string(entry.join("SKILL.md")) else {
            continue;
        };
        let (name, description) = front_matter(&text);
        let name = name.unwrap_or_else(|| file_name(&entry));
        if !name.is_empty() {
            add(format!("{trigger}{prefix}{name}"), description);
        }
    }
}

/// `<dir>/**/<command>.md`; a subfolder becomes part of the name: `git:commit`.
fn commands(dir: &Path, trigger: &str, prefix: &str, add: &mut impl FnMut(String, String)) {
    fn walk(dir: &Path, scope: &str, found: &mut Vec<(String, PathBuf)>) {
        for entry in entries(dir) {
            let name = file_name(&entry);
            if entry.is_dir() {
                walk(&entry, &format!("{scope}{name}:"), found);
            } else if let Some(stem) = name.strip_suffix(".md") {
                found.push((format!("{scope}{stem}"), entry));
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, "", &mut found);
    for (name, path) in found {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let (_, description) = front_matter(&text);
        add(format!("{trigger}{prefix}{name}"), description);
    }
}

/// Claude plugins enabled in user, project or local settings, with their folders.
fn claude_plugins(root: &Path, project: Option<&Path>) -> Vec<(String, PathBuf)> {
    let mut enabled = BTreeMap::new();
    let mut settings = vec![root.join("settings.json")];
    if let Some(project) = project {
        settings.push(project.join(".claude/settings.json"));
        settings.push(project.join(".claude/settings.local.json"));
    }
    for path in settings {
        if let Some(plugins) = json(&path)
            .and_then(|value| value.get("enabledPlugins").cloned())
            .and_then(|value| value.as_object().cloned())
        {
            for (key, value) in plugins {
                enabled.insert(key, value.as_bool() == Some(true));
            }
        }
    }
    let Some(installed) = json(&root.join("plugins/installed_plugins.json"))
        .and_then(|value| value.get("plugins").cloned())
        .and_then(|value| value.as_object().cloned())
    else {
        return Vec::new();
    };
    installed
        .into_iter()
        .filter(|(key, _)| enabled.get(key) == Some(&true))
        .filter_map(|(key, installs)| {
            let path = installs.as_array()?.iter().find_map(|install| {
                let scope = install.get("projectPath").and_then(|path| path.as_str());
                (scope.is_none() || scope.map(Path::new) == project)
                    .then(|| install.get("installPath")?.as_str().map(PathBuf::from))
                    .flatten()
            })?;
            Some((key.split('@').next()?.to_owned(), path))
        })
        .collect()
}

/// Codex plugins enabled in `config.toml`, each at its newest cached version.
fn codex_plugins(root: &Path) -> Vec<(String, PathBuf)> {
    let Some(config) = std::fs::read_to_string(root.join("config.toml"))
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
    else {
        return Vec::new();
    };
    let Some(plugins) = config.get("plugins").and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    plugins
        .iter()
        .filter(|(_, value)| value.get("enabled").and_then(toml::Value::as_bool) == Some(true))
        .filter_map(|(key, _)| {
            let (plugin, marketplace) = key.split_once('@')?;
            let versions = entries(&root.join("plugins/cache").join(marketplace).join(plugin));
            Some((plugin.to_owned(), versions.into_iter().max()?))
        })
        .collect()
}

/// The `name` and `description` of a Markdown file's YAML front matter. A file
/// without one is described by its first line of text.
fn front_matter(text: &str) -> (Option<String>, String) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        let first = text.lines().map(str::trim).find(|line| !line.is_empty());
        return (
            None,
            first
                .unwrap_or_default()
                .trim_start_matches('#')
                .trim()
                .to_owned(),
        );
    }
    let mut name = None;
    let mut description = String::new();
    let mut folding = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            break;
        }
        if folding && line.starts_with([' ', '\t']) {
            if !description.is_empty() {
                description.push(' ');
            }
            description.push_str(line.trim());
            continue;
        }
        folding = false;
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(unquote(value)).filter(|value| !value.is_empty());
        } else if let Some(value) = line.strip_prefix("description:") {
            description = unquote(value);
            folding = matches!(description.as_str(), "" | ">" | "|" | ">-" | "|-");
            if folding {
                description.clear();
            }
        }
    }
    (name, description)
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
        .to_owned()
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| !file_name(path).starts_with('.'))
        .collect();
    entries.sort();
    entries
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn claude_lists_user_project_and_enabled_plugin_skills_only() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let claude = home.path().join(".claude");
        write(
            &claude.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: >\n  Reviews the\n  current diff\n---\nbody",
        );
        write(
            &claude.join("commands/git/commit.md"),
            "# Commit staged work\n",
        );
        write(
            &project.path().join(".claude/skills/deploy/SKILL.md"),
            "---\ndescription: \"Ship it\"\n---\n",
        );
        let on = home.path().join("plugins/on");
        let off = home.path().join("plugins/off");
        write(&on.join("skills/draw/SKILL.md"), "---\nname: draw\n---\n");
        write(
            &off.join("skills/hidden/SKILL.md"),
            "---\nname: hidden\n---\n",
        );
        write(
            &claude.join("plugins/installed_plugins.json"),
            &serde_json::json!({"plugins": {
                "art@market": [{"scope": "user", "installPath": on}],
                "secret@market": [{"scope": "user", "installPath": off}],
            }})
            .to_string(),
        );
        write(
            &claude.join("settings.json"),
            r#"{"enabledPlugins": {"art@market": true, "secret@market": false}}"#,
        );
        let skills = discover(Agent::Claude, home.path(), Some(project.path()));
        assert!(skills.iter().any(|skill| skill.text == "/clear"));
        let listed: Vec<_> = skills
            .iter()
            .filter(|skill| {
                skill.text == "/review"
                    || !CLAUDE_BUILTINS.iter().any(|(text, _)| *text == skill.text)
            })
            .map(|skill| (skill.text.as_str(), skill.description.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                ("/art:draw", ""),
                ("/deploy", "Ship it"),
                ("/git:commit", "Commit staged work"),
                ("/review", "Reviews the current diff"),
            ]
        );
    }

    #[test]
    fn codex_lists_dollar_skills_prompts_and_the_newest_enabled_plugin() {
        let home = tempfile::tempdir().unwrap();
        let codex = home.path().join(".codex");
        write(
            &codex.join("skills/imagegen/SKILL.md"),
            "---\nname: imagegen\n---\n",
        );
        write(&codex.join("prompts/plan.md"), "Plan the work\n");
        write(
            &codex.join("plugins/cache/market/docs/1.0.0/skills/old/SKILL.md"),
            "---\nname: old\n---\n",
        );
        write(
            &codex.join("plugins/cache/market/docs/2.0.0/skills/pdf/SKILL.md"),
            "---\nname: pdf\n---\n",
        );
        write(
            &codex.join("config.toml"),
            "[plugins.\"docs@market\"]\nenabled = true\n",
        );
        let texts: Vec<_> = discover(Agent::Codex, home.path(), None)
            .into_iter()
            .map(|skill| skill.text)
            .filter(|text| text == "/model" || !CODEX_BUILTINS.iter().any(|(b, _)| b == text))
            .collect();
        assert_eq!(texts, ["$docs:pdf", "$imagegen", "/model", "/prompts:plan"]);
    }
}
