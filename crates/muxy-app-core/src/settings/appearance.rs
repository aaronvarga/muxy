use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use muxy_core::file_lock::{self, FileLock};
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
pub enum InterfaceSize {
    #[default]
    Regular,
    Large,
    ExtraLarge,
    Huge,
}

impl InterfaceSize {
    pub const fn multiplier(self) -> f32 {
        match self {
            Self::Regular => 1.0,
            Self::Large => 1.12,
            Self::ExtraLarge => 1.24,
            Self::Huge => 1.40,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentsPanelGrouping {
    #[default]
    Agents,
    Worktrees,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentsPanelIcon {
    #[default]
    Agent,
    Project,
}

/// How much room each row of the agents panel takes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentsPanelDensity {
    /// Two lines per agent: its title, then its agent and status.
    #[default]
    Normal,
    /// One short line per agent, with its icon as its only mark.
    Compact,
}

/// How Settings → Extensions lays out a list of extensions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionsLayout {
    /// Cards with a screenshot above the name.
    Grid,
    /// One row per extension.
    List,
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
    pub command_center_group_by_project: bool,
    pub command_center_layout: crate::command_center::GridLayout,
    /// Sidebar icon background; empty follows the theme accent.
    pub command_center_color: String,
    /// Terminal order in the dashboard, independent of project tabs and splits.
    #[serde(with = "terminal_order")]
    pub command_center_order: Vec<(crate::ServerId, muxy_protocol::SessionId)>,
    pub dark_theme: String,
    pub light_theme: String,
    pub interface_size: InterfaceSize,
    pub sidebar_expanded: bool,
    pub sidebar_vibrancy: bool,
    pub sidebar_vibrancy_level: u8,
    pub sidebar_expanded_width: Option<f32>,
    pub sidebar_collapsed_style: SidebarCollapsedStyle,
    /// Project icon size in the sidebar, in unscaled points.
    pub sidebar_icon_size: u8,
    /// Background alpha for colored project tiles; unset uses the theme default.
    pub project_icon_opacity: Option<u8>,
    pub status_bar_visible: bool,
    /// Each pane shows a header with its icon, title and actions.
    pub pane_headers: bool,
    pub commands_bar_visible: bool,
    pub tips_visible: bool,
    /// The welcome tour was finished or skipped.
    pub onboarding_completed: bool,
    pub auto_expand_worktrees: bool,
    pub worktree_show_unread: bool,
    pub worktree_recent_order: bool,
    pub worktree_recent: Vec<crate::ProjectId>,
    pub project_recent: Vec<crate::ProjectId>,
    pub hidden_worktrees: std::collections::BTreeSet<crate::ProjectId>,
    /// Folder groups whose automatic subfolders include hidden (dot) folders.
    pub hidden_subfolders_shown: std::collections::BTreeSet<crate::ProjectId>,
    /// Titlebar buttons the user hid, by key; extension buttons use
    /// `top:<extension>:<item>`.
    pub hidden_titlebar_buttons: std::collections::BTreeSet<String>,
    /// Status bar items the user hid, by key.
    pub hidden_status_bar_items: std::collections::BTreeSet<String>,
    pub tab_focused_expanded: std::collections::BTreeMap<crate::ProjectId, bool>,
    /// Servers whose sidebar group was expanded or collapsed by hand; the
    /// others follow `server_expanded`'s default.
    pub sidebar_servers_expanded: std::collections::BTreeMap<crate::ServerId, bool>,
    /// The sidebar lists other computers' servers under this computer's.
    pub sidebar_remote_servers: bool,
    pub remember_projects_without_tabs: bool,
    /// Kept only while `remember_projects_without_tabs` is on.
    pub projects_without_tabs_shown: bool,
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
    pub agents_panel_grouped: bool,
    pub agents_panel_icon: AgentsPanelIcon,
    pub agents_panel_density: AgentsPanelDensity,
    pub agents_panel_project_colors: bool,
    pub agents_panel_idle_time: bool,
    pub agent_sessions_visible: bool,
    pub agent_sessions_position: AgentSessionsPosition,
    pub agent_sessions_pinned: bool,
    pub agent_sessions_detached: bool,
    pub agent_sessions_frame: Option<[f32; 4]>,
    pub agent_sessions_width: Option<f32>,
    pub agent_sessions_height: Option<f32>,
    pub agent_sessions_providers: Vec<muxy_protocol::AgentProvider>,
    /// How Settings lists installed extensions.
    pub extensions_installed_layout: ExtensionsLayout,
    /// How Settings lists marketplace extensions.
    pub extensions_browse_layout: ExtensionsLayout,
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
            command_center_group_by_project: false,
            command_center_layout: crate::command_center::GridLayout::Auto,
            command_center_color: String::new(),
            command_center_order: Vec::new(),
            dark_theme: "Muxy".into(),
            light_theme: "Muxy Light".into(),
            interface_size: InterfaceSize::default(),
            sidebar_expanded: false,
            sidebar_vibrancy: true,
            sidebar_vibrancy_level: 50,
            sidebar_expanded_width: None,
            sidebar_collapsed_style: SidebarCollapsedStyle::default(),
            sidebar_icon_size: SIDEBAR_ICON_SIZE.1,
            project_icon_opacity: None,
            status_bar_visible: true,
            pane_headers: true,
            commands_bar_visible: true,
            tips_visible: true,
            onboarding_completed: false,
            auto_expand_worktrees: false,
            worktree_show_unread: true,
            worktree_recent_order: true,
            worktree_recent: Vec::new(),
            project_recent: Vec::new(),
            hidden_worktrees: std::collections::BTreeSet::new(),
            hidden_subfolders_shown: std::collections::BTreeSet::new(),
            hidden_titlebar_buttons: std::collections::BTreeSet::new(),
            hidden_status_bar_items: std::collections::BTreeSet::new(),
            tab_focused_expanded: std::collections::BTreeMap::new(),
            sidebar_servers_expanded: std::collections::BTreeMap::new(),
            sidebar_remote_servers: true,
            remember_projects_without_tabs: false,
            projects_without_tabs_shown: false,
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
            agents_panel_grouped: true,
            agents_panel_icon: AgentsPanelIcon::default(),
            agents_panel_density: AgentsPanelDensity::default(),
            agents_panel_project_colors: false,
            agents_panel_idle_time: false,
            agent_sessions_visible: false,
            agent_sessions_position: AgentSessionsPosition::default(),
            agent_sessions_pinned: true,
            agent_sessions_detached: false,
            agent_sessions_frame: None,
            agent_sessions_width: None,
            agent_sessions_height: None,
            agent_sessions_providers: crate::agent_sessions::PROVIDERS
                .into_iter()
                .filter(|provider| crate::agent_sessions::supported(*provider))
                .collect(),
            extensions_installed_layout: ExtensionsLayout::List,
            extensions_browse_layout: ExtensionsLayout::Grid,
        }
    }
}

