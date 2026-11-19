//! Request variants, proof types & budgets shared by the public API and COM worker.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::supervisor::EffectGate;

/// Native identity of the destination window, taken from the host's own
/// target snapshot. The host keeps its snapshot type; only these three
/// facts reach the worker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeTarget {
    pub process_id: Option<u32>,
    pub window_handle: Option<isize>,
    pub focused_control_handle: Option<isize>,
}

/// Decisions the host owns and the worker only applies. Each is a pure
/// function; none is given a default so a host cannot forget one and
/// silently weaken a fail-closed check.
#[derive(Clone, Copy)]
pub struct Rules {
    /// Canonical line-break form both sides of a readback are compared under.
    pub collapse_line_break_runs: fn(&str) -> String,
    /// `(before, after, needle)`: is the added text attributable to this insertion.
    pub insertion_attributable: fn(&str, &str, &str) -> bool,
    /// Same, for a terminal grid where whitespace and box-drawing are squashed.
    pub terminal_grid_insertion_attributable: fn(&str, &str, &str) -> bool,
    /// `(is_password, keyboard_focusable, writable_value, text_edit)`: may a
    /// web-area descendant be bound as the editor.
    pub web_area_editor_is_bindable: fn(Option<bool>, bool, bool, bool) -> bool,
}

/// Deadlines and bounds. `Config::new` fills the numeric values with the
/// values the worker shipped with; the host overrides any field.
#[derive(Clone)]
pub struct Config {
    /// Deadline for one read-only request.
    pub request_timeout: Duration,
    /// Largest accepted UIA text length in UTF-16 units; larger is a rejection.
    pub max_proof_utf16_units: usize,
    /// Largest accepted proof text in UTF-8 bytes across one transaction.
    pub max_proof_utf8_bytes: usize,
    /// Retained stop elements kept at once; the oldest is dropped first.
    pub max_retained_stop_targets: usize,
    /// Retained stop elements older than this are dropped (at request time and
    /// by an idle wake-up, so an idle worker does not keep them forever).
    pub retained_stop_target_ttl: Duration,
    /// One retry of a definitive-negative editable / unknown password probe.
    pub transient_retry_delay: Duration,
    /// Wall-clock cap on the web-area ancestor walk.
    pub web_walk_budget: Duration,
    /// Hop cap on the web-area ancestor walk.
    pub web_walk_max_hops: usize,
    /// Hop cap on the window-membership walk (`worker.rs::window_membership`):
    /// the most parent steps taken from the element to its nearest HWND-backed
    /// ancestor before the proof is `Unproven` (`membership_depth_cap`). Separate
    /// from `web_walk_max_hops` because the two walks answer different questions
    /// and a chromium tree needs a longer climb to its render-widget HWND than the
    /// web-area walk needs to a Document. Clamped to 1..=256 by `validated`;
    /// default 64 (unmeasured: chosen above the ~10 to 30 ControlView levels
    /// expected for a Chromium or Electron editor, with room for nested web apps;
    /// to be measured on installed browsers, journey J19). The walk is still
    /// bounded in time by `web_walk_budget` and the request deadline.
    pub membership_max_hops: usize,
    /// Row cap for terminal accessibility rows.
    pub terminal_max_rows: usize,
    /// Abandoned workers (threads still blocked in a provider call) tolerated
    /// at once. At the cap, calls fail fast and no thread is spawned.
    pub max_abandoned_workers: usize,
    /// How long past its deadline an in-flight request may run before its
    /// worker is declared hung and abandoned.
    pub abandon_grace: Duration,
    /// How long a new worker may spend initialising (COM apartment, UIA
    /// instance, provider timeouts) before it serves its first request. Past
    /// this plus `abandon_grace` it is abandoned exactly like a worker stuck in
    /// a request, counts against `max_abandoned_workers`, and is replaced.
    pub worker_init_timeout: Duration,
    /// Deadline of one release unit: a `ReleaseStopTarget` request, and each
    /// prune / exit drop of retained elements. A COM `Release` into a hung
    /// provider can block like any other call, so it runs inside a supervised
    /// window of this length; past it plus `abandon_grace` the worker is
    /// abandoned like a worker hung in a request. Also the queue-expiry of a
    /// release request (an expired one is skipped; TTL / cap pruning releases
    /// the element later).
    pub release_timeout: Duration,
    /// Most COM references the whole process may hold leaked at once (elements
    /// deliberately never released because their worker was abandoned for a hang,
    /// or a release window could not be opened; see `pins::LeakLedger`). A clean
    /// close or a normal exit releases its pins and leaks nothing. Each
    /// abandonment leaks at most `max_retained_stop_targets` pins, so the default
    /// of 64 allows at least eight abandonments with full stores. At the limit new
    /// stop pins are refused (`capture_stop_target` returns `None`, event
    /// `uia_pins_suspended`) until the process restarts: the count is
    /// process-wide, only ever grows, and leaked references are never reclaimed.
    pub max_forgotten_pins: usize,
    /// UIA connection timeout (`IUIAutomation2::SetConnectionTimeout`) applied
    /// to each worker's own UIA instance; zero leaves the system default.
    pub provider_connection_timeout: Duration,
    /// UIA transaction timeout (`IUIAutomation2::SetTransactionTimeout`),
    /// same scope; zero leaves the system default (20 s).
    pub provider_transaction_timeout: Duration,
    /// Receives `(event, detail)` for worker lifecycle events:
    /// `uia_worker_abandoned` (detail includes `phase=init|request`),
    /// `uia_worker_exhausted`, `uia_worker_recovered`,
    /// `uia_worker_shutdown`, `uia_worker_init_failed`, `uia_provider_timeouts`,
    /// `uia_pins_suspended`,
    /// and `uia_effect_not_issued` / `uia_effect_target_changed` /
    /// `uia_effect_issued_unconfirmed` for every externally visible side effect
    /// (`kind=focus reason=...`; `uia_effect_target_changed` is the
    /// window-membership proof and the exact-window re-check:
    /// `unbound|window_gone|owner_changed|process_replaced|member_unproven|member_root_changed|not_foreground`
    /// from the final re-check, `deadline|not_foreground|process_replaced|element_pid_changed|element_replaced|focus_precheck_refused`
    /// from the focus path's first reads (focus only), and `element_outside_window|membership_unbound|membership_deadline|membership_depth_cap|membership_parent_unreadable|membership_no_window_ancestor|membership_handle_unreadable|membership_root_unreadable`
    /// from the proof).
    /// Details are counts and
    /// millisecond durations only, never proof text.
    pub event_sink: Option<fn(&str, &str)>,
    pub rules: Rules,
}

