//! Startup sequencing for an app that opens as a hidden-and-shown hub: what to do with the hub, the
//! global hotkeys, the engine and the launch splash at startup, which renderer surfaces must be
//! ready before the hidden hub is parked, the legal-gate latch, and the ordered worker-then-main
//! setup.
//!
//! Donor: the startup sequencing of a desktop app that opens as a hidden-and-shown hub (the
//! decision table, the legal-gate latch, the renderer park, the worker-then-main setup).
//!
//! This module supplies decisions and helpers. It never reorders the host's steps and never writes
//! a log line of its own: every outcome is a typed value and the host logs it in its own words
//! (the gate tags are read by QA). The host owns the order of its startup; the one order this
//! module enforces is the one a helper's name states (`arm_then_try_park`, `run_ordered_setup`).
//!
//! * [`plan`] and [`StartupPlan`]: the pure decision table over [`StartupFacts`].
//! * [`surface_kind`]: which surface the hub presents (legal gate, onboarding, app).
//! * [`reveal_hub_at_startup`]: the hide, show and park sequence for a [`HubAtStartup`] decision,
//!   including the hidden-QA create branch; the host supplies facts, calls, wording and tags.
//! * [`splash_plan`] and [`LegalGateLatch`]: the launch splash and the returning-user legal hold.
//! * [`ReadinessPark`]: park the hidden hub once named renderer surfaces report ready.
//! * [`run_ordered_setup`], [`spawn_or_inline`]: worker thread then main thread, and a spawn that
//!   degrades to inline work.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, Runtime, WebviewWindow};

use crate::hub_window::EventSink;
use crate::startup_park::StartupPark;

/// What the host knows at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupFacts {
    /// Onboarding (and any legal acceptance the host folds into it) is complete.
    pub onboarding_complete: bool,
    /// A runtime model the host needs after onboarding is missing.
    pub models_missing: bool,
}

/// Why a startup gate is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldReason {
    /// Onboarding is not complete.
    Onboarding,
    /// Onboarding is complete but a runtime model is missing.
    RuntimeModels,
}

/// One startup gate: global hotkeys, the macOS fn hotkey, or the engine consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Open,
    Held(HeldReason),
}

/// What the hub does at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubAtStartup {
    /// Onboarded and ready: hide the hub (when one exists) and park its renderer.
    Park,
    /// Onboarding is incomplete: hide the onboarding shell surfaces, then show the hub.
    ShowForOnboarding,
    /// Onboarded but a runtime model is missing: dismiss the splash, then show the hub.
    ShowForMissingModels,
}

/// The decisions for one startup. Every field is a function of [`StartupFacts`] alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupPlan {
    pub onboarding_complete: bool,
    /// Onboarded and no model missing: the runtime may start.
    pub runtime_ready: bool,
    pub hub: HubAtStartup,
    pub hotkeys: Gate,
    pub fn_hotkeys: Gate,
    pub engine: Gate,
}

/// The decision table. Pure: it reads nothing and logs nothing.
pub fn plan(facts: StartupFacts) -> StartupPlan {
    let onboarding_complete = facts.onboarding_complete;
    let runtime_ready = onboarding_complete && !facts.models_missing;
    let gate = if runtime_ready {
        Gate::Open
    } else if !onboarding_complete {
        Gate::Held(HeldReason::Onboarding)
    } else {
        Gate::Held(HeldReason::RuntimeModels)
    };
    let hub = if runtime_ready {
        HubAtStartup::Park
    } else if !onboarding_complete {
        HubAtStartup::ShowForOnboarding
    } else {
        HubAtStartup::ShowForMissingModels
    };
    StartupPlan {
        onboarding_complete,
        runtime_ready,
        hub,
        hotkeys: gate,
        fn_hotkeys: gate,
        engine: gate,
    }
}

/// The surface the hub presents. Resolved from the two INDEPENDENT flags: conflating them makes a
/// returning user's re-acceptance open at full app size and skip the splash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceKind {
    /// Legal acceptance is not current.
    LegalGate,
    /// Accepted, onboarding not done.
    Onboarding,
    App,
}