impl Appearance {
    /// Whether agents panel rows can wear their project's color: beside
    /// agent icons only, since a project icon already wears it, and not in
    /// compact rows, whose icon is their only mark.
    pub fn agents_panel_colors_apply(&self) -> bool {
        self.agents_panel_icon == AgentsPanelIcon::Agent
            && self.agents_panel_density == AgentsPanelDensity::Normal
    }

    /// Whether the sidebar lists `server`'s projects under its heading: this
    /// computer's until collapsed, another computer's once expanded.
    pub fn server_expanded(&self, server: crate::ServerId) -> bool {
        self.sidebar_servers_expanded
            .get(&server)
            .copied()
            .unwrap_or_else(|| server.is_local())
    }

    pub fn theme(&self, dark: bool) -> &str {
        if dark {
            &self.dark_theme
        } else {
            &self.light_theme
        }
    }

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
                    && key != "sidebar_servers_expanded"
                    && key != "hidden_worktrees"
                    && previous_values.get(key) != Some(value)
                {
                    changes.insert(key.clone(), value.clone());
                }
            }
        }
        edit_document(path, |document| {
            let target = document.entry("appearance").or_insert(previous_values);
            let target = target
                .as_table_mut()
                .ok_or_else(|| io::Error::other("appearance must be a table"))?;
            target.extend(changes);
            if self.project_icon_opacity.is_none() && previous.project_icon_opacity.is_some() {
                target.remove("project_icon_opacity");
            }
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
            for (server, expanded) in &self.sidebar_servers_expanded {
                if previous.sidebar_servers_expanded.get(server) != Some(expanded) {
                    saved.sidebar_servers_expanded.insert(*server, *expanded);
                }
            }
            for server in previous.sidebar_servers_expanded.keys() {
                if !self.sidebar_servers_expanded.contains_key(server) {
                    saved.sidebar_servers_expanded.remove(server);
                }
            }
            target.insert(
                "sidebar_servers_expanded".into(),
                toml::Value::try_from(&saved.sidebar_servers_expanded)?,
            );
            Ok(saved)
        })
    }
}

