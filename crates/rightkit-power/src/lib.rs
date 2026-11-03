//! The device's power and session state: sleep, resume, session lock, battery, energy saver,
//! display state, and keeping the computer awake (#957).
//!
//! [`Power::spawn`] starts a platform reader that writes what the system reports into shared
//! [`Conditions`] and queues [`Event`]s, then calls the app's wake function. The app reads both
//! in its own pass and decides what to do: lock a vault on [`Event::Locked`], reconnect on
//! [`Event::Resumed`], save on [`Event::Suspending`]. This crate reports; it never decides.
//!
//! [`PowerAssertion`] and [`PowerKind`] (keep the computer awake) live in
//! `rightkit-child-process` and are re-exported here so callers have one import.
//!
//! Platform support is a table in the README. A platform that cannot report a field leaves its
//! default, the unconstrained value, so an app that reads it never holds work back because a
//! platform says nothing.

#![deny(unsafe_code)]

use std::io;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
#[cfg(target_os = "windows")]
use std::time::Duration;
use std::time::SystemTime;

pub use rightkit_child_process::{PowerAssertion, PowerKind};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
use self::macos::Reader;
#[cfg(target_os = "windows")]
use self::windows::Reader;

/// What the system says about power and the session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Conditions {
    /// The user asked the system to save energy: Low Power Mode on macOS, battery saver on
    /// Windows.
    pub saver: bool,
    /// Running on battery (Windows).
    pub on_battery: bool,
    /// Every display is off (Windows).
    pub display_off: bool,
    /// The session is locked (macOS screen lock, Windows session lock).
    pub locked: bool,
}

/// Something the system is about to do, or has just done.
#[derive(Debug)]
pub enum Event {
    /// The system is about to sleep. Save, then [`Ack::done`]. On Windows the system waits up
    /// to 1.5 s for the answer; on macOS it does not wait.
    Suspending(Ack),
    /// The system woke up. A connection that was open across the sleep can still look
    /// established while nothing arrives through it: reconnect it now.
    Resumed,
    /// The user is signing out or shutting down (Windows). Save, then [`Ack::done`]; the system
    /// is held back for up to 3 s.
    EndingSession(Ack),
    /// The session was locked.
    Locked,
    /// The session was unlocked.
    Unlocked,
}

/// Holds back the system, for a bounded time, until the app has handled an event the system
/// will not wait long for. `Ack::default()` makes one for an [`Event`] sent with
/// [`Power::send`].
#[derive(Clone, Default)]
pub struct Ack(Arc<(Mutex<bool>, Condvar)>);

impl std::fmt::Debug for Ack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ack")
    }
}

impl Ack {
    /// The app has done what the event asked.
    pub fn done(&self) {
        let (done, ring) = &*self.0;
        *done.lock().unwrap_or_else(PoisonError::into_inner) = true;
        ring.notify_all();
    }

    /// Whether the app has answered.
    pub fn is_done(&self) -> bool {
        *self.0 .0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits until [`done`](Self::done) or `timeout`; returns whether the app answered in time.
    #[cfg(target_os = "windows")]
    pub(crate) fn wait(&self, timeout: Duration) -> bool {
        let (done, ring) = &*self.0;
        let guard = done.lock().unwrap_or_else(PoisonError::into_inner);
        let (guard, _) = ring
            .wait_timeout_while(guard, timeout, |done| !*done)
            .unwrap_or_else(PoisonError::into_inner);
        *guard
    }
}

/// How long a Windows reader waits for the app before the system sleeps (the suspend callback
/// gets about two seconds).
#[cfg(target_os = "windows")]
pub(crate) const SUSPEND_ACK: Duration = Duration::from_millis(1500);
/// How long a Windows reader holds back the end of the session.
#[cfg(target_os = "windows")]
pub(crate) const END_SESSION_ACK: Duration = Duration::from_secs(3);

type Hook = Arc<dyn Fn() + Send + Sync>;

/// What the readers write and the app reads.
pub(crate) struct Shared {
    conditions: Mutex<Conditions>,
    events: Mutex<Vec<Event>>,
    wake: Box<dyn Fn() + Send + Sync>,
    on_suspending: Mutex<Option<Hook>>,
    /// When the system last said it was going to sleep, by the wall clock, which keeps counting
    /// through the sleep.
    suspended_at: Mutex<Option<SystemTime>>,
}

impl Shared {
    fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            conditions: Mutex::new(Conditions::default()),
            events: Mutex::new(Vec::new()),
            wake: Box::new(wake),
            on_suspending: Mutex::new(None),
            suspended_at: Mutex::new(None),
        }
    }

    /// Applies `change` and wakes the app if it changed anything.
    #[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
    pub(crate) fn update(&self, change: impl FnOnce(&mut Conditions)) {
        let mut conditions = self
            .conditions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let before = *conditions;
        change(&mut conditions);
        let after = *conditions;
        drop(conditions);
        if after != before {
            log::info!("power and session: {after:?}");
            (self.wake)();
        }
    }

    /// Notes the time and runs the app's hook as the system is about to sleep.
    #[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
    pub(crate) fn begin_suspending(&self) {
        *self
            .suspended_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(SystemTime::now());
        let hook = self
            .on_suspending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        // A panicking hook must not end a reader thread or unwind through an OS callback.
        if let Some(hook) = hook {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook())).is_err() {
                log::warn!("power and session: the suspending hook panicked");
            }
        }
    }

    /// Queues `event` and wakes the app.
    #[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
    pub(crate) fn push(&self, event: Event) {
        log::info!("power and session: {event:?}");
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
        (self.wake)();
    }

    /// Queues the event `make` builds and waits up to `timeout` for the app to answer it.
    #[cfg(target_os = "windows")]
    pub(crate) fn push_and_wait(&self, make: impl FnOnce(Ack) -> Event, timeout: Duration) {
        let ack = Ack::default();
        let event = make(ack.clone());
        if matches!(event, Event::Suspending(_)) {
            self.begin_suspending();
        }
        self.push(event);
        if !ack.wait(timeout) {
            log::warn!("power and session: the app did not answer within {timeout:?}");
        }
    }
}

