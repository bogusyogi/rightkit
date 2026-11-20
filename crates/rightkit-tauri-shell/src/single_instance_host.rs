//! Single-instance claim and forwarded-launch handling for an app's `run()`. Moved from HeardRight
//! `app_run.rs` (`single_instance_identifier`, `claim_single_instance`, `handle_forwarded_launch`
//! and the four statics that deferred a forwarded launch until startup finished).
//!
//! The claim is made before the Tauri builder exists, so a launch forwarded early is parked until
//! setup binds the app handle. On macOS a forwarded launch is also held until the native startup
//! continuation is ready. Host inputs: the identifier, the reveal call, the event sink and the
//! fixed stderr line for a refused claim.

use crate::SingleInstanceGuard;
use crate::{ClaimFailure, ClaimOptions, ClaimOutcome, InstanceMessage, SingleInstance};
use std::sync::Mutex;

/// Host event sink: `(event, detail)`.
pub type EventSink = fn(&str, &str);

/// The single-instance identity of a build whose QA runs must not forward to (and exit for) the
/// user's running app: `base` when `suite_root` is `None` or empty, else `<base>.qa-<hash of
/// suite_root>`. The host decides whether a QA build exists and where the suite root comes from.
pub fn scoped_identifier(base: &str, suite_root: Option<&str>) -> String {
    use std::hash::{Hash, Hasher};
    match suite_root {
        Some(root) if !root.is_empty() => {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            root.hash(&mut hasher);
            format!("{base}.qa-{:016x}", hasher.finish())
        }
        _ => base.to_string(),
    }
}

/// Claim the single-instance primary. A launch that reaches a live primary (this build or an older
/// one on `tauri-plugin-single-instance`) is forwarded and this process exits 0. A live copy that
/// did not accept this launch exits 1 after printing `refused_line` to stderr (a fixed line: the
/// detail can name a per-user service folder, it is in the sink's line). When arbitration could not
/// run, `None` is returned so the user is not locked out of launching. Returns the guard that must
/// live for the whole run.
pub fn claim_or_exit(
    identifier: &str,
    on_forwarded: impl Fn() + Send + Sync + 'static,
    sink: EventSink,
    refused_line: &str,
) -> Option<SingleInstanceGuard> {
    let outcome = SingleInstance::claim_with_options(
        identifier,
        ClaimOptions::with_legacy(identifier).holding_legacy(),
        InstanceMessage::from_process(),
        move |_message| on_forwarded(),
    );
    match outcome {
        ClaimOutcome::Primary(guard) => Some(guard),
        ClaimOutcome::Forwarded => {
            sink("single_instance_forwarded", "exiting");
            std::process::exit(0);
        }
        ClaimOutcome::Failed(failure @ (ClaimFailure::Forward(_) | ClaimFailure::Legacy(_))) => {
            // A live copy exists but did not accept this launch: never become a second primary.
            sink("single_instance_failed", &failure.to_string());
            eprintln!("{refused_line}");
            std::process::exit(1);
        }
        ClaimOutcome::Failed(failure) => {
            // Arbitration could not run; do not lock the user out of launching.
            sink("single_instance_unarbitrated", &failure.to_string());
            None
        }
    }
}

/// Where a forwarded launch waits until the app can show itself. One `static` per app.
pub struct ForwardedLaunch {
    state: Mutex<ForwardState>,
}

struct ForwardState {
    app: Option<tauri::AppHandle>,
    pending: bool,
    startup_ready: bool,
}

impl ForwardState {
    fn take_ready(&mut self) -> Option<tauri::AppHandle> {
        if !self.pending || (cfg!(target_os = "macos") && !self.startup_ready) {
            return None;
        }
        let app = self.app.clone()?;
        self.pending = false;
        Some(app)
    }
}

impl Default for ForwardedLaunch {
    fn default() -> Self {
        Self::new()
    }
}

impl ForwardedLaunch {
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(ForwardState {
                app: None,
                pending: false,
                startup_ready: false,
            }),
        }
    }

    /// A second launch was forwarded to this primary: show the app via `show` (on macOS on the
    /// main thread, and only once the startup continuation is ready; until then it is parked).
    pub fn forwarded(&self, show: fn(&tauri::AppHandle), sink: EventSink) {
        let (app, deferred) = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.pending = true;
            let deferred = cfg!(target_os = "macos") && state.app.is_some() && !state.startup_ready;
            (state.take_ready(), deferred)
        };
        if deferred {
            sink("single_instance_deferred", "startup_pending");
        }
        let Some(app) = app else {
            return;
        };
        #[cfg(target_os = "macos")]
        {
            let show_app = app.clone();
            if let Err(error) = app.run_on_main_thread(move || show(&show_app)) {
                // A failed enqueue did not deliver this launch. Retain it for the next
                // forwarded launch, bind or startup-ready notification to retry.
                self.state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .pending = true;
                sink("single_instance_schedule_failed", &error.to_string());
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = sink;
            show(&app);
        }
    }

    /// Setup has the app handle: remember it and replay a launch that arrived before. On macOS the
    /// replay waits for [`Self::mark_startup_ready`]. Call from Tauri setup (macOS main thread).
    pub fn bind_app(&self, app: &tauri::AppHandle, show: fn(&tauri::AppHandle)) {
        let ready = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.app.get_or_insert_with(|| app.clone());
            state.take_ready()
        };
        if let Some(app) = ready {
            show(&app);
        }
    }

    /// macOS: true once the native startup continuation has run (page loads of the hub wait for it).
    pub fn startup_ready(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .startup_ready
    }

    /// macOS: the native startup continuation is ready. Shows the app now when a launch was parked.
    /// Call from the native continuation on the macOS main thread.
    pub fn mark_startup_ready(&self, app: &tauri::AppHandle, show: fn(&tauri::AppHandle)) {
        let ready = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.startup_ready = true;
            state.app.get_or_insert_with(|| app.clone());
            state.take_ready()
        };
        if let Some(app) = ready {
            show(&app);
        }
    }
}