pub(crate) fn save_section(path: &Path, section: &str, values: &impl Serialize) -> Result<()> {
    let values = toml::Value::try_from(values)?;
    let values = values
        .as_table()
        .ok_or_else(|| io::Error::other(format!("{section} must be a table")))?;
    edit_document(path, |document| {
        let target = document
            .entry(section)
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        target
            .as_table_mut()
            .ok_or_else(|| io::Error::other(format!("{section} must be a table")))?
            .extend(values.clone());
        Ok(())
    })
}

/// Sets or removes each key of `section.table`, keeping everything else.
pub(crate) fn save_entries(
    path: &Path,
    section: &str,
    table: &str,
    entries: Vec<(&str, Option<toml::Value>)>,
) -> Result<toml::Value> {
    edit_document(path, |document| {
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
        Ok(toml::Value::Table(values.clone()))
    })
}

pub(crate) fn replace_section(path: &Path, section: &str, values: &impl Serialize) -> Result<()> {
    replace_sections(path, [(section, toml::Value::try_from(values)?)])
}

pub(super) fn replace_sections<'a>(
    path: &Path,
    sections: impl IntoIterator<Item = (&'a str, toml::Value)>,
) -> Result<()> {
    edit_document(path, |document| {
        for (section, value) in sections {
            document.insert(section.into(), value);
        }
        Ok(())
    })
}

/// Reads the document, lets `change` edit it and writes it back, all while
/// holding the file's lock, so concurrent saves of other sections are kept.
pub(super) fn edit_document<T>(
    path: &Path,
    change: impl FnOnce(&mut toml::Table) -> Result<T>,
) -> Result<T> {
    let _lock = lock(path)?;
    let mut document = read_document(path)?;
    let changed = change(&mut document)?;
    atomic_write(path, &toml::to_string_pretty(&document)?)?;
    Ok(changed)
}

/// Locks `<file>.lock` beside `path` for one read-change-write of the file.
pub(super) fn lock(path: &Path) -> Result<FileLock> {
    const TIMEOUT: Duration = Duration::from_secs(2);
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("settings path has no parent"))?;
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("settings path has no file name"))?
        .to_string_lossy();
    let lock = file_lock::acquire(&path.with_file_name(format!("{name}.lock")), TIMEOUT).map_err(
        file_lock::busy(format!(
            "{name} is still being saved by another change; try again"
        )),
    )?;
    Ok(lock)
}

pub(crate) fn atomic_write(path: &Path, source: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("settings path has no parent"))?;
    fs::create_dir_all(parent)?;
    muxy_core::fs::write_atomic(path, source.as_bytes(), 0o644).map_err(io::Error::from)?;
    Ok(())
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
    fn concurrent_saves_of_different_sections_all_land() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.toml");
        let sections: Vec<String> = (0..8).map(|index| format!("section_{index}")).collect();
        std::thread::scope(|scope| {
            let saves: Vec<_> = sections
                .iter()
                .map(|section| {
                    let path = &path;
                    scope.spawn(move || -> Result<()> {
                        for round in 0..20 {
                            let mut values = toml::Table::new();
                            values.insert("round".into(), round.into());
                            save_section(path, section, &values)?;
                        }
                        Ok(())
                    })
                })
                .collect();
            saves
                .into_iter()
                .try_for_each(|save| save.join().map_err(|_| io::Error::other("save panicked"))?)
        })?;
        let document = read_document(&path)?;
        for section in &sections {
            assert_eq!(
                document[section]["round"].as_integer(),
                Some(19),
                "{section}"
            );
        }
        Ok(())
    }

    #[test]
    fn a_held_lock_fails_the_save_after_the_timeout_and_keeps_the_file() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let settings = directory.path().join("settings.toml");
        let terminal = directory.path().join("terminal.toml");
        save_section(&settings, "window", &toml::Table::new())?;
        crate::settings::TerminalSettings::load_native(&terminal)?;
        let sources = [fs::read(&settings)?, fs::read(&terminal)?];
        let _settings_lock = lock(&settings)?;
        let _terminal_lock = lock(&terminal)?;

        let started = std::time::Instant::now();
        let error = save_section(&settings, "panes", &toml::Table::new())
            .err()
            .ok_or("the save should time out")?;
        assert!(started.elapsed() >= Duration::from_secs(2));
        assert_eq!(
            error.to_string(),
            "settings.toml is still being saved by another change; try again"
        );
        let error = crate::settings::TerminalSettings::edit_native(&terminal, |_| Ok(()))
            .err()
            .ok_or("the save should time out")?;
        assert_eq!(
            error.to_string(),
            "terminal.toml: terminal.toml is still being saved by another change; try again"
        );
        assert_eq!([fs::read(&settings)?, fs::read(&terminal)?], sources);
        Ok(())
    }

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
