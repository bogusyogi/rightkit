//! Presentation-aware presence and Do-Not-Disturb.
//!
//! Donor: CodeRight `presence_dnd.rs` (itself a port of Clyde's `environment.rs`): the overlap
//! thresholds, the meeting/share keyword tables, the preference gates and the manual-DND layering are
//! kept verbatim. The platform probe is new: on macOS it reads the real on-screen window list from
//! CoreGraphics (`CGWindowListCopyWindowInfo`, owner names need no permission), on other platforms it
//! reports an empty observation and `controls_supported() == false` instead of inventing state.
//!
//! `effective_dnd = manual || auto` and gates [`crate::notify`]. Auto-DND turns on while a meeting or
//! screen-share window is visible and the app opted in with [`AutoPrefs::auto_dnd_meetings`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const CONTROLS_SUPPORTED: bool = cfg!(target_os = "macos");
pub const POLL_SECONDS: u64 = 2;
const FULLSCREEN_OVERLAP: f64 = 0.97;
const MEETING_OVERLAP: f64 = 0.18;

pub fn controls_supported() -> bool {
    CONTROLS_SUPPORTED
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub fullscreen_on_main_monitor: bool,
    pub meeting_or_share_active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A visible window from the platform collector. `bounds: None` is skipped (malformed record).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowObservation {
    pub owner: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub layer: i32,
    pub bounds: Option<Rect>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoPrefs {
    #[serde(default)]
    pub auto_hide_fullscreen: bool,
    #[serde(default)]
    pub auto_dnd_meetings: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AutoState {
    pub hidden_for_fullscreen: bool,
    pub dnd_for_meeting: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceState {
    pub manual_dnd: bool,
    pub auto_hidden: bool,
    pub auto_dnd: bool,
    pub effective_dnd: bool,
    pub controls_supported: bool,
}

#[derive(Default)]
pub struct PresenceDnd {
    manual: AtomicBool,
    auto_hidden: AtomicBool,
    auto_dnd: AtomicBool,
    prefs: Mutex<AutoPrefs>,
}

impl PresenceDnd {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set_manual_dnd(&self, on: bool) {
        self.manual.store(on, Ordering::Relaxed);
    }
    /// Returns `(hidden_changed, dnd_changed)`.
    pub fn set_auto(&self, next: AutoState) -> (bool, bool) {
        let h = self.auto_hidden.swap(next.hidden_for_fullscreen, Ordering::Relaxed);
        let d = self.auto_dnd.swap(next.dnd_for_meeting, Ordering::Relaxed);
        (h != next.hidden_for_fullscreen, d != next.dnd_for_meeting)
    }
    pub fn effective_dnd(&self) -> bool {
        self.manual.load(Ordering::Relaxed) || self.auto_dnd.load(Ordering::Relaxed)
    }
    pub fn prefs(&self) -> AutoPrefs {
        *self.prefs.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn set_prefs(&self, p: AutoPrefs) {
        *self.prefs.lock().unwrap_or_else(|e| e.into_inner()) = p;
    }
    pub fn snapshot(&self) -> PresenceState {
        PresenceState {
            manual_dnd: self.manual.load(Ordering::Relaxed),
            auto_hidden: self.auto_hidden.load(Ordering::Relaxed),
            auto_dnd: self.auto_dnd.load(Ordering::Relaxed),
            effective_dnd: self.effective_dnd(),
            controls_supported: CONTROLS_SUPPORTED,
        }
    }
}

/// Apply the preference gates. Disabling a control clears its stale automatic state.
pub fn reconcile(prefs: AutoPrefs, env: Environment) -> AutoState {
    AutoState {
        hidden_for_fullscreen: prefs.auto_hide_fullscreen && env.fullscreen_on_main_monitor,
        dnd_for_meeting: prefs.auto_dnd_meetings && env.meeting_or_share_active,
    }
}

/// Fullscreen / meeting detection over normalised observations. Windows on non-zero layers,
/// malformed bounds, the app's own windows, WindowServer and Dock are ignored.
pub fn detect(monitor: Rect, windows: &[WindowObservation], own_name: &str) -> Environment {
    let monitor_area = area(&monitor);
    let mut env = Environment::default();
    for w in windows {
        if w.layer != 0 || ignore(&w.owner, &w.title, own_name) {
            continue;
        }
        let Some(b) = w.bounds else { continue };
        let (overlap_ratio, window_ratio) = if monitor_area > 0.0 { (overlap(&b, &monitor) / monitor_area, area(&b) / monitor_area) } else { (0.0, 0.0) };
        if overlap_ratio >= FULLSCREEN_OVERLAP {
            env.fullscreen_on_main_monitor = true;
        }
        if is_meeting(&w.owner, &w.title) && (overlap_ratio >= MEETING_OVERLAP || window_ratio >= MEETING_OVERLAP) {
            env.meeting_or_share_active = true;
        }
        if env.fullscreen_on_main_monitor && env.meeting_or_share_active {
            break;
        }
    }
    env
}

fn ignore(owner: &str, title: &str, own_name: &str) -> bool {
    let (o, t, own) = (owner.to_ascii_lowercase(), title.to_ascii_lowercase(), own_name.to_ascii_lowercase());
    o.is_empty() || o == "window server" || o == "dock" || (!own.is_empty() && (o == own || t.contains(&own)))
}

fn is_meeting(owner: &str, title: &str) -> bool {
    let (o, t) = (owner.to_ascii_lowercase(), title.to_ascii_lowercase());
    const OWNERS: [&str; 10] = ["zoom", "zoom.us", "microsoft teams", "teams", "webex", "slack", "feishu", "lark", "tencent meeting", "腾讯会议"];
    const TITLES: [&str; 14] = [
        "zoom meeting", "google meet", " meet ", "meeting", "huddle", "webex", "teams", "sharing", "presenting", "screen share", "share screen", "共享", "演示", "正在共享",
    ];
    OWNERS.iter().any(|k| o.contains(k)) || TITLES.iter().any(|k| t.contains(k)) || t.contains("正在演示")
}

fn area(r: &Rect) -> f64 {
    r.width.max(0.0) * r.height.max(0.0)
}

fn overlap(a: &Rect, b: &Rect) -> f64 {
    let (l, t) = (a.x.max(b.x), a.y.max(b.y));
    let (r, bt) = ((a.x + a.width).min(b.x + b.width), (a.y + a.height).min(b.y + b.height));
    (r - l).max(0.0) * (bt - t).max(0.0)
}

/// The real probe: main display bounds and the visible window list. `Err` when the OS refuses.
pub fn observe_os() -> Result<(Rect, Vec<WindowObservation>), String> {
    platform::observe()
}

/// Detect against the live OS for an app named `own_name`.
pub fn detect_live(own_name: &str) -> Result<(Environment, usize), String> {
    let (monitor, windows) = observe_os()?;
    Ok((detect(monitor, &windows, own_name), windows.len()))
}

/// Start the polling loop. It samples only while a preference needs it, never calls into the app
/// thread, and ends when the process does. `on_change` receives the new state when an automatic
/// flag flips.
pub fn start_loop(state: Arc<PresenceDnd>, own_name: String, on_change: impl Fn(PresenceState) + Send + 'static) {
    if !CONTROLS_SUPPORTED {
        return;
    }
    let _ = std::thread::Builder::new().name("rk-shell-presence".into()).spawn(move || loop {
        std::thread::sleep(Duration::from_secs(POLL_SECONDS));
        let prefs = state.prefs();
        let next = if prefs.auto_hide_fullscreen || prefs.auto_dnd_meetings {
            match detect_live(&own_name) {
                Ok((env, _)) => reconcile(prefs, env),
                Err(_) => continue,
            }
        } else {
            AutoState::default()
        };
        let (hidden, dnd) = state.set_auto(next);
        if hidden || dnd {
            on_change(state.snapshot());
        }
    });
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

    use super::{Rect, WindowObservation};

    #[repr(C)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    struct CGSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *mut c_void;
        fn CGMainDisplayID() -> u32;
        fn CGDisplayBounds(display: u32) -> CGRect;
    }
    const ON_SCREEN_ONLY: u32 = 1;
    const EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;

    type Dict = NSDictionary<NSString, AnyObject>;

    fn string(d: &Dict, key: &str) -> Option<String> {
        d.objectForKey(&NSString::from_str(key))?.downcast_ref::<NSString>().map(|s| s.to_string())
    }
    fn number(d: &Dict, key: &str) -> Option<f64> {
        d.objectForKey(&NSString::from_str(key))?.downcast_ref::<NSNumber>().map(|n| n.doubleValue())
    }

    pub fn observe() -> Result<(Rect, Vec<WindowObservation>), String> {
        // SAFETY: plain CoreGraphics queries; the returned CFArray is owned (+1) and toll-free
        // bridged to NSArray, so `Retained::from_raw` takes over that single reference.
        let (bounds, raw) = unsafe {
            let b = CGDisplayBounds(CGMainDisplayID());
            (Rect { x: b.origin.x, y: b.origin.y, width: b.size.width, height: b.size.height }, CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP_ELEMENTS, 0))
        };
        // SAFETY: see above.
        let list: Retained<NSArray<Dict>> = unsafe { Retained::from_raw(raw.cast()) }.ok_or("CGWindowListCopyWindowInfo returned no list")?;
        let mut out = Vec::new();
        for i in 0..list.count() {
            let d = list.objectAtIndex(i);
            let frame = d.objectForKey(&NSString::from_str("kCGWindowBounds")).and_then(|o| o.downcast::<NSDictionary>().ok());
            let bounds = frame.and_then(|f| {
                // CoreGraphics returns an untyped NSDictionary; reinterpret its known NSString /
                // AnyObject parameters before using typed objectForKey accessors.
                let f: &Dict = unsafe { f.cast_unchecked() };
                Some(Rect { x: number(f, "X")?, y: number(f, "Y")?, width: number(f, "Width")?, height: number(f, "Height")? })
            });
            out.push(WindowObservation {
                owner: string(&d, "kCGWindowOwnerName").unwrap_or_default(),
                title: string(&d, "kCGWindowName").unwrap_or_default(),
                layer: number(&d, "kCGWindowLayer").unwrap_or(0.0) as i32,
                bounds,
            });
        }
        Ok((bounds, out))
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{Rect, WindowObservation};
    pub fn observe() -> Result<(Rect, Vec<WindowObservation>), String> {
        Err("window observation is not implemented on this platform".into())
    }
}
