//! Host inputs for paste delivery. Everything that is a choice of the app using
//! this crate (timeouts and budgets, per-app process lists, the event sink, the
//! delivery method, the accessibility reads of the macOS shell) comes in here,
//! so the delivery code carries no product name, no copy and no tuning value.
//!
//! No field has a default on purpose: a host spells out every bound, so a
//! missing one is a compile error and never a silently different timeout.

#[cfg(target_os = "windows")]
use std::path::PathBuf;

use crate::evidence_gate::AppTargetPolicy;

/// What the delivery gate requires of the secure-field (password) check when the target is a
/// terminal pane. The host chooses; there is no default and no "unknown is safe" reading of either
/// value.
///
/// Neither value relaxes the confirmed-pane requirement: a terminal window reached without a bound
/// pane that is re-resolved and `PanePreflight::Current` inside the authorization of the write is
/// refused (`terminal_pane_unverified`) under both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalPaneSecurity {
    /// A pane needs fresh affirmative evidence that it is not a secure field, like any other
    /// target. None exists for a pane, so every automatic write into one is refused and the host
    /// offers Copy.
    RequireEvidence,
    /// The host declares the password-field check not applicable to a terminal pane. A write is
    /// authorized only when the bound pane is re-resolved inside that authorization and is
    /// `PanePreflight::Current` (before the paste chord and again before Return). The decision's
    /// secure input is `SecureEvidence::NotApplicableByHostPolicy`, and every such write logs
    /// `delivery_terminal_pane_policy`. Risk the host accepts: dictating while a terminal shows a
    /// password prompt sends the words to that prompt.
    NotApplicableWhenPaneConfirmed,
}

/// The accepted range of [`DeliveryConfig::uia_membership_max_hops`], the same range the UIA worker
/// clamps to.
pub const UIA_MEMBERSHIP_MAX_HOPS_RANGE: std::ops::RangeInclusive<usize> = 1..=256;

/// Every numeric bound and per-app list of the paste transaction.
///
/// Times are milliseconds. Fields that apply to one OS only are unused on the
/// other; the host still sets them.
#[derive(Debug, Clone, Copy)]
pub struct DeliveryConfig {
    // ---- Paste transaction (both OSes) ----
    /// Budget of one evidence-gate decision before a paste keystroke or Return: the secure-field
    /// read, the fresh focused-control read, and the write that follows. A slower decision
    /// refuses (copy fallback). Must cover the slowest honest probe: on macOS
    /// `secure_retry_deadline_ms` plus an AX read; on Windows the UIA reads are bounded by
    /// `uia_request_timeout_ms` each, so a budget under twice that can refuse on a hung provider
    /// by design.
    pub write_evidence_budget_ms: u64,
    /// Longest wait for the stop hotkey's physical modifiers to lift BEFORE the evidence is
    /// sampled (Windows). Nothing waits after the authorization: the chord and Return are fixed
    /// batches sent in one call. A key still held at the cap refuses the write (copy recovery);
    /// the last modifier sample is part of the authorization.
    pub write_modifier_settle_cap_ms: u64,
    /// Wait for the promised-clipboard receipt (the target requested our data).
    pub paste_receipt_timeout_ms: u64,
    /// Hard cap on the quiet wait after the last receipt before the prior
    /// clipboard is restored.
    pub restore_quiet_cap_ms: u64,
    /// Bound on waiting for the previous delivery's background clipboard restore.
    pub clipboard_restore_wait_ms: u64,
    /// Bound on waiting for readback to show the pasted text before Return.
    pub enter_insertion_settle_ms: u64,
    /// Settle before Return when the editor has no readback channel.
    pub enter_blind_settle_ms: u64,
    /// Poll interval while waiting for readback before Return.
    pub enter_settle_interval_ms: u64,

    // ---- macOS paste transaction ----
    /// Readback deadline when the target saw the key event but gave no receipt.
    pub paste_readback_timeout_ms: u64,
    /// Sleep between readback polls.
    pub readback_retry_interval_ms: u64,
    /// Time added to the receipt timeout for the Cmd+V event probe's own lifetime.
    pub probe_extra_ms: u64,
    pub probe_max_events: usize,
    pub probe_ready_timeout_ms: u64,
    pub probe_max_lifetime_ms: u64,
    pub probe_poll_interval_ms: u64,
    /// Total time the secure-field read may retry.
    pub secure_retry_deadline_ms: u64,
    /// Sleep between secure-field read retries.
    pub secure_retry_interval_ms: u64,
    /// Re-asks of a transient focus/target check before it counts as a change.
    pub settle_max_retries: u8,
    pub settle_retry_interval_ms: u64,

    // ---- Windows paste transaction ----
    /// Receipt/readback race deadline for a terminal target.
    pub terminal_paste_readback_timeout_ms: u64,
    /// Wall-clock cap of the FRESH WezTerm pane check taken at dispatch: once inside the
    /// authorization of the paste chord and once inside the authorization of Return. It sits
    /// inside `write_evidence_budget_ms`, which must cover it plus the other samples. One cold
    /// `wezterm cli` launch measured about 145 ms on the field Dell, and a pane check that runs out
    /// of budget refuses the write (explicit recovery), so this is set well above one launch.
    pub wezterm_dispatch_probe_ms: u64,
    /// A terminal with no readback baseline waits this long for the receipt.
    pub terminal_no_baseline_receipt_grace_ms: u64,
    /// Per-poll budget for post-paste readback on a terminal.
    pub terminal_readback_poll_ms: u64,
    /// Per-poll budget for post-paste readback elsewhere.
    pub readback_poll_ms: u64,
    /// Wait slice of the receipt/readback race loop.
    pub receipt_poll_ms: u64,
    /// Per-poll budget of the readback wait before Return.
    pub enter_settle_poll_budget_ms: u64,
    /// How long an unknown terminal paste waits for a late receipt before the
    /// transcript is left on the clipboard.
    pub late_receipt_wait_ms: u64,
    pub late_receipt_poll_ms: u64,
    /// Budget of one UIA text read used as delivery or context proof.
    pub uia_text_read_ms: u64,
    /// Budget of the WezTerm pin validation.
    pub wezterm_validate_budget_ms: u64,

