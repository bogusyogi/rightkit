//! macOS: `UNUserNotificationCenter`.
//!
//! - Replace by key: the request identifier is the key; adding a request with the identifier of one
//!   already delivered or pending replaces it. Close removes both.
//! - Actions: a notification with buttons gets a category whose identifier encodes its buttons.
//!   Categories are registered as a union: every category ever used in the process stays registered, as
//!   `setNotificationCategories` replaces the whole set.
//! - Clicks: one `UNUserNotificationCenterDelegate` (installed with the first call that reaches the
//!   center) forwards body clicks, dismissals and button presses to the registered activation handler,
//!   and lets the banner show while the app is in front.
//! - Badge: `setBadgeCount` (macOS 13 and later) on the notification center, which needs the Badge
//!   authorization requested with the first permission prompt.
//!
//! UserNotifications raises an Objective-C exception in a process without an app bundle, so every entry
//! point checks for a `.app` bundle with an identifier first and returns `Error::Unsupported` otherwise.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_foundation::{NSArray, NSBundle, NSError, NSObject, NSObjectProtocol, NSSet, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationAction, UNNotificationActionOptions, UNNotificationCategory,
    UNNotificationCategoryOptions, UNNotificationPresentationOptions, UNNotificationRequest,
    UNNotificationResponse, UNNotificationSettings, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

use crate::{
    Action, Activation, ActivationKind, Capabilities, Delivered, Error, Notification, Permission,
    Sound,
};

const REPLY: Duration = Duration::from_secs(10);
/// Authorization shows a system prompt on first use; give the person time to answer it.
const PROMPT: Duration = Duration::from_secs(60);

const DEFAULT_ACTION: &str = "com.apple.UNNotificationDefaultActionIdentifier";
const DISMISS_ACTION: &str = "com.apple.UNNotificationDismissActionIdentifier";

pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        actions: true,
        activation: true,
        replace: true,
        close: true,
        permission: true,
        badge: true,
        delivered: true,
    }
}

// ---- delegate ---------------------------------------------------------------------------------

define_class!(
    // SAFETY: NSObject has no subclassing requirements and this class has no Drop and no ivars.
    #[unsafe(super(NSObject))]
    #[name = "RightKitNotificationDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        #[allow(non_snake_case)]
        fn userNotificationCenter_willPresentNotification_withCompletionHandler(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion_handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            // Show the banner and list entry even while the app is in front.
            completion_handler.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        #[allow(non_snake_case)]
        fn userNotificationCenter_didReceiveNotificationResponse_withCompletionHandler(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &block2::DynBlock<dyn Fn()>,
        ) {
            let action = response.actionIdentifier().to_string();
            let key = response.notification().request().identifier().to_string();
            let kind = match action.as_str() {
                DEFAULT_ACTION => ActivationKind::Default,
                DISMISS_ACTION => ActivationKind::Dismiss,
                other => ActivationKind::Action(other.to_owned()),
            };
            crate::dispatch(Activation { key, kind });
            completion_handler.call(());
        }
    }
);

impl Delegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Keeps the delegate alive (the center holds it weakly).
struct Keep(#[allow(dead_code)] Retained<Delegate>);

// SAFETY: the value is only held to keep the object alive; it is never used from this wrapper.
unsafe impl Send for Keep {}
unsafe impl Sync for Keep {}

static DELEGATE: OnceLock<Keep> = OnceLock::new();

fn install_delegate(center: &UNUserNotificationCenter) {
    DELEGATE.get_or_init(|| {
        let delegate = Delegate::new();
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        Keep(delegate)
    });
}

pub(crate) fn activation_registered() {
    if let Ok(center) = center() {
        install_delegate(&center);
    }
}

// ---- center and permission --------------------------------------------------------------------

fn center() -> Result<Retained<UNUserNotificationCenter>, Error> {
    let bundle = NSBundle::mainBundle();
    if bundle.bundleIdentifier().is_none() || !bundle.bundlePath().to_string().ends_with(".app") {
        return Err(Error::Unsupported(
            "native notifications need an app bundle (.app) with a bundle identifier".into(),
        ));
    }
    let center = UNUserNotificationCenter::currentNotificationCenter();
    install_delegate(&center);
    Ok(center)
}

fn status(center: &Retained<UNUserNotificationCenter>) -> Result<UNAuthorizationStatus, Error> {
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |settings: std::ptr::NonNull<UNNotificationSettings>| {
        // SAFETY: the OS passes a live settings object for the duration of the callback.
        let _ = tx.send(unsafe { settings.as_ref() }.authorizationStatus());
    });
    center.getNotificationSettingsWithCompletionHandler(&block);
    rx.recv_timeout(REPLY)
        .map_err(|_| Error::Timeout("notification settings did not answer".into()))
}

