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
//!
//! Lifecycle (HANDOFF.md, "Lifecycle"): each [`register`] is one generation
//! (`lifecycle::Liveness`). [`unregister`] (or the next `register`, or a failed one) ends it:
//! (1) events of the ended set start nothing, and a start (`on_tap`, `Debounced`, `Immediate`)
//! queued before the end but run after it does nothing; (2) a `TapHold` chord held at that moment
//! is released once, as a real release would be (hold-release runs if it was held for the
//! threshold), and the user's later real release is ignored, so it never runs twice; a release
//! already accepted always runs (an end is never dropped); (3) the OS registrations are dropped.
//! The per-press `rk-chord-action` threads live as long as the host callback they run. Starts
//! (`on_tap`, `Debounced`, `Immediate` on Windows) are capped at [`MAX_START_JOBS`] alive at once
//! in the process and ends (`on_hold_release`) at [`MAX_END_JOBS`]: a start past its cap is dropped
//! and reported (`chord_action_capped`), an end past its cap is dropped and reported the same way
//! (only possible with that many release callbacks already hung); a refused thread is reported as
//! `chord_action_spawn_failed` instead of panicking.
//!
//! The host's sink is never called on the hotkey loop thread, on the main thread, or on a thread
//! of `register` or `unregister`: every line (the host's `press_log`, `release_log` and `log`, the
//! cap and spawn reports, an optional chord's warning) is queued on the registration's own bounded
//! lane (`Reporter`, depth 32, newest dropped and counted, `chord_report_dropped`) and delivered by
//! one `rk-chord-report` thread. A sink that never returns therefore pins that thread only: the
//! hotkey loop thread, which `unregister` joins when it drops the OS registrations, only ever
//! queues. The lane is closed when the registration ends (after the last line of the end, so
//! nothing is lost) and the thread drains and exits. The sink never receives a key, a chord text
//! or an operating-system message: only the host's own static lines and stable tags.
//!
//! A hold-release runs only for a tap that actually STARTED: a press whose `on_tap` never ran (the
//! registration ended first, or the release arrived before the job started) gets no release, so the
//! host is never told to stop something it was never told to start. And it never runs ahead of its
//! tap: `on_tap` calls of one chord run one at a time (`Slot::tap_run`), and a release that arrives
//! while `on_tap` is still executing is run by that same start job right after `on_tap` returns
//! ([`request_release`]); otherwise it runs on an end job. An end job still counts against
//! [`MAX_END_JOBS`], a release run by a start job against [`MAX_START_JOBS`] (it is part of that
//! job). A hung `on_tap` therefore also holds back later starts of the same chord and the release of
//! the one that is hung. `register` and `unregister`
//! take turns (one process-wide lock held across each), so two of them can never overwrite each
//! other's registration.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::chord::{parse_chord, Chord};
use crate::hold::HotkeyState;
use crate::lifecycle::{lock, Lane, Liveness, ResolverCap};

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
    /// `Some(event)`: a failed registration is logged to the sink under `event` with the message
    /// `optional_chord_unavailable` and skipped instead of failing the set.
    pub optional_warning_event: Option<&'static str>,
    pub action: Action,
}

pub struct ChordHost {
    pub event_sink: EventSink,
    pub run_on_main: RunOnMain,
}

/// `reason` of a `register <chord> failed: <reason>` message from the owner (the error returned to
/// the caller; it is not put into a sink line).
fn failure_reason(message: &str) -> &str {
    message
        .split_once(" failed: ")
        .map(|(_, reason)| reason)
        .unwrap_or(message)
}

/// Depth of a registration's report lane.
const REPORT_DEPTH: usize = 32;

/// Every host sink line of one registration (see the module notes): queued without a lock or a
/// wait, delivered by `rk-chord-report`. A closed lane (the registration ended, or no thread could
/// be created) drops the line.
struct Reporter {
    lane: Lane<LogLine>,
}