impl Config {
    /// `release_timeout` with a 50 ms floor so a zero cannot make every release
    /// window expire before it opens.
    pub(crate) fn release_budget(&self) -> Duration {
        self.release_timeout.max(Duration::from_millis(50))
    }

    /// The configuration with every host value clamped to a range the worker
    /// can use without panicking or misbehaving: no `Duration` that overflows an
    /// `Instant`, no zero count that refuses every call. `Uia::new` applies it.
    /// A zero `request_timeout` is kept (calls are then refused without
    /// queueing, as documented); a zero provider timeout is kept (system default).
    pub fn validated(mut self) -> Self {
        let max = Duration::from_secs(60);
        let floor = Duration::from_millis(50);
        self.request_timeout = self.request_timeout.min(max);
        self.max_proof_utf16_units = self.max_proof_utf16_units.clamp(1, 1 << 20);
        self.max_proof_utf8_bytes = self.max_proof_utf8_bytes.clamp(1, 4 << 20);
        self.max_retained_stop_targets = self.max_retained_stop_targets.clamp(1, 64);
        self.retained_stop_target_ttl = self
            .retained_stop_target_ttl
            .clamp(Duration::from_secs(1), Duration::from_secs(7 * 24 * 3600));
        self.transient_retry_delay = self.transient_retry_delay.min(Duration::from_secs(5));
        self.web_walk_budget = self.web_walk_budget.min(max);
        self.web_walk_max_hops = self.web_walk_max_hops.clamp(1, 1024);
        self.membership_max_hops = self.membership_max_hops.clamp(1, 256);
        self.terminal_max_rows = self.terminal_max_rows.clamp(1, 4096);
        self.max_abandoned_workers = self.max_abandoned_workers.clamp(1, 64);
        self.abandon_grace = self.abandon_grace.min(max);
        self.worker_init_timeout = self.worker_init_timeout.clamp(floor, max);
        self.release_timeout = self.release_timeout.clamp(floor, max);
        self.max_forgotten_pins = self.max_forgotten_pins.clamp(1, 4096);
        self.provider_connection_timeout = self.provider_connection_timeout.min(max);
        self.provider_transaction_timeout = self.provider_transaction_timeout.min(max);
        self
    }