/// Watches the system's power and session state for the app.
pub struct Power {
    shared: Arc<Shared>,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    reader: Option<Reader>,
}

impl Power {
    /// Starts the platform's reader. `wake` runs on a reader's or the system's thread whenever
    /// the conditions change or an event is queued; it should ask for the app's next pass and
    /// return at once.
    ///
    /// A reader that cannot start is logged and left out: the app still gets a `Power`, with
    /// default conditions and no events.
    pub fn spawn(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let shared = Arc::new(Shared::new(wake));
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        let reader = match Reader::start(Arc::clone(&shared)) {
            Ok(reader) => Some(reader),
            Err(error) => {
                log::warn!("power and session state unavailable: {error}");
                None
            }
        };
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        log::warn!("power and session state is not read on this platform");
        Self {
            shared,
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            reader,
        }
    }

    /// The latest conditions. On macOS, Low Power Mode is read here (one message to
    /// `NSProcessInfo`) rather than followed through a notification; a change found here wakes
    /// the app like any other.
    pub fn conditions(&self) -> Conditions {
        #[cfg(target_os = "macos")]
        if self.reader.is_some() {
            macos::read_low_power(&self.shared);
        }
        *self
            .shared
            .conditions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The events since the last call, oldest first. Call it in every pass: a Windows reader
    /// holds the system back until the app answers an [`Ack`], or for a short while at most.
    pub fn events(&self) -> Vec<Event> {
        std::mem::take(
            &mut *self
                .shared
                .events
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Has the reader run `hook` itself as the system is about to sleep, before the app's pass
    /// reads [`Event::Suspending`]. For work that must not wait, such as pausing playback or
    /// locking a vault. Keep it short; a later call replaces it. Do not capture the `Power`.
    pub fn on_suspending(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self
            .shared
            .on_suspending
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(hook));
    }

    /// When the system last said it was going to sleep, by the wall clock, taken once. On
    /// waking, the time since then is how long the machine slept. The wall clock can be set
    /// back meanwhile, so handle [`SystemTime::duration_since`] failing.
    pub fn take_suspended_at(&self) -> Option<SystemTime> {
        self.shared
            .suspended_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Keeps the computer from idle sleep while the returned assertion is held. Errors as
    /// [`PowerAssertion::acquire`] does (`Unsupported` where the host cannot).
    pub fn keep_awake(&self, reason: &str, kind: PowerKind) -> io::Result<PowerAssertion> {
        PowerAssertion::acquire(reason, kind)
    }
}

/// Keeps the app's work out of App Nap on macOS while it is held, and ends when dropped.
///
/// macOS slows an app whose windows are covered or hidden: its timers fire late and its threads
/// get less time. Hold an `Activity` while the app does work the user is waiting for that has to
/// keep pace (audio playing, a transfer), and drop it when the work ends. It does not keep the
/// Mac awake (use [`Power::keep_awake`]). On other platforms it does nothing.
#[must_use = "the activity ends when it is dropped"]
pub struct Activity {
    #[cfg(target_os = "macos")]
    _token: macos::Activity,
}

impl Activity {
    /// Begins an activity. `reason` is what the system shows for it (`pmset -g assertions`).
    pub fn begin(reason: &str) -> Self {
        #[cfg(target_os = "macos")]
        {
            Self {
                _token: macos::Activity::begin(reason),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = reason;
            Self {}
        }
    }
}

impl std::fmt::Debug for Activity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Activity")
    }
}
