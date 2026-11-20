//! macOS activation policy and Dock visibility for an app that is a menu-bar app until it shows a
//! window.
//!
//! Re-applying the SAME activation policy is not a no-op at the AppKit level: it can churn
//! window and key-window ordering, which was observed to desync a CGEventTap's modifier-flag
//! tracking mid-hold. [`Dock::set_policy`] therefore skips the native call when nothing changed,
//! and never caches an attempted transition (a rejected change is retried by the next call).
//!
//! Dock visibility is applied after a window transition has finished
//! ([`Dock::set_visibility_after_transition`]): transforming Dock presence during an AppKit window
//! hand-off reorders windows and shows a one-frame flash. A hide is asserted a second time after a
//! pause, because Tao ignores a hide attempted shortly after a show. A newer request supersedes an
//! older one (epoch), so a window that reopened meanwhile is never hidden.
//!
//! One state owner: every transition ([`Dock::set_policy`], [`Dock::apply_visibility`], the delayed
//! change) is registered under the owner's gate, then checked and applied by a closure that runs
//! on the GUI thread (`run_on_main_thread`). The staleness check and the native call therefore
//! cannot be reordered by another thread, and a request that was superseded while it waited for the
//! GUI thread is dropped at apply time. An immediate [`Dock::apply_visibility`] bumps the epoch, so
//! it also supersedes every pending delayed change and reassertion. An app must route every
//! activation-policy and Dock change through its one `Dock`; a second owner (for example the
//! shell's own `HideToBackground` activation transitions) must stay disabled.
//!
//! Every timing, event name and the telemetry sink are host inputs. One [`Dock`] per process: the
//! host keeps it in a `static`.
#![cfg(target_os = "macos")]

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use tauri::{ActivationPolicy, AppHandle, Runtime};

/// The host's telemetry sink: event name, detail.
pub type EventSink = fn(&str, &str);

/// Longest pause either timing may name.
pub const MAX_PAUSE: Duration = Duration::from_secs(5);

/// The event names this module reports under. Success events carry `regular`, `accessory`,
/// `prohibited` or `unknown` (policy) and `visible` or `hidden` (visibility); error events carry
/// `<policy>: <error>` and `visible=<bool> error=<error>`. A change that could not be handed to the
/// GUI thread is reported on the error event with `schedule_failed=<error>` in place of the error.
#[derive(Debug, Clone, Copy)]
pub struct DockEvents {
    pub policy: &'static str,
    pub policy_error: &'static str,
    pub visibility: &'static str,
    pub visibility_error: &'static str,
}

/// When a Dock change follows a window transition.
#[derive(Debug, Clone, Copy)]
pub struct DockTiming {
    /// Wait after the request before the change is applied (the transition's duration plus margin).
    pub settle: Duration,
    /// For a hide: wait after the first attempt before it is asserted once more.
    pub reassert_after: Duration,
}

/// The policy and Dock state this module has applied. `const`-constructible, for a host `static`.
pub struct Dock {
    // 0 = never set, 1 = Regular, 2 = Accessory, 3 = Prohibited.
    policy_applied: AtomicU8,
    // 0 = never set, 1 = visible, 2 = hidden.
    visibility_applied: AtomicU8,
    visible_wanted: AtomicBool,
    epoch: AtomicU64,
    // Held while a request is registered and while a change is checked and applied.
    gate: Mutex<()>,
}

impl Default for Dock {
    fn default() -> Self {
        Self::new()
    }
}

