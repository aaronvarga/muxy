use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use muxy_protocol::{
    FileChanges, FilesAction, FilesReply, FilesRequest, Message, ProjectId, ServerPath,
};
use notify::{EventKind, RecursiveMode, Watcher};

use crate::{Registry, connection::Outbox};

use super::{Result, error, root::Root, wire_path};

/// Folders one connection can watch at once, each recursively.
pub(crate) const MAX_WATCHES: usize = 32;

#[derive(Default)]
pub(crate) struct Subscriptions {
    /// One watch per project folder; `None` is the project's own.
    watches: HashMap<(ProjectId, Option<ServerPath>), FileWatch>,
}

impl Subscriptions {
    pub(crate) fn request(
        &mut self,
        registry: &Registry,
        output: &Arc<Outbox>,
        request: &FilesRequest,
    ) -> Result<FilesReply> {
        let key = (request.project, request.root.clone());
        if request.action == FilesAction::Unwatch {
            self.watches.remove(&key);
            return Ok(FilesReply::Done);
        }
        if self.watches.len() >= MAX_WATCHES && !self.watches.contains_key(&key) {
            return Err(error("Too many file subscriptions"));
        }
        let root = Root::open(&registry.files_root(request)?)?;
        let events = Arc::downgrade(output);
        let catalog = Arc::downgrade(&registry.catalog);
        let project = request.project;
        let folder = request.root.clone();
        let watch = FileWatch::new(&root.path, move |mut changes| {
            // A folder removed from the project stops reporting at once, even
            // before this connection's client unwatches it.
            let listed = folder.as_ref().is_none_or(|folder| {
                catalog.upgrade().is_some_and(|catalog| {
                    catalog.project(project).is_ok_and(|descriptor| {
                        descriptor.directory == *folder
                            || catalog.folders(&descriptor).contains(folder)
                    })
                })
            });
            if listed && let Some(events) = events.upgrade() {
                changes.root.clone_from(&folder);
                events.push_control(Message::FilesChanged { project, changes });
            }
        })?;
        self.watches.insert(key, watch);
        Ok(FilesReply::Done)
    }
}

struct FileWatch {
    _watcher: notify::RecommendedWatcher,
    stop: Arc<AtomicBool>,
    wake: mpsc::SyncSender<()>,
    task: Option<JoinHandle<()>>,
}

impl FileWatch {
    fn new(root: &Path, publish: impl Fn(FileChanges) + Send + 'static) -> Result<Self> {
        let (wake, events) = mpsc::sync_channel(1);
        let pending = Arc::new(Mutex::new(FileChanges::default()));
        let notify = wake.clone();
        let changes = Arc::clone(&pending);
        let directory = root.to_owned();
        let mut watcher = notify::RecommendedWatcher::new(
            move |event: notify::Result<notify::Event>| {
                let mut pending = changes.lock().unwrap_or_else(PoisonError::into_inner);
                match event {
                    Ok(event) if matches!(event.kind, EventKind::Access(_)) => return,
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
                            let path = wire_path(relative);
                            if path.0.len() > 4096 {
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
                if pending.rescan || !pending.paths.is_empty() {
                    let _ = notify.try_send(());
                }
            },
            notify::Config::default().with_follow_symlinks(false),
        )
        .map_err(error)?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(error)?;
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&stop);
        let task = std::thread::Builder::new()
            .name("file-watch".into())
            .spawn(move || {
                while events.recv().is_ok() {
                    let deadline = Instant::now() + Duration::from_millis(300);
                    while !cancelled.load(Ordering::Acquire) {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() || events.recv_timeout(remaining).is_err() {
                            break;
                        }
                    }
                    if cancelled.load(Ordering::Acquire) {
                        return;
                    }
                    let changes = std::mem::take(
                        &mut *pending.lock().unwrap_or_else(PoisonError::into_inner),
                    );
                    if changes.rescan || !changes.paths.is_empty() {
                        publish(changes);
                    }
                }
            })
            .map_err(error)?;
        Ok(Self {
            _watcher: watcher,
            stop,
            wake,
            task: Some(task),
        })
    }
}

impl Drop for FileWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}
