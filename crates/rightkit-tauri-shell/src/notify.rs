//! Native notifications, badge and sound policy.
//!
//! Donor: CodeRight `notifications.rs` (per-target sound cooldown, `navigate_path || title` key,
//! non-negative badge, foreground attention cue, Do-Not-Disturb suppression through
//! [`crate::presence`]). The delivery itself is `rightkit-notifications` (#958), the neutral owner
//! usable without Tauri: `UNUserNotificationCenter` on macOS, WinRT toasts on Windows and
//! freedesktop notifications on Linux. This module keeps only the shell's policy around it: the
//! sound cooldown, Do-Not-Disturb, the QA-quiet rules, and the
//! typed request and result shapes. The delivered-notification read-back QA uses is `Client::delivered` of the
//! neutral crate.
//!
//! Windows: a toast is attributed to an AppUserModelID (AUMID). An installed app owns one through
//! its Start-menu shortcut (Tauri's NSIS/MSI installers set it to the bundle `identifier`); the
//! shell passes `tauri.conf.json`'s `identifier` as the default and [`set_app_id`] overrides it.
//! An unpackaged dev build has no AUMID, so toasts fall back to PowerShell's and carry its
//! name and icon. [`delivered`] is empty on Windows and Linux.
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
    /// Post even while the shell's Do-Not-Disturb (manual or an automatic meeting) is in effect.
    /// Default `false`. The OS Focus mode is not bypassed.
    #[serde(default)]
    pub bypass_do_not_disturb: bool,
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

static APP_ID: Mutex<Option<String>> = Mutex::new(None);
/// Toast attribution used before an app registers its own: PowerShell's AUMID (dev builds only).
pub const FALLBACK_APP_ID: &str =
    r"{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe";

/// Windows toast AppUserModelID. Call before the first notification; overrides the default the
/// shell derives from the Tauri bundle identifier. Ignored off Windows.
pub fn set_app_id(id: &str) {
    *APP_ID.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(id.trim().to_owned()).filter(|s| !s.is_empty());
}

pub(crate) fn set_default_app_id(id: &str) {
    let mut g = APP_ID.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() && !id.trim().is_empty() {
        *g = Some(id.trim().to_owned());
    }
}

/// The AUMID toasts will use (configured, else [`FALLBACK_APP_ID`]).
pub fn app_id() -> String {
    resolve_app_id(APP_ID.lock().unwrap_or_else(|e| e.into_inner()).as_deref())
}

pub fn resolve_app_id(configured: Option<&str>) -> String {
    configured
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(FALLBACK_APP_ID)
        .to_owned()
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

/// Why a request is not posted before reaching the platform (`empty-title`, `dnd`), if it is not.
/// `bypass_do_not_disturb` skips only the Do-Not-Disturb check.
pub fn suppression(req: &NotificationRequest, dnd: bool) -> Option<&'static str> {
    if req.title.trim().is_empty() {
        Some("empty-title")
    } else if dnd && !req.bypass_do_not_disturb {
        Some("dnd")
    } else {
        None
    }
}

/// Post a notification unless the request is empty or Do-Not-Disturb is in effect.
pub fn post(
    req: &NotificationRequest,
    state: &NotificationState,
    dnd: bool,
) -> Result<NotifyOutcome, ShellError> {
    if let Some(why) = suppression(req, dnd) {
        return Ok(NotifyOutcome {
            delivered: false,
            suppressed: Some(why),
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

mod platform {
    use rightkit_notifications::{Client, Error, Notification, Permission, Sound};

    use super::{DeliveredNotification, NotificationPlan, NotificationRequest};
    use crate::error::ShellError;

    pub const SUPPORTED: bool = cfg!(any(target_os = "macos", windows, target_os = "linux"));

    /// The client for the AUMID the shell resolved (configured, else the bundle identifier).
    fn client() -> Client {
        Client::new(&super::app_id())
    }

    fn shell_error(e: Error) -> ShellError {
        match e {
            Error::Unsupported(m) => ShellError::Unsupported(m),
            Error::PermissionDenied(m) => ShellError::PermissionDenied(m),
            Error::Timeout(m) => ShellError::Timeout(m),
            Error::Backend(m) => ShellError::Other(m),
        }
    }

    pub fn authorization() -> Result<String, ShellError> {
        Ok(match client().permission().map_err(shell_error)? {
            Permission::Granted => "authorized",
            Permission::Denied => "denied",
            Permission::NotDetermined => "not-determined",
            Permission::Provisional => "provisional",
            _ => "unsupported",
        }
        .to_owned())
    }

    pub fn post(
        id: &str,
        req: &NotificationRequest,
        plan: &NotificationPlan,
    ) -> Result<(), ShellError> {
        let client = client();
        let quiet = crate::lifecycle::qa_background();
        if quiet && matches!(client.permission(), Ok(Permission::NotDetermined)) {
            return Err(ShellError::PermissionDenied(
                "notification authorization is not determined; background QA never opens a permission dialog".into(),
            ));
        }
        // Background QA posts silently; otherwise the planned sound, if any.
        let sound = match (&plan.sound_name, quiet) {
            (Some(name), false) => Sound::Named(name.clone()),
            _ => Sound::Silent,
        };
        let notification = Notification::new(req.title.trim())
            .body(req.body.trim())
            .key(id)
            .thread(plan.key.as_str())
            .sound(sound);
        client.send(&notification).map(|_| ()).map_err(shell_error)
    }

    /// Withdraw delivered notifications by id; an empty list withdraws all of this app's.
    pub fn clear(ids: &[String]) -> Result<(), ShellError> {
        if ids.is_empty() {
            return client().close_all().map_err(shell_error);
        }
        let client = client();
        for id in ids {
            client.close(id).map_err(shell_error)?;
        }
        Ok(())
    }

    pub fn delivered() -> Result<Vec<DeliveredNotification>, ShellError> {
        client()
            .delivered()
            .map(|list| {
                list.into_iter()
                    .map(|n| DeliveredNotification {
                        id: n.key,
                        title: n.title,
                        body: n.body,
                        thread: n.thread,
                    })
                    .collect()
            })
            .map_err(shell_error)
    }
}
