//! The foreground lease (AG-12; the contract is `docs/contracts/foreground-lease.md`, "The foreground
//! lease"): the one way the agent may take the screen. The screen tools work in the background
//! first (accessibility actions, input addressed to one window); when no such route can do a step
//! and the owner allowed it (`agentForeground`, owner-only, off by default), the agent asks for a
//! lease on one app. The lease raises that app's tier ceiling from "targeted" to "foreground" for
//! its own calls, for at most two minutes, and ends the moment the person touches the keyboard or
//! the pointer.
//!
//! * **Who.** Only the engine's own agent running a task the owner started (`Requester::Agent`) on
//!   the local door. A call from the ChatGPT gateway, from a chat's agent or from a plain terminal
//!   never gets one: `request` answers "no lease" and the quiet route's refusal stands.
//! * **Asking.** By the owner's mode (`agentForegroundMode`): `ask` approves every lease (a session grant
//!   on the engine's approval record, family `foreground:<app>`, spent when the lease starts); `when-idle`
//!   asks only when the owner has touched the keyboard or pointer within `agentForegroundIdleSeconds`; `task`
//!   asks nothing, because the caller is the owner's own agent on a task the owner started. A lease lasts
//!   `agentForegroundSeconds` (at most 600).
//! * **Warning.** After the approval and before the takeover, `warn` shows a pill for two seconds
//!   (`screen.lease.current`, `state: warning`, polled by the owner app) that names the app and the owner
//!   can cancel (`screen.lease.end` with `cancel`). While the lease is held the owner app shows a border
//!   (`state: held`).
//! * **Never.** On the host app itself, System Settings or a security prompt (the hard floor), with secure
//!   input on, a password field focused or a protected window (the crate's capture-safety probe
//!   must say `Safe`; an answer it cannot give is a refusal), or when nothing can be said to be in
//!   front, which includes a locked screen. Anything that sends, buys or publishes still asks: the
//!   lease changes how an action is delivered, not whether it is allowed.
//! * **The person's input.** The operating system keeps a counter of the last real input. The lease
//!   records it when it starts and again after every foreground action of its own (the agent's
//!   events are then already counted), and at the next call a counter that moved means the person
//!   did something: the lease ends and the call is answered `user_in_control`. An unreadable counter
//!   counts as the person's input. During a single foreground action the crate's own input watch
//!   (`InputWatch`, on only for that action, `screen.rs` `WatchGuard`) stops it with `UserInControl`.
//!
//! Written, not compiled, not run.

use super::approvals;
use super::settings::{Settings, hard_floor};
use super::{Origin, Refusal, Requester, Result};
use rightkit_automation::{CaptureSafety, FrontApp, Platform, Target, probe_capture_safety};
use serde_json::{Value, json};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The warning before a takeover.
const WARNING: Duration = Duration::from_secs(2);
/// How long to wait for the person's app to come back to the front.
const RESTORE_WAIT: Duration = Duration::from_millis(1500);

// ---- the operating system's counter of the last real input -------------------------------------

#[cfg(windows)]
mod input {
    /// `LASTINPUTINFO`.
    #[repr(C)]
    struct LastInputInfo {
        size: u32,
        time: u32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetLastInputInfo(info: *mut LastInputInfo) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetTickCount() -> u32;
    }

    /// Seconds since the last input event of the session; `None` when the counter cannot be read.
    pub fn idle_secs() -> Option<f64> {
        let last = Mark::now()?.0;
        // SAFETY: a plain query of the tick count; it takes nothing.
        let now = unsafe { GetTickCount() };
        Some(f64::from(now.wrapping_sub(last)) / 1000.0)
    }

    /// The tick of the last input event of the session (the agent's own included).
    #[derive(Clone, Copy)]
    pub struct Mark(u32);