fn permission_of(status: UNAuthorizationStatus) -> Permission {
    match status {
        UNAuthorizationStatus::Authorized => Permission::Granted,
        UNAuthorizationStatus::Denied => Permission::Denied,
        UNAuthorizationStatus::NotDetermined => Permission::NotDetermined,
        UNAuthorizationStatus::Provisional | UNAuthorizationStatus::Ephemeral => {
            Permission::Provisional
        }
        _ => Permission::Unsupported,
    }
}

pub(crate) fn permission() -> Result<Permission, Error> {
    Ok(permission_of(status(&center()?)?))
}

pub(crate) fn request_permission() -> Result<Permission, Error> {
    let center = center()?;
    if status(&center)? == UNAuthorizationStatus::NotDetermined {
        let (tx, rx) = mpsc::channel();
        let block = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
            let _ = tx.send(granted.as_bool());
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert
                | UNAuthorizationOptions::Sound
                | UNAuthorizationOptions::Badge,
            &block,
        );
        rx.recv_timeout(PROMPT).map_err(|_| {
            Error::Timeout("notification permission prompt was not answered".into())
        })?;
    }
    Ok(permission_of(status(&center)?))
}

fn ensure_authorized(center: &Retained<UNUserNotificationCenter>) -> Result<(), Error> {
    match permission_of(status(center)?) {
        Permission::Denied => Err(denied()),
        Permission::NotDetermined => match request_permission()? {
            Permission::Denied => Err(denied()),
            _ => Ok(()),
        },
        _ => Ok(()),
    }
}

fn denied() -> Error {
    Error::PermissionDenied("notifications are not allowed for this app; enable them in System Settings > Notifications".into())
}

// ---- categories (actions) ---------------------------------------------------------------------

/// Every action set ever used, by category identifier.
static CATEGORIES: Mutex<BTreeMap<String, Vec<Action>>> = Mutex::new(BTreeMap::new());

fn category_id(actions: &[Action]) -> String {
    let parts: Vec<String> = actions
        .iter()
        .map(|a| format!("{}={}", a.id, a.title))
        .collect();
    format!("rk.actions:{}", parts.join("|"))
}

/// Registers the category for `actions` (idempotent) and returns its identifier.
fn register_category(center: &UNUserNotificationCenter, actions: &[Action]) -> String {
    let id = category_id(actions);
    let mut known = CATEGORIES.lock().unwrap_or_else(|e| e.into_inner());
    if !known.contains_key(&id) {
        known.insert(id.clone(), actions.to_vec());
        let categories: Vec<Retained<UNNotificationCategory>> = known
            .iter()
            .map(|(category, actions)| {
                let buttons: Vec<Retained<UNNotificationAction>> = actions
                    .iter()
                    .map(|a| {
                        UNNotificationAction::actionWithIdentifier_title_options(
                            &NSString::from_str(&a.id),
                            &NSString::from_str(&a.title),
                            UNNotificationActionOptions::Foreground,
                        )
                    })
                    .collect();
                let no_intents: Retained<NSArray<NSString>> = NSArray::new();
                UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                    &NSString::from_str(category),
                    &NSArray::from_retained_slice(&buttons),
                    &no_intents,
                    UNNotificationCategoryOptions::empty(),
                )
            })
            .collect();
        center.setNotificationCategories(&NSSet::from_retained_slice(&categories));
    }
    id
}

