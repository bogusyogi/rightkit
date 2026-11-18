//! A set of global chords with tap, hold and debounce semantics, registered through this crate's
//! `GlobalHotkeys` (Carbon on macOS, `RegisterHotKey` plus a release poll on Windows), which
//! delivers both `Pressed` and `Released`.
//!
//! Moved out of an app's hotkey module. Product-neutral: the host supplies the chords, the guard
//! and threshold timings, what each press triggers and a telemetry sink.
//!
//! Semantics, as before:
//! - [`Action::TapHold`]: a press runs `on_tap` at once (after its guard), so an idle toggle starts
//!   on key-down instead of waiting for key-up. A release after at least `hold_threshold` runs
//!   `on_hold_release` (push-to-talk style); a shorter release does nothing, the tap already fired.
//! - [`Action::Debounced`]: a press runs once per `guard_ms`.
//! - [`Action::Immediate`]: a press runs every time.
//! - Each binding has its own guard slot (a shared one dropped a different chord pressed soon after
//!   another's release).
//! - Windows delivers on the hotkey loop thread, so work leaves it (`std::thread::spawn`); macOS
//!   delivers on the main thread, where `Debounced` and `Immediate` work has always run inline.
//!   `TapHold` callbacks run on a spawned thread on both.
//! - Registration order is significant: a failure of a required binding rolls back everything
//!   registered before it (dropping the holder unregisters). An `optional` binding that fails is
//!   reported through the sink and skipped.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::chord::{parse_chord, Chord};
use crate::HotkeyState;

/// Host event sink: `(event, message)`.
pub type EventSink = fn(&str, &str);

/// A callback run when a chord fires. Runs off the registration thread (see the module notes).
pub type Callback = Arc<dyn Fn() + Send + Sync + 'static>;

