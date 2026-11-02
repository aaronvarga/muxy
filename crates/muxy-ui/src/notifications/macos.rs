use std::collections::HashSet;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_channel::{Receiver, Sender};
use block2::{DynBlock, RcBlock};
use muxy_core::capability::Capability;
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSArray, NSBundle, NSError, NSObject, NSObjectProtocol, NSString, NSURL};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationAttachment, UNNotificationPresentationOptions, UNNotificationRequest,
    UNNotificationResponse, UNNotificationSettings, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

use super::Event;

struct IconFile(PathBuf);

impl Drop for IconFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn icon_attachment(
    icon: &crate::status_menu::ProjectIcon,
) -> Option<(Retained<UNNotificationAttachment>, Arc<IconFile>)> {
    use image::ImageEncoder;
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let image = crate::status_menu::project_icon(icon);
    let pixels = crate::bitmap::render_rgba(&image, 64, 64)?;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&pixels, 64, 64, image::ExtendedColorType::Rgba8)
        .ok()?;
    let path = std::env::temp_dir().join(format!(
        "muxy-notification-{}-{}.png",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .ok()?;
    let guard = Arc::new(IconFile(path));
    file.write_all(&png).ok()?;
    drop(file);
    let url = NSURL::from_file_path(&guard.0)?;
    // SAFETY: No options dictionary is supplied; AppKit rendered the local PNG.
    let attachment = unsafe {
        UNNotificationAttachment::attachmentWithIdentifier_URL_options_error(
            &NSString::from_str("project"),
            &url,
            None,
        )
    }
    .ok()?;
    Some((attachment, guard))
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "MuxyNotificationDelegate"]
    #[ivars = Sender<Event>]
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
            let _ = self.ivars().try_send(Event::Opened(response.notification().request().identifier().to_string()));
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
    permission: Arc<Permission>,
}

/// Whether the user lets this app notify, as macOS last said.
#[derive(Debug)]
struct Permission {
    denied: Mutex<bool>,
    events: Sender<Event>,
}

impl Permission {
    fn capability(denied: bool) -> Capability {
        if denied {
            Capability::Unavailable(
                crate::tr!("Notifications are turned off for Muxy in System Settings.").to_string(),
            )
        } else {
            Capability::Supported
        }
    }

    fn current(&self) -> Capability {
        Self::capability(*self.denied.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Tells the app when the answer has changed.
    fn report(&self, denied: bool) {
        let mut current = self.denied.lock().unwrap_or_else(PoisonError::into_inner);
        if *current != denied {
            *current = denied;
            let _ = self
                .events
                .try_send(Event::Capability(Self::capability(denied)));
        }
    }
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

    pub(super) fn capability(&self) -> Capability {
        self.permission.current()
    }

    /// Asks macOS whether the user still lets this app notify.
    pub(super) fn refresh(&self) {
        let permission = Arc::clone(&self.permission);
        let completion = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // SAFETY: macOS passes valid settings for the duration of the call.
            let status = unsafe { settings.as_ref() }.authorizationStatus();
            permission.report(status == UNAuthorizationStatus::Denied);
        });
        self.center
            .getNotificationSettingsWithCompletionHandler(&completion);
    }

    /// Unbundled binaries cannot use the macOS notification service.
    pub(super) fn start() -> Result<(Self, Receiver<Event>), Capability> {
        if NSBundle::mainBundle().bundleIdentifier().is_none() {
            return Err(Capability::Unavailable(
                crate::tr!("Desktop notifications need Muxy to run from its app bundle.")
                    .to_string(),
            ));
        }
        let (sender, receiver) = async_channel::bounded(32);
        let permission = Arc::new(Permission {
            denied: Mutex::new(false),
            events: sender.clone(),
        });
        let delegate = Delegate::alloc().set_ivars(sender);
        // SAFETY: initialization of our NSObject subclass, with its ivars installed.
        let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };
        let center = UNUserNotificationCenter::currentNotificationCenter();
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // Dock badges need authorization even before the first notification is delivered.
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Badge,
            &RcBlock::new(|_: Bool, _: *mut NSError| {}),
        );
        let service = Self {
            center,
            _delegate: delegate,
            pending: Arc::default(),
            permission,
        };
        service.refresh();
        Ok((service, receiver))
    }

    /// The system brings the app forward for a click on any notification.
    pub(super) fn inform(&self, id: &str, title: &str, body: &str) {
        self.deliver(id, title, body, None);
    }

    pub(super) fn deliver(
        &self,
        id: &str,
        title: &str,
        body: &str,
        icon: Option<&crate::status_menu::ProjectIcon>,
    ) {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        content.setSound(Some(&UNNotificationSound::defaultSound()));
        let icon_file = icon.and_then(icon_attachment).map(|(attachment, file)| {
            content.setAttachments(&NSArray::from_retained_slice(&[attachment]));
            file
        });
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(id),
            &content,
            None,
        );
        let center = self.center.clone();
        let id = id.to_owned();
        let pending = Arc::clone(&self.pending);
        let permission = Arc::clone(&self.permission);
        pending.register(id.clone());
        let completion = RcBlock::new(move |granted: Bool, _: *mut NSError| {
            permission.report(!granted.as_bool());
            pending.authorized(&id, granted.as_bool(), || {
                let icon_file = icon_file.clone();
                let submitted = RcBlock::new(move |_: *mut NSError| {
                    // Retain the source PNG until macOS has accepted or rejected the attachment.
                    let _ = &icon_file;
                });
                center.addNotificationRequest_withCompletionHandler(&request, Some(&submitted));
            });
        });
        self.center
            .requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert
                    | UNAuthorizationOptions::Sound
                    | UNAuthorizationOptions::Badge,
                &completion,
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_artwork_becomes_a_bounded_upright_notification_attachment() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_fn(18, 18, |_, y| {
            image::Rgba(if y < 9 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            })
        })
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("logo");
        for logo in [Some(Arc::from(png.into_inner())), None] {
            let icon = crate::status_menu::ProjectIcon {
                logo,
                glyph: "sf:house.fill".into(),
                color: "#123456".into(),
            };
            let (attachment, file) = icon_attachment(&icon).expect("image attachment");
            let path = attachment.URL().to_file_path().expect("attachment path");
            let bitmap = image::open(&path).expect("read PNG").to_rgba8();
            assert_eq!(bitmap.dimensions(), (64, 64));
            if icon.logo.is_some() {
                assert!(bitmap.get_pixel(32, 8)[0] > 200, "red stays above blue");
                assert!(bitmap.get_pixel(32, 55)[2] > 200, "blue stays below red");
            }
            let source = file.0.clone();
            drop(file);
            assert!(!source.exists(), "temporary source is cleaned up");
        }
    }

    #[test]
    fn a_denied_permission_is_reported_once_and_lifted_when_allowed_again() {
        let (events, reported) = async_channel::bounded(4);
        let permission = Permission {
            denied: Mutex::new(false),
            events,
        };
        permission.report(false);
        assert!(reported.try_recv().is_err(), "nothing changed");
        permission.report(true);
        permission.report(true);
        assert!(matches!(
            reported.try_recv(),
            Ok(Event::Capability(Capability::Unavailable(_)))
        ));
        assert!(reported.try_recv().is_err(), "reported once");
        assert!(!permission.current().is_supported());
        permission.report(false);
        assert_eq!(
            reported.try_recv(),
            Ok(Event::Capability(Capability::Supported))
        );
    }
}
