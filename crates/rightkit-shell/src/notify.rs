//! Native notifications, badge and sound policy.
//!
//! Donor: CodeRight `notifications.rs` (per-target sound cooldown, `navigate_path || title` key,
//! non-negative badge, foreground attention cue, Do-Not-Disturb suppression through
//! [`crate::presence`]). The delivery itself is a direct OS call instead of a plugin:
//! `UNUserNotificationCenter` on macOS, which also gives the read-back used by QA
//! ([`delivered`], [`authorization`]). Other platforms report `unsupported` until their native
//! toast path is verified on a host of that platform; nothing is faked.
//!
//! Notification settings that apps persist (sound on/off, sound name, per-category toggles —
//! HeardRight and MailRight keep these in their own stores) arrive on every request; the shell owns
//! the delivery policy, the app owns its preferences.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::ShellError;

/// Per-target notification-sound cooldown (CodeRight: Omnigent's value).
pub const SOUND_THROTTLE_MS: u64 = 3_000;
/// macOS system sound used when the caller does not choose one.
pub const DEFAULT_SOUND: &str = "Glass";

#[derive(Default)]
pub struct NotificationState {
    last_sound: Mutex<HashMap<String, Instant>>,
    badge: AtomicU32,
    seq: AtomicU64,
}

impl NotificationState {
    pub fn new() -> Self {
        Self::default()
    }

    /// True for the first call for a key and again once the cooldown has elapsed.
    pub fn should_play_sound_at(&self, key: &str, now: Instant) -> bool {
        let mut last = self.last_sound.lock().unwrap_or_else(|e| e.into_inner());
        if last
            .get(key)
            .and_then(|at| now.checked_duration_since(*at))
            .is_some_and(|d| d < Duration::from_millis(SOUND_THROTTLE_MS))
        {
            return false;
        }
        last.insert(key.to_owned(), now);
        true
    }

    pub fn set_badge(&self, count: i64) -> u32 {
        let n = normalize_badge(count);
        self.badge.store(n, Ordering::Relaxed);
        n
    }

    pub fn badge(&self) -> u32 {
        self.badge.load(Ordering::Relaxed)
    }