    impl Mark {
        pub fn now() -> Option<Mark> {
            let mut info = LastInputInfo {
                size: std::mem::size_of::<LastInputInfo>() as u32,
                time: 0,
            };
            // SAFETY: `info` is a valid `LASTINPUTINFO` with its size set, as the call requires.
            let ok = unsafe { GetLastInputInfo(&mut info) };
            (ok != 0).then_some(Mark(info.time))
        }

        /// Whether an input event came after this mark; `None` when the counter cannot be read.
        pub fn moved(&self) -> Option<bool> {
            Mark::now().map(|now| now.0 != self.0)
        }
    }
}

#[cfg(target_os = "macos")]
mod input {
    use std::time::Instant;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event: u32) -> f64;
    }

    /// `kCGEventSourceStateCombinedSessionState`.
    const COMBINED_SESSION: i32 = 0;
    /// `kCGAnyInputEventType`.
    const ANY_INPUT: u32 = u32::MAX;
    /// Seconds a counter may read less than expected before it counts as an input event.
    const SLACK: f64 = 0.3;

    /// Seconds since the last input event of the session (the agent's own included), and when that
    /// was read.
    #[derive(Clone, Copy)]
    pub struct Mark {
        since: f64,
        at: Instant,
    }

    fn since_last() -> Option<f64> {
        // SAFETY: a plain query of the window server's counter; it takes two integers.
        let since = unsafe { CGEventSourceSecondsSinceLastEventType(COMBINED_SESSION, ANY_INPUT) };
        (since.is_finite() && since >= 0.0).then_some(since)
    }

    /// Seconds since the last input event of the session; `None` when the counter cannot be read.
    pub fn idle_secs() -> Option<f64> {
        since_last()
    }

    impl Mark {
        pub fn now() -> Option<Mark> {
            since_last().map(|since| Mark {
                since,
                at: Instant::now(),
            })
        }

        /// Whether an input event came after this mark; `None` when the counter cannot be read.
        pub fn moved(&self) -> Option<bool> {
            let expected = self.since + self.at.elapsed().as_secs_f64();
            since_last().map(|now| now + SLACK < expected)
        }
    }
}

/// Other platforms have no input counter the crate can read: every reading is `None`, so the
/// idle and moved checks refuse instead of guessing.
#[cfg(not(any(windows, target_os = "macos")))]
mod input {
    /// Seconds since the last input event; never readable here.
    pub fn idle_secs() -> Option<f64> {
        None
    }

    /// A mark of the last input event; never readable here.
    #[derive(Clone, Copy)]
    pub struct Mark;

    impl Mark {
        pub fn now() -> Option<Mark> {
            None
        }

        /// Whether an input event came after this mark; never readable here.
        pub fn moved(&self) -> Option<bool> {
            None
        }
    }
}

use input::Mark;

// ---- the lease -------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The pill is up; the owner can still cancel.
    Warning,
    /// The border is up; the agent's calls on this app may use the foreground.
    Held,
}

struct Lease {
    pid: i32,
    app_name: String,
    phase: Phase,
    until: Instant,
    /// How long the lease lasts once held (`agentForegroundSeconds`).
    length: Duration,
    warned_until: Instant,
    mark: Mark,
    cancelled: bool,
}

static STATE: Mutex<Option<Lease>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<Lease>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn user_in_control() -> Refusal {
    Refusal::new(
        "user_in_control",
        "you used the keyboard or the pointer, so the agent's foreground lease ended; the agent waits for you and asks again if it still needs the screen",
    )
}

fn refused(why: &str) -> Refusal {
    Refusal::new(
        "lease_refused",
        format!("the foreground lease was not given: {why}"),
    )
}

/// Ends the lease when the person's input came after its last mark, and answers `user_in_control`
/// once. Called at the top of every screen tool.
pub fn check_user() -> Result<()> {
    let mut slot = lock();
    let Some(lease) = slot.as_ref() else {
        return Ok(());
    };
    if Instant::now() >= lease.until {
        *slot = None;
        return Ok(());
    }
    if lease.phase == Phase::Warning {
        return Ok(());
    }
    if lease.mark.moved().unwrap_or(true) {
        *slot = None;
        return Err(user_in_control());
    }
    Ok(())
}

