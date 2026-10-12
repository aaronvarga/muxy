//! Native delivery only; notification history and policy belong to callers.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;

#[cfg(any(test, feature = "test-support"))]
use std::sync::{Arc, Mutex, PoisonError};

use async_channel::Receiver;
#[cfg(any(test, feature = "test-support"))]
use async_channel::Sender;
use muxy_core::capability::Capability;

#[cfg(target_os = "macos")]
pub use macos::CAPABILITY;
#[cfg(target_os = "macos")]
use macos::Service;
#[cfg(not(target_os = "macos"))]
pub use unsupported::CAPABILITY;
#[cfg(not(target_os = "macos"))]
use unsupported::Service;

#[derive(Debug)]
enum Backend {
    Native(Service),
    #[cfg(any(test, feature = "test-support"))]
    Simulated(SimulatedNotifications),
    Absent(Capability),
}

#[derive(Debug)]
pub struct Notifications(Backend);

impl Notifications {
    /// Starts the platform's notification service. The receiver yields the
    /// identifier of each notification the user opens.
    pub fn start() -> (Self, Receiver<String>) {
        match Service::start() {
            Ok((service, opened)) => (Self(Backend::Native(service)), opened),
            Err(capability) => {
                let (_, opened) = async_channel::bounded(1);
                (Self(Backend::Absent(capability)), opened)
            }
        }
    }

    pub fn capability(&self) -> Capability {
        match &self.0 {
            Backend::Native(_) => Capability::Supported,
            #[cfg(any(test, feature = "test-support"))]
            Backend::Simulated(_) => Capability::Supported,
            Backend::Absent(capability) => capability.clone(),
        }
    }

    pub fn deliver(&self, id: &str, title: &str, body: &str) {
        match &self.0 {
            Backend::Native(service) => service.deliver(id, title, body),
            #[cfg(any(test, feature = "test-support"))]
            Backend::Simulated(simulated) => simulated.shown().push(id.to_owned()),
            Backend::Absent(_) => {}
        }
    }

    pub fn clear(&self, ids: &[String]) {
        match &self.0 {
            Backend::Native(service) => service.clear(ids),
            #[cfg(any(test, feature = "test-support"))]
            Backend::Simulated(simulated) => simulated.shown().retain(|id| !ids.contains(id)),
            Backend::Absent(_) => {}
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug)]
pub struct SimulatedNotifications {
    shown: Arc<Mutex<Vec<String>>>,
    opened: Sender<String>,
}

#[cfg(any(test, feature = "test-support"))]
impl SimulatedNotifications {
    pub fn start() -> (Notifications, Receiver<String>, Self) {
        let (sender, opened) = async_channel::bounded(32);
        let simulated = Self {
            shown: Arc::default(),
            opened: sender,
        };
        (
            Notifications(Backend::Simulated(simulated.clone())),
            opened,
            simulated,
        )
    }

    fn shown(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
        self.shown.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Identifiers delivered and not yet cleared, in delivery order.
    pub fn visible(&self) -> Vec<String> {
        self.shown().clone()
    }

    /// Acts as the user opening the notification `id`.
    pub fn open(&self, id: &str) {
        let _ = self.opened.try_send(id.to_owned());
    }
}