impl Reporter {
    /// The lane and its delivery thread. Must be ended with [`Reporter::close`] (the thread holds
    /// the reporter for the dropped-line count, so the lane ends by its close marker).
    fn start(sink: EventSink) -> Arc<Self> {
        let (lane, rx) = Lane::<LogLine>::with_depth(REPORT_DEPTH);
        let reporter = Arc::new(Self { lane });
        let drain = Arc::clone(&reporter);
        let spawned = std::thread::Builder::new()
            .name("rk-chord-report".to_string())
            .spawn(move || {
                // A panicking sink must not end the thread.
                let deliver = |event: &str, message: &str| {
                    let _ = catch_unwind(AssertUnwindSafe(|| sink(event, message)));
                };
                let overflow = || {
                    let dropped = drain.lane.take_dropped();
                    if dropped > 0 {
                        deliver("chord_report_dropped", &format!("dropped={dropped}"));
                    }
                };
                while let Ok((event, message)) = rx.recv() {
                    deliver(event, message);
                    overflow();
                }
                overflow();
            });
        if spawned.is_err() {
            reporter.lane.close();
        }
        reporter
    }

    /// Queue one line; never blocks.
    fn line(&self, event: &'static str, message: &'static str) {
        let _ = self.lane.send((event, message));
    }

    /// End the lane: what is queued is still delivered, then the thread exits.
    fn close(&self) {
        self.lane.close();
    }
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

/// A unit of work handed to a thread (or run inline on macOS for `Debounced`/`Immediate`).
type Job = Box<dyn FnOnce() + Send + 'static>;

/// The id -> slot table of one registration.
type Table = Mutex<Vec<(u32, Arc<Slot>)>>;

/// Everything one [`register`] made: the OS registrations plus the generation that scopes what
/// its callbacks may still do.
struct Installed {
    /// Ended by [`unregister`] (or a failed registration): after that no event of this set starts
    /// anything, and a start queued but not yet run does nothing.
    live: Arc<Liveness>,
    table: Arc<Table>,
    /// Closed by `end_install`, after the registration's last line.
    reporter: Arc<Reporter>,
    holder: os::Holder,
}

static INSTALLED: Mutex<Option<Installed>> = Mutex::new(None);
/// `register` and `unregister` (and the registration they end) take turns: held across each whole
/// operation, so a concurrent pair cannot overwrite `INSTALLED` or leave a registration alive after
/// `unregister` returned. On macOS it is taken on the main thread (where registration runs), never
/// while waiting for the main thread.
static OP_LOCK: Mutex<()> = Mutex::new(());

/// Most bindings one [`register`] accepts; more is an error and registers nothing (the registration
/// loop and the table are proportional to the host's list).
pub const MAX_BINDINGS: usize = 64;
/// `rk-chord-action` threads running a START (`on_tap`, Windows `Debounced`/`Immediate`) at once.
pub const MAX_START_JOBS: usize = 8;
/// `rk-chord-action` threads running an END (`on_hold_release`) at once.
pub const MAX_END_JOBS: usize = 32;
static START_JOBS: ResolverCap = ResolverCap::new(MAX_START_JOBS);
static END_JOBS: ResolverCap = ResolverCap::new(MAX_END_JOBS);

/// Per-press state: cancelled/settled, queued, or started.
const TAP_IDLE: u8 = 0;
const TAP_QUEUED: u8 = 1;
const TAP_STARTED: u8 = 2;

/// One accepted press, retained by its own queued job even after a short release.
/// Later presses never reuse its state or its release request.
struct TapPress {
    pressed_at: Instant,
    state: AtomicU8,
    running: AtomicBool,
    release_wanted: AtomicBool,
}

impl TapPress {
    fn new() -> Self {
        Self {
            pressed_at: Instant::now(),
            state: AtomicU8::new(TAP_QUEUED),
            running: AtomicBool::new(false),
            release_wanted: AtomicBool::new(false),
        }
    }
}

/// Per-binding runtime state: debounce, currently held press & callback serialization.
struct Slot {
    action: Action,
    /// Only the press awaiting its physical release or teardown. Released short taps remain owned
    /// by their queued jobs, so another press cannot overwrite their pending start.
    held: Mutex<Option<Arc<TapPress>>>,
    last_fire: Mutex<Option<Instant>>,
    /// Serialize this chord's start callbacks; per-press state stays independent while waiting.
    tap_run: Mutex<()>,
}

impl Slot {
    fn new(action: Action) -> Self {
        Self {
            action,
            held: Mutex::new(None),
            last_fire: Mutex::new(None),
            tap_run: Mutex::new(()),
        }
    }
}

/// Settle this press's release behind its own start, exactly once. A running start consumes its
/// release request after returning; an already-finished start uses an end job.
fn request_release(press: &TapPress, release: Callback, spawn_end: &dyn Fn(Job)) {
    press.release_wanted.store(true, Ordering::SeqCst);
    if !press.running.load(Ordering::SeqCst) && press.release_wanted.swap(false, Ordering::SeqCst) {
        spawn_end(Box::new(move || release()));
    }
}

/// Per-action guard: returns true (and stamps `now`) if more than `guard_ms` has elapsed since the
/// last fire on this slot.
fn debounce_ok(slot: &Mutex<Option<Instant>>, guard_ms: u64) -> bool {
    let mut last = lock(slot);
    let now = Instant::now();
    let ok = last
        .map(|t| now.duration_since(t) > Duration::from_millis(guard_ms))
        .unwrap_or(true);
    if ok {
        *last = Some(now);
    }
    ok
}

/// Whether a release (or an unregister while the chord is held) runs the hold-release callback:
/// the chord was held for at least the threshold.
fn release_runs(pressed_at: Option<Instant>, now: Instant, threshold: Duration) -> bool {
    let held = pressed_at
        .map(|t| now.saturating_duration_since(t))
        .unwrap_or(Duration::ZERO);
    held >= threshold
}

/// Which cap a chord job counts against.
#[derive(Clone, Copy)]
enum JobKind {
    /// Starts something (`on_tap`, `Debounced`, `Immediate`).
    Start,
    /// Ends something a start began (`on_hold_release`).
    End,
}

/// Run `job` on its own thread, counted against its cap. A refused thread, or a cap that is full,
/// is reported (queued, `Reporter`) instead of panicking on the hotkey loop thread; the job is then
/// lost (nothing can run it).
fn spawn_job(reporter: &Reporter, kind: JobKind, job: Job) {
    let (cap, full) = match kind {
        JobKind::Start => (&START_JOBS, "start"),
        JobKind::End => (&END_JOBS, "end"),
    };
    let Some(permit) = cap.try_acquire() else {
        reporter.line("chord_action_capped", full);
        return;
    };
    // A failed spawn drops the closure and with it the permit.
    if std::thread::Builder::new()
        .name("rk-chord-action".to_string())
        .spawn(move || {
            let _permit = permit;
            job()
        })
        .is_err()
    {
        reporter.line("chord_action_spawn_failed", "thread_spawn_refused");
    }
}

/// Windows delivers on the hotkey loop thread, so work leaves it; macOS delivers on the main
/// thread, where these actions have always run inline.
fn run_action(reporter: &Reporter, job: Job) {
    if cfg!(target_os = "windows") {
        spawn_job(reporter, JobKind::Start, job);
    } else {
        job();
    }
}

fn log(reporter: &Reporter, line: Option<LogLine>) {
    if let Some((event, message)) = line {
        reporter.line(event, message);
    }
}

/// Job routes for one dispatch turn; callback execution policy is unchanged.
struct JobRunners<'a> {
    start: &'a dyn Fn(Job),
    end: &'a dyn Fn(Job),
    inline: &'a dyn Fn(Job),
}

