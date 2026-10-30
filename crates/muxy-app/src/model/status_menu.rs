use super::AppModel;

impl AppModel {
    pub(crate) fn has_status_menu(&self) -> bool {
        #[cfg(target_os = "macos")]
        return self.status_menu.is_some();
        #[cfg(not(target_os = "macos"))]
        false
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn start_status_menu(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((menu, events)) = muxy_ui::status_menu::StatusMenu::new() else {
            return;
        };
        self.status_menu = Some(menu);
        self.refresh_existing_sessions(cx);
        cx.spawn(async move |model, cx| {
            while let Ok(id) = events.recv().await {
                if model
                    .update(cx, |model, cx| model.choose_status_session(&id, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Called for inventory, activity and title changes, never terminal frames.
    pub(crate) fn sync_status_menu(&mut self) {
        #[cfg(target_os = "macos")]
        if self.status_menu.is_some() {
            let entries = self.status_menu_entries();
            if let Some(menu) = &mut self.status_menu {
                menu.update(entries);
            }
        }
    }

    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn status_menu_entries(&self) -> Vec<muxy_ui::status_menu::Entry> {
        use muxy_app_core::{
            PaneContent,
            activity::{ActivityIndicator, indicator},
        };
        use muxy_ui::{
            status_menu::{Attention, Entry, ProjectIcon},
            tr,
        };
        use std::collections::{HashMap, HashSet};

        let mut titles: HashMap<_, _> = self
            .state
            .projects()
            .iter()
            .flat_map(|project| {
                project
                    .tabs
                    .iter()
                    .flat_map(|tab| &tab.panes)
                    .filter_map(|pane| match pane.content {
                        PaneContent::Terminal { session: Some(id) } => {
                            Some(((project.server_id, id), pane.title.as_str()))
                        }
                        _ => None,
                    })
            })
            .collect();
        titles.extend(
            self.command_center
                .tiles
                .iter()
                .filter(|_| self.command_center.active)
                .map(|tile| ((tile.server, tile.session.info.id), tile.title.as_str())),
        );
        let mut seen = HashSet::new();
        self.running_sessions()
            .into_iter()
            .filter_map(|(server, session)| {
                let id = session.info.id;
                if !seen.insert((server, id)) {
                    return None;
                }
                let project = self.state.project(session.info.project)?;
                let project_name = project
                    .parent_id
                    .and_then(|parent| self.state.project(parent))
                    .map_or_else(
                        || project.name.clone(),
                        |parent| format!("{} / {}", parent.name, project.name),
                    );
                let title = titles
                    .get(&(server, id))
                    .filter(|title| !title.trim().is_empty())
                    .map_or_else(
                        || tr!("Terminal %lld", id.get()).to_string(),
                        |title| (*title).to_owned(),
                    );
                let status = self
                    .activity(server)
                    .map_or(ActivityIndicator::None, |activity| {
                        indicator(&activity.snapshot, |session| session == id)
                    });
                let (attention, suffix) = match status {
                    ActivityIndicator::Blocked => {
                        (Some(Attention::Input), format!(" · {}", tr!("Needs input")))
                    }
                    ActivityIndicator::Completed => (
                        Some(Attention::Unread),
                        format!(" · {}", tr!("Unread update")),
                    ),
                    _ => (None, String::new()),
                };
                let remote = self
                    .server_name(server)
                    .filter(|_| !server.is_local())
                    .map_or_else(String::new, |name| format!(" · {name}"));
                let label = format!("{project_name} · {title}{remote}{suffix}")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                Some(Entry {
                    id: format!("{server}:{}", id.get()),
                    label,
                    attention,
                    icon: ProjectIcon {
                        logo: project.logo.clone(),
                        glyph: project.icon.clone().unwrap_or_else(|| {
                            if project.home && server.is_local() {
                                "sf:house.fill".into()
                            } else {
                                muxy_app_core::initial(&project.name).to_uppercase()
                            }
                        }),
                        color: project.color.as_str().to_owned(),
                    },
                })
            })
            .collect()
    }

    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn choose_status_session(&mut self, id: &str, cx: &mut gpui::Context<Self>) {
        if self.quitting != super::Quitting::Idle
            || self.close_prompt.is_some()
            || self.close_request.is_some()
        {
            return;
        }
        if id == "command-center" {
            self.open_command_center(cx);
        } else {
            let Some((server, session)) = id.split_once(':').and_then(|(server, id)| {
                Some((
                    server.parse::<muxy_app_core::ServerId>().ok()?,
                    muxy_protocol::SessionId::new(id.parse().ok()?)?,
                ))
            }) else {
                return;
            };
            // A menu can remain open while its session ends or its server disconnects.
            let Some((_, mut listed)) = self
                .running_sessions()
                .into_iter()
                .find(|(owner, entry)| *owner == server && entry.info.id == session)
            else {
                return;
            };
            let original = self
                .session_pane(server, session)
                .map(|(project, tab, pane)| (project.id, tab, pane));
            if let Some((project, tab, pane)) = original {
                self.select_project(project, cx);
                self.select_tab(tab, cx);
                self.focus_pane(pane, cx);
            } else {
                self.select_project(listed.info.project, cx);
                listed.attached = false;
                self.open_existing_session(listed.info.project, &listed, cx);
            }
        }
        self.dismiss_overlay(cx);
        cx.activate(true);
        self.raise_window(cx);
    }
}