    pub fn new(rules: Rules) -> Self {
        Self {
            request_timeout: Duration::from_millis(600),
            max_proof_utf16_units: 32_768,
            max_proof_utf8_bytes: 32_768,
            max_retained_stop_targets: 8,
            retained_stop_target_ttl: Duration::from_secs(300),
            transient_retry_delay: Duration::from_millis(140),
            web_walk_budget: Duration::from_millis(400),
            web_walk_max_hops: 40,
            membership_max_hops: 64,
            terminal_max_rows: 256,
            max_abandoned_workers: 3,
            abandon_grace: Duration::from_millis(200),
            worker_init_timeout: Duration::from_millis(1_000),
            release_timeout: Duration::from_millis(500),
            max_forgotten_pins: 64,
            provider_connection_timeout: Duration::from_millis(500),
            provider_transaction_timeout: Duration::from_millis(1_000),
            event_sink: None,
            rules,
        }
    }
}

/// Per-handle ids for pin tokens (`supervisor::mint_handle_id`).
pub(crate) static NEXT_HANDLE_ID: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusedElementIdentity {
    pub process_id: i32,
    pub native_window_handle: isize,
    pub runtime_id: Vec<i32>,
}

/// Independent UIA text representations captured before paste. Each side is
/// bounded so one proof transaction retains at most 64 KiB across before/after
/// reads, and confirmation compares only matching representations.
#[derive(Clone, Debug)]
pub struct TextProof {
    pub value: Option<String>,
    pub document: Option<String>,
    /// Accessibility rows exposed by VS Code's integrated terminal when
    /// screen-reader mode is enabled. Kept separate from editor text so a
    /// proof cannot accidentally compare different UIA representations.
    pub terminal_rows: Option<String>,
    pub terminal_scope_runtime_id: Option<Vec<i32>>,
    pub terminal_scope_native_window_handle: Option<isize>,
}

impl TextProof {
    /// Pick one complete representation consistently. Prefer nonempty Value /
    /// Document text so an empty placeholder cannot mask real content; retain
    /// an explicitly readable empty representation for ConfirmedEmpty.
    pub fn authoritative_text(&self) -> Option<&str> {
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

    pub fn confirms(&self, after: &Self, needle: &str, rules: &Rules) -> bool {
        let collapse = rules.collapse_line_break_runs;
        let insertion_attributable = rules.insertion_attributable;
        let terminal_grid_insertion_attributable = rules.terminal_grid_insertion_attributable;
        let needle = collapse(needle);
        self.value
            .as_deref()
            .zip(after.value.as_deref())
            .is_some_and(|(before, after)| {
                insertion_attributable(&collapse(before), &collapse(after), &needle)
            })
            || self
                .document
                .as_deref()
                .zip(after.document.as_deref())
                .is_some_and(|(before, after)| {
                    insertion_attributable(&collapse(before), &collapse(after), &needle)
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
                            && terminal_grid_insertion_attributable(
                                &collapse(before),
                                &collapse(after),
                                &needle,
                            )
                    },
                )
    }
}

/// Destination captured when recording stops. The UIA identity is kept out of
/// the host's persisted target snapshot (`target`, whatever type the host
/// uses) so delivery can bind to a virtual editor even when its Win32 child
/// HWND is unstable.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct StopTarget<T> {
    pub target: T,
    /// Foreground window the capture was proven against.
    pub window_handle: isize,
    pub process_creation_time: u64,
    pub identity: FocusedElementIdentity,
    pub focused_text_input: Option<bool>,
    pub is_password: Option<bool>,
    /// Opaque pin token. It encodes the worker generation that captured the
    /// element; on any other generation every pin call fails closed without
    /// reaching a worker. Never zero. Treat as opaque (do not parse or forge).
    pub element_token: u64,
}

/// The fields of a `StopTarget` the worker reads; carries no host type, so
/// the request channel stays non-generic.
#[derive(Clone, Debug)]
pub(crate) struct StopRef {
    pub(crate) window_handle: isize,
    pub(crate) process_creation_time: u64,
    pub(crate) identity: FocusedElementIdentity,
    pub(crate) element_token: u64,
}

impl<T> StopTarget<T> {
    pub(crate) fn stop_ref(&self) -> StopRef {
        StopRef {
            window_handle: self.window_handle,
            process_creation_time: self.process_creation_time,
            identity: self.identity.clone(),
            element_token: self.element_token,
        }
    }
}