pub const fn surface_kind(legal_current: bool, onboarding_done: bool) -> SurfaceKind {
    if !legal_current {
        SurfaceKind::LegalGate
    } else if !onboarding_done {
        SurfaceKind::Onboarding
    } else {
        SurfaceKind::App
    }
}

/// What to do with the launch splash at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplashPlan {
    /// Present the splash (an onboarded user; a first-run user's onboarding owns that hand-over).
    pub build: bool,
    /// Visible at once. `false` for an onboarded user held at the legal gate: built hidden, and
    /// the acceptance shows it.
    pub visible: bool,
    /// Hold the launch splash for the legal acceptance: call [`LegalGateLatch::hold`] after the
    /// splash is built and log the deferral.
    pub hold_for_legal_gate: bool,
}

/// `onboarding_done` and `legal_current` are the two INDEPENDENT flags: an onboarded user whose
/// legal acceptance is stale is held at the gate, a first-run user is not.
pub const fn splash_plan(onboarding_done: bool, legal_current: bool) -> SplashPlan {
    SplashPlan {
        build: onboarding_done,
        visible: legal_current,
        hold_for_legal_gate: onboarding_done && !legal_current,
    }
}

/// Set at startup when an already-onboarded user is held at the legal gate, so their engine boot on
/// acceptance can raise the splash (`hub_window::controller::HubController::splash_for_engine_boot`
/// takes it exactly once). Never set for a first-run user.
pub struct LegalGateLatch(AtomicBool);

impl LegalGateLatch {
    pub const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// A returning user is held at the legal gate.
    pub fn hold(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// True exactly once after [`LegalGateLatch::hold`].
    pub fn take(&self) -> bool {
        self.0.swap(false, Ordering::SeqCst)
    }

    pub fn is_held(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl Default for LegalGateLatch {
    fn default() -> Self {
        Self::new()
    }
}

/// What [`ReadinessPark::try_park`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkOutcome {
    /// Not armed yet, or a required surface has not reported. Nothing was taken.
    NotReady,
    /// The park was taken but the window does not exist.
    WindowMissing,
    /// The park was taken but the window is visible, so it was not suspended.
    SkippedVisible,
    /// The window was hidden and the host's `suspend` ran.
    Parked,
}

/// Park the hidden hub's renderer once every named surface has reported ready and the host has
/// armed the park. Suspending earlier would stall the frontend mid-mount. The two events can
/// arrive in either order; whichever comes last takes the park. `const`-constructible for a `static`.
pub struct ReadinessPark {
    park: StartupPark,
    names: &'static [&'static str],
}

impl ReadinessPark {
    /// `names` are the surfaces that must all report ready; a surface's bit is its index. The host
    /// owns the names (they are the renderer's contract). At most 64.
    pub const fn new(names: &'static [&'static str]) -> Self {
        assert!(!names.is_empty() && names.len() <= 64);
        let required = if names.len() == 64 {
            u64::MAX
        } else {
            (1u64 << names.len()) - 1
        };
        Self {
            park: StartupPark::new(required),
            names,
        }
    }

    /// The host hid the window at startup and wants it parked once ready.
    pub fn arm(&self) {
        self.park.arm();
    }

    /// A surface reported ready. `false` when `name` is not one of the required surfaces (nothing
    /// is marked; the host reports the unknown name in its own words).
    pub fn mark(&self, name: &str) -> bool {
        match self.names.iter().position(|known| *known == name) {
            Some(index) => {
                self.park.mark_ready(1u64 << index);
                true
            }
            None => false,
        }
    }

    /// True exactly once: every surface is ready and the park was armed.
    pub fn take_if_ready(&self) -> bool {
        self.park.take_if_ready()
    }

