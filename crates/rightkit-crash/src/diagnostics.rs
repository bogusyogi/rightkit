//! One call that wires a process's diagnostics (feature `diagnostics`): the crash hook and the
//! daily-rolling `tracing` file logs, under one app data root.
//!
//! ```text
//! <data_root>/crashes/                       redacted crash reports (rightkit-crash)
//! <data_root>/logs/<slug>-<kind>.YYYY-MM-DD.jsonl   daily JSONL tracing logs (rightkit-logs)
//! ```
//!
//! [`init_diagnostics`] installs the panic hook first, so a panic while the log writer starts is
//! still recorded, then the global `tracing` subscriber. Every recorded crash report is also
//! written to the log as one `error` event (target `rightkit_crash`) carrying the redacted kind,
//! process kind, location and message (`crash_message`), so the log shows where the process stopped. A log
//! setup failure never fails startup: the crash hook stays installed, one line goes to stderr,
//! and [`Diagnostics::log_error`] says why.

use crate::{install_panic_hook, CrashConfig, ProcessKind};
use rightkit_logs::{install_tracing, TracingFileConfig, TracingGuard};
use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};

/// Folder under the data root that receives crash reports.
pub const CRASH_SUBDIR: &str = "crashes";
/// Folder under the data root that receives the daily tracing logs.
pub const LOG_SUBDIR: &str = "logs";

/// Settings for [`init_diagnostics`]. [`DiagnosticsConfig::new`] derives both halves from one
/// data root; every field stays public so an app can adjust either half before installing.
#[derive(Debug, Clone)]
pub struct DiagnosticsConfig {
    /// The crash half. `crash_dir` is `<data_root>/crashes` and `process_kind` is set.
    pub crash: CrashConfig,
    /// The log half. `dir` is `<data_root>/logs` and `file_prefix` is `<app slug>-<kind>`, so
    /// the window host and the engine of one app write separate daily families.
    pub logs: TracingFileConfig,
    /// Write each recorded crash report to the log as one `error` event (default `true`). Any
    /// sink already on `crash` still runs first.
    pub log_crashes: bool,
}

impl DiagnosticsConfig {
    /// Derive both halves for one process boundary of `app_name` under `data_root`.
    pub fn new(
        kind: ProcessKind,
        app_name: impl Into<String>,
        app_version: impl Into<String>,
        data_root: impl AsRef<Path>,
    ) -> Self {
        let app_name = app_name.into();
        let root = data_root.as_ref();
        let prefix = log_file_prefix(&app_name, kind);
        Self {
            crash: CrashConfig::new(app_name, app_version, root.join(CRASH_SUBDIR))
                .process_kind(kind),
            logs: TracingFileConfig::new(root.join(LOG_SUBDIR), prefix),
            log_crashes: true,
        }
    }
}

/// What [`init_diagnostics`] installed. Hold it for the life of the process: dropping it flushes
/// and stops the log writer (the crash hook stays installed).
#[must_use = "dropping Diagnostics flushes and stops the log writer"]
pub struct Diagnostics {
    crash: CrashConfig,
    log_dir: PathBuf,
    tracing: Option<TracingGuard>,
    log_error: Option<io::Error>,
}

impl Diagnostics {
    /// The crash configuration the hook was installed with, for `record_report`,
    /// `record_helper_exit`, `HangWatchdog::start` or a support bundle.
    pub fn crash_config(&self) -> &CrashConfig {
        &self.crash
    }

    /// Where crash reports are written.
    pub fn crash_dir(&self) -> &Path {
        &self.crash.crash_dir
    }

    /// Where the daily log files are written.
    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    /// The log writer, when it started: `status`, `dropped_events` and `clear` go through it.
    pub fn tracing(&self) -> Option<&TracingGuard> {
        self.tracing.as_ref()
    }

    /// Why the log writer did not start (invalid prefix or filter, unusable directory, or a
    /// global subscriber already installed); `None` when it is running.
    pub fn log_error(&self) -> Option<&io::Error> {
        self.log_error.as_ref()
    }
}

/// Install the crash hook, then the daily-rolling `tracing` file logs, for one process. Call
/// once, early in `main`, after the app has decided its data root. Never fails: a log setup
/// failure is reported on stderr and through [`Diagnostics::log_error`], and crash capture
/// keeps working.
///
/// Under `panic = "abort"` the crash's log event is queued but may not reach the file before the
/// process ends; the crash report file is written synchronously either way.
pub fn init_diagnostics(config: DiagnosticsConfig) -> Diagnostics {
    let DiagnosticsConfig {
        crash,
        logs,
        log_crashes,
    } = config;
    let hook_config = if log_crashes {
        crash.clone().then_sink(log_crash_report)
    } else {
        crash.clone()
    };
    install_panic_hook(hook_config);
    let log_dir = logs.dir.clone();
    let (tracing, log_error) = match install_tracing(logs) {
        Ok(guard) => (Some(guard), None),
        Err(error) => {
            eprintln!(
                "{}: file logging is off ({error}); crash reports are still recorded.",
                crash.app_name
            );
            (None, Some(error))
        }
    };
    Diagnostics {
        crash,
        log_dir,
        tracing,
        log_error,
    }
}

/// One `error` event per recorded crash report. The report is already redacted.
fn log_crash_report(report: &Value) {
    let field = |name: &str| report.get(name).and_then(Value::as_str).unwrap_or("");
    tracing::error!(
        target: "rightkit_crash",
        kind = field("kind"),
        process_kind = field("process_kind"),
        location = field("location"),
        crash_message = field("message"),
        "crash report recorded"
    );
}

/// `<app slug>-<kind>`: lowercase ASCII letters and digits, runs of anything else become one
/// `-`. An app name with no usable character becomes `app`.
pub(crate) fn log_file_prefix(app_name: &str, kind: ProcessKind) -> String {
    let mut slug = String::with_capacity(app_name.len());
    for ch in app_name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        slug.push_str("app");
    }
    format!("{slug}-{}", kind.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_prefix_is_a_lowercase_slug_of_the_app_name_and_the_process_kind() {
        assert_eq!(
            log_file_prefix("RightApp", ProcessKind::Window),
            "rightapp-window"
        );
        assert_eq!(
            log_file_prefix("My App 2!", ProcessKind::Engine),
            "my-app-2-engine"
        );
        assert_eq!(
            log_file_prefix("  /..\\ ", ProcessKind::Helper),
            "app-helper"
        );
        assert_eq!(
            log_file_prefix("Café/Ünï", ProcessKind::Window),
            "caf-n-window"
        );
    }

    #[test]
    fn new_derives_both_halves_from_one_data_root() {
        let root = Path::new("data-root");
        let config = DiagnosticsConfig::new(ProcessKind::Engine, "RightApp", "1.2.3", root);
        assert_eq!(config.crash.crash_dir, root.join("crashes"));
        assert_eq!(config.crash.app_name, "RightApp");
        assert_eq!(config.crash.app_version, "1.2.3");
        assert_eq!(config.crash.declared_kind(), Some(ProcessKind::Engine));
        assert_eq!(config.logs.dir, root.join("logs"));
        assert_eq!(config.logs.file_prefix, "rightapp-engine");
        assert!(config.logs.json);
        assert!(config.log_crashes);
    }
}
