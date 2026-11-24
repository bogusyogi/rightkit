//! Routing for verified menu-bar actions and Shortcuts intents, and legacy intent-queue cleanup.
//!
//! The transport authenticates the sender and checks the action against the host's allowlist
//! (see `menubar_helper`, macOS). What remains is policy: skip proof-of-life actions, hop menu
//! actions onto the main thread, gate intents behind onboarding, and map each action name to a host
//! operation. This module is OS-independent so the routing can run and be tested anywhere; the host
//! implements [`IntentHost`] for its operations.

use std::path::PathBuf;

use tauri::{AppHandle, Runtime};

/// The host's operations for Shortcuts intents. Each returns the host's error text on failure.
pub trait IntentHost<R: Runtime> {
    /// Whether the product surfaces may act yet (for example onboarding is complete).
    fn surfaces_enabled(&self) -> bool;
    /// Start capture.
    fn start(&self, app: &AppHandle<R>) -> Result<(), String>;
    /// Stop capture.
    fn stop(&self, app: &AppHandle<R>) -> Result<(), String>;
    /// Toggle capture.
    fn toggle(&self, app: &AppHandle<R>) -> Result<(), String>;
    /// Cancel capture.
    fn cancel(&self, app: &AppHandle<R>) -> Result<(), String>;
    /// Open the history surface (called on the main thread).
    fn open_history(&self, app: &AppHandle<R>) -> Result<(), String>;
}

/// Event names written by [`route_intent`]; the host's vocabulary, so no name is fixed here.
#[derive(Clone, Copy)]
pub struct IntentEvents {
    /// `action=<a> id=<id>` for every intent.
    pub received: &'static str,
    /// Detail `onboarding_incomplete`, when the gate refuses.
    pub ignored: &'static str,
    /// `action=<a>` for an action outside the routed set.
    pub rejected: &'static str,
    /// The error of a routed action.
    pub error: &'static str,
}

/// Route one verified intent. `log` is the host's `(event, detail)` sink.
///
/// `start`, `stop`, `toggle` and `cancel` run in place; `open_history` is scheduled on the main
/// thread and its error is logged there. Any other action is logged as rejected and ignored.
pub fn route_intent<R: Runtime, H: IntentHost<R> + Clone + Send + 'static>(
    app: &AppHandle<R>,
    host: &H,
    action: &str,
    id: &str,
    events: IntentEvents,
    log: fn(&str, &str),
) {
    log(events.received, &format!("action={action} id={id}"));
    if !host.surfaces_enabled() {
        log(events.ignored, "onboarding_incomplete");
        return;
    }
    let result = match action {
        "start" => host.start(app),
        "stop" => host.stop(app),
        "toggle" => host.toggle(app),
        "cancel" => host.cancel(app),
        "open_history" => {
            let handle = app.clone();
            let host = host.clone();
            app.run_on_main_thread(move || {
                if let Err(error) = host.open_history(&handle) {
                    log(events.error, &error);
                }
            })
            .map_err(|error| format!("schedule history window: {error}"))
        }
        other => {
            log(events.rejected, &format!("action={other}"));
            return;
        }
    };
    if let Err(error) = result {
        log(events.error, &error);
    }
}

/// Run a verified menu action on the main thread.
///
/// Actions named in `quiet` (heartbeats, helper-ready) are proof of life only and cause no
/// main-thread hop. Everything else runs `dispatch(app, action)` on the main thread; a scheduling
/// failure is logged as `error_event`.
pub fn dispatch_on_main<R: Runtime>(
    app: &AppHandle<R>,
    action: &str,
    quiet: &[&str],
    error_event: &str,
    log: fn(&str, &str),
    dispatch: impl FnOnce(&AppHandle<R>, &str) + Send + 'static,
) {
    if quiet.contains(&action) {
        return;
    }
    let handle = app.clone();
    let action = action.to_string();
    if let Err(error) = app.run_on_main_thread(move || dispatch(&handle, &action)) {
        log(error_event, &error.to_string());
    }
}

/// Remove queue files an older build left behind. `files` are `(log name, path)` pairs. A removed
/// file is logged as `removed_event` with its name, a missing file is silent and any other error
/// is logged as `error_event` with `<name>: <error>`.
pub fn remove_legacy_files(
    files: impl IntoIterator<Item = (String, PathBuf)>,
    removed_event: &str,
    error_event: &str,
    log: fn(&str, &str),
) {
    for (name, path) in files {
        match std::fs::remove_file(&path) {
            Ok(()) => log(removed_event, &name),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log(error_event, &format!("{name}: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
    fn log(event: &str, detail: &str) {
        LOG.lock().unwrap().push(format!("{event}:{detail}"));
    }

    #[test]
    fn legacy_files_removed_and_missing_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("a");
        std::fs::write(&present, b"x").unwrap();
        remove_legacy_files(
            [
                ("a".to_string(), present.clone()),
                ("b".to_string(), dir.path().join("b")),
            ],
            "removed",
            "err",
            log,
        );
        assert!(!present.exists());
        assert_eq!(*LOG.lock().unwrap(), vec!["removed:a".to_string()]);
    }
}