    /// Take the park if it is ready, then suspend the window `label` unless it is visible. `suspend`
    /// is the host's suspend (its idle wiring and any pre-suspend hook).
    pub fn try_park<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        label: &str,
        suspend: &dyn Fn(&WebviewWindow<R>),
    ) -> ParkOutcome {
        if !self.take_if_ready() {
            return ParkOutcome::NotReady;
        }
        let Some(window) = app.get_webview_window(label) else {
            return ParkOutcome::WindowMissing;
        };
        if window.is_visible().unwrap_or(true) {
            return ParkOutcome::SkippedVisible;
        }
        suspend(&window);
        ParkOutcome::Parked
    }

    /// Arm the park, then try it. The order is the point: arming first means a surface report that
    /// already arrived parks the window now, and one that arrives later parks it from
    /// [`ReadinessPark::try_park`]. The caller hid the window first.
    pub fn arm_then_try_park<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        label: &str,
        suspend: &dyn Fn(&WebviewWindow<R>),
    ) -> ParkOutcome {
        self.arm();
        self.try_park(app, label, suspend)
    }
}

/// How [`reveal_hub_at_startup`] parks a hidden hub.
#[derive(Clone, Copy)]
pub enum ParkMode<'a> {
    /// Suspend at once (macOS, where the hub is hidden before its renderer matters).
    Immediate,
    /// Arm the park and suspend when the named surfaces are ready (Windows).
    Readiness(&'a ReadinessPark),
}

/// A log line: `(event, detail)`.
pub type Line = (&'static str, &'static str);

/// The host's wording for [`reveal_hub_at_startup`] and park reports. The tags are read by QA.
#[derive(Debug, Clone, Copy)]
pub struct HubStartupWording {
    /// The hidden-QA run needed a hub and one was created.
    pub hidden_qa_created: Line,
    /// Hiding the hub failed (detail: the error text).
    pub hide_error: &'static str,
    /// Parked at once.
    pub parked_immediate: Line,
    /// Parked once the readiness surfaces reported.
    pub parked_ready: Line,
    /// The park was taken but the hub is visible.
    pub park_skipped_visible: Line,
    pub shown_for_onboarding: Line,
    pub shown_for_missing_models: Line,
}

impl HubStartupWording {
    /// Log what a park attempt did.
    pub fn report_park(&self, outcome: ParkOutcome, sink: EventSink) {
        match outcome {
            ParkOutcome::SkippedVisible => {
                sink(self.park_skipped_visible.0, self.park_skipped_visible.1)
            }
            ParkOutcome::Parked => sink(self.parked_ready.0, self.parked_ready.1),
            ParkOutcome::NotReady | ParkOutcome::WindowMissing => {}
        }
    }
}

type QaHubNeeded<'a, R> = &'a dyn Fn(&AppHandle<R>) -> bool;

/// The host's side of [`reveal_hub_at_startup`]. Every call that can present a window is the
/// host's, so its own reveal guard and hidden-QA rules apply to it.
pub struct HubStartupHost<'a, R: Runtime> {
    pub label: &'a str,
    pub park: ParkMode<'a>,
    pub wording: HubStartupWording,
    pub sink: EventSink,
    /// `Some` only in a QA build: a hidden QA run needs a hub that no window declares (macOS).
    pub qa_hub_needed: Option<QaHubNeeded<'a, R>>,
    /// Shows the hub (the host's show; in a hidden QA run it builds the hub hidden).
    pub show_hub: &'a dyn Fn(&AppHandle<R>),
    /// Suspends the hub's renderer.
    pub suspend: &'a dyn Fn(&WebviewWindow<R>),
    /// Onboarding: hide the host's onboarding shell surfaces before the hub shows.
    pub hide_onboarding_surfaces: &'a dyn Fn(&AppHandle<R>),
    /// Missing model: before the hub shows (the host dismisses its splash).
    pub before_show_missing_models: &'a dyn Fn(&AppHandle<R>),
}