/// One hotkey event of one registration. Starts (`on_tap`, `Debounced`, `Immediate`) are gated by
/// the generation twice: here, and again when the job finally runs, so a start that was queued
/// before [`unregister`] and runs after it does nothing. The release of a hold is an END (it stops
/// what the tap started), so once accepted it always runs.
fn handle(
    table: &Table,
    live: &Arc<Liveness>,
    id: u32,
    state: HotkeyState,
    sink: &Reporter,
    runners: JobRunners<'_>,
) {
    let JobRunners {
        start: spawn,
        end: spawn_end,
        inline,
    } = runners;
    if !live.is_live() {
        return;
    }
    let slot = lock(table)
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
                on_hold_release,
                guard_ms,
                press_log,
                ..
            },
            HotkeyState::Pressed,
        ) => {
            let press = {
                let mut held = lock(&slot.held);
                // Repeated down notifications cannot replace a press still awaiting its up.
                if held.is_some() || !debounce_ok(&slot.last_fire, *guard_ms) {
                    return;
                }
                let press = Arc::new(TapPress::new());
                *held = Some(Arc::clone(&press));
                press
            };
            log(sink, *press_log);
            let on_tap = on_tap.clone();
            // The release this start runs itself when a long release arrives while `on_tap` runs.
            let on_release = on_hold_release.clone();
            let live = Arc::clone(live);
            let job_slot = Arc::clone(&slot);
            spawn(Box::new(move || {
                // One start of this chord at a time; the release of a started tap runs inside this
                // turn when it arrives early, so it can never run ahead of `on_tap`.
                let _turn = lock(&job_slot.tap_run);
                // `running` goes up BEFORE the state can read "started": a release that sees a
                // started tap also sees that its start is still running.
                press.running.store(true, Ordering::SeqCst);
                // Starts only if the registration is live AND no long release cancelled it.
                let started = live.is_live()
                    && press
                        .state
                        .compare_exchange(
                            TAP_QUEUED,
                            TAP_STARTED,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        )
                        .is_ok();
                if !started {
                    press.running.store(false, Ordering::SeqCst);
                    return;
                }
                // A panicking host callback must not leave the chord "running" for ever.
                let _ = catch_unwind(AssertUnwindSafe(|| on_tap()));
                press.running.store(false, Ordering::SeqCst);
                if press.release_wanted.swap(false, Ordering::SeqCst) {
                    let _ = catch_unwind(AssertUnwindSafe(|| on_release()));
                }
            }));
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
            let Some(press) = lock(&slot.held).take() else {
                return;
            };
            if release_runs(Some(press.pressed_at), Instant::now(), *hold_threshold) {
                // A long release ends only its own press. A queued tap is cancelled; a started
                // tap receives its release behind its own callback, regardless of later presses.
                if press.state.swap(TAP_IDLE, Ordering::SeqCst) == TAP_STARTED {
                    log(sink, *release_log);
                    request_release(&press, on_hold_release.clone(), spawn_end);
                }
            }
            // A short tap's queued start retains `press` through its captured Arc.
        }

        (Action::Debounced { run, guard_ms }, HotkeyState::Pressed) => {
            if debounce_ok(&slot.last_fire, *guard_ms) {
                let run = run.clone();
                let live = Arc::clone(live);
                inline(Box::new(move || {
                    if live.is_live() {
                        run()
                    }
                }));
            }
        }
        (Action::Immediate { run, log: line }, HotkeyState::Pressed) => {
            log(sink, *line);
            let run = run.clone();
            let live = Arc::clone(live);
            inline(Box::new(move || {
                if live.is_live() {
                    run()
                }
            }));
        }
        _ => {}
    }
}