    fn next_id(&self) -> String {
        format!(
            "rk-{}-{}",
            std::process::id(),
            self.seq.fetch_add(1, Ordering::Relaxed)
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationRequest {
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// Where the click should navigate; also the sound-throttle key (falls back to the title).
    #[serde(default)]
    pub navigate_path: Option<String>,
    #[serde(default)]
    pub sound_enabled: bool,
    #[serde(default)]
    pub sound_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationPlan {
    pub key: String,
    pub play_sound: bool,
    pub sound_name: Option<String>,
}

pub fn notification_key(navigate_path: Option<&str>, title: &str) -> String {
    navigate_path
        .filter(|p| !p.is_empty())
        .unwrap_or(title)
        .to_owned()
}

pub fn plan(
    req: &NotificationRequest,
    state: &NotificationState,
    now: Instant,
) -> NotificationPlan {
    let key = notification_key(req.navigate_path.as_deref(), &req.title);
    let play_sound = req.sound_enabled && state.should_play_sound_at(&key, now);
    NotificationPlan {
        key,
        play_sound,
        sound_name: play_sound.then(|| {
            req.sound_name
                .as_deref()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or(DEFAULT_SOUND)
                .to_owned()
        }),
    }
}

pub fn normalize_badge(count: i64) -> u32 {
    count.clamp(0, i64::from(u32::MAX)) as u32
}

/// What happened to a request.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyOutcome {
    pub delivered: bool,
    /// Why nothing was posted (`dnd`, `empty-title`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suppressed: Option<&'static str>,
    pub id: Option<String>,
    pub sound: bool,
}

/// One notification the OS reports as delivered to this app.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeliveredNotification {
    pub id: String,
    pub title: String,
    pub body: String,
    pub thread: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyStatus {
    /// `authorized`, `denied`, `not-determined`, `provisional`, `ephemeral`, or `unsupported`.
    pub authorization: String,
    pub supported: bool,
    pub delivered: Vec<DeliveredNotification>,
    pub badge: u32,
}

/// Post a notification unless the request is empty or Do-Not-Disturb is in effect.
pub fn post(
    req: &NotificationRequest,
    state: &NotificationState,
    dnd: bool,
) -> Result<NotifyOutcome, ShellError> {
    if req.title.trim().is_empty() {
        return Ok(NotifyOutcome {
            delivered: false,
            suppressed: Some("empty-title"),
            id: None,
            sound: false,
        });
    }
    if dnd {
        return Ok(NotifyOutcome {
            delivered: false,
            suppressed: Some("dnd"),
            id: None,
            sound: false,
        });
    }
    let plan = plan(req, state, Instant::now());
    let id = state.next_id();
    platform::post(&id, req, &plan)?;
    Ok(NotifyOutcome {
        delivered: true,
        suppressed: None,
        id: Some(id),
        sound: plan.play_sound,
    })
}

pub fn authorization() -> String {
    platform::authorization().unwrap_or_else(|e| format!("unavailable: {}", e.message()))
}

pub fn delivered() -> Result<Vec<DeliveredNotification>, ShellError> {
    platform::delivered()
}

pub fn clear_delivered(ids: &[String]) -> Result<(), ShellError> {
    platform::clear(ids)
}

pub fn status(state: &NotificationState) -> NotifyStatus {
    NotifyStatus {
        authorization: authorization(),
        supported: platform::SUPPORTED,
        delivered: delivered().unwrap_or_default(),
        badge: state.badge(),
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSArray, NSBundle, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent,
        UNNotification, UNNotificationInterruptionLevel, UNNotificationRequest,
        UNNotificationSettings, UNNotificationSound, UNUserNotificationCenter,
    };

    use super::{DeliveredNotification, NotificationPlan, NotificationRequest};
    use crate::error::ShellError;

    pub const SUPPORTED: bool = true;
    const REPLY: Duration = Duration::from_secs(10);
    /// Authorization shows a system prompt on first use; give the person time to answer it.
    const PROMPT: Duration = Duration::from_secs(60);

    /// UserNotifications raises an Objective-C exception for a process without an app bundle.
    fn center() -> Result<Retained<UNUserNotificationCenter>, ShellError> {
        let bundle = NSBundle::mainBundle();
        if bundle.bundleIdentifier().is_none() || !bundle.bundlePath().to_string().ends_with(".app")
        {
            return Err(ShellError::Unsupported(
                "native notifications need an app bundle (.app) with a bundle identifier".into(),
            ));
        }
        Ok(UNUserNotificationCenter::currentNotificationCenter())
    }

    fn settings(
        center: &Retained<UNUserNotificationCenter>,
    ) -> Result<UNAuthorizationStatus, ShellError> {
        let (tx, rx) = mpsc::channel();
        let block = RcBlock::new(move |s: std::ptr::NonNull<UNNotificationSettings>| {
            // SAFETY: the OS passes a live settings object for the duration of the callback.
            let _ = tx.send(unsafe { s.as_ref() }.authorizationStatus());
        });
        center.getNotificationSettingsWithCompletionHandler(&block);
        rx.recv_timeout(REPLY)
            .map_err(|_| ShellError::Timeout("notification settings did not answer".into()))
    }

    pub fn authorization() -> Result<String, ShellError> {
        Ok(match settings(&center()?)? {
            UNAuthorizationStatus::Authorized => "authorized",
            UNAuthorizationStatus::Denied => "denied",
            UNAuthorizationStatus::NotDetermined => "not-determined",
            UNAuthorizationStatus::Provisional => "provisional",
            UNAuthorizationStatus::Ephemeral => "ephemeral",
            _ => "unknown",
        }
        .to_owned())
    }

    fn ensure_authorized(center: &Retained<UNUserNotificationCenter>) -> Result<(), ShellError> {
        let status = settings(center)?;
        if status == UNAuthorizationStatus::NotDetermined {
            if std::env::var_os("RIGHTKIT_QA_BACKGROUND").as_deref()
                == Some(std::ffi::OsStr::new("1"))
            {
                return Err(ShellError::PermissionDenied("notification authorization is not determined; background QA never opens a permission dialog".into()));
            }
            let (tx, rx) = mpsc::channel();
            let block = RcBlock::new(move |granted: Bool, _err: *mut NSError| {
                let _ = tx.send(granted.as_bool());
            });
            center.requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert
                    | UNAuthorizationOptions::Sound
                    | UNAuthorizationOptions::Badge,
                &block,
            );
            let granted = rx.recv_timeout(PROMPT).map_err(|_| {
                ShellError::Timeout("notification permission prompt was not answered".into())
            })?;
            if !granted {
                return Err(denied());
            }
            return Ok(());
        }
        match status {
            UNAuthorizationStatus::Denied => Err(denied()),
            _ => Ok(()),
        }
    }

    fn denied() -> ShellError {
        ShellError::PermissionDenied("notifications are not allowed for this app; enable them in System Settings > Notifications".into())
    }

    pub fn post(
        id: &str,
        req: &NotificationRequest,
        plan: &NotificationPlan,
    ) -> Result<(), ShellError> {
        let center = center()?;
        ensure_authorized(&center)?;
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&req.title));
        content.setBody(&NSString::from_str(&req.body));
        content.setThreadIdentifier(&NSString::from_str(&plan.key));
        let quiet_qa = std::env::var_os("RIGHTKIT_QA_BACKGROUND").as_deref()
            == Some(std::ffi::OsStr::new("1"));
        if quiet_qa {
            content.setInterruptionLevel(UNNotificationInterruptionLevel::Passive);
            content.setSound(None);
        } else if let Some(name) = &plan.sound_name {
            content.setSound(Some(&UNNotificationSound::soundNamed(&NSString::from_str(
                name,
            ))));
        }
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(id),
            &content,
            None,
        );
        let (tx, rx) = mpsc::channel();
        let block = RcBlock::new(move |err: *mut NSError| {
            // SAFETY: a non-null error pointer is a live NSError for the duration of the callback.
            let msg = (!err.is_null()).then(|| unsafe { &*err }.localizedDescription().to_string());
            let _ = tx.send(msg);
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&block));
        match rx
            .recv_timeout(REPLY)
            .map_err(|_| ShellError::Timeout("notification center did not answer".into()))?
        {
            None => Ok(()),
            Some(m) => Err(ShellError::Other(format!("show notification: {m}"))),
        }
    }

    pub fn delivered() -> Result<Vec<DeliveredNotification>, ShellError> {
        let center = center()?;
        let (tx, rx) = mpsc::channel();
        let block = RcBlock::new(move |list: std::ptr::NonNull<NSArray<UNNotification>>| {
            // SAFETY: the OS passes a live array for the duration of the callback.
            let list = unsafe { list.as_ref() };
            let mut out = Vec::new();
            for i in 0..list.count() {
                let n = list.objectAtIndex(i);
                let r = n.request();
                let c = r.content();
                out.push(DeliveredNotification {
                    id: r.identifier().to_string(),
                    title: c.title().to_string(),
                    body: c.body().to_string(),
                    thread: c.threadIdentifier().to_string(),
                });
            }
            let _ = tx.send(out);
        });
        center.getDeliveredNotificationsWithCompletionHandler(&block);
        rx.recv_timeout(REPLY)
            .map_err(|_| ShellError::Timeout("delivered notifications did not answer".into()))
    }

    pub fn clear(ids: &[String]) -> Result<(), ShellError> {
        let center = center()?;
        if ids.is_empty() {
            center.removeAllDeliveredNotifications();
        } else {
            let strings: Vec<Retained<NSString>> =
                ids.iter().map(|s| NSString::from_str(s)).collect();
            center.removeDeliveredNotificationsWithIdentifiers(&NSArray::from_retained_slice(
                &strings,
            ));
        }
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{DeliveredNotification, NotificationPlan, NotificationRequest};
    use crate::error::ShellError;

    pub const SUPPORTED: bool = false;

    fn unsupported() -> ShellError {
        ShellError::Unsupported("native notifications are implemented for macOS; this platform's toast path is not yet verified".into())
    }
    pub fn authorization() -> Result<String, ShellError> {
        Ok("unsupported".into())
    }
    pub fn post(_: &str, _: &NotificationRequest, _: &NotificationPlan) -> Result<(), ShellError> {
        Err(unsupported())
    }
    pub fn delivered() -> Result<Vec<DeliveredNotification>, ShellError> {
        Err(unsupported())
    }
    pub fn clear(_: &[String]) -> Result<(), ShellError> {
        Err(unsupported())
    }
}
