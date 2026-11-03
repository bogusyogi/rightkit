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

const OMITTED_MESSAGE: &str = "[oversized window message omitted]";
const OMITTED_STACK: &str = "[oversized window stack omitted]";
const OMITTED_SOURCE: &str = "[oversized window source omitted]";

// Never truncate before scrubbing: a partial credential may evade both pattern & known-value
// redaction. Inspect only enough input to reject an oversized field, then scrub accepted input.
fn bounded_message(text: &str) -> &str {
    if text.chars().nth(MAX_WINDOW_MESSAGE_CHARS).is_some() {
        OMITTED_MESSAGE
    } else {
        text
    }
}

fn bounded_stack(text: &str) -> &str {
    if text.len() > MAX_WINDOW_STACK_BYTES {
        OMITTED_STACK
    } else {
        text
    }
}

fn bounded_source(text: &str) -> &str {
    for (count, ch) in text.chars().take(MAX_SOURCE_CHARS + 1).enumerate() {
        if ch == '?' || ch == '#' {
            // Removing a suffix could expose a fragment of a configured secret.
            return OMITTED_SOURCE;
        }
        if count == MAX_SOURCE_CHARS {
            return OMITTED_SOURCE;
        }
    }
    text
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
    let message = bounded_message(&report.message);
    let stack = bounded_stack(&report.stack);
    let source = report.source.as_deref().map(bounded_source);
    let mut config = config.clone().process_kind(ProcessKind::Window);
    // A JS error is not a process crash: never take a native sample/minidump per renderer error.
    config.native_capture = crate::NativeCapture::Off;
    record_report(&config, kind, message, source, "webview", stack)
}
