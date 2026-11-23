//! The central evidence gate: the only way an automatic write into another application is issued.
//!
//! Every automatic write (paste keystroke, streamed chunk, backspace batch, cancel-erase, Enter)
//! must first pass [`authorize_with_input`] immediately before it is issued. It takes FRESH
//! evidence, not anything captured earlier:
//!
//! * the focused control compared with the bound target, at control level ([`ControlMatch`]),
//! * the secure-field state as a tri-state (`Some(true)` secure, `Some(false)` not secure,
//!   `None` unknown),
//! * a deadline covering sampling and the write itself, and
//! * LAST, the physical modifier keys as a tri-state (`Some(true)` all up, `Some(false)` one held,
//!   `None` unknown). Every write passes it; there is no authorization without that sample.
//!
//! The decision ([`decide`], pure) is: write only on (same control, confirmed not secure, in
//! time), and then only with the modifier keys sampled up. Unknown, error, timeout, a changed
//! control, a held modifier and an unreadable modifier state all refuse. A successful authorization
//! returns a single-use [`WritePermit`]; the write primitives in `gated_writes` require one and
//! are the only code that calls the platform keystroke functions, so a call site cannot write
//! without having passed the gate. A source scan in the tests keeps it so.
//!
//! One exception exists, and only a host can declare it: [`SecureEvidence::NotApplicableByHostPolicy`]
//! replaces the secure-field probe for a terminal pane the delivery is bound to, when the host chose
//! `TerminalPaneSecurity::NotApplicableWhenPaneConfirmed`. It is a distinct named state, never
//! `Some(false)`, it still needs `ControlMatch::Same` (for a pane: a pane check that returned
//! `Current` inside the same authorization), and a terminal window without such a bound pane is
//! refused (`Refusal::TerminalPaneUnverified`) under every policy.
//!
//! On macOS the control sample also observes the foreground application (`app_target`: bundle
//! identifier from the OS, classified by the host's `AppTargetPolicy`). It is bound at capture and
//! observed again inside every authorization, after the control read and before the modifier
//! sample, so the order stays secure, control and application, modifier keys last. A terminal, an
//! unclassified application (streaming always; paste per the host's choice), an unknown or error
//! observation and a changed identity refuse with their own tags (`ControlMatch::App`).
//!
//! The safe fallback on refusal belongs to the route: paste copies to the clipboard (or leaves the
//! clipboard as found); streaming stops, and what it leaves in the field is reported to the host
//! as an explicit `streaming::ReconcileOutcome`. This module never retries.

use std::time::{Duration, Instant};

use rightkit_transcript_library::delivery::CopyFallbackReason;

mod app_target;

pub use app_target::*;

/// How strongly a [`ControlId`] pins one control. Ordered weakest to strongest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum IdentityLevel {
    /// Process and top-level window only. Used solely for targets whose control cannot be told
    /// apart by design: an opaque app listed by the host, and a WezTerm pane that is confirmed
    /// `PanePreflight::Current` by its own probe.
    Window,
    /// Native focused control handle (Windows `hwndFocus`, macOS AX focused element identity).
    Control,
    /// UI Automation runtime id of the focused element.
    Element,
}

/// UIA element identity: the runtime id and the native window handle it resolved against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElementKey {
    pub native_window: isize,
    pub runtime_id: Vec<i32>,
}

/// An observed or bound control. Equality of two of these is the "same control" test, applied at
/// the bound side's [`IdentityLevel`] by [`control_match`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlId {
    pub level: IdentityLevel,
    pub process_id: u32,
    pub window: isize,
    pub control: Option<isize>,
    pub element: Option<ElementKey>,
}

impl ControlId {
    /// The level is the strongest evidence present.
    pub fn new(
        process_id: u32,
        window: isize,
        control: Option<isize>,
        element: Option<ElementKey>,
    ) -> Self {
        let level = if element.is_some() {
            IdentityLevel::Element
        } else if control.is_some() {
            IdentityLevel::Control
        } else {
            IdentityLevel::Window
        };
        Self {
            level,
            process_id,
            window,
            control,
            element,
        }
    }

