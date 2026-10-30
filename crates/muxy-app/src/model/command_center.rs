use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

use gpui::{Context, ListAlignment, ListOffset, ListState, Window, px};
use muxy_app_core::{PaneContent, PaneId, ProjectId, ServerId, command_center::GridLayout};
use muxy_protocol::{ProjectSession, SessionId, Size};
use muxy_ui::tr;

use super::{AppModel, Quitting};
use crate::boot::Work;
use crate::views::terminal::pane::PaneState;

#[derive(Clone)]
pub(crate) struct Tile {
    pub(crate) pane: PaneId,
    pub(crate) server: ServerId,
    pub(crate) session: ProjectSession,
    pub(crate) title: String,
    pub(crate) error: Option<String>,
}

pub(crate) struct CommandCenter {
    pub(crate) active: bool,
    pub(crate) tiles: Vec<Tile>,
    pub(crate) visible: Vec<PaneId>,
    pub(crate) focused: Option<PaneId>,
    pub(crate) columns: usize,
    pub(crate) scroll: ListState,
    pub(crate) scrollbar: muxy_ui::scrollbar::ListScrollbar,
    pub(crate) layout_anchor: muxy_ui::popover::PopoverAnchor,
    pub(crate) grid: Option<([u8; 2], gpui::Size<gpui::Pixels>, usize)>,
    render_epoch: u64,
}

impl Default for CommandCenter {
    fn default() -> Self {
        Self {
            active: false,
            tiles: Vec::new(),
            visible: Vec::new(),
            focused: None,
            columns: 1,
            scroll: ListState::new(0, ListAlignment::Top, px(0.0)),
            scrollbar: muxy_ui::scrollbar::ListScrollbar::default(),
            layout_anchor: muxy_ui::popover::PopoverAnchor::default(),
            grid: None,
            render_epoch: 0,
        }
    }
}

impl CommandCenter {
    pub(crate) fn tile(&self, pane: PaneId) -> Option<&Tile> {
        self.tiles.iter().find(|tile| tile.pane == pane)
    }

    pub(super) fn leave(&mut self) {
        self.active = false;
        self.visible.clear();
        self.focused = None;
        self.render_epoch = self.render_epoch.wrapping_add(1);
    }
}

impl AppModel {
    pub(crate) fn open_command_center(&mut self, cx: &mut Context<Self>) {
        if self.quitting != Quitting::Idle || self.close_request.is_some() {
            return;
        }
        self.dismiss_overlay(cx);
        self.dismiss_composer(false, cx);
        self.close_voice(cx);
        self.cancel_titlebar_drag(cx);
        self.command_center.active = true;
        self.refresh_existing_sessions(cx);
        self.sync_command_center(cx);
        self.focus_requested = true;
        cx.notify();
    }

    pub(crate) fn close_command_center(&mut self, cx: &mut Context<Self>) {
        self.command_center.leave();
        self.sync_visible(cx);
        self.focus_requested = true;
        cx.notify();
    }

    pub(crate) fn set_command_center_layout(&mut self, layout: GridLayout, cx: &mut Context<Self>) {
        self.appearance.command_center_layout = layout;
        self.command_center.grid = None;
        self.command_center.scroll.scroll_to(ListOffset::default());
        self.save_appearance(cx);
        cx.notify();
    }