/// A chord still held when its registration ends is treated as released then: a `TapHold` held for
/// at least its threshold runs its hold-release (the host never keeps a capture it started on the
/// press whose release can no longer be seen), a shorter hold does nothing, exactly as a real
/// release would. The press time is taken, so a release racing this call runs it at most once.
fn flush_held(slots: &[Arc<Slot>], now: Instant, sink: &Reporter, spawn_end: &dyn Fn(Job)) {
    for slot in slots {
        let Action::TapHold {
            on_hold_release,
            hold_threshold,
            release_log,
            ..
        } = &slot.action
        else {
            continue;
        };
        let Some(press) = lock(&slot.held).take() else {
            continue;
        };
        // Teardown settles the held press only. Earlier short taps have their own job state &
        // cannot be confused with this one; queued jobs also observe the ended generation.
        if release_runs(Some(press.pressed_at), now, *hold_threshold)
            && press.state.swap(TAP_IDLE, Ordering::SeqCst) == TAP_STARTED
        {
            log(sink, *release_log);
            request_release(&press, on_hold_release.clone(), spawn_end);
        }
    }
}

/// End one registration: close the generation first (so nothing new starts), settle held chords,
/// then drop the OS registrations (which stops the crate's hotkey thread; it only ever spawned
/// jobs, so that join is short). The only thing that can outlive this is a host callback that is
/// already running on its own thread.
fn end_install(installed: Installed) {
    let Installed {
        live,
        table,
        reporter,
        holder,
    } = installed;
    live.end();
    let slots: Vec<Arc<Slot>> = lock(&table).iter().map(|(_, slot)| slot.clone()).collect();
    flush_held(&slots, Instant::now(), &reporter, &|job| {
        spawn_job(&reporter, JobKind::End, job)
    });
    drop(holder);
    // After the last line of this registration: the thread delivers what is queued and exits.
    reporter.close();
}

