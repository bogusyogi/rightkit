//! freedesktop notifications over D-Bus (`notify-rust`, zbus backend). Replace uses the server's
//! notification id, which a map keeps per key. No action buttons, no badge, no per-app permission.

use std::collections::HashMap;
use std::sync::Mutex;

use notify_rust::{Notification as Native, NotificationHandle};

use crate::{Capabilities, Delivered, Error, Notification, Permission};

static SHOWN: Mutex<Option<HashMap<String, NotificationHandle>>> = Mutex::new(None);

pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        actions: false,
        activation: false,
        replace: true,
        close: true,
        permission: false,
        badge: false,
        delivered: false,
    }
}

pub(crate) fn permission() -> Result<Permission, Error> {
    Ok(Permission::Unsupported)
}

pub(crate) fn request_permission() -> Result<Permission, Error> {
    Ok(Permission::Unsupported)
}

pub(crate) fn activation_registered() {}

pub(crate) fn post(app_id: &str, key: &str, notification: &Notification) -> Result<(), Error> {
    let mut guard = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
    let shown = guard.get_or_insert_with(HashMap::new);
    if let Some(handle) = shown.get_mut(key) {
        handle.summary(&notification.title).body(&notification.body);
        // The notification stays on screen when an in-place update fails; the post is not an error.
        let _ = handle.update();
        return Ok(());
    }
    let handle = Native::new()
        .appname(app_id)
        .summary(&notification.title)
        .body(&notification.body)
        .show()
        .map_err(|e| Error::Backend(format!("show notification: {e}")))?;
    shown.insert(key.to_owned(), handle);
    Ok(())
}

pub(crate) fn close(_app_id: &str, key: &str) -> Result<(), Error> {
    let handle = SHOWN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|m| m.remove(key));
    if let Some(handle) = handle {
        handle.close();
    }
    Ok(())
}

pub(crate) fn set_badge(_app_id: &str, _count: u32) -> Result<(), Error> {
    Err(Error::Unsupported("Linux has no notification badge".into()))
}

/// Closes every notification this process shows. The freedesktop service offers no read-back of other
/// processes' notifications.
pub(crate) fn close_all(_app_id: &str) -> Result<(), Error> {
    let handles: Vec<NotificationHandle> = SHOWN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .map(|m| m.drain().map(|(_, handle)| handle).collect())
        .unwrap_or_default();
    for handle in handles {
        handle.close();
    }
    Ok(())
}

/// The freedesktop service has no read-back.
pub(crate) fn delivered(_app_id: &str) -> Result<Vec<Delivered>, Error> {
    Ok(Vec::new())
}