    pub(crate) fn close_command_center_terminal(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if !self.command_center.active
            || self.quitting != Quitting::Idle
            || self.close_prompt.is_some()
            || self.close_request.is_some()
            || self.pending_close.is_some()
        {
            return;
        }
        let Some(tile) = self.command_center.tile(pane).cloned() else {
            return;
        };
        let window = self.window;
        self.dismiss_overlay(cx);
        self.close_prompt = Some(cx.spawn(async move |model, cx| {
            let response =
                crate::views::confirm::prompt_close_terminal(window, &tile.title, cx).await;
            let _ = model.update(cx, |model, cx| {
                model.finish_confirmation(cx);
                model.focus_requested = true;
                match response {
                    Ok(true)
                        if model.quitting == Quitting::Idle
                            && model.command_center.active
                            && model.ready(tile.server)
                            && model.confirmed(tile.server)
                            && model.command_center.tile(pane).is_some_and(|current| {
                                current.server == tile.server
                                    && current.session.info.id == tile.session.info.id
                            }) =>
                    {
                        let previous = model.state.clone();
                        if let Err(error) = model
                            .state
                            .close_session_panes(tile.server, tile.session.info.id)
                        {
                            model.state = previous;
                            model.fail(error.to_string(), cx);
                            return;
                        }
                        model.state.queue_discard(tile.server, tile.session.info.id);
                        if !model.save(cx) {
                            model.state = previous;
                            return;
                        }
                        model.forget_listed_session(tile.server, tile.session.info.id);
                        model.sync_command_center(cx);
                        model.discard_pending(tile.server, cx);
                    }
                    Err(error) => model.fail(error, cx),
                    Ok(_) => {}
                }
            });
        }));
        cx.notify();
    }

    pub(crate) fn reorder_command_center(
        &mut self,
        source: PaneId,
        target: PaneId,
        cx: &mut Context<Self>,
    ) {
        if !self.command_center.active || self.quitting != Quitting::Idle || source == target {
            return;
        }
        let index = |pane| {
            self.command_center
                .tiles
                .iter()
                .position(|tile| tile.pane == pane)
        };
        let (Some(from), Some(to)) = (index(source), index(target)) else {
            return;
        };
        let tile = self.command_center.tiles.remove(from);
        self.command_center.tiles.insert(to, tile);
        self.appearance.command_center_order = self
            .command_center
            .tiles
            .iter()
            .map(|tile| (tile.server, tile.session.info.id))
            .collect();
        self.save_appearance(cx);
        self.command_center.grid = None;
        self.command_center.render_epoch = self.command_center.render_epoch.wrapping_add(1);
        self.command_center.focused = Some(source);
        self.command_center
            .scroll
            .scroll_to_reveal_item(to / self.command_center.columns.max(1));
        self.focus_requested = true;
        cx.notify();
    }

    pub(crate) fn sync_command_center(&mut self, cx: &mut Context<Self>) {
        if !self.command_center.active {
            return;
        }
        let mut seen = HashSet::new();
        let previous: HashMap<_, _> = self
            .command_center
            .tiles
            .iter()
            .map(|tile| ((tile.server, tile.session.info.id), tile))
            .collect();
        let labels: HashMap<_, _> = self
            .state
            .projects()
            .iter()
            .flat_map(|project| {
                project
                    .tabs
                    .iter()
                    .flat_map(|tab| &tab.panes)
                    .filter_map(|pane| match pane.content {
                        PaneContent::Terminal {
                            session: Some(session),
                        } => Some(((project.server_id, session), pane.title.as_str())),
                        _ => None,
                    })
            })
            .collect();
        let mut tiles: Vec<_> = self
            .command_center_sessions()
            .into_iter()
            .filter(|(server, session)| seen.insert((*server, session.info.id)))
            .map(|(server, session)| {
                let previous = previous.get(&(server, session.info.id));
                let pane = previous.map_or_else(PaneId::new, |tile| tile.pane);
                let title = previous
                    .map(|tile| tile.title.clone())
                    .or_else(|| {
                        labels
                            .get(&(server, session.info.id))
                            .map(|title| (*title).to_owned())
                    })
                    .unwrap_or_else(|| tr!("Terminal %lld", session.info.id.get()).to_string());
                let error = previous.and_then(|tile| tile.error.clone());
                Tile {
                    pane,
                    server,
                    session,
                    title,
                    error,
                }
            })
            .collect();
        let order: HashMap<_, _> = self
            .appearance
            .command_center_order
            .iter()
            .copied()
            .enumerate()
            .map(|(index, key)| (key, index))
            .collect();
        tiles.sort_by_key(|tile| {
            order
                .get(&(tile.server, tile.session.info.id))
                .copied()
                .unwrap_or(usize::MAX)
        });
        self.command_center.tiles = tiles;
        self.command_center.render_epoch = self.command_center.render_epoch.wrapping_add(1);
        let tiles = &self.command_center.tiles;
        self.command_center
            .visible
            .retain(|id| tiles.iter().any(|tile| tile.pane == *id));
        if self
            .command_center
            .focused
            .is_some_and(|id| self.command_center.tile(id).is_none())
        {
            self.command_center.focused = None;
        }
        self.sync_visible(cx);
        cx.notify();
    }

