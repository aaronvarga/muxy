use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use async_channel::{Receiver, Sender};
use block2::{DynBlock, RcBlock};
use muxy_core::capability::Capability;
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSArray, NSBundle, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNNotificationSound, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "MuxyNotificationDelegate"]
    #[ivars = Sender<String>]
    #[derive(Debug)]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn present(&self, _: &UNUserNotificationCenter, _: &UNNotification, completion: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>) {
            completion.call((UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn respond(&self, _: &UNUserNotificationCenter, response: &UNNotificationResponse, completion: &DynBlock<dyn Fn()>) {
            let _ = self.ivars().try_send(response.notification().request().identifier().to_string());
            completion.call(());
        }
    }
);

pub const CAPABILITY: Capability = Capability::Supported;

#[derive(Debug)]
pub(super) struct Service {
    center: Retained<UNUserNotificationCenter>,
    _delegate: Retained<Delegate>,
    pending: Arc<PendingDeliveries>,
}

#[derive(Debug, Default)]
struct PendingDeliveries(Mutex<HashSet<String>>);

impl PendingDeliveries {
    fn register(&self, id: String) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id);
    }

    fn cancel(&self, ids: &[String], clear: impl FnOnce()) {
        let mut pending = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        pending.retain(|id| !ids.contains(id));
        clear();
    }

    fn authorized(&self, id: &str, granted: bool, submit: impl FnOnce()) {
        let mut pending = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if pending.remove(id) && granted {
            submit();
        }
    }
}

impl Service {
    pub(super) fn clear(&self, ids: &[String]) {
        // Serialize cancellation with submission from the authorization callback.
        self.pending.cancel(ids, || {
            let ids: Vec<_> = ids.iter().map(|id| NSString::from_str(id)).collect();
            let ids = NSArray::from_retained_slice(&ids);
            self.center
                .removePendingNotificationRequestsWithIdentifiers(&ids);
            self.center
                .removeDeliveredNotificationsWithIdentifiers(&ids);
        });
    }

    /// Unbundled binaries cannot use the macOS notification service.
    pub(super) fn start() -> Result<(Self, Receiver<String>), Capability> {
        if NSBundle::mainBundle().bundleIdentifier().is_none() {
            return Err(Capability::Unavailable(
                crate::tr!("Desktop notifications need Muxy to run from its app bundle.")
                    .to_string(),
            ));
        }
        let (sender, receiver) = async_channel::bounded(32);
        let delegate = Delegate::alloc().set_ivars(sender);
        // SAFETY: initialization of our NSObject subclass, with its ivars installed.
        let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };
        let center = UNUserNotificationCenter::currentNotificationCenter();
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        Ok((
            Self {
                center,
                _delegate: delegate,
                pending: Arc::default(),
            },
            receiver,
        ))
    }

    pub(super) fn deliver(&self, id: &str, title: &str, body: &str) {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        content.setSound(Some(&UNNotificationSound::defaultSound()));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(id),
            &content,
            None,
        );
        let center = self.center.clone();
        let id = id.to_owned();
        let pending = Arc::clone(&self.pending);
        pending.register(id.clone());
        let completion = RcBlock::new(move |granted: Bool, _: *mut NSError| {
            pending.authorized(&id, granted.as_bool(), || {
                center.addNotificationRequest_withCompletionHandler(&request, None);
            });
        });
        self.center
            .requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &completion,
            );
    }
}
