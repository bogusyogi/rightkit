//! Input recency and blocking states for "Alerts follow you" (RK-6).
//!
//! What it reads: one number, how long ago the person last used the keyboard or mouse (a duration,
//! never a key code, never content), and a few yes/no/unknown facts the OS exposes without any
//! permission prompt. It installs no hook, no event tap and no listener; every call is a one-shot query.
//!
//! "Typing now" is not keystroke capture: it is only "some input arrived within the last
//! [`TYPING_WINDOW_SECS`] seconds". The OS gives no way to tell a key from a mouse move here, and this
//! module does not try.
//!
//! A fact the OS cannot give without a permission prompt, or that this platform does not expose, is
//! [`Signal::Unknown`], never a guess. The routing decision treats Unknown as not blocking (see
//! [`report_from`]); the report keeps the Unknown so the host can tell.
//!
//! Excluded because they need a permission prompt or elevated access (not called anywhere):
//! - macOS Do Not Disturb / Focus: `INFocusStatusCenter` needs a Focus Status authorisation prompt,
//!   and reading `~/Library/DoNotDisturb/db/Assertions.json` needs Full Disk Access. Reported Unknown.
//! - macOS window titles for meeting detection: `kCGWindowName` of other apps' windows is empty without
//!   Screen Recording permission. Owner names and bounds (used here, through [`crate::presence`]) need none.
//! - Any event tap (`CGEventTapCreate`), `NSEvent` global monitor, `SetWindowsHookEx`, raw input or
//!   `IOHIDManager` listening: they capture input, and on macOS prompt for Input Monitoring.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Input within this many seconds counts as "typing now". Recency only.
pub const TYPING_WINDOW_SECS: u64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Signal {
    Yes,
    No,
    Unknown,
}

impl Signal {
    pub fn from_bool(b: bool) -> Self {
        if b {
            Signal::Yes
        } else {
            Signal::No
        }
    }
}

/// Wire-identical to `rightkit_agent_engine_attention::inbox::PresenceState` (same variants, same
/// camelCase names), so a host can serialise this and deserialise it there, or match it one to one.
/// That crate has no variant for a locked screen; a locked screen is reported here as `DoNotDisturb`
/// (not in use, alerts go elsewhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RoutePresence {
    Available,
    InMeeting,
    Fullscreen,
    DoNotDisturb,
}

/// Raw readings from the platform, before they are combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawSignals {
    /// Time since the last real input; `None` when the OS would not say.
    pub idle: Option<Duration>,
    pub screen_locked: Signal,
    pub fullscreen: Signal,
    pub do_not_disturb: Signal,
    pub in_meeting: Signal,
}

impl RawSignals {
    pub const UNKNOWN: RawSignals = RawSignals {
        idle: None,
        screen_locked: Signal::Unknown,
        fullscreen: Signal::Unknown,
        do_not_disturb: Signal::Unknown,
        in_meeting: Signal::Unknown,
    };
}

/// What `route_item` consumes (`last_input_at` and `presence`) plus the facts behind it. The host adds
/// its device id and kind to make a `DevicePresence`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityReport {
    /// Unix seconds of the last real input; `None` when unknown. Same meaning as
    /// `DevicePresence::last_input_at`.
    pub last_input_at: Option<i64>,
    /// Seconds since the last input at the time of the sample.
    pub idle_secs: Option<u64>,
    /// Input within [`TYPING_WINDOW_SECS`]; `Unknown` when input time is unknown.
    pub typing_now: Signal,
    pub screen_locked: Signal,
    pub fullscreen: Signal,
    pub do_not_disturb: Signal,
    pub in_meeting: Signal,
    /// The single state `route_item` reads. Precedence: locked or Do Not Disturb, then meeting,
    /// then fullscreen; otherwise `Available`. `Unknown` signals do not block.
    pub presence: RoutePresence,
}

/// Combine raw readings. Pure.
pub fn report_from(now_unix: i64, raw: &RawSignals, typing_window: Duration) -> ActivityReport {
    let idle_secs = raw.idle.map(|d| d.as_secs());
    let last_input_at = raw
        .idle
        .map(|d| now_unix.saturating_sub(i64::try_from(d.as_secs()).unwrap_or(i64::MAX)));
    let typing_now = match raw.idle {
        Some(d) => Signal::from_bool(d <= typing_window),
        None => Signal::Unknown,
    };
    let presence = if raw.screen_locked == Signal::Yes || raw.do_not_disturb == Signal::Yes {
        RoutePresence::DoNotDisturb
    } else if raw.in_meeting == Signal::Yes {
        RoutePresence::InMeeting
    } else if raw.fullscreen == Signal::Yes {
        RoutePresence::Fullscreen
    } else {
        RoutePresence::Available
    };
    ActivityReport {
        last_input_at,
        idle_secs,
        typing_now,
        screen_locked: raw.screen_locked,
        fullscreen: raw.fullscreen,
        do_not_disturb: raw.do_not_disturb,
        in_meeting: raw.in_meeting,
        presence,
    }
}

/// Read the OS now. `own_name` is the app's own name, so its windows are not taken for a meeting or a
/// fullscreen app (macOS only; see [`crate::presence::detect`]).
pub fn sample(own_name: &str) -> ActivityReport {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    report_from(
        now,
        &platform::read(own_name),
        Duration::from_secs(TYPING_WINDOW_SECS),
    )
}

/// Time since the last real input, or `None` when the OS would not say or the platform has no source.
pub fn idle_duration() -> Option<Duration> {
    platform::idle()
}

#[cfg(windows)]
mod platform {
    use std::time::Duration;

    use super::{RawSignals, Signal};