    /// Only rows intersecting the viewport may attach terminals. A deferred
    /// callback from a previous layout must not resurrect hidden tiles.
    pub(crate) fn command_center_rows(
        &mut self,
        rows: Range<usize>,
        columns: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.command_center.columns = columns;
        self.command_center.render_epoch = self.command_center.render_epoch.wrapping_add(1);
        let epoch = self.command_center.render_epoch;
        let visible: Vec<_> = self
            .command_center
            .tiles
            .iter()
            .skip(rows.start.saturating_mul(columns))
            .take(rows.len().saturating_mul(columns))
            .map(|tile| tile.pane)
            .collect();
        if visible == self.command_center.visible {
            return;
        }
        let model = cx.weak_entity();
        window.defer(cx, move |_, cx| {
            let _ = model.update(cx, |model, cx| {
                if !model.command_center.active || model.command_center.render_epoch != epoch {
                    return;
                }
                model.command_center.visible = visible;
                if model
                    .command_center
                    .focused
                    .is_none_or(|pane| !model.command_center.visible.contains(&pane))
                {
                    model.command_center.focused = model.command_center.visible.first().copied();
                    model.focus_requested = true;
                }
                model.sync_visible(cx);
                cx.notify();
            });
        });
    }

    pub(super) fn command_center_focus(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if self.command_center.active && self.command_center.tile(pane).is_some() {
            self.command_center.focused = Some(pane);
            self.focus_requested = true;
            cx.notify();
        }
    }

    pub(super) fn command_center_direction(
        &mut self,
        direction: muxy_app_core::Direction,
        cx: &mut Context<Self>,
    ) {
        use muxy_app_core::Direction;
        let Some(index) = self
            .command_center
            .tiles
            .iter()
            .position(|tile| Some(tile.pane) == self.active_pane())
        else {
            return;
        };
        let columns = self.command_center.columns.max(1);
        let target = match direction {
            Direction::Left if index % columns > 0 => index.checked_sub(1),
            Direction::Right if index % columns + 1 < columns => index.checked_add(1),
            Direction::Up => index.checked_sub(columns),
            Direction::Down => index.checked_add(columns),
            _ => None,
        };
        if let Some(index) = target.filter(|index| *index < self.command_center.tiles.len()) {
            let pane = self.command_center.tiles[index].pane;
            self.command_center
                .scroll
                .scroll_to_reveal_item(index / columns);
            self.command_center_focus(pane, cx);
        }
    }

    pub(crate) fn open_command_center_project(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        let Some(tile) = self.command_center.tile(pane).cloned() else {
            return;
        };
        let original = self.state.project(tile.session.info.project)
            .into_iter().flat_map(|project| &project.tabs)
            .flat_map(|tab| &tab.panes)
            .find(|pane| matches!(pane.content, PaneContent::Terminal { session: Some(id) } if id == tile.session.info.id))
            .map(|pane| pane.id);
        self.command_center.leave();
        if let Some(original) = original {
            if self.state.focus_pane(original).is_ok() {
                self.changed(cx);
            }
        } else {
            self.select_project(tile.session.info.project, cx);
            // A dashboard attachment has no saved project pane to reopen.
            let mut session = tile.session;
            session.attached = false;
            self.open_existing_session(session.info.project, &session, cx);
        }
        self.focus_requested = true;
        cx.notify();
    }