// ---- post, close, badge -----------------------------------------------------------------------

pub(crate) fn post(_app_id: &str, key: &str, notification: &Notification) -> Result<(), Error> {
    if notification.title.trim().is_empty() {
        return Err(Error::Backend("a notification needs a title".into()));
    }
    let center = center()?;
    ensure_authorized(&center)?;
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(&notification.title));
    content.setBody(&NSString::from_str(&notification.body));
    content.setThreadIdentifier(&NSString::from_str(
        notification.thread.as_deref().unwrap_or(key),
    ));
    match &notification.sound {
        Sound::Silent => content.setSound(None),
        Sound::Default => content.setSound(Some(&UNNotificationSound::defaultSound())),
        Sound::Named(name) => content.setSound(Some(&UNNotificationSound::soundNamed(
            &NSString::from_str(name),
        ))),
    }
    if !notification.actions.is_empty() {
        let category = register_category(&center, &notification.actions);
        content.setCategoryIdentifier(&NSString::from_str(&category));
    }
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(key),
        &content,
        None,
    );
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: a non-null error pointer is a live NSError for the duration of the callback.
        let message =
            (!error.is_null()).then(|| unsafe { &*error }.localizedDescription().to_string());
        let _ = tx.send(message);
    });
    center.addNotificationRequest_withCompletionHandler(&request, Some(&block));
    match rx
        .recv_timeout(REPLY)
        .map_err(|_| Error::Timeout("notification center did not answer".into()))?
    {
        None => Ok(()),
        Some(message) => Err(Error::Backend(format!("show notification: {message}"))),
    }
}

pub(crate) fn close(_app_id: &str, key: &str) -> Result<(), Error> {
    let center = center()?;
    let ids = NSArray::from_retained_slice(&[NSString::from_str(key)]);
    center.removeDeliveredNotificationsWithIdentifiers(&ids);
    center.removePendingNotificationRequestsWithIdentifiers(&ids);
    Ok(())
}

/// Withdraws every delivered and pending notification of this app.
pub(crate) fn close_all(_app_id: &str) -> Result<(), Error> {
    let center = center()?;
    center.removeAllDeliveredNotifications();
    center.removeAllPendingNotificationRequests();
    Ok(())
}

/// What the notification center still shows for this app.
pub(crate) fn delivered(_app_id: &str) -> Result<Vec<Delivered>, Error> {
    let center = center()?;
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |list: std::ptr::NonNull<NSArray<UNNotification>>| {
        // SAFETY: the OS passes a live array for the duration of the callback.
        let list = unsafe { list.as_ref() };
        let mut out = Vec::new();
        for i in 0..list.count() {
            let request = list.objectAtIndex(i).request();
            let content = request.content();
            out.push(Delivered {
                key: request.identifier().to_string(),
                title: content.title().to_string(),
                body: content.body().to_string(),
                thread: content.threadIdentifier().to_string(),
            });
        }
        let _ = tx.send(out);
    });
    center.getDeliveredNotificationsWithCompletionHandler(&block);
    rx.recv_timeout(REPLY)
        .map_err(|_| Error::Timeout("delivered notifications did not answer".into()))
}

pub(crate) fn set_badge(_app_id: &str, count: u32) -> Result<(), Error> {
    let center = center()?;
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: a non-null error pointer is a live NSError for the duration of the callback.
        let message =
            (!error.is_null()).then(|| unsafe { &*error }.localizedDescription().to_string());
        let _ = tx.send(message);
    });
    center.setBadgeCount_withCompletionHandler(count as isize, Some(&block));
    match rx
        .recv_timeout(REPLY)
        .map_err(|_| Error::Timeout("badge update did not answer".into()))?
    {
        None => Ok(()),
        Some(message) => Err(Error::Backend(format!("set badge: {message}"))),
    }
}
