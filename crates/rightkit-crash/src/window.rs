//! Renderer and webview error capture (opt-in feature `window-errors`).
//!
//! The JavaScript side (`@rightkit/logs` `installWindowCrashCapture`) scrubs and rate-limits
//! `window.onerror` and `unhandledrejection` events and sends them to an app-registered Tauri
//! command. That command calls [`record_window_error`], which records a [`crate::CrashReport`]
//! with `process_kind = window` through [`crate::record_report`], so the owner scrub, known-secret
//! redaction, retention and sink all apply. The Rust side bounds and rate-limits again because
//! the webview is not a trusted caller. Nothing is recorded unless the app wires the command.

use crate::hook::{record_report, CrashConfig, ProcessKind};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Largest message accepted from the webview, in characters.
pub const MAX_WINDOW_MESSAGE_CHARS: usize = 1_000;
/// Largest stack accepted from the webview, in bytes.
pub const MAX_WINDOW_STACK_BYTES: usize = 8 * 1024;
const MAX_SOURCE_CHARS: usize = 120;

/// One renderer error as sent by the webview. Untrusted: every field is bounded and scrubbed.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WindowErrorReport {
    /// `error` or `unhandledrejection`; anything else is recorded as `error`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub stack: String,
    /// Script file name and position, if the browser gave one. Never a full URL with a query.
    #[serde(default)]
    pub source: Option<String>,
}

/// Rate limit for webview reports: at most `max_per_window` in any `window`, and `max_total` for
/// the life of the gate (the process). Share one gate for the whole app.
#[derive(Debug)]
pub struct WindowErrorGate {
    window: Duration,
    max_per_window: u32,
    max_total: u32,
    state: Mutex<GateState>,
}

#[derive(Debug)]
struct GateState {
    started: Option<Instant>,
    in_window: u32,
    total: u32,
}

impl Default for WindowErrorGate {
    fn default() -> Self {
        Self::new(Duration::from_secs(60), 5, 25)
    }
}

impl WindowErrorGate {
    pub fn new(window: Duration, max_per_window: u32, max_total: u32) -> Self {
        Self {
            window,
            max_per_window,
            max_total,
            state: Mutex::new(GateState {
                started: None,
                in_window: 0,
                total: 0,
            }),
        }
    }

    /// Whether one more report may be recorded now. Counts it when it may.
    pub fn allow(&self) -> bool {
        self.allow_at(Instant::now())
    }

    pub(crate) fn allow_at(&self, now: Instant) -> bool {
        let mut s = match self.state.lock() {
            Ok(s) => s,
            Err(poisoned) => poisoned.into_inner(),
        };
        if s.total >= self.max_total {
            return false;
        }
        match s.started {
            Some(start) if now.saturating_duration_since(start) < self.window => {}
            _ => {
                s.started = Some(now);
                s.in_window = 0;
            }
        }
        if s.in_window >= self.max_per_window {
            return false;
        }
        s.in_window += 1;
        s.total += 1;
        true
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn truncate_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text[..cut].to_string()
}

/// Record one renderer error as a window crash report. Returns the report path, or `None` when
/// the gate refused it, recording is off, or nothing could be written. The report kind is
/// `window_error` or `window_rejection`; `process_kind` is always `window`.
pub fn record_window_error(
    config: &CrashConfig,
    gate: &WindowErrorGate,
    report: &WindowErrorReport,
) -> Option<PathBuf> {
    if !gate.allow() {
        return None;
    }
    let kind = if report.kind == "unhandledrejection" {
        "window_rejection"
    } else {
        "window_error"
    };
    let message = truncate_chars(&report.message, MAX_WINDOW_MESSAGE_CHARS);
    let stack = truncate_bytes(&report.stack, MAX_WINDOW_STACK_BYTES);
    let source = report
        .source
        .as_deref()
        .map(|s| truncate_chars(s.split(['?', '#']).next().unwrap_or(""), MAX_SOURCE_CHARS));
    let config = config.clone().process_kind(ProcessKind::Window);
    record_report(
        &config,
        kind,
        &message,
        source.as_deref(),
        "webview",
        &stack,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::CrashConfig;
    use std::sync::{Arc, Mutex};

    fn config_with_sink(out: Arc<Mutex<Vec<serde_json::Value>>>) -> CrashConfig {
        CrashConfig::new("t", "1.0.0", std::env::temp_dir())
            .write_report_file(false)
            .with_sink(move |v| out.lock().unwrap().push(v))
    }

    #[test]
    fn gate_limits_per_window_and_total() {
        let gate = WindowErrorGate::new(Duration::from_secs(10), 2, 3);
        let t0 = Instant::now();
        assert!(gate.allow_at(t0));
        assert!(gate.allow_at(t0));
        assert!(!gate.allow_at(t0 + Duration::from_secs(1)));
        assert!(gate.allow_at(t0 + Duration::from_secs(11)));
        assert!(!gate.allow_at(t0 + Duration::from_secs(30)), "total cap");
    }

    #[test]
    fn records_window_report_scrubbed_and_capped() {
        let out = Arc::new(Mutex::new(Vec::new()));
        let config = config_with_sink(out.clone()).redact_values(vec!["hunter2-secret".into()]);
        let gate = WindowErrorGate::default();
        let report = WindowErrorReport {
            kind: "unhandledrejection".into(),
            message: format!("bad hunter2-secret {}", "x".repeat(5_000)),
            stack: "y".repeat(100_000),
            source: Some("app://index.js?token=abc#frag".into()),
        };
        record_window_error(&config, &gate, &report);
        let got = out.lock().unwrap();
        assert_eq!(got.len(), 1);
        let v = &got[0];
        assert_eq!(v["kind"], "window_rejection");
        assert_eq!(v["process_kind"], "window");
        assert_eq!(v["location"], "app://index.js");
        let message = v["message"].as_str().unwrap();
        assert!(!message.contains("hunter2-secret"));
        assert!(message.chars().count() <= MAX_WINDOW_MESSAGE_CHARS);
        assert!(v["backtrace"].as_str().unwrap().len() <= MAX_WINDOW_STACK_BYTES + 16);
    }

    #[test]
    fn refused_report_writes_nothing() {
        let out = Arc::new(Mutex::new(Vec::new()));
        let config = config_with_sink(out.clone());
        let gate = WindowErrorGate::new(Duration::from_secs(60), 1, 10);
        let report = WindowErrorReport::default();
        record_window_error(&config, &gate, &report);
        record_window_error(&config, &gate, &report);
        assert_eq!(
            out.lock().unwrap().len(),
            1,
            "second report is rate limited"
        );
    }

    #[test]
    fn unknown_kind_is_window_error() {
        let out = Arc::new(Mutex::new(Vec::new()));
        let config = config_with_sink(out.clone());
        let report = WindowErrorReport {
            kind: "../../etc".into(),
            ..Default::default()
        };
        record_window_error(&config, &WindowErrorGate::default(), &report);
        assert_eq!(out.lock().unwrap()[0]["kind"], "window_error");
    }
}