fn register_all(
    holder: &mut os::Holder,
    bindings: &[Binding],
    slots: &[Arc<Slot>],
    table: &Table,
    reporter: &Reporter,
) -> Result<(), String> {
    for (binding, slot) in bindings.iter().zip(slots) {
        let label = binding.label;
        let chord =
            parse_chord(binding.chord).map_err(|e| format!("register {label} failed: {e}"))?;
        match os::register(holder, &chord) {
            Ok(id) => lock(table).push((id, slot.clone())),
            Err(_) if binding.optional_warning_event.is_some() => {
                // Optional chord: reported, not fatal. The line is the host's event name and a tag:
                // neither the label nor the operating system's message is put into sink text.
                reporter.line(
                    binding.optional_warning_event.unwrap_or_default(),
                    "optional_chord_unavailable",
                );
            }
            Err(message) => {
                return Err(format!(
                    "register {label} failed: {}",
                    failure_reason(&message)
                ));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn on_main_thread() -> bool {
    extern "C" {
        fn pthread_main_np() -> i32;
    }
    // SAFETY: queries whether this thread owns the application's main run loop.
    unsafe { pthread_main_np() != 0 }
}

fn install(bindings: Vec<Binding>, event_sink: EventSink) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if !on_main_thread() {
        return Err("hotkey registration requires main thread".into());
    }

    if bindings.len() > MAX_BINDINGS {
        return Err(format!(
            "register chords failed: {} bindings, at most {MAX_BINDINGS}",
            bindings.len()
        ));
    }
    let _turn = lock(&OP_LOCK);
    // A registration still live is ended first (a new registration cannot coexist with it: the
    // chords would conflict, and its callbacks must not act on the new one). On macOS this runs
    // on the main thread, where the old registrations belong.
    let previous = lock(&INSTALLED).take();
    if let Some(previous) = previous {
        end_install(previous);
    }

    let live = Liveness::begin();
    let slots: Vec<Arc<Slot>> = bindings
        .iter()
        .map(|binding| Arc::new(Slot::new(binding.action.clone())))
        .collect();
    let table: Arc<Table> = Arc::new(Mutex::new(Vec::new()));

    let reporter = Reporter::start(event_sink);
    let dispatch: Arc<dyn Fn(u32, HotkeyState) + Send + Sync> = {
        let table = Arc::clone(&table);
        let live = Arc::clone(&live);
        let reporter = Arc::clone(&reporter);
        Arc::new(move |id, state| {
            handle(
                &table,
                &live,
                id,
                state,
                &reporter,
                JobRunners {
                    start: &|job| spawn_job(&reporter, JobKind::Start, job),
                    end: &|job| spawn_job(&reporter, JobKind::End, job),
                    inline: &|job| run_action(&reporter, job),
                },
            )
        })
    };

    let holder = match os::create(dispatch) {
        Ok(holder) => holder,
        Err(message) => {
            reporter.close();
            return Err(message);
        }
    };
    let mut installed = Installed {
        live,
        table: Arc::clone(&table),
        reporter: Arc::clone(&reporter),
        holder,
    };
    match register_all(&mut installed.holder, &bindings, &slots, &table, &reporter) {
        Ok(()) => {
            *lock(&INSTALLED) = Some(installed);
            Ok(())
        }
        Err(message) => {
            // Ends the generation and drops the holder, which unregisters everything registered
            // so far.
            end_install(installed);
            Err(message)
        }
    }
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
        // A queued timeout cancels the job. Once running, wait for its actual ownership result.
        let phase = Arc::new(AtomicU8::new(0)); // queued, running, cancelled
        let job_phase = Arc::clone(&phase);
        let (tx, rx) = std::sync::mpsc::channel();
        run_on_main(Box::new(move || {
            if job_phase
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

/// End every registration made by [`register`]: no chord starts anything afterwards, a chord held
/// at this moment is released once (see [`flush_held`]), and the OS registrations are dropped. On
/// macOS they are released on the main thread, where they were made; the error is the scheduling
/// error text, for the host to word. Does not wait for host callbacks that are already running.
pub fn unregister(run_on_main: &RunOnMain) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        fn end_on_main() {
            if !on_main_thread() {
                return;
            }
            let _turn = lock(&OP_LOCK);
            let installed = lock(&INSTALLED).take();
            if let Some(installed) = installed {
                end_install(installed);
            }
        }
        if on_main_thread() {
            end_on_main();
            return Ok(());
        }
        // Keep Carbon ownership in INSTALLED until scheduling succeeds & main-thread work runs.
        run_on_main(Box::new(end_on_main))
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = run_on_main;
        // Take and end under one turn: a concurrent `register` either finishes before this takes
        // the registration (and is ended) or starts after this returned.
        let _turn = lock(&OP_LOCK);
        let installed = lock(&INSTALLED).take();
        if let Some(installed) = installed {
            end_install(installed);
        }
        Ok(())
    }
}
