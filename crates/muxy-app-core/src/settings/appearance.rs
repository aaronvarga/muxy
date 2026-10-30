use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppLayout {
    #[default]
    ProjectFocused,
    TabFocused,
    AgentsFocused,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectOrder {
    #[default]
    Manual,
    Name,
    NameDescending,
    RecentlyActive,
    DateAdded,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarCollapsedStyle {
    #[default]
    Icons,
    Hidden,
}

/// Minimum, default and maximum sidebar project icon sizes.
pub const SIDEBAR_ICON_SIZE: (u8, u8, u8) = (20, 28, 48);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentsPanelGrouping {
    #[default]
    Agents,
    Worktrees,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentsPanelDock {
    #[default]
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionsPosition {
    #[default]
    Right,
    Bottom,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent appearance preferences"
)]
pub struct Appearance {
    pub layout: AppLayout,
    pub command_center_layout: crate::command_center::GridLayout,
    /// Sidebar icon background; empty follows the theme accent.
    pub command_center_color: String,
    /// Terminal order in the dashboard, independent of project tabs and splits.
    #[serde(with = "terminal_order")]
    pub command_center_order: Vec<(crate::ServerId, muxy_protocol::SessionId)>,
    pub dark_theme: String,
    pub light_theme: String,
    pub sidebar_expanded: bool,
    pub sidebar_vibrancy: bool,
    pub sidebar_vibrancy_level: u8,
    pub sidebar_expanded_width: Option<f32>,
    pub sidebar_collapsed_style: SidebarCollapsedStyle,
    /// Project icon size in the sidebar, in unscaled points.
    pub sidebar_icon_size: u8,
    pub status_bar_visible: bool,
    pub commands_bar_visible: bool,
    pub tips_visible: bool,
    pub auto_expand_worktrees: bool,
    pub worktree_show_unread: bool,
    pub worktree_recent_order: bool,
    pub worktree_recent: Vec<crate::ProjectId>,
    pub project_recent: Vec<crate::ProjectId>,
    pub hidden_worktrees: std::collections::BTreeSet<crate::ProjectId>,
    /// Titlebar buttons the user hid, by key; extension buttons use
    /// `top:<extension>:<item>`.
    pub hidden_titlebar_buttons: std::collections::BTreeSet<String>,
    /// Status bar items the user hid, by key.
    pub hidden_status_bar_items: std::collections::BTreeSet<String>,
    pub tab_focused_expanded: std::collections::BTreeMap<crate::ProjectId, bool>,
    #[serde(rename = "tab_focused_focus")]
    pub sidebar_focus: bool,
    #[serde(rename = "tab_focused_project_order")]
    pub sidebar_project_order: ProjectOrder,
    /// The extension whose sidebar replaces the built-in one; empty for built-in.
    pub extension_sidebar: String,
    /// The app language as `<extension>:<localization>`; empty for English.
    pub language: String,
    pub agents_panel_visible: bool,
    pub agents_panel_detached: bool,
    pub agents_panel_dock: AgentsPanelDock,
    pub agents_panel_right_width: Option<f32>,
    pub agents_panel_frame: Option<[f32; 4]>,
    pub agents_panel_docked_height: Option<f32>,
    pub agents_panel_grouping: AgentsPanelGrouping,
    pub agent_sessions_visible: bool,
    pub agent_sessions_position: AgentSessionsPosition,
    pub agent_sessions_pinned: bool,
    pub agent_sessions_width: Option<f32>,
    pub agent_sessions_height: Option<f32>,
    pub agent_sessions_providers: Vec<muxy_protocol::AgentProvider>,
}

mod terminal_order {
    use crate::ServerId;
    use muxy_protocol::SessionId;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        order: &[(ServerId, SessionId)],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        // TOML integers cannot represent the upper half of the session ID range.
        serializer.collect_seq(
            order
                .iter()
                .map(|(server, id)| (server, id.get().to_string())),
        )
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<(ServerId, SessionId)>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Id {
            Text(String),
            Number(SessionId),
        }
        Vec::<(ServerId, Id)>::deserialize(deserializer)?
            .into_iter()
            .map(|(server, id)| {
                let id = match id {
                    Id::Text(text) => text
                        .parse::<std::num::NonZeroU64>()
                        .map(SessionId::from)
                        .map_err(serde::de::Error::custom)?,
                    Id::Number(id) => id,
                };
                Ok((server, id))
            })
            .collect()
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            layout: AppLayout::default(),
            command_center_layout: crate::command_center::GridLayout::Auto,
            command_center_color: String::new(),
            command_center_order: Vec::new(),
            dark_theme: "Muxy".into(),
            light_theme: "Muxy Light".into(),
            sidebar_expanded: false,
            sidebar_vibrancy: true,
            sidebar_vibrancy_level: 50,
            sidebar_expanded_width: None,
            sidebar_collapsed_style: SidebarCollapsedStyle::default(),
            sidebar_icon_size: SIDEBAR_ICON_SIZE.1,
            status_bar_visible: true,
            commands_bar_visible: true,
            tips_visible: true,
            auto_expand_worktrees: false,
            worktree_show_unread: true,
            worktree_recent_order: true,
            worktree_recent: Vec::new(),
            project_recent: Vec::new(),
            hidden_worktrees: std::collections::BTreeSet::new(),
            hidden_titlebar_buttons: std::collections::BTreeSet::new(),
            hidden_status_bar_items: std::collections::BTreeSet::new(),
            tab_focused_expanded: std::collections::BTreeMap::new(),
            sidebar_focus: false,
            sidebar_project_order: ProjectOrder::default(),
            extension_sidebar: String::new(),
            language: String::new(),
            agents_panel_visible: true,
            agents_panel_detached: false,
            agents_panel_dock: AgentsPanelDock::default(),
            agents_panel_right_width: None,
            agents_panel_frame: None,
            agents_panel_docked_height: None,
            agents_panel_grouping: AgentsPanelGrouping::default(),
            agent_sessions_visible: false,
            agent_sessions_position: AgentSessionsPosition::default(),
            agent_sessions_pinned: true,
            agent_sessions_width: None,
            agent_sessions_height: None,
            agent_sessions_providers: crate::agent_sessions::PROVIDERS
                .into_iter()
                .filter(|provider| crate::agent_sessions::supported(*provider))
                .collect(),
        }
    }
}

impl Appearance {
    pub fn load(path: &Path) -> Result<Self> {
        let document = read_document(path)?;
        document.get("appearance").cloned().map_or_else(
            || Ok(Self::default()),
            |value| value.try_into().map_err(Into::into),
        )
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        save_section(path, "appearance", self)
    }

    pub fn save_changes(&self, previous: &Self, path: &Path) -> Result<Self> {
        let current = toml::Value::try_from(self)?;
        let previous_values = toml::Value::try_from(previous)?;
        let mut changes = toml::Table::new();
        if let Some(current) = current.as_table() {
            for (key, value) in current {
                if key != "tab_focused_expanded"
                    && key != "hidden_worktrees"
                    && previous_values.get(key) != Some(value)
                {
                    changes.insert(key.clone(), value.clone());
                }
            }
        }
        let mut document = read_document(path)?;
        let target = document.entry("appearance").or_insert(previous_values);
        let target = target
            .as_table_mut()
            .ok_or_else(|| io::Error::other("appearance must be a table"))?;
        target.extend(changes);
        let mut saved: Self = toml::Value::Table(target.clone()).try_into()?;
        for project in self.hidden_worktrees.difference(&previous.hidden_worktrees) {
            saved.hidden_worktrees.insert(*project);
        }
        for project in previous.hidden_worktrees.difference(&self.hidden_worktrees) {
            saved.hidden_worktrees.remove(project);
        }
        target.insert(
            "hidden_worktrees".into(),
            toml::Value::try_from(&saved.hidden_worktrees)?,
        );
        for (project, expanded) in &self.tab_focused_expanded {
            if previous.tab_focused_expanded.get(project) != Some(expanded) {
                saved.tab_focused_expanded.insert(*project, *expanded);
            }
        }
        for project in previous.tab_focused_expanded.keys() {
            if !self.tab_focused_expanded.contains_key(project) {
                saved.tab_focused_expanded.remove(project);
            }
        }
        target.insert(
            "tab_focused_expanded".into(),
            toml::Value::try_from(&saved.tab_focused_expanded)?,
        );
        write_document(path, &document)?;
        Ok(saved)
    }
}

pub(crate) fn save_section(path: &Path, section: &str, values: &impl Serialize) -> Result<()> {
    let mut document = read_document(path)?;
    let values = toml::Value::try_from(values)?;
    let values = values
        .as_table()
        .ok_or_else(|| io::Error::other(format!("{section} must be a table")))?;
    let target = document
        .entry(section)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    target
        .as_table_mut()
        .ok_or_else(|| io::Error::other(format!("{section} must be a table")))?
        .extend(values.clone());
    write_document(path, &document)
}

/// Sets or removes each key of `section.table`, keeping everything else.
pub(crate) fn save_entries(
    path: &Path,
    section: &str,
    table: &str,
    entries: Vec<(&str, Option<toml::Value>)>,
) -> Result<toml::Value> {
    let mut document = read_document(path)?;
    let values = document
        .entry(section)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let values = values
        .as_table_mut()
        .ok_or_else(|| io::Error::other(format!("{section} must be a table")))?;
    let target = values
        .entry(table)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if !target.is_table() {
        *target = toml::Value::Table(toml::Table::new());
    }
    if let Some(target) = target.as_table_mut() {
        for (key, value) in entries {
            match value {
                Some(value) => {
                    target.insert(key.into(), value);
                }
                None => {
                    target.remove(key);
                }
            }
        }
    }
    let saved = toml::Value::Table(values.clone());
    write_document(path, &document)?;
    Ok(saved)
}

pub(crate) fn replace_section(path: &Path, section: &str, values: &impl Serialize) -> Result<()> {
    replace_sections(path, [(section, toml::Value::try_from(values)?)])
}

pub(super) fn replace_sections<'a>(
    path: &Path,
    sections: impl IntoIterator<Item = (&'a str, toml::Value)>,
) -> Result<()> {
    let mut document = read_document(path)?;
    for (section, value) in sections {
        document.insert(section.into(), value);
    }
    write_document(path, &document)
}

pub(super) fn write_document(path: &Path, document: &toml::Table) -> Result<()> {
    atomic_write(path, &toml::to_string_pretty(document)?)
}

pub(crate) fn atomic_write(path: &Path, source: &str) -> Result<()> {
    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("settings path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".settings-{}-{}.tmp",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| -> io::Result<()> {
        file.write_all(source.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(Into::into)
}

pub(super) fn read_document(path: &Path) -> Result<toml::Table> {
    match fs::read_to_string(path) {
        Ok(source) => source.parse().map_err(Into::into),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_order_saves_full_width_ids_and_accepts_earlier_numeric_entries() -> Result<()> {
        let server = crate::ServerId::local();
        let directory =
            std::env::temp_dir().join(format!("muxy-terminal-order-{}", crate::ProjectId::new()));
        let path = directory.join("settings.toml");
        let initial = Appearance::default();
        initial.save(&path)?;
        let mut changed = initial.clone();
        changed.command_center_order = vec![(
            server,
            muxy_protocol::SessionId::new(u64::MAX).expect("session"),
        )];
        let saved = changed.save_changes(&initial, &path)?;
        assert_eq!(saved, changed);
        assert_eq!(Appearance::load(&path)?, changed);
        fs::write(
            &path,
            format!("[appearance]\ncommand_center_order = [[\"{server}\", 7]]\n"),
        )?;
        assert_eq!(
            Appearance::load(&path)?.command_center_order,
            [(server, muxy_protocol::SessionId::new(7).expect("session"))]
        );
        for invalid in ["0", "18446744073709551616", "invalid"] {
            fs::write(
                &path,
                format!("[appearance]\ncommand_center_order = [[\"{server}\", \"{invalid}\"]]\n"),
            )?;
            assert!(Appearance::load(&path).is_err());
        }
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn worktree_visibility_defaults_and_independent_saved_choices() -> Result<()> {
        let first = crate::ProjectId::new();
        let second = crate::ProjectId::new();
        let directory = std::env::temp_dir().join(format!("muxy-worktree-visibility-{first}"));
        let path = directory.join("settings.toml");
        let initial = Appearance::load(&path)?;
        assert!(initial.hidden_worktrees.is_empty());
        initial.save(&path)?;
        let mut one = initial.clone();
        one.hidden_worktrees.insert(first);
        one.save_changes(&initial, &path)?;
        let mut two = initial.clone();
        two.hidden_worktrees.insert(second);
        let saved = two.save_changes(&initial, &path)?;
        assert_eq!(saved.hidden_worktrees, [first, second].into());
        let previous_one = one.clone();
        one.hidden_worktrees.remove(&first);
        let saved = one.save_changes(&previous_one, &path)?;
        assert_eq!(saved.hidden_worktrees, [second].into());
        assert_eq!(Appearance::load(&path)?, saved);
        fs::remove_dir_all(directory)?;
        Ok(())
    }
}