    /// A window-level binding. Only for the documented targets listed on
    /// [`IdentityLevel::Window`].
    pub fn window_level(process_id: u32, window: isize) -> Self {
        Self {
            level: IdentityLevel::Window,
            process_id,
            window,
            control: None,
            element: None,
        }
    }
}

/// Result of comparing a fresh observation with the bound target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlMatch {
    Same,
    Changed,
    /// The fresh observation is missing or weaker than the binding requires.
    Unknown,
    /// The fresh foreground window belongs to a process on the host's terminal list, and the
    /// delivery is not bound to a terminal pane that was just re-confirmed (no bound pane, or a
    /// pane check that did not return current). Refused under every terminal-pane policy.
    TerminalUnverified,
    /// macOS: the foreground application is refused by the host's [`AppTargetPolicy`] (a terminal,
    /// an unclassified application where the use does not permit one, an unknown or error
    /// observation) or is not the application bound at capture. The tag says which.
    App(AppRefusal),
}

/// Compare a fresh observation with the bound control at the bound control's level. A missing
/// observation, or one without the evidence the binding needs, is `Unknown`, never `Same`.
pub fn control_match(bound: &ControlId, fresh: Option<&ControlId>) -> ControlMatch {
    let Some(fresh) = fresh else {
        return ControlMatch::Unknown;
    };
    if fresh.process_id != bound.process_id || fresh.window != bound.window {
        return ControlMatch::Changed;
    }
    if let Some(bound_control) = bound.control {
        match fresh.control {
            Some(fresh_control) if fresh_control == bound_control => {}
            Some(_) => return ControlMatch::Changed,
            None => return ControlMatch::Unknown,
        }
    }
    if bound.level == IdentityLevel::Element {
        match (&bound.element, &fresh.element) {
            (Some(bound_element), Some(fresh_element)) => {
                if bound_element != fresh_element {
                    return ControlMatch::Changed;
                }
            }
            _ => return ControlMatch::Unknown,
        }
    }
    ControlMatch::Same
}

/// The kind of automatic write being authorized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteKind {
    PasteKeystroke,
    StreamChunk,
    StreamBackspace,
    CancelErase,
    Enter,
}

/// Why the gate refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The focused field is positively secure.
    Secure,
    /// Secure-field state could not be read (error, timeout or unreadable): treated as refusal.
    SecureUnknown,
    ControlChanged,
    /// Control identity could not be observed at the bound level.
    ControlUnknown,
    /// Sampling took longer than the budget, or the permit was redeemed after it expired.
    Timeout,
    /// A permit was redeemed for a write kind it was not issued for.
    KindMismatch,
    /// The user's physical modifier keys were down when the input state was sampled (the last
    /// sample of [`authorize_with_input`]). The chord is not adjusted for held keys any more, so
    /// the write is refused instead.
    InputBusy,
    /// The modifier state could not be read (the sample was `None`; on macOS the automation crate
    /// has no modifier probe). Unknown is never "keys are up": the write is refused.
    InputUnknown,
    /// A terminal window is the target, and there is no bound pane re-confirmed inside this
    /// authorization. The host's terminal-pane policy never relaxes this: it relaxes only the
    /// secure-field requirement of a confirmed pane.
    TerminalPaneUnverified,
    /// macOS: the foreground application was refused or changed (see [`AppRefusal`]); the code is
    /// the application refusal's own tag.
    App(AppRefusal),
}

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Refusal::Secure => "secure_target",
            Refusal::SecureUnknown => "secure_unknown",
            Refusal::ControlChanged => "control_changed",
            Refusal::ControlUnknown => "control_unknown",
            Refusal::Timeout => "evidence_timeout",
            Refusal::KindMismatch => "kind_mismatch",
            Refusal::InputBusy => "modifiers_held",
            Refusal::InputUnknown => "modifiers_unknown",
            Refusal::TerminalPaneUnverified => "terminal_pane_unverified",
            Refusal::App(refusal) => refusal.code(),
        }
    }

    /// The paste route's copy-fallback reason.
    pub fn fallback_reason(self) -> CopyFallbackReason {
        match self {
            Refusal::ControlChanged
            | Refusal::ControlUnknown
            | Refusal::App(AppRefusal::IdentityChanged) => CopyFallbackReason::FocusChanged,
            other => CopyFallbackReason::Other(other.code().to_string()),
        }
    }
}

