//! Windows: WinRT toasts through the `windows` crate (not `tauri-winrt-notification`, which cannot
//! tag a toast), so a send with a key replaces the shown toast (toast tag and group) and a close
//! withdraws it from the history. The badge is the Start and taskbar number badge of an AUMID app.
//!
//! Known gaps, reported by `capabilities()`: no click or action callback (activation after the app
//! exited needs COM activation registered by the installer; a click while running is not wired), no
//! action buttons, no per-app permission prompt, and no taskbar overlay icon (only the numeric badge).

use windows::core::HSTRING;
use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{
    BadgeNotification, BadgeUpdateManager, NotificationSetting, ToastNotification,
    ToastNotificationManager,
};

use crate::{Capabilities, Delivered, Error, Notification, Permission, Sound};

/// Toast group for everything this crate posts.
const GROUP: &str = "rightkit";

fn backend(what: &str, error: windows::core::Error) -> Error {
    Error::Backend(format!("{what}: {error}"))
}

pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        actions: false,
        activation: false,
        replace: true,
        close: true,
        permission: true,
        badge: true,
        delivered: false,
    }
}

fn notifier(app_id: &str) -> Result<windows::UI::Notifications::ToastNotifier, Error> {
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(app_id))
        .map_err(|e| backend("toast notifier", e))
}

/// The OS setting for the app's toasts (user or group policy can disable them).
pub(crate) fn permission() -> Result<Permission, Error> {
    // Without an app id the notifier cannot attribute toasts; ask for the process default.
    let notifier = ToastNotificationManager::CreateToastNotifier()
        .map_err(|e| backend("toast notifier", e))?;
    let setting = notifier
        .Setting()
        .map_err(|e| backend("toast setting", e))?;
    Ok(if setting == NotificationSetting::Enabled {
        Permission::Granted
    } else {
        Permission::Denied
    })
}

/// Windows has no per-app prompt: the answer is the current setting.
pub(crate) fn request_permission() -> Result<Permission, Error> {
    permission()
}

pub(crate) fn activation_registered() {}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// A toast tag is at most 64 characters (older Windows 10 builds); longer keys use a stable hash.
fn tag(key: &str) -> String {
    if key.chars().count() <= 64 {
        return key.to_owned();
    }
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("rk-{hash:016x}")
}

fn sound_event(sound: &Sound) -> Option<&'static str> {
    match sound {
        Sound::Silent => None,
        Sound::Default => Some("Notification.Default"),
        Sound::Named(name) => Some(match name.trim().to_ascii_lowercase().as_str() {
            "im" => "Notification.IM",
            "mail" => "Notification.Mail",
            "reminder" => "Notification.Reminder",
            "sms" => "Notification.SMS",
            _ => "Notification.Default",
        }),
    }
}

fn toast_xml(key: &str, notification: &Notification) -> String {
    let audio = match sound_event(&notification.sound) {
        Some(event) => format!("<audio src=\"ms-winsoundevent:{event}\"/>"),
        None => "<audio silent=\"true\"/>".to_owned(),
    };
    format!(
        "<toast launch=\"{}\"><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>{audio}</toast>",
        escape(key),
        escape(notification.title.trim()),
        escape(notification.body.trim()),
    )
}

pub(crate) fn post(app_id: &str, key: &str, notification: &Notification) -> Result<(), Error> {
    if notification.title.trim().is_empty() {
        return Err(Error::Backend("a notification needs a title".into()));
    }
    let xml = XmlDocument::new().map_err(|e| backend("xml document", e))?;
    xml.LoadXml(&HSTRING::from(toast_xml(key, notification)))
        .map_err(|e| backend("toast xml", e))?;
    let toast =
        ToastNotification::CreateToastNotification(&xml).map_err(|e| backend("toast", e))?;
    toast
        .SetTag(&HSTRING::from(tag(key)))
        .map_err(|e| backend("toast tag", e))?;
    toast
        .SetGroup(&HSTRING::from(GROUP))
        .map_err(|e| backend("toast group", e))?;
    notifier(app_id)?
        .Show(&toast)
        .map_err(|e| backend("show toast", e))
}

pub(crate) fn close(app_id: &str, key: &str) -> Result<(), Error> {
    // A toast that is not shown is not an error.
    let history = ToastNotificationManager::History().map_err(|e| backend("toast history", e))?;
    let tag = HSTRING::from(tag(key));
    let _ = history.RemoveGroupedTagWithId(&tag, &HSTRING::from(GROUP), &HSTRING::from(app_id));
    Ok(())
}

pub(crate) fn set_badge(app_id: &str, count: u32) -> Result<(), Error> {
    let updater =
        BadgeUpdateManager::CreateBadgeUpdaterForApplicationWithId(&HSTRING::from(app_id))
            .map_err(|e| backend("badge updater", e))?;
    if count == 0 {
        return updater.Clear().map_err(|e| backend("clear badge", e));
    }
    let xml = XmlDocument::new().map_err(|e| backend("xml document", e))?;
    xml.LoadXml(&HSTRING::from(format!(
        "<badge value=\"{}\"/>",
        count.min(99)
    )))
    .map_err(|e| backend("badge xml", e))?;
    let badge =
        BadgeNotification::CreateBadgeNotification(&xml).map_err(|e| backend("badge", e))?;
    updater
        .Update(&badge)
        .map_err(|e| backend("update badge", e))
}

/// Clears this app's toast history. Nothing shown is not an error.
pub(crate) fn close_all(app_id: &str) -> Result<(), Error> {
    let history = ToastNotificationManager::History().map_err(|e| backend("toast history", e))?;
    let _ = history.ClearWithId(&HSTRING::from(app_id));
    Ok(())
}

/// The toast history carries no readable content through this crate.
pub(crate) fn delivered(_app_id: &str) -> Result<Vec<Delivered>, Error> {
    Ok(Vec::new())
}
