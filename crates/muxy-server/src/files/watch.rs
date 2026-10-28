use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::Duration;

use muxy_protocol::{
    FileChanges, FilesAction, FilesReply, FilesRequest, Message, ProjectId, ServerPath,
};
use notify::EventKind;

use crate::catalog::Catalog;
use crate::watch::{DebouncedWatch, MAX_DELAY, Subscribers, Subscription, Timing};
use crate::{Registry, connection::Connection};

use super::{Result, error, root::Root};

/// Folders one connection can watch at once, each recursively.
pub(crate) const MAX_WATCHES: usize = 32;

/// A watched project folder: the project, and the extra folder or `None` for
/// the project's own directory.
pub(crate) type WatchKey = (ProjectId, Option<ServerPath>);

const TIMING: Timing = Timing {
    quiet: Duration::from_millis(300),
    max_delay: MAX_DELAY,
};

/// The project folders one connection watches.
#[derive(Default)]
pub(crate) struct Subscriptions {
    watches: HashMap<WatchKey, Subscription<WatchKey>>,
}

impl Subscriptions {
    pub(crate) fn request(
        &mut self,
        registry: &Registry,
        output: &Arc<Connection>,
        request: &FilesRequest,
    ) -> Result<FilesReply> {
        let key = (request.project, request.root.clone());
        if request.action == FilesAction::Unwatch {
            self.watches.remove(&key);
            return Ok(FilesReply::Done);
        }
        if self.watches.contains_key(&key) {
            return Ok(FilesReply::Done);
        }
        if self.watches.len() >= MAX_WATCHES {
            return Err(error("Too many file subscriptions"));
        }
        let root = Root::open(&registry.files_root(request)?)?;
        let catalog = Arc::downgrade(&registry.catalog);
        let subscription = registry
            .files
            .watches
            .subscribe(key.clone(), output, |subscribers| {
                watch(&root.path, key.clone(), catalog, subscribers).map(Some)
            })?
            .ok_or_else(|| error("Nothing to watch"))?;
        self.watches.insert(key, subscription);
        Ok(FilesReply::Done)
    }
}

fn watch(
    root: &Path,
    (project, folder): WatchKey,
    catalog: Weak<Catalog>,
    subscribers: Subscribers,
) -> Result<DebouncedWatch> {
    let directory = root.to_owned();
    DebouncedWatch::new(
        "file-watch",
        TIMING,
        notify::Config::default().with_follow_symlinks(false),
        &[root],
        move |pending: &mut FileChanges, event: notify::Result<notify::Event>| {
            match event {
                Ok(event) if matches!(event.kind, EventKind::Access(_)) => return false,
                Ok(event) => {
                    if event.need_rescan() {
                        pending.merge(&FileChanges {
                            paths: Vec::new(),
                            rescan: true,
                            root: None,
                        });
                    }
                    for path in &event.paths {
                        let Ok(relative) = path.strip_prefix(&directory) else {
                            continue;
                        };
                        if relative.components().any(|part| part.as_os_str() == ".git") {
                            continue;
                        }
                        let path = ServerPath::from(relative);
                        if path.0.len() > muxy_protocol::MAX_PATH {
                            pending.merge(&FileChanges {
                                paths: Vec::new(),
                                rescan: true,
                                root: None,
                            });
                        } else {
                            pending.merge(&FileChanges {
                                paths: vec![path],
                                rescan: false,
                                root: None,
                            });
                        }
                    }
                }
                Err(_) => pending.merge(&FileChanges {
                    paths: Vec::new(),
                    rescan: true,
                    root: None,
                }),
            }
            pending.rescan || !pending.paths.is_empty()
        },
        move |mut changes: FileChanges| {
            if !changes.rescan && changes.paths.is_empty() {
                return;
            }
            // A folder removed from the project stops reporting at once, even
            // before its subscribers unwatch it.
            let listed = folder.as_ref().is_none_or(|folder| {
                catalog.upgrade().is_some_and(|catalog| {
                    catalog.project(project).is_ok_and(|descriptor| {
                        descriptor.directory == *folder
                            || catalog.folders(&descriptor).contains(folder)
                    })
                })
            });
            if listed {
                changes.root.clone_from(&folder);
                subscribers.send(&Message::FilesChanged { project, changes });
            }
        },
    )
}