impl Dock {
    pub const fn new() -> Self {
        Self {
            policy_applied: AtomicU8::new(0),
            visibility_applied: AtomicU8::new(0),
            visible_wanted: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            gate: Mutex::new(()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        self.gate.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record the wanted Dock state and take a new epoch; every older request is now stale.
    fn register(&self, visible: bool) -> u64 {
        let _gate = self.lock();
        self.visible_wanted.store(visible, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Apply `policy` on the GUI thread (inline when called on it), unless it is the one already
    /// applied; that check runs on the GUI thread, immediately before the native call.
    pub fn set_policy<R: Runtime>(
        &'static self,
        app: &AppHandle<R>,
        policy: ActivationPolicy,
        events: &DockEvents,
        sink: EventSink,
    ) {
        let (name, code) = match policy {
            ActivationPolicy::Regular => ("regular", 1u8),
            ActivationPolicy::Accessory => ("accessory", 2u8),
            ActivationPolicy::Prohibited => ("prohibited", 3u8),
            _ => ("unknown", 0u8),
        };
        let events = *events;
        let task_app = app.clone();
        let scheduled = app.run_on_main_thread(move || {
            let outcome = {
                let _gate = self.lock();
                if self.policy_applied.load(Ordering::SeqCst) == code {
                    None
                } else {
                    let result = task_app.set_activation_policy(policy);
                    // Never cache an attempted transition: if AppKit rejects it during launch the
                    // caller must be able to retry instead of leaving a Regular app stuck in the
                    // Dock.
                    if result.is_ok() {
                        self.policy_applied.store(code, Ordering::SeqCst);
                    }
                    Some(result)
                }
            };
            match outcome {
                Some(Ok(())) => sink(events.policy, name),
                Some(Err(err)) => sink(events.policy_error, &format!("{name}: {err}")),
                None => {}
            }
        });
        if let Err(err) = scheduled {
            sink(
                events.policy_error,
                &format!("{name}: schedule_failed={err}"),
            );
        }
    }

    /// Show or hide the Dock icon now (as soon as the GUI thread runs it; inline when called on
    /// it). `force` applies it even when it is the state already applied. It supersedes every
    /// pending delayed change and reassertion, and is itself dropped if a newer request arrives
    /// before the GUI thread applies it.
    pub fn apply_visibility<R: Runtime>(
        &'static self,
        app: &AppHandle<R>,
        visible: bool,
        force: bool,
        events: &DockEvents,
        sink: EventSink,
    ) {
        let epoch = self.register(visible);
        self.schedule_visibility(app, visible, force, epoch, *events, sink);
    }

    /// Hand one visibility change to the GUI thread. The closure re-checks the epoch and the wanted
    /// state under the gate immediately before the native call, so a stale change is dropped there.
    fn schedule_visibility<R: Runtime>(
        &'static self,
        app: &AppHandle<R>,
        visible: bool,
        force: bool,
        epoch: u64,
        events: DockEvents,
        sink: EventSink,
    ) {
        let code = if visible { 1 } else { 2 };
        let task_app = app.clone();
        let scheduled = app.run_on_main_thread(move || {
            let outcome = {
                let _gate = self.lock();
                if self.epoch.load(Ordering::SeqCst) != epoch
                    || self.visible_wanted.load(Ordering::SeqCst) != visible
                    || (!force && self.visibility_applied.load(Ordering::SeqCst) == code)
                {
                    None
                } else {
                    let result = task_app.set_dock_visibility(visible);
                    if result.is_ok() {
                        self.visibility_applied.store(code, Ordering::SeqCst);
                    }
                    Some(result)
                }
            };
            match outcome {
                Some(Ok(())) => sink(
                    events.visibility,
                    if visible { "visible" } else { "hidden" },
                ),
                Some(Err(error)) => sink(
                    events.visibility_error,
                    &format!("visible={visible} error={error}"),
                ),
                None => {}
            }
        });
        if let Err(error) = scheduled {
            sink(
                events.visibility_error,
                &format!("visible={visible} schedule_failed={error}"),
            );
        }
    }

    /// Show or hide the Dock icon once a window transition has finished. The request is registered
    /// at once (superseding every older one); the change runs on the GUI thread after
    /// `timing.settle` and is applied only if no newer request arrived by then and the wanted state
    /// is still `visible`. A hide is asserted again after `timing.reassert_after` under the same
    /// check, so it is dropped if the Dock was wanted visible meanwhile. Both pauses are capped at
    /// [`MAX_PAUSE`].
    pub fn set_visibility_after_transition<R: Runtime>(
        &'static self,
        app: &AppHandle<R>,
        visible: bool,
        timing: DockTiming,
        events: DockEvents,
        sink: EventSink,
    ) {
        let settle = timing.settle.min(MAX_PAUSE);
        let reassert_after = timing.reassert_after.min(MAX_PAUSE);
        let epoch = self.register(visible);
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(settle);
            self.schedule_visibility(&app, visible, false, epoch, events, sink);
            if !visible {
                std::thread::sleep(reassert_after);
                self.schedule_visibility(&app, false, true, epoch, events, sink);
            }
        });
    }
}