/// The secure-field input of the decision. It is a closed pair on purpose: a probe's tri-state,
/// or the one named exception the host declared for a confirmed terminal pane. The exception is
/// not `Some(false)`: it never claims the field was observed to be non-secure, and the type and
/// the logs keep the two apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecureEvidence {
    /// A probe's answer: `Some(true)` secure, `Some(false)` not secure, `None` unknown.
    Probed(Option<bool>),
    /// The host declared (`TerminalPaneSecurity::NotApplicableWhenPaneConfirmed`) that the
    /// password-field check does not apply to a terminal pane. Passes only together with
    /// `ControlMatch::Same`, which for a pane means a pane check that returned `Current` inside
    /// this same authorization. Only `windows_secure_evidence` builds it, and only for a bound
    /// terminal pane under that policy.
    NotApplicableByHostPolicy,
}

/// The pure decision. Positive secure evidence wins over everything, then a changed, unknown,
/// unverified-terminal or application-refused control, then unknown security, then the deadline. Only (same, not secure,
/// in time) passes.
pub fn decide(
    control: ControlMatch,
    secure: Option<bool>,
    elapsed: Duration,
    budget: Duration,
) -> Result<(), Refusal> {
    decide_evidence(control, SecureEvidence::Probed(secure), elapsed, budget)
}

/// [`decide`] over [`SecureEvidence`]. `NotApplicableByHostPolicy` skips only the secure-field
/// terms (positive secure and unknown secure); the control and the deadline still decide.
pub fn decide_evidence(
    control: ControlMatch,
    secure: SecureEvidence,
    elapsed: Duration,
    budget: Duration,
) -> Result<(), Refusal> {
    if secure == SecureEvidence::Probed(Some(true)) {
        return Err(Refusal::Secure);
    }
    match control {
        ControlMatch::Changed => return Err(Refusal::ControlChanged),
        ControlMatch::Unknown => return Err(Refusal::ControlUnknown),
        ControlMatch::TerminalUnverified => return Err(Refusal::TerminalPaneUnverified),
        ControlMatch::App(refusal) => return Err(Refusal::App(refusal)),
        ControlMatch::Same => {}
    }
    if secure == SecureEvidence::Probed(None) {
        return Err(Refusal::SecureUnknown);
    }
    if elapsed > budget {
        return Err(Refusal::Timeout);
    }
    Ok(())
}

/// Single-use proof that a write passed the gate. Not `Clone`, not constructible outside this
/// module. The write primitive redeems it as the LAST step before it issues the write: nothing
/// that waits, sleeps, retries or reads the outside world may run between `redeem` and the
/// effect-only call that sends the input (`gated_writes` is built that way).
#[derive(Debug)]
pub struct WritePermit {
    kind: WriteKind,
    valid_until: Instant,
    secure: SecureEvidence,
}

impl WritePermit {
    pub fn kind(&self) -> WriteKind {
        self.kind
    }

    /// True when the secure-field requirement was waived by the host's terminal-pane policy for
    /// this write. The caller reads it before redeeming and logs after the send (never between
    /// the authorization and the effect).
    pub fn secure_not_applicable(&self) -> bool {
        self.secure == SecureEvidence::NotApplicableByHostPolicy
    }

    /// Check a permit for a write of one of `allowed`. Fails when the permit is for another
    /// kind or when the sampling deadline has passed.
    pub(crate) fn validate(&self, allowed: &[WriteKind]) -> Result<(), Refusal> {
        if !allowed.contains(&self.kind) {
            return Err(Refusal::KindMismatch);
        }
        if Instant::now() > self.valid_until {
            return Err(Refusal::Timeout);
        }
        Ok(())
    }

    /// Consume this permit after all pre-send hooks; its original deadline still applies.
    pub fn redeem(self, allowed: &[WriteKind]) -> Result<WriteKind, Refusal> {
        self.validate(allowed)?;
        Ok(self.kind)
    }
}