/// Outcome of `Uia::restore_stop_target_outcome`. A focus restore is an
/// externally visible side effect, so "did not happen" and "may have happened"
/// are different answers and are never merged into one `false`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    /// Focus was issued and the stop element is verifiably focused again.
    Restored,
    /// Focus was NOT issued: a precondition failed, the pin was stale, the
    /// pinned window closed or was replaced during a slow provider read, the
    /// request was refused, expired, abandoned or never queued. Nothing in the
    /// target application or the user's focus was changed by this call.
    NotIssued,
    /// Focus WAS issued (the call was made, or may still be running on an
    /// abandoned worker) but a verified restore did not result: the call
    /// failed, validation failed, or the deadline passed first. The caller must
    /// treat this as a possible focus change.
    IssuedUnconfirmed,
}

impl RestoreOutcome {
    /// True only for a verified restore.
    pub fn restored(self) -> bool {
        self == RestoreOutcome::Restored
    }

    /// True when the user's focus may have moved (issued, not verified).
    pub fn focus_may_have_changed(self) -> bool {
        self == RestoreOutcome::IssuedUnconfirmed
    }
}

/// What the worker proves at capture; the host's target is attached afterwards.
pub(crate) struct CapturedStop {
    pub(crate) window_handle: isize,
    pub(crate) process_creation_time: u64,
    pub(crate) identity: FocusedElementIdentity,
    pub(crate) focused_text_input: Option<bool>,
    pub(crate) is_password: Option<bool>,
    pub(crate) element_token: u64,
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
    Identity {
        deadline: Instant,
        process_id: i32,
        control_handle: isize,
        window_handle: isize,
        reply: mpsc::SyncSender<Option<FocusedElementIdentity>>,
    },
    ReadText {
        deadline: Instant,
        target: NativeTarget,
        expected: FocusedElementIdentity,
        reply: mpsc::SyncSender<Option<TextProof>>,
    },
    ReadStopText {
        deadline: Instant,
        stop: StopRef,
        reply: mpsc::SyncSender<Option<TextProof>>,
    },
    CaptureStopTarget {
        deadline: Instant,
        target: NativeTarget,
        reply: mpsc::SyncSender<Option<CapturedStop>>,
    },
    ValidateStopTarget {
        deadline: Instant,
        stop: StopRef,
        reply: mpsc::SyncSender<bool>,
    },
    StopTargetIsGone {
        deadline: Instant,
        stop: StopRef,
        reply: mpsc::SyncSender<Option<bool>>,
    },
    RestoreStopTarget {
        deadline: Instant,
        stop: StopRef,
        gate: EffectGate,
        reply: mpsc::SyncSender<RestoreOutcome>,
    },
    /// Does the hotkey proof's control name a web-area/document ancestor of the
    /// retained stop element (bounded walk, non-secure, editable)?
    WebContainerNamed {
        deadline: Instant,
        stop: StopRef,
        hotkey_control: isize,
        window_handle: isize,
        reply: mpsc::SyncSender<bool>,
    },
    /// Releases a COM pointer, which can block in a hung provider. `deadline`
    /// is send time plus `Config::release_timeout` and is informational: the
    /// worker ALWAYS honours a release (a release queued past it is not skipped)
    /// and runs the drop in its own supervised window (`run_release_unit`).
    /// There is no reply: a release is fire-and-forget.
    ReleaseStopTarget { deadline: Instant, token: u64 },
}

impl Request {
    /// Deadline of the request. Total on purpose: there is no deadline-less
    /// variant (adding one is a compile error here), because every unit of work
    /// the worker runs must be one the supervisor can abandon.
    pub(crate) fn deadline(&self) -> Instant {
        match self {
            Request::Editable { deadline, .. }
            | Request::Password { deadline, .. }
            | Request::Identity { deadline, .. }
            | Request::ReadText { deadline, .. }
            | Request::ReadStopText { deadline, .. }
            | Request::CaptureStopTarget { deadline, .. }
            | Request::ValidateStopTarget { deadline, .. }
            | Request::StopTargetIsGone { deadline, .. }
            | Request::RestoreStopTarget { deadline, .. }
            | Request::WebContainerNamed { deadline, .. }
            | Request::ReleaseStopTarget { deadline, .. } => *deadline,
        }
    }
}
