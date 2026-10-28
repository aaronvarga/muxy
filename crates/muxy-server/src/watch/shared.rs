use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use muxy_protocol::{Message, ProjectId};

use super::DebouncedWatch;
use crate::ServerError;
use crate::connection::Connection;

/// One watch per project (or per `K`, such as a project folder), whoever asks
/// for it. A watch stops when its last subscription is dropped.
pub(crate) struct Shared<K = ProjectId>(Arc<Mutex<HashMap<K, Arc<Slot>>>>);

impl<K> Clone for Shared<K> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<K> Default for Shared<K> {
    fn default() -> Self {
        Self(Arc::default())
    }
}

#[derive(Default)]
struct Slot {
    watch: Mutex<Option<DebouncedWatch>>,
    subscribers: Subscribers,
}

/// The connections a watch reports to, held weakly so a closed connection
/// never stays alive for its watch.
#[derive(Clone, Default)]
pub(crate) struct Subscribers(Arc<Mutex<Vec<Weak<Connection>>>>);

impl Subscribers {
    pub(crate) fn send(&self, message: &Message) {
        let mut connections = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        connections.retain(|connection| match connection.upgrade() {
            Some(connection) => {
                connection.push_control(message.clone());
                true
            }
            None => false,
        });
    }

    fn add(&self, connection: Weak<Connection>) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(connection);
    }

    /// Returns whether anyone is still subscribed.
    fn remove(&self, connection: &Weak<Connection>) -> bool {
        let mut connections = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(index) = connections
            .iter()
            .position(|other| other.ptr_eq(connection))
        {
            connections.swap_remove(index);
        }
        connections.retain(|other| other.strong_count() > 0);
        !connections.is_empty()
    }
}

pub(crate) struct Subscription<K: Clone + Eq + std::hash::Hash = ProjectId> {
    shared: Shared<K>,
    project: K,
    slot: Arc<Slot>,
    connection: Weak<Connection>,
}

impl<K: Clone + Eq + std::hash::Hash> Subscription<K> {
    pub(crate) fn project(&self) -> K {
        self.project.clone()
    }
}

impl<K: Clone + Eq + std::hash::Hash> Shared<K> {
    /// Subscribes `connection` to the project's watch, starting it with `start`
    /// when nobody watches the project yet. `start` returns `None` when there
    /// is nothing to watch.
    pub(crate) fn subscribe(
        &self,
        project: K,
        connection: &Arc<Connection>,
        start: impl FnOnce(Subscribers) -> Result<Option<DebouncedWatch>, ServerError>,
    ) -> Result<Option<Subscription<K>>, ServerError> {
        loop {
            let slot = Arc::clone(self.slots().entry(project.clone()).or_default());
            let mut watch = slot.watch.lock().unwrap_or_else(PoisonError::into_inner);
            if !self.holds(&project, &slot) {
                continue;
            }
            if watch.is_none() {
                match start(slot.subscribers.clone()) {
                    Ok(Some(started)) => *watch = Some(started),
                    Ok(None) => {
                        self.forget(&project, &slot);
                        return Ok(None);
                    }
                    Err(error) => {
                        self.forget(&project, &slot);
                        return Err(error);
                    }
                }
            }
            let connection = Arc::downgrade(connection);
            slot.subscribers.add(Weak::clone(&connection));
            drop(watch);
            return Ok(Some(Subscription {
                shared: self.clone(),
                project,
                slot,
                connection,
            }));
        }
    }

    fn slots(&self) -> std::sync::MutexGuard<'_, HashMap<K, Arc<Slot>>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn holds(&self, project: &K, slot: &Arc<Slot>) -> bool {
        self.slots()
            .get(project)
            .is_some_and(|held| Arc::ptr_eq(held, slot))
    }

    /// Called with the slot's watch locked, so nobody joins it meanwhile.
    fn forget(&self, project: &K, slot: &Arc<Slot>) {
        let mut slots = self.slots();
        if slots
            .get(project)
            .is_some_and(|held| Arc::ptr_eq(held, slot))
        {
            slots.remove(project);
        }
    }

    #[cfg(test)]
    pub(crate) fn watching(&self) -> usize {
        self.slots().len()
    }
}

impl<K: Clone + Eq + std::hash::Hash> Drop for Subscription<K> {
    fn drop(&mut self) {
        let stopped_watch = {
            let mut watch = self
                .slot
                .watch
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if self.slot.subscribers.remove(&self.connection) {
                None
            } else {
                self.shared.forget(&self.project, &self.slot);
                watch.take()
            }
        };
        drop(stopped_watch);
    }
}

impl<K: Clone + Eq + std::hash::Hash> std::fmt::Debug for Shared<K> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Shared")
            .field("projects", &self.slots().len())
            .finish()
    }
}