/// Whether a lease on the app `pid` stands now (checked after `check_user`).
pub fn standing(pid: i32) -> bool {
    lock()
        .as_ref()
        .is_some_and(|l| l.pid == pid && l.phase == Phase::Held && Instant::now() < l.until)
}

/// What a lease would be for.
pub struct Wanted<'a> {
    pub pid: i32,
    pub window: u64,
    pub app_id: &'a str,
    pub app_name: &'a str,
}

/// The session family of one app's lease. A long id is hashed.
/// Whether the owner's mode lets this lease start without a question: `task` always (the caller is
/// the owner's own agent), `when-idle` when the owner has not touched the keyboard or pointer for
/// `agentForegroundIdleSeconds`, `ask` never. An unreadable counter is not idle.
fn unasked(settings: &Settings) -> bool {
    match settings.agent_foreground_mode.as_str() {
        "task" => true,
        "when-idle" => input::idle_secs()
            .is_some_and(|idle| idle >= settings.agent_foreground_idle_seconds as f64),
        _ => false,
    }
}

fn family(app_id: &str) -> String {
    let id = app_id.to_lowercase();
    if id.len() <= 100 {
        format!("foreground:{id}")
    } else {
        format!("foreground:{}", approvals::bind_of(&[id.as_bytes()]))
    }
}

/// The checks that never give way: the hard floor, a surface the crate's probe calls sensitive or
/// cannot rule out, and a front that cannot be named (a locked screen).
fn safety(want: &Wanted) -> Result<()> {
    if hard_floor(want.app_id) {
        return Err(refused(
            "the app itself, System Settings and security prompts are never taken over",
        ));
    }
    match probe_capture_safety(&Target::window(want.pid, want.window)).verdict {
        CaptureSafety::Safe => {}
        CaptureSafety::Unsafe(_) => {
            return Err(refused(
                "a password field, secure input or a protected window is showing",
            ));
        }
        _ => {
            return Err(refused(
                "the app cannot rule out a password field or secure input on this screen",
            ));
        }
    }
    let front = rightkit_automation::foreign::foreground_window().ok_or_else(|| {
        refused("the app cannot tell what is in front, which includes a locked screen")
    })?;
    let owner = front.owner.to_lowercase();
    if ["loginwindow", "lockapp", "logonui", "screensaver"]
        .iter()
        .any(|word| owner.contains(word))
    {
        return Err(refused("the screen is locked"));
    }
    Ok(())
}

/// Asks for a lease on one app. `Ok(false)`: no lease is available to this caller or with these
/// settings, and the quiet route's own result stands. `Ok(true)`: the owner approved and the
/// checks passed, the lease exists and is in its warning phase (call `warn` next). An error:
/// `approval_required` (repeat the call once the owner approved) or `lease_refused`.
pub fn request(
    settings: &Settings,
    origin: Origin,
    requester: &Requester,
    want: &Wanted,
) -> Result<bool> {
    if !settings.agent_foreground
        || origin != Origin::Local
        || !matches!(requester, Requester::Agent)
        || !approvals::is_engine_process()
    {
        return Ok(false);
    }
    safety(want)?;
    let family = family(want.app_id);
    if !unasked(settings) && !approvals::session_active_held(origin, &family)? {
        return Err(approvals::request_session_held(
            origin,
            &family,
            &format!(
                "let the agent take over the screen to work in {} when no quieter way can do the step, for {} seconds (your approval mode decides whether the next one asks too). A pill names the app two seconds before, a border shows while it has the screen, any key or pointer movement of yours ends it at once, and your front window is put back. It never does this with a password field showing, secure input on or the screen locked, and anything that sends, buys or publishes still asks",
                want.app_name, settings.agent_foreground_seconds,
            ),
        ));
    }
    let mark = Mark::now().ok_or_else(|| {
        refused("the app cannot read the system's last-input counter, so it could not tell when you return")
    })?;
    // `ask` means every lease: the grant that let this one start is spent.
    if settings.agent_foreground_mode == "ask" {
        let _ = approvals::revoke_sessions_held(&family);
    }
    let length = Duration::from_secs(settings.agent_foreground_seconds);
    let now = Instant::now();
    *lock() = Some(Lease {
        pid: want.pid,
        app_name: want.app_name.to_string(),
        phase: Phase::Warning,
        until: now + WARNING + length,
        length,
        warned_until: now + WARNING,
        mark,
        cancelled: false,
    });
    Ok(true)
}