    // ---- Stop-target capture (macOS) ----
    pub start_capture_budget_ms: u64,
    /// How long to wait for a late hotkey-continuity proof.
    pub start_proof_wait_ms: u64,
    pub start_proof_poll_ms: u64,
    pub start_continuity_wait_ms: u64,
    pub pin_retry_max_attempts: u8,
    pub pin_retry_interval_ms: u64,
    /// Character cap of one editor context read.
    pub editor_read_max_chars: usize,
    pub editor_read_max_attempts: u8,
    pub editor_read_retry_interval_ms: u64,
    /// Bounded re-read of the retained editor's content (at least one attempt).
    pub bound_read_budget_ms: u64,
    pub bound_read_attempts: u32,
    pub bound_read_retry_interval_ms: u64,

    // ---- UIA worker (Windows) ----
    pub uia_request_timeout_ms: u64,
    pub uia_max_proof_utf16_units: usize,
    pub uia_max_proof_utf8_bytes: usize,
    pub uia_max_retained_stop_targets: usize,
    pub uia_retained_stop_target_ttl_ms: u64,
    pub uia_transient_retry_delay_ms: u64,
    pub uia_web_walk_budget_ms: u64,
    pub uia_web_walk_max_hops: usize,
    /// The most parent steps the UIA worker climbs to prove an element belongs to the captured
    /// window before a focus or value effect (`rightkit-automation` `Config::membership_max_hops`).
    /// Valid range [`UIA_MEMBERSHIP_MAX_HOPS_RANGE`] (1 to 256); a value outside it is clamped when
    /// the worker is built and the clamp is logged (`delivery_config_clamped`). No default.
    pub uia_membership_max_hops: usize,
    pub uia_terminal_max_rows: usize,
    pub uia_max_abandoned_workers: usize,
    pub uia_abandon_grace_ms: u64,
    pub uia_provider_connection_timeout_ms: u64,
    pub uia_provider_transaction_timeout_ms: u64,

    // ---- WezTerm (Windows) ----
    pub wezterm_pin_resolve_budget_ms: u64,
    pub wezterm_max_output_bytes: usize,

    // ---- Terminal panes (Windows) ----
    /// The host's terminal-pane secure-field policy (see [`TerminalPaneSecurity`]). No default.
    pub terminal_pane_security: TerminalPaneSecurity,

    // ---- Per-app identifiers (Windows executable names, matched case-insensitively) ----
    /// Apps whose focused-child handle and editability are unstable: delivery
    /// binds to their process and top-level window, and the receipt stays the
    /// mutation gate.
    pub opaque_process_names: &'static [&'static str],
    /// Windows. The host's terminal list: the WezTerm GUI process names. A pane pin is taken instead of a UIA
    /// pin for these. Every entry point that can write identifies a terminal by matching the
    /// foreground window's process against this list (never by the pin's type alone), and refuses a
    /// terminal reached without a bound, re-confirmed pane. The list is matched by process name
    /// (case-insensitive), so a renamed binary is not recognised. Empty means no terminal is
    /// identified, which is the host's explicit choice.
    pub wezterm_gui_process_names: &'static [&'static str],

    // ---- Target classification (macOS; the host still sets it on every OS) ----
    /// The host's classification of macOS applications by bundle identifier
    /// (`evidence_gate::AppTargetPolicy::new`, validated and bounded; no default). The foreground
    /// application is observed from the OS before the clipboard is touched and again inside the
    /// authorization of the paste and of Return. A terminal on the policy's list refuses both
    /// (macOS has no pane confirmation, so `terminal_pane_security` does not apply there);
    /// an application on neither list follows the policy's `UnclassifiedPaste`; an unknown or
    /// error observation and a changed identity refuse. Streaming takes the same value through
    /// `StreamingHost::app_policy`. Not consulted on Windows.
    pub macos_app_policy: &'static AppTargetPolicy,
}

/// What the host app supplies. One `&'static DeliveryHost` is passed to every
/// delivery call.
#[derive(Debug, Clone, Copy)]
pub struct DeliveryHost {
    pub config: DeliveryConfig,
    /// `(event, detail)`. Details carry counts, outcome codes and ids only,
    /// never transcript or field text.
    pub event_sink: fn(&str, &str),
    /// The user's delivery method setting ("paste" or "keystroke").
    pub delivery_method: fn() -> String,
    /// Latency timeline hook: delivery started.
    pub mark_delivery_started: fn(),
    /// Latency timeline hook: the paste keystroke was issued.
    pub mark_paste_issued: fn(),
    /// The user's profile directory, for the process-specific WezTerm socket.
    #[cfg(target_os = "windows")]
    pub profile_dir: fn() -> Option<PathBuf>,
    /// macOS accessibility reads the host owns (its retry and timeout policy).
    #[cfg(target_os = "macos")]
    pub focused_text_input: fn() -> Option<bool>,
    #[cfg(target_os = "macos")]
    pub focused_target_identity: fn() -> Option<isize>,
    #[cfg(target_os = "macos")]
    pub focused_window_title: fn(i32) -> Option<String>,
}

impl DeliveryHost {
    pub fn log(&self, event: &str, detail: &str) {
        (self.event_sink)(event, detail);
    }
}