/// Sample fresh evidence and decide. `secure` is read first, `control` second and `input_quiet`
/// LAST, so a control that moved after the secure read is caught by the control comparison, and
/// one that moved before it is caught too (the secure read then described the new control while
/// the comparison shows it is not the bound one). `budget` covers all three samples and the write
/// that follows.
///
/// `input_quiet` is the tri-state sample of the user's physical modifier keys (Ctrl, Shift, Alt and
/// Win on Windows; Shift, Control, Option, Command and Fn on macOS)
/// (`gated_writes::modifiers_quiet`): `Some(true)` all up, `Some(false)` one held
/// (`Refusal::InputBusy`), `None` unreadable (`Refusal::InputUnknown`). The input sent afterwards
/// is a fixed prepared batch that does not look at, wait for or adjust for held keys, so the check
/// that keeps it correct sits inside the authorization, after the (slow) secure and control reads
/// and before the permit. There is no overload without the sample: a caller cannot authorize a
/// write by omitting it. Callers wait for the keys to lift BEFORE calling this
/// (`gated_writes::settle_modifiers`).
pub fn authorize_with_input(
    kind: WriteKind,
    budget: Duration,
    secure: impl FnOnce() -> Option<bool>,
    control: impl FnOnce() -> ControlMatch,
    input_quiet: impl FnOnce() -> Option<bool>,
) -> Result<WritePermit, Refusal> {
    authorize_evidence(
        kind,
        budget,
        || SecureEvidence::Probed(secure()),
        control,
        input_quiet,
    )
}