/// The two seconds between the approval and the takeover. The owner can cancel meanwhile
/// (`end(true)`): the lease is gone and this answers `lease_cancelled`.
pub fn warn() -> Result<()> {
    loop {
        {
            let mut slot = lock();
            let Some(lease) = slot.as_mut() else {
                return Err(Refusal::new(
                    "lease_cancelled",
                    "the foreground lease was cancelled; nothing was taken over",
                ));
            };
            if lease.cancelled {
                *slot = None;
                return Err(Refusal::new(
                    "lease_cancelled",
                    "the owner cancelled the foreground lease; nothing was taken over",
                ));
            }
            if Instant::now() >= lease.warned_until {
                lease.phase = Phase::Held;
                // The lease runs from the takeover, not from the pill.
                lease.until = Instant::now() + lease.length;
                if let Some(mark) = Mark::now() {
                    lease.mark = mark;
                }
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// After the agent's own foreground action: its events are counted now, so the mark moves to here.
pub fn rebase() {
    if let Some(lease) = lock().as_mut()
        && let Some(mark) = Mark::now()
    {
        lease.mark = mark;
    }
}

/// The front app now, to put back after a foreground action.
pub fn front_before<P: Platform>(platform: &P) -> Option<FrontApp> {
    platform.frontmost()
}

/// Brings `before` to the front again and says whether it is there (the app, and its window when
/// both are known). An app that cannot be brought back is reported as not restored.
pub fn restore<P: Platform>(platform: &P, before: &FrontApp) -> bool {
    let back = |platform: &P| {
        platform.frontmost().is_some_and(|now| {
            now.pid == before.pid
                && (before.window_id.is_none()
                    || now.window_id.is_none()
                    || now.window_id == before.window_id)
        })
    };
    if back(platform) {
        return true;
    }
    platform.activate(before.pid);
    let until = Instant::now() + RESTORE_WAIT;
    while Instant::now() < until {
        if back(platform) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// The lease on screen now, for the owner app: `{active: false}` or `{active: true, state (warning or
/// held), app, secondsLeft}`. Owner-only (`screen.lease.current`).
pub fn current() -> Value {
    let slot = lock();
    match slot.as_ref() {
        Some(lease) if Instant::now() < lease.until => {
            let left = match lease.phase {
                Phase::Warning => lease.warned_until.saturating_duration_since(Instant::now()),
                Phase::Held => lease.until.saturating_duration_since(Instant::now()),
            };
            json!({
                "active": true,
                "state": if lease.phase == Phase::Warning { "warning" } else { "held" },
                "app": lease.app_name,
                "secondsLeft": left.as_secs_f64().ceil() as u64,
            })
        }
        _ => json!({"active": false}),
    }
}

/// The owner ends the lease (`screen.lease.end`, the owner app's Esc or its pill's Cancel). During the
/// warning it cancels the takeover; after it, it ends the lease.
pub fn end() -> Value {
    let mut slot = lock();
    let warning = match slot.as_mut() {
        Some(lease) => {
            let warning = lease.phase == Phase::Warning;
            if warning {
                lease.cancelled = true;
            }
            warning
        }
        None => return json!({"ended": false}),
    };
    if warning {
        json!({"ended": true, "state": "cancelled"})
    } else {
        *slot = None;
        json!({"ended": true, "state": "ended"})
    }
}