/// Carry out a [`HubAtStartup`] decision in its fixed order. `Park`: create a hidden-QA hub if the
/// host needs one, hide the hub when it exists, then park it (the park is armed before the first
/// attempt, so a surface that already reported parks now and a later one parks from
/// [`ReadinessPark::try_park`]). `ShowForOnboarding`: hide the onboarding surfaces, then show.
/// `ShowForMissingModels`: host prelude, then show. Every line goes through `sink` in the host's
/// words.
pub fn reveal_hub_at_startup<R: Runtime>(
    app: &AppHandle<R>,
    hub: HubAtStartup,
    host: &HubStartupHost<'_, R>,
) {
    let w = &host.wording;
    match hub {
        HubAtStartup::Park => {
            if host.qa_hub_needed.is_some_and(|needed| needed(app)) {
                (host.show_hub)(app);
                (host.sink)(w.hidden_qa_created.0, w.hidden_qa_created.1);
            }
            let Some(window) = app.get_webview_window(host.label) else {
                return;
            };
            if let Err(err) = window.hide() {
                (host.sink)(w.hide_error, &err.to_string());
                return;
            }
            match host.park {
                ParkMode::Immediate => {
                    (host.suspend)(&window);
                    (host.sink)(w.parked_immediate.0, w.parked_immediate.1);
                }
                ParkMode::Readiness(park) => {
                    let outcome = park.arm_then_try_park(app, host.label, host.suspend);
                    w.report_park(outcome, host.sink);
                }
            }
        }
        HubAtStartup::ShowForOnboarding => {
            (host.hide_onboarding_surfaces)(app);
            (host.show_hub)(app);
            (host.sink)(w.shown_for_onboarding.0, w.shown_for_onboarding.1);
        }
        HubAtStartup::ShowForMissingModels => {
            (host.before_show_missing_models)(app);
            (host.show_hub)(app);
            (host.sink)(w.shown_for_missing_models.0, w.shown_for_missing_models.1);
        }
    }
}

/// Log tags of [`run_ordered_setup`]; the host's words, so QA matching stays byte-identical.
#[derive(Debug, Clone, Copy)]
pub struct SetupTags {
    /// The worker thread could not be spawned (detail: the OS error).
    pub spawn_failed: &'static str,
    /// The main-thread step returned an error (detail: the error text); the app exits with 1.
    pub main_failed: &'static str,
    /// The main-thread step could not be scheduled (detail: the error text); the app exits with 1.
    pub schedule_failed: &'static str,
}

/// Run `worker` on a named thread, then `main` on the Tauri main thread with an app handle, in that
/// order. Use it when `setup` must not hold the main thread for slow work (crash recovery over a
/// keychain and a database) that has to finish before anything native starts. A `main` error or a
/// scheduling failure is reported through `sink` and exits the app with status 1. Returns the
/// spawn error (after reporting it) when the worker thread cannot start; then neither step ran.
pub fn run_ordered_setup<R, W, M>(
    app: &AppHandle<R>,
    thread_name: &str,
    tags: SetupTags,
    sink: EventSink,
    worker: W,
    main: M,
) -> Result<(), std::io::Error>
where
    R: Runtime,
    W: FnOnce() + Send + 'static,
    M: FnOnce(AppHandle<R>) -> tauri::Result<()> + Send + 'static,
{
    let worker_app = app.clone();
    let spawned = std::thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || {
            worker();
            let main_app = worker_app.clone();
            let scheduled = worker_app.run_on_main_thread(move || {
                if let Err(err) = main(main_app.clone()) {
                    sink(tags.main_failed, &err.to_string());
                    main_app.exit(1);
                }
            });
            if let Err(err) = scheduled {
                sink(tags.schedule_failed, &err.to_string());
                worker_app.exit(1);
            }
        });
    match spawned {
        Ok(_) => Ok(()),
        Err(err) => {
            sink(tags.spawn_failed, &err.to_string());
            Err(err)
        }
    }
}

/// Run `work` on a named thread. When the thread cannot be spawned (a spawn failure is fatal under
/// `panic=abort` if left to `expect`), call `spawn_failed` with the error and then run `fallback`
/// inline, so the work still happens.
pub fn spawn_or_inline<W, E, F>(name: &str, work: W, spawn_failed: E, fallback: F)
where
    W: FnOnce() + Send + 'static,
    E: FnOnce(&std::io::Error),
    F: FnOnce(),
{
    if let Err(err) = std::thread::Builder::new()
        .name(name.to_string())
        .spawn(work)
    {
        spawn_failed(&err);
        fallback();
    }
}
