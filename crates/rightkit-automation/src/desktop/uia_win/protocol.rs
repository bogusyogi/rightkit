//! Request protocol and shared state between the public API and the COM
//! worker: request variants, proof types, budgets, and circuit-breaker
//! statics.

use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::win::uia::El;

use rightkit_transcript_library::delivery::TargetSnapshot;

pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_millis(600);
pub(crate) const MAX_PROOF_UTF16_UNITS: usize = 32_768;
pub(crate) const MAX_PROOF_UTF8_BYTES: usize = 32_768;

pub(crate) static SET_VALUE_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
pub(crate) static SET_VALUE_CIRCUIT_OPEN: AtomicBool = AtomicBool::new(false);
pub(crate) static NEXT_STOP_TOKEN: AtomicU64 = AtomicU64::new(1);

pub(crate) const MAX_RETAINED_STOP_TARGETS: usize = 8;
pub(crate) const RETAINED_STOP_TARGET_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusedElementIdentity {
    pub(crate) process_id: i32,
    pub(crate) native_window_handle: isize,
    pub(crate) runtime_id: Vec<i32>,
}

/// Independent UIA text representations captured before paste. Each side is
/// bounded so one proof transaction retains at most 64 KiB across before/after
/// reads, and confirmation compares only matching representations.
#[derive(Clone, Debug)]
pub struct TextProof {
    pub(crate) value: Option<String>,
    pub(crate) document: Option<String>,
    /// Accessibility rows exposed by VS Code's integrated terminal when
    /// screen-reader mode is enabled. Kept separate from editor text so a
    /// proof cannot accidentally compare different UIA representations.
    pub(crate) terminal_rows: Option<String>,
    pub(crate) terminal_scope_runtime_id: Option<Vec<i32>>,
    pub(crate) terminal_scope_native_window_handle: Option<isize>,
}

impl TextProof {
    /// Pick one complete representation consistently. Prefer nonempty Value /
    /// Document text so an empty placeholder cannot mask real content; retain
    /// an explicitly readable empty representation for ConfirmedEmpty.
    pub(crate) fn authoritative_text(&self) -> Option<&str> {
        self.value
            .as_deref()
            .filter(|text| !text.is_empty())
            .or_else(|| self.document.as_deref().filter(|text| !text.is_empty()))
            .or_else(|| {
                self.terminal_rows
                    .as_deref()
                    .filter(|text| !text.is_empty())
            })
            .or(self.value.as_deref())
            .or(self.document.as_deref())
            .or(self.terminal_rows.as_deref())
    }

    pub(crate) fn confirms(&self, after: &Self, needle: &str) -> bool {
        let needle = crate::desktop::collapse_line_break_runs(needle);
        self.value
            .as_deref()
            .zip(after.value.as_deref())
            .is_some_and(|(before, after)| {
                rightkit_transcript_library::delivery::insertion_attributable(
                    &crate::desktop::collapse_line_break_runs(before),
                    &crate::desktop::collapse_line_break_runs(after),
                    &needle,
                )
            })
            || self
                .document
                .as_deref()
                .zip(after.document.as_deref())
                .is_some_and(|(before, after)| {
                    rightkit_transcript_library::delivery::insertion_attributable(
                        &crate::desktop::collapse_line_break_runs(before),
                        &crate::desktop::collapse_line_break_runs(after),
                        &needle,
                    )
                })
            || self
                .terminal_rows
                .as_deref()
                .zip(after.terminal_rows.as_deref())
                .zip(
                    self.terminal_scope_runtime_id
                        .as_ref()
                        .zip(after.terminal_scope_runtime_id.as_ref()),
                )
                .zip(
                    self.terminal_scope_native_window_handle
                        .zip(after.terminal_scope_native_window_handle),
                )
                .is_some_and(
                    |(((before, after), (before_id, after_id)), (before_hwnd, after_hwnd))| {
                        before_id == after_id
                            && before_hwnd == after_hwnd
                            && rightkit_transcript_library::delivery::terminal_grid_insertion_attributable(
                                &crate::desktop::collapse_line_break_runs(before),
                                &crate::desktop::collapse_line_break_runs(after),
                                &needle,
                            )
                    },
                )
    }
}

/// Destination captured when recording stops. The UIA identity is kept out of
/// `TargetSnapshot` (which is persisted in history) so delivery can bind to a
/// virtual editor even when its Win32 child HWND is unstable.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct StopTarget {
    pub(crate) target: TargetSnapshot,
    pub(crate) process_creation_time: u64,
    pub(crate) identity: FocusedElementIdentity,
    pub(crate) focused_text_input: Option<bool>,
    pub(crate) is_password: Option<bool>,
    pub(crate) element_token: u64,
}

pub(crate) struct RetainedStopTarget {
    pub(crate) element: El,
    pub(crate) captured_at: Instant,
}

pub(crate) enum Request {
    Editable {
        deadline: Instant,
        reply: mpsc::SyncSender<Option<bool>>,
    },
    Password {
        deadline: Instant,
        reply: mpsc::SyncSender<Option<bool>>,
    },
    // Dormant (native insertion is gated off) but pinned by contract tests.
    #[allow(dead_code)]
    NativeCapability {
        deadline: Instant,
        reply: mpsc::SyncSender<Option<bool>>,
    },
    Identity {
        deadline: Instant,
        process_id: i32,
        control_handle: isize,
        window_handle: isize,
        reply: mpsc::SyncSender<Option<FocusedElementIdentity>>,
    },
    ReadText {
        deadline: Instant,
        target: TargetSnapshot,
        expected: FocusedElementIdentity,
        reply: mpsc::SyncSender<Option<TextProof>>,
    },
    ReadStopText {
        deadline: Instant,
        stop: StopTarget,
        reply: mpsc::SyncSender<Option<TextProof>>,
    },
    // Dormant (native insertion is gated off) but pinned by contract tests.
    #[allow(dead_code)]
    SetValue {
        deadline: Instant,
        text: String,
        target: TargetSnapshot,
        expected: FocusedElementIdentity,
        reply: mpsc::SyncSender<Option<bool>>,
    },
    CaptureStopTarget {
        deadline: Instant,
        target: TargetSnapshot,
        reply: mpsc::SyncSender<Option<StopTarget>>,
    },
    ValidateStopTarget {
        deadline: Instant,
        stop: StopTarget,
        reply: mpsc::SyncSender<bool>,
    },
    StopTargetIsGone {
        deadline: Instant,
        stop: StopTarget,
        reply: mpsc::SyncSender<Option<bool>>,
    },
    RestoreStopTarget {
        deadline: Instant,
        stop: StopTarget,
        reply: mpsc::SyncSender<bool>,
    },
    /// Does the hotkey proof's control name a web-area/document ancestor of the
    /// retained stop element (bounded walk, non-secure, editable)?
    WebContainerNamed {
        deadline: Instant,
        stop: StopTarget,
        hotkey_control: isize,
        window_handle: isize,
        reply: mpsc::SyncSender<bool>,
    },
    ReleaseStopTarget {
        token: u64,
        reply: mpsc::SyncSender<()>,
    },
}