/// Runs a job on the process's main thread (macOS Carbon registrations belong to the main run
/// loop). Returns the scheduling error text on failure.
pub type RunOnMain =
    Arc<dyn Fn(Box<dyn FnOnce() + Send + 'static>) -> Result<(), String> + Send + Sync + 'static>;

/// One telemetry line `(event, message)`.
pub type LogLine = (&'static str, &'static str);

#[derive(Clone)]
pub enum Action {
    TapHold {
        on_tap: Callback,
        on_hold_release: Callback,
        guard_ms: u64,
        hold_threshold: Duration,
        /// Logged when `on_tap` fires.
        press_log: Option<LogLine>,
        /// Logged when `on_hold_release` fires.
        release_log: Option<LogLine>,
    },
    Debounced {
        run: Callback,
        guard_ms: u64,
    },
    Immediate {
        run: Callback,
        /// Logged on every press.
        log: Option<LogLine>,
    },
}

#[derive(Clone)]
pub struct Binding {
    /// Name used in failure messages (`register <label> failed: ...`).
    pub label: &'static str,
    /// Chord text parsed by [`parse_chord`].
    pub chord: &'static str,
    /// `Some(event)`: a failed registration is logged to the sink under `event` as
    /// `<label> unavailable: <reason>` and skipped instead of failing the set.
    pub optional_warning_event: Option<&'static str>,
    pub action: Action,
}

pub struct ChordHost {
    pub event_sink: EventSink,
    pub run_on_main: RunOnMain,
}

/// `reason` of a `register <chord> failed: <reason>` message from the owner.
fn failure_reason(message: &str) -> &str {
    message
        .split_once(" failed: ")
        .map(|(_, reason)| reason)
        .unwrap_or(message)
}

#[cfg(target_os = "windows")]
mod os {
    use super::*;
    use crate::windows::GlobalHotkeys;

    pub struct Holder(GlobalHotkeys);

    pub fn create(dispatch: Arc<dyn Fn(u32, HotkeyState) + Send + Sync>) -> Result<Holder, String> {
        GlobalHotkeys::with_state(move |id, state| dispatch(id as u32, state)).map(Holder)
    }

    pub fn register(holder: &mut Holder, chord: &Chord) -> Result<u32, String> {
        holder
            .0
            .register(chord)
            .map(|id| id as u32)
            .map_err(|error| error.message())
    }
}

#[cfg(target_os = "macos")]
mod os {
    use super::*;
    use crate::macos::GlobalHotkeys;

    /// Carbon registrations belong to the main run loop: created, used and dropped only there.
    pub struct Holder(GlobalHotkeys);
    // SAFETY: the holder is only ever touched on the main thread (see `install` and `unregister`).
    unsafe impl Send for Holder {}

    pub fn create(dispatch: Arc<dyn Fn(u32, HotkeyState) + Send + Sync>) -> Result<Holder, String> {
        GlobalHotkeys::new(move |id, state| dispatch(id.0, state))
            .map(Holder)
            .map_err(|status| format!("install hotkey dispatcher failed: OSStatus {status}"))
    }

    pub fn register(holder: &mut Holder, chord: &Chord) -> Result<u32, String> {
        holder
            .0
            .register(chord)
            .map(|id| id.0)
            .map_err(|error| error.message())
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod os {
    use super::*;

    /// `create` never succeeds here, so no registration exists to release. The explicit `Drop`
    /// states the contract the callers rely on (dropping the holder unregisters everything it
    /// registered, as on macOS and Windows) so `drop(holder)` means the same on every target.
    pub struct Holder;

    impl Drop for Holder {
        fn drop(&mut self) {}
    }

    pub fn create(
        _dispatch: Arc<dyn Fn(u32, HotkeyState) + Send + Sync>,
    ) -> Result<Holder, String> {
        Err("global hotkeys are not supported on this platform".to_string())
    }

    pub fn register(_holder: &mut Holder, _chord: &Chord) -> Result<u32, String> {
        Err("global hotkeys are not supported on this platform".to_string())
    }
}

static HOLDER: Mutex<Option<os::Holder>> = Mutex::new(None);

/// Per-binding runtime state: its own guard slot and, for `TapHold`, the press time.
struct Slot {
    action: Action,
    /// Timestamp of the most recent Pressed event of a `TapHold`. None when not currently held.
    pressed_at: Mutex<Option<Instant>>,
    last_fire: Mutex<Option<Instant>>,
}

/// Per-action guard: returns true (and stamps `now`) if more than `guard_ms` has elapsed since the
/// last fire on this slot.
fn debounce_ok(slot: &Mutex<Option<Instant>>, guard_ms: u64) -> bool {
    let mut last = slot.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    let ok = last
        .map(|t| now.duration_since(t) > Duration::from_millis(guard_ms))
        .unwrap_or(true);
    if ok {
        *last = Some(now);
    }
    ok
}

/// Windows delivers on the hotkey loop thread, so work leaves it; macOS delivers on the main
/// thread, where these actions have always run inline.
fn run_action(work: impl FnOnce() + Send + 'static) {
    if cfg!(target_os = "windows") {
        let _ = std::thread::Builder::new()
            .name("rk-chord-action".into())
            .spawn(work);
    } else {
        work();
    }
}

fn log(sink: EventSink, line: Option<LogLine>) {
    if let Some((event, message)) = line {
        sink(event, message);
    }
}

#[cfg(target_os = "macos")]
fn on_main_thread() -> bool {
    extern "C" {
        fn pthread_main_np() -> i32;
    }
    unsafe { pthread_main_np() != 0 }
}

fn install(bindings: Vec<Binding>, event_sink: EventSink) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if !on_main_thread() {
        return Err("hotkey registration requires main thread".into());
    }
    if HOLDER.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
        return Err("hotkeys already registered".into());
    }
    let slots: Vec<Arc<Slot>> = bindings
        .iter()
        .map(|binding| {
            Arc::new(Slot {
                action: binding.action.clone(),
                pressed_at: Mutex::new(None),
                last_fire: Mutex::new(None),
            })
        })
        .collect();
    let table: Arc<Mutex<Vec<(u32, Arc<Slot>)>>> = Arc::new(Mutex::new(Vec::new()));

    let dispatch: Arc<dyn Fn(u32, HotkeyState) + Send + Sync> = {
        let table = table.clone();
        Arc::new(move |id, state| {
            let slot = table
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .find(|(known, _)| *known == id)
                .map(|(_, slot)| slot.clone());
            let Some(slot) = slot else {
                return;
            };
            match (&slot.action, state) {
                (
                    Action::TapHold {
                        on_tap,
                        guard_ms,
                        press_log,
                        ..
                    },
                    HotkeyState::Pressed,
                ) => {
                    *slot.pressed_at.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(Instant::now());
                    if !debounce_ok(&slot.last_fire, *guard_ms) {
                        return;
                    }
                    log(event_sink, *press_log);
                    let on_tap = on_tap.clone();
                    let _ = std::thread::Builder::new()
                        .name("rk-chord-tap".into())
                        .spawn(move || on_tap());
                }
                (
                    Action::TapHold {
                        on_hold_release,
                        hold_threshold,
                        release_log,
                        ..
                    },
                    HotkeyState::Released,
                ) => {
                    let pressed_at = slot
                        .pressed_at
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take();
                    let delta = pressed_at.map(|t| t.elapsed()).unwrap_or(Duration::ZERO);
                    if delta >= *hold_threshold {
                        log(event_sink, *release_log);
                        let on_release_hold = on_hold_release.clone();
                        let _ = std::thread::Builder::new()
                            .name("rk-chord-release".into())
                            .spawn(move || on_release_hold());
                    }
                }
                (Action::Debounced { run, guard_ms }, HotkeyState::Pressed) => {
                    if debounce_ok(&slot.last_fire, *guard_ms) {
                        let run = run.clone();
                        run_action(move || run());
                    }
                }
                (Action::Immediate { run, log: line }, HotkeyState::Pressed) => {
                    log(event_sink, *line);
                    let run = run.clone();
                    run_action(move || run());
                }
                _ => {}
            }
        })
    };

    let mut holder = os::create(dispatch)?;
    for (binding, slot) in bindings.iter().zip(slots) {
        let label = binding.label;
        let chord =
            parse_chord(binding.chord).map_err(|e| format!("register {label} failed: {e}"))?;
        match os::register(&mut holder, &chord) {
            Ok(id) => table
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((id, slot)),
            Err(message) if binding.optional_warning_event.is_some() => {
                // Optional chord: reported, not fatal.
                event_sink(
                    binding.optional_warning_event.unwrap_or_default(),
                    &format!("{label} unavailable: {}", failure_reason(&message)),
                );
            }
            Err(message) => {
                // Dropping the holder unregisters everything registered so far.
                drop(holder);
                return Err(format!(
                    "register {label} failed: {}",
                    failure_reason(&message)
                ));
            }
        }
    }
    *HOLDER.lock().unwrap_or_else(|e| e.into_inner()) = Some(holder);
    Ok(())
}

/// Register the chord set. Pressed and Released both arrive; see the module notes.
pub fn register(bindings: Vec<Binding>, host: ChordHost) -> Result<(), String> {
    let ChordHost {
        event_sink,
        run_on_main,
    } = host;

    #[cfg(target_os = "macos")]
    {
        if on_main_thread() {
            return install(bindings, event_sink);
        }
        // A timed-out queued job must not install registrations later.
        use std::sync::atomic::{AtomicU8, Ordering};
        let phase = Arc::new(AtomicU8::new(0)); // queued, running, cancelled
        let worker_phase = Arc::clone(&phase);
        let (tx, rx) = std::sync::mpsc::channel();
        run_on_main(Box::new(move || {
            if worker_phase
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return;
            }
            let _ = tx.send(install(bindings, event_sink));
        }))
        .map_err(|e| format!("register hotkeys failed: main thread unavailable: {e}"))?;
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(result) => result,
            Err(_)
                if phase
                    .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok() =>
            {
                Err("register hotkeys failed: main thread did not respond".into())
            }
            // An installation already executing owns resources; await its actual result.
            Err(_) => rx
                .recv()
                .map_err(|_| "register hotkeys failed: dispatcher ended".to_string())?,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = run_on_main;
        install(bindings, event_sink)
    }
}

/// Drop every registration made by [`register`]. On macOS the registrations are released on the
/// main thread, where they were made; the error is the scheduling error text, for the host to
/// word.
pub fn unregister(run_on_main: &RunOnMain) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if on_main_thread() {
            HOLDER.lock().unwrap_or_else(|e| e.into_inner()).take();
            return Ok(());
        }
        // Keep Carbon resources inside their holder until main-thread execution.
        // Scheduling failure therefore leaves existing registrations owned & retryable.
        run_on_main(Box::new(move || {
            if on_main_thread() {
                HOLDER.lock().unwrap_or_else(|e| e.into_inner()).take();
            }
        }))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = run_on_main;
        HOLDER.lock().unwrap_or_else(|e| e.into_inner()).take();
        Ok(())
    }
}