    pub(super) fn pane_server(&self, pane: PaneId) -> Option<ServerId> {
        self.command_center
            .tile(pane)
            .map(|tile| tile.server)
            .or_else(|| self.state.pane_server(pane))
    }

    pub(super) fn command_center_project(&self, pane: PaneId) -> Option<ProjectId> {
        self.command_center
            .tile(pane)
            .map(|tile| tile.session.info.project)
    }

    pub(super) fn attach_command_center(
        &mut self,
        pane: PaneId,
        size: Size,
        cx: &mut Context<Self>,
    ) {
        let Some(tile) = self.command_center.tile(pane).cloned() else {
            return;
        };
        if !self.command_center.active
            || !self.command_center.visible.contains(&pane)
            || self.pending.contains_key(&pane)
            || !self.ready(tile.server)
            || !self.confirmed(tile.server)
        {
            return;
        }
        let Some(project) = self.state.project(tile.session.info.project) else {
            return;
        };
        let directory = project.directory.clone();
        if let Some(tile) = self
            .command_center
            .tiles
            .iter_mut()
            .find(|tile| tile.pane == pane)
        {
            tile.error = None;
        }
        self.pending.insert(pane, tile.server);
        self.send(
            tile.server,
            Work::Attach {
                pane,
                project: tile.session.info.project,
                session: Some(tile.session.info.id),
                directory,
                size,
                environment: BTreeMap::new(),
            },
            cx,
        );
    }

    pub(super) fn receive_command_center_attached(
        &mut self,
        server: ServerId,
        pane: PaneId,
        session: SessionId,
        attachment: muxy_client::Attachment,
        cx: &mut Context<Self>,
    ) {
        self.pending.remove(&pane);
        let matches = self.command_center.active
            && self.command_center.visible.contains(&pane)
            && self
                .command_center
                .tile(pane)
                .is_some_and(|tile| tile.server == server && tile.session.info.id == session);
        if matches && let Some(view) = self.terminal(&pane).map(|pane| pane.view.clone()) {
            let channel = attachment.channel;
            if let Some(size) = view.update(cx, |pane, cx| pane.attach(server, attachment, cx)) {
                self.send(server, Work::Resize(channel, size), cx);
            }
        } else {
            self.send(server, Work::Detach(attachment.channel), cx);
        }
    }

    pub(super) fn command_center_attach_failed(
        &mut self,
        pane: PaneId,
        error: &muxy_client::ClientError,
        cx: &mut Context<Self>,
    ) {
        self.pending.remove(&pane);
        if let Some(tile) = self
            .command_center
            .tiles
            .iter_mut()
            .find(|tile| tile.pane == pane)
        {
            tile.error = Some(error.to_string());
        }
        if let Some(view) = self.terminal(&pane) {
            view.view
                .update(cx, |pane, cx| pane.set_state(PaneState::Disconnected, cx));
        }
    }

    pub(crate) fn retry_command_center(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if let Some(size) = self
            .terminal(&pane)
            .and_then(|pane| pane.view.read(cx).viewport())
        {
            self.start_attach(pane, size, cx);
        }
        cx.notify();
    }

    pub(super) fn command_center_menu(
        &self,
        pane: PaneId,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        use crate::views::menu::{Command, Item};
        let items = vec![
            Item::action(tr!("Copy"), Command::TerminalCopy(pane)),
            Item::action(tr!("Paste"), Command::TerminalPaste(pane)),
            Item::action(tr!("Select All"), Command::TerminalSelectAll(pane)),
            Item::action(
                tr!("Select Command Output"),
                Command::TerminalSelectCommandOutput(pane),
            ),
            Item::action(tr!("Open in Project"), Command::CenterProject(pane)).separated(),
            Item::action(tr!("Close Terminal Session…"), Command::ClosePane(pane)).separated(),
        ];
        let model = cx.weak_entity();
        let window = self.window;
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                let _ = model.update(cx, |model, cx| {
                    if model.command_center.active && model.command_center.visible.contains(&pane) {
                        model.open_menu(items, position, window, cx);
                    }
                });
            });
        });
    }
}