/// [`authorize_with_input`] with the secure sample as [`SecureEvidence`], so a host-declared
/// exception is a named state and not a probe answer. Sampling order is unchanged: secure first,
/// then control (for a terminal pane this is where the fresh pane check runs), then the modifier
/// keys last.
pub fn authorize_evidence(
    kind: WriteKind,
    budget: Duration,
    secure: impl FnOnce() -> SecureEvidence,
    control: impl FnOnce() -> ControlMatch,
    input_quiet: impl FnOnce() -> Option<bool>,
) -> Result<WritePermit, Refusal> {
    let started = Instant::now();
    let secure = secure();
    let control = control();
    let quiet = input_quiet();
    decide_evidence(control, secure, started.elapsed(), budget)?;
    match quiet {
        Some(true) => {}
        Some(false) => return Err(Refusal::InputBusy),
        None => return Err(Refusal::InputUnknown),
    }
    Ok(WritePermit {
        kind,
        valid_until: started + budget,
        secure,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGET: Duration = Duration::from_millis(50);
    const FAST: Duration = Duration::from_millis(1);

    /// Test helper: the production API has no overload without a modifier sample, so the
    /// existing cases pass an explicit "all keys up".
    fn authorize(
        kind: WriteKind,
        budget: Duration,
        secure: impl FnOnce() -> Option<bool>,
        control: impl FnOnce() -> ControlMatch,
    ) -> Result<WritePermit, Refusal> {
        authorize_with_input(kind, budget, secure, control, || Some(true))
    }

    fn element(window: isize, runtime: i32) -> ControlId {
        ControlId::new(
            7,
            window,
            Some(11),
            Some(ElementKey {
                native_window: 11,
                runtime_id: vec![42, runtime],
            }),
        )
    }

    #[test]
    fn every_control_and_secure_combination_in_time() {
        use ControlMatch::*;
        let cases = [
            (Same, Some(false), Ok(())),
            (Same, Some(true), Err(Refusal::Secure)),
            (Same, None, Err(Refusal::SecureUnknown)),
            (Changed, Some(false), Err(Refusal::ControlChanged)),
            (Changed, Some(true), Err(Refusal::Secure)),
            (Changed, None, Err(Refusal::ControlChanged)),
            (Unknown, Some(false), Err(Refusal::ControlUnknown)),
            (Unknown, Some(true), Err(Refusal::Secure)),
            (Unknown, None, Err(Refusal::ControlUnknown)),
        ];
        for (control, secure, expected) in cases {
            assert_eq!(
                decide(control, secure, FAST, BUDGET),
                expected,
                "{control:?} {secure:?}"
            );
        }
    }

    #[test]
    fn a_probe_that_ran_over_budget_refuses_even_with_good_evidence() {
        assert_eq!(
            decide(ControlMatch::Same, Some(false), BUDGET + FAST, BUDGET),
            Err(Refusal::Timeout)
        );
        assert_eq!(
            decide(ControlMatch::Same, Some(false), BUDGET, BUDGET),
            Ok(())
        );
    }

    #[test]
    fn unknown_security_alone_never_passes() {
        assert_eq!(
            decide(ControlMatch::Same, None, FAST, BUDGET),
            Err(Refusal::SecureUnknown)
        );
    }

    #[test]
    fn only_same_not_secure_in_time_yields_a_permit() {
        let ok = authorize(
            WriteKind::StreamChunk,
            BUDGET,
            || Some(false),
            || ControlMatch::Same,
        );
        assert_eq!(ok.map(|p| p.kind()), Ok(WriteKind::StreamChunk));
        for (secure, control) in [
            (None, ControlMatch::Same),
            (Some(true), ControlMatch::Same),
            (Some(false), ControlMatch::Changed),
            (Some(false), ControlMatch::Unknown),
        ] {
            assert!(authorize(WriteKind::Enter, BUDGET, || secure, || control).is_err());
        }
    }

    #[test]
    fn a_slow_secure_probe_times_out() {
        let result = authorize(
            WriteKind::PasteKeystroke,
            Duration::from_millis(2),
            || {
                std::thread::sleep(Duration::from_millis(15));
                Some(false)
            },
            || ControlMatch::Same,
        );
        assert_eq!(result.err(), Some(Refusal::Timeout));
    }

    #[test]
    fn a_permit_expires_and_is_bound_to_its_kind() {
        let permit = authorize(
            WriteKind::StreamChunk,
            Duration::from_millis(2),
            || Some(false),
            || ControlMatch::Same,
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(
            permit.redeem(&[WriteKind::StreamChunk]),
            Err(Refusal::Timeout)
        );

        let permit = authorize(
            WriteKind::Enter,
            BUDGET,
            || Some(false),
            || ControlMatch::Same,
        )
        .unwrap();
        assert_eq!(
            permit.redeem(&[WriteKind::StreamChunk, WriteKind::StreamBackspace]),
            Err(Refusal::KindMismatch)
        );
        let permit = authorize(
            WriteKind::Enter,
            BUDGET,
            || Some(false),
            || ControlMatch::Same,
        )
        .unwrap();
        assert_eq!(permit.redeem(&[WriteKind::Enter]), Ok(WriteKind::Enter));
    }

    #[test]
    fn refusal_maps_to_the_copy_fallback() {
        assert_eq!(
            Refusal::ControlChanged.fallback_reason(),
            CopyFallbackReason::FocusChanged
        );
        assert_eq!(
            Refusal::ControlUnknown.fallback_reason(),
            CopyFallbackReason::FocusChanged
        );
        assert_eq!(
            Refusal::Secure.fallback_reason(),
            CopyFallbackReason::Other("secure_target".to_string())
        );
        assert_eq!(
            Refusal::SecureUnknown.fallback_reason(),
            CopyFallbackReason::Other("secure_unknown".to_string())
        );
    }

    #[test]
    fn same_element_matches_and_a_moved_control_does_not() {
        let bound = element(100, 1);
        assert_eq!(
            control_match(&bound, Some(&element(100, 1))),
            ControlMatch::Same
        );
        // Another control in the same window and process.
        assert_eq!(
            control_match(&bound, Some(&element(100, 2))),
            ControlMatch::Changed
        );
        // Another window.
        assert_eq!(
            control_match(&bound, Some(&element(101, 1))),
            ControlMatch::Changed
        );
    }

    #[test]
    fn a_different_focus_handle_in_the_same_window_is_a_change() {
        let bound = ControlId::new(7, 100, Some(5), None);
        let moved = ControlId::new(7, 100, Some(6), None);
        assert_eq!(control_match(&bound, Some(&moved)), ControlMatch::Changed);
        assert_eq!(
            control_match(&bound, Some(&bound.clone())),
            ControlMatch::Same
        );
    }

    #[test]
    fn missing_or_weaker_evidence_is_unknown_never_same() {
        let bound = element(100, 1);
        assert_eq!(control_match(&bound, None), ControlMatch::Unknown);
        // Fresh read resolved the focus handle but not the UIA element.
        let weaker = ControlId::new(7, 100, Some(11), None);
        assert_eq!(control_match(&bound, Some(&weaker)), ControlMatch::Unknown);
        // Fresh read has no focus handle at all.
        let windowed = ControlId::window_level(7, 100);
        assert_eq!(
            control_match(&bound, Some(&windowed)),
            ControlMatch::Unknown
        );
        let handle_bound = ControlId::new(7, 100, Some(5), None);
        assert_eq!(
            control_match(&handle_bound, Some(&windowed)),
            ControlMatch::Unknown
        );
    }

    #[test]
    fn window_level_binding_matches_on_process_and_window_only() {
        let bound = ControlId::window_level(7, 100);
        let fresh = ControlId::new(7, 100, Some(9), None);
        assert_eq!(control_match(&bound, Some(&fresh)), ControlMatch::Same);
        let other = ControlId::new(8, 100, Some(9), None);
        assert_eq!(control_match(&bound, Some(&other)), ControlMatch::Changed);
    }

    #[test]
    fn identity_level_is_the_strongest_evidence_present() {
        assert_eq!(
            ControlId::new(1, 2, None, None).level,
            IdentityLevel::Window
        );
        assert_eq!(
            ControlId::new(1, 2, Some(3), None).level,
            IdentityLevel::Control
        );
        assert_eq!(element(2, 1).level, IdentityLevel::Element);
        assert!(IdentityLevel::Window < IdentityLevel::Control);
        assert!(IdentityLevel::Control < IdentityLevel::Element);
    }

    /// No source file other than `gated_writes.rs` may call a platform keystroke or typing
    /// primitive, so every automatic write has to hold a `WritePermit`.
    #[test]
    fn raw_write_primitives_appear_only_in_gated_writes() {
        const FORBIDDEN: &[&str] = &[
            "type_unicode_text",
            "type_unicode_chunked",
            "send_backspaces",
            "send_enter_keystroke",
            "send_enter_chord",
            "send_paste_keystroke",
            "send_paste_chord",
            "paste_combo",
            "press_return",
            "post_key",
            "keybd_event",
            "CGEventPost",
            // The dormant UIA native value write of `rightkit-automation`.
            "set_value(",
        ];
        let sources: &[(&str, &str)] = &[
            (
                "evidence_gate/app_target.rs",
                include_str!("evidence_gate/app_target.rs"),
            ),
            ("streaming.rs", include_str!("streaming.rs")),
            (
                "streaming/reconcile.rs",
                include_str!("streaming/reconcile.rs"),
            ),
            ("paste_gate.rs", include_str!("paste_gate.rs")),
            ("restore_gate.rs", include_str!("restore_gate.rs")),
            (
                "paste_delivery/backend.rs",
                include_str!("paste_delivery/backend.rs"),
            ),
            (
                "paste_delivery/targets.rs",
                include_str!("paste_delivery/targets.rs"),
            ),
            (
                "paste_delivery/controls.rs",
                include_str!("paste_delivery/controls.rs"),
            ),
            (
                "paste_delivery/host.rs",
                include_str!("paste_delivery/host.rs"),
            ),
            (
                "paste_delivery/mod.rs",
                include_str!("paste_delivery/mod.rs"),
            ),
            ("delivery/insert.rs", include_str!("delivery/insert.rs")),
            (
                "delivery/keystrokes.rs",
                include_str!("delivery/keystrokes.rs"),
            ),
        ];
        for (name, text) in sources {
            for needle in FORBIDDEN {
                assert!(
                    !text.lines().any(|line| {
                        let line = line.trim_start();
                        !line.starts_with("//")
                            && !line.starts_with("pub fn ")
                            && !line.starts_with("pub(crate) fn ")
                            && line.contains(needle)
                    }),
                    "{name} calls {needle} directly; route it through gated_writes with a WritePermit"
                );
            }
        }
    }
}