    #[repr(C)]
    struct LastInputInfo {
        cb_size: u32,
        dw_time: u32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetLastInputInfo(plii: *mut LastInputInfo) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetTickCount() -> u32;
    }
    #[link(name = "shell32")]
    extern "system" {
        /// HRESULT; `*pquns` is a QUERY_USER_NOTIFICATION_STATE.
        fn SHQueryUserNotificationState(pquns: *mut i32) -> i32;
    }

    /// Both `dwTime` and `GetTickCount()` are the same 32-bit millisecond tick that wraps every 49.7
    /// days, so the wrapping difference is the idle time as long as it is under 49.7 days.
    pub fn idle() -> Option<Duration> {
        let mut info = LastInputInfo {
            cb_size: std::mem::size_of::<LastInputInfo>() as u32,
            dw_time: 0,
        };
        // SAFETY: `info` is a valid LASTINPUTINFO with `cbSize` set; the call only writes `dwTime`.
        let ok = unsafe { GetLastInputInfo(&mut info) };
        if ok == 0 {
            return None;
        }
        // SAFETY: no arguments, no state.
        let now = unsafe { GetTickCount() };
        Some(Duration::from_millis(u64::from(
            now.wrapping_sub(info.dw_time),
        )))
    }

    // QUERY_USER_NOTIFICATION_STATE values.
    const QUNS_NOT_PRESENT: i32 = 1; // locked, screen saver, or another fast-user-switch session active
    const QUNS_BUSY: i32 = 2; // fullscreen app, or Presentation Settings applied
    const QUNS_RUNNING_D3D_FULL_SCREEN: i32 = 3;
    const QUNS_PRESENTATION_MODE: i32 = 4;
    const QUNS_ACCEPTS_NOTIFICATIONS: i32 = 5;
    const QUNS_QUIET_TIME: i32 = 6; // Focus assist / Do Not Disturb
    const QUNS_APP: i32 = 7; // fullscreen Store app

    pub fn read(_own_name: &str) -> RawSignals {
        let mut raw = RawSignals {
            idle: idle(),
            ..RawSignals::UNKNOWN
        };
        let mut state: i32 = 0;
        // SAFETY: `state` is a valid out pointer for the call's duration.
        let hr = unsafe { SHQueryUserNotificationState(&mut state) };
        if hr < 0 {
            return raw;
        }
        match state {
            QUNS_NOT_PRESENT => raw.screen_locked = Signal::Yes,
            QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE | QUNS_APP => {
                raw.screen_locked = Signal::No;
                raw.fullscreen = Signal::Yes;
                raw.do_not_disturb = Signal::No;
            }
            QUNS_QUIET_TIME => {
                raw.screen_locked = Signal::No;
                raw.fullscreen = Signal::No;
                raw.do_not_disturb = Signal::Yes;
            }
            QUNS_ACCEPTS_NOTIFICATIONS => {
                raw.screen_locked = Signal::No;
                raw.fullscreen = Signal::No;
                raw.do_not_disturb = Signal::No;
            }
            _ => {}
        }
        // Meetings: this crate has no window observation on Windows, so `in_meeting` stays Unknown.
        raw
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;
    use std::time::Duration;

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSDictionary, NSNumber, NSString};

    use super::{RawSignals, Signal};

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        /// `state`: CGEventSourceStateID (int32). `event_type`: CGEventType (uint32).
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
        /// Copy-rule dictionary (+1), or null when there is no console session.
        fn CGSessionCopyCurrentDictionary() -> *mut c_void;
    }
    const HID_SYSTEM_STATE: i32 = 1; // kCGEventSourceStateHIDSystemState
    const ANY_INPUT_EVENT: u32 = u32::MAX; // kCGAnyInputEventType (~0)

    pub fn idle() -> Option<Duration> {
        // SAFETY: a timestamp query; no pointers, and no events are read or posted.
        let secs =
            unsafe { CGEventSourceSecondsSinceLastEventType(HID_SYSTEM_STATE, ANY_INPUT_EVENT) };
        if secs.is_finite() && secs >= 0.0 {
            Some(Duration::from_secs_f64(secs))
        } else {
            None
        }
    }

    /// `CGSSessionScreenIsLocked` is present (true) in the session dictionary only while locked.
    fn screen_locked() -> Signal {
        // SAFETY: Copy-rule return, owned (+1) and toll-free bridged to NSDictionary, so
        // `from_raw` takes over that single reference. Null means no session.
        let dict: Option<Retained<NSDictionary<NSString, AnyObject>>> =
            unsafe { Retained::from_raw(CGSessionCopyCurrentDictionary().cast()) };
        let Some(dict) = dict else {
            return Signal::Unknown;
        };
        let locked = dict
            .objectForKey(&NSString::from_str("CGSSessionScreenIsLocked"))
            .and_then(|o| o.downcast_ref::<NSNumber>().map(|n| n.boolValue()))
            .unwrap_or(false);
        Signal::from_bool(locked)
    }

    pub fn read(own_name: &str) -> RawSignals {
        // Do Not Disturb / Focus needs a permission prompt or Full Disk Access: excluded, stays Unknown.
        let mut raw = RawSignals {
            idle: idle(),
            screen_locked: screen_locked(),
            ..RawSignals::UNKNOWN
        };
        if let Ok((env, _)) = crate::presence::detect_live(own_name) {
            raw.fullscreen = Signal::from_bool(env.fullscreen_on_main_monitor);
            raw.in_meeting = Signal::from_bool(env.meeting_or_share_active);
        }
        raw
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use std::time::Duration;

    use super::RawSignals;

    pub fn idle() -> Option<Duration> {
        None
    }
    pub fn read(_own_name: &str) -> RawSignals {
        RawSignals::UNKNOWN
    }
}
