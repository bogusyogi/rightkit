//! Capture-safety probe: is a target app or window showing a surface that must not be
//! recorded (a secure or password field, a window that opted out of capture)?
//!
//! The answer is tri-state and never optimistic:
//! - [`CaptureSafety::Unsafe`]: at least one check found a sensitive surface.
//! - [`CaptureSafety::Safe`]: every check this platform defines ran and found nothing.
//! - [`CaptureSafety::Unknown`]: no check found anything, but at least one could not run
//!   (permission missing, window gone, focus not readable, platform without a probe).
//!
//! `Safe` means "the checks listed in [`CaptureSafetyReport::evidence`] saw nothing". It is not
//! a guarantee that nothing sensitive is on screen: a page of text, a notification or a
//! secret rendered in an ordinary view is invisible to every check here. A caller that must
//! refuse or mask should treat `Unknown` like `Unsafe`.
//!
//! The classification is pure ([`classify`]) and tested on every OS; only the evidence
//! collection is per-OS (`macos::capture_safety`, `win::capture_safety`). Evidence per OS:
//!
//! | OS | Check | Source |
//! |---|---|---|
//! | macOS | secure event input | `IsSecureEventInputEnabled` (system-wide) |
//! | macOS | focused secure text field | AX focused element, role or subrole `AXSecureTextField` |
//! | macOS | window not shareable | `kCGWindowSharingState == 0` in `CGWindowListCopyWindowInfo` |
//! | Windows | window excluded from capture | `GetWindowDisplayAffinity` is `WDA_MONITOR` or `WDA_EXCLUDEFROMCAPTURE` |
//! | Windows | focused password control | `ES_PASSWORD` on a focused `Edit`/`RichEdit`, or UIA `IsPassword` |
//! | other | none | always `Unknown` |

use crate::model::Target;

/// Why a target is not safe to capture.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SafetyReason {
    /// macOS secure event input is on. The OS flag is system-wide: it says some process is
    /// reading a secure field, not that the target is. Reported against every target.
    SecureEventInput,
    /// The target app's focused element is a secure text field (macOS AX).
    SecureTextFieldFocused,
    /// The target's focused control is a password field (Windows).
    PasswordFieldFocused,
    /// The window's sharing type is none: the OS withholds its pixels from other processes (macOS).
    WindowSharingNone { window_id: u64 },
    /// The window set a display affinity that hides it from capture (Windows). `affinity` is the
    /// raw `WDA_*` value: `0x1` monitor-only, `0x11` exclude from capture.
    WindowDisplayAffinity { window_id: u64, affinity: u32 },
}

/// Which check a piece of evidence belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SafetyCheck {
    SecureEventInput,
    FocusedSecureField,
    WindowCapturable,
    /// The probe itself (no platform support, no evidence at all).
    Probe,
}

/// Why a check could not decide.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GapKind {
    /// A permission the check needs is not granted (for example Accessibility on macOS).
    PermissionMissing(&'static str),
    /// The named window is not known to the OS, or belongs to another process.
    TargetWindowNotFound,
    /// The app has no visible window to check.
    NoVisibleWindow,
    /// The OS call failed or the value was absent.
    ReadFailed(&'static str),
    /// The focused element is a container (web view, group) whose inner focus the OS does not
    /// expose, so a secure field inside it cannot be ruled out.
    FocusOpaque,
    /// The OS returned a value this crate does not recognise.
    UnrecognisedValue(i64),
    /// This platform has no probe.
    UnsupportedPlatform,
    /// No check ran at all.
    NoEvidence,
}

/// A check that could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyGap {
    pub check: SafetyCheck,
    pub kind: GapKind,
}

/// The result of one check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyOutcome {
    /// The check ran and saw nothing sensitive.
    Clear,
    /// The check found a sensitive surface.
    Found(SafetyReason),
    /// The check could not decide.
    Unavailable(GapKind),
}

/// One check and its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyEvidence {
    pub check: SafetyCheck,
    pub outcome: SafetyOutcome,
}

impl SafetyEvidence {
    pub fn new(check: SafetyCheck, outcome: SafetyOutcome) -> Self {
        SafetyEvidence { check, outcome }
    }
}

/// The verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CaptureSafety {
    /// Every check that applies on this platform ran and found nothing.
    Safe,
    /// At least one sensitive surface was found (all findings are listed).
    Unsafe(Vec<SafetyReason>),
    /// Nothing was found, but these checks could not run.
    Unknown(Vec<SafetyGap>),
}

impl CaptureSafety {
    pub fn is_safe(&self) -> bool {
        matches!(self, CaptureSafety::Safe)
    }
}

/// The verdict and the evidence behind it, so a caller can log why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSafetyReport {
    pub verdict: CaptureSafety,
    pub evidence: Vec<SafetyEvidence>,
}

/// Reduces evidence to a verdict. Any finding makes it `Unsafe` (a finding stands even when
/// other checks failed). Otherwise any unavailable check makes it `Unknown`. `Safe` needs at
/// least one check and every check clear, so an empty list is `Unknown`.
pub fn classify(evidence: &[SafetyEvidence]) -> CaptureSafety {
    let mut found = Vec::new();
    let mut gaps = Vec::new();
    for e in evidence {
        match &e.outcome {
            SafetyOutcome::Clear => {}
            SafetyOutcome::Found(reason) => {
                if !found.contains(reason) {
                    found.push(reason.clone());
                }
            }
            SafetyOutcome::Unavailable(kind) => gaps.push(SafetyGap {
                check: e.check,
                kind: kind.clone(),
            }),
        }
    }
    if !found.is_empty() {
        CaptureSafety::Unsafe(found)
    } else if !gaps.is_empty() {
        CaptureSafety::Unknown(gaps)
    } else if evidence.is_empty() {
        CaptureSafety::Unknown(vec![SafetyGap {
            check: SafetyCheck::Probe,
            kind: GapKind::NoEvidence,
        }])
    } else {
        CaptureSafety::Safe
    }
}

/// Probes `target` and reports whether it is safe to capture. Reads OS state only; it sends no
/// input and changes nothing. On macOS the focus check needs the Accessibility grant, and
/// without it the verdict can be at best `Unknown`. The focus checks describe the target app's
/// focus, not one window's. Never panics; an OS failure becomes an `Unavailable` check.
pub fn probe_capture_safety(target: &Target) -> CaptureSafetyReport {
    let evidence = collect(target);
    CaptureSafetyReport {
        verdict: classify(&evidence),
        evidence,
    }
}

#[cfg(target_os = "macos")]
fn collect(target: &Target) -> Vec<SafetyEvidence> {
    crate::macos::capture_safety::collect(target)
}

#[cfg(windows)]
fn collect(target: &Target) -> Vec<SafetyEvidence> {
    crate::win::capture_safety::collect(target)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn collect(_target: &Target) -> Vec<SafetyEvidence> {
    vec![SafetyEvidence::new(
        SafetyCheck::Probe,
        SafetyOutcome::Unavailable(GapKind::UnsupportedPlatform),
    )]
}

// ------------------------------------------------------------------ pure per-OS rules

/// `kCGWindowSharingState`: 0 none, 1 read-only, 2 read-write. A missing or other value is a gap.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn sharing_outcome(window_id: u64, state: Option<i64>) -> SafetyOutcome {
    match state {
        Some(0) => SafetyOutcome::Found(SafetyReason::WindowSharingNone { window_id }),
        Some(1) | Some(2) => SafetyOutcome::Clear,
        Some(other) => SafetyOutcome::Unavailable(GapKind::UnrecognisedValue(other)),
        None => SafetyOutcome::Unavailable(GapKind::ReadFailed("kCGWindowSharingState")),
    }
}

/// One row of the macOS window list.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WindowRow {
    pub window_id: u64,
    pub pid: i32,
    pub on_screen: bool,
    pub sharing_state: Option<i64>,
}

/// Window-sharing evidence for a target. A named window is checked wherever it is (a recorded
/// window may sit off every display); an app target checks its on-screen windows. A named
/// window that is missing, or owned by another pid, is a gap, never a clear result.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn sharing_evidence(rows: &[WindowRow], target: &Target) -> Vec<SafetyEvidence> {
    let check = SafetyCheck::WindowCapturable;
    let ev = |outcome| SafetyEvidence::new(check, outcome);
    if let Some(id) = target.window_id {
        return match rows
            .iter()
            .find(|r| r.window_id == id && r.pid == target.pid)
        {
            Some(r) => vec![ev(sharing_outcome(id, r.sharing_state))],
            None => vec![ev(SafetyOutcome::Unavailable(
                GapKind::TargetWindowNotFound,
            ))],
        };
    }
    let out: Vec<_> = rows
        .iter()
        .filter(|r| r.pid == target.pid && r.on_screen)
        .map(|r| ev(sharing_outcome(r.window_id, r.sharing_state)))
        .collect();
    if out.is_empty() {
        vec![ev(SafetyOutcome::Unavailable(GapKind::NoVisibleWindow))]
    } else {
        out
    }
}

/// Roles whose focus says nothing about a field inside them (web content, groups, the window).
#[cfg(any(target_os = "macos", test))]
const OPAQUE_AX_ROLES: [&str; 5] = [
    "AXWebArea",
    "AXGroup",
    "AXScrollArea",
    "AXWindow",
    "AXApplication",
];

/// Classifies the deepest focused AX element by role and subrole. `None` role means the
/// element could not be read.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn ax_focus_outcome(role: Option<&str>, subrole: Option<&str>) -> SafetyOutcome {
    if role == Some("AXSecureTextField") || subrole == Some("AXSecureTextField") {
        return SafetyOutcome::Found(SafetyReason::SecureTextFieldFocused);
    }
    match role {
        None => SafetyOutcome::Unavailable(GapKind::ReadFailed("AXRole")),
        Some(r) if OPAQUE_AX_ROLES.contains(&r) => SafetyOutcome::Unavailable(GapKind::FocusOpaque),
        Some(_) => SafetyOutcome::Clear,
    }
}

/// `GetWindowDisplayAffinity` value: 0 none, 1 monitor, 0x11 exclude from capture. `None`
/// means the call failed.
#[cfg(any(windows, test))]
pub(crate) fn affinity_outcome(window_id: u64, affinity: Option<u32>) -> SafetyOutcome {
    match affinity {
        Some(0) => SafetyOutcome::Clear,
        Some(a @ (0x1 | 0x11)) => SafetyOutcome::Found(SafetyReason::WindowDisplayAffinity {
            window_id,
            affinity: a,
        }),
        Some(other) => SafetyOutcome::Unavailable(GapKind::UnrecognisedValue(i64::from(other))),
        None => SafetyOutcome::Unavailable(GapKind::ReadFailed("GetWindowDisplayAffinity")),
    }
}

/// True for a classic `Edit` or `RichEdit` control with `ES_PASSWORD` (0x20). The bit means
/// something else on other window classes, so the class is checked first.
#[cfg(any(windows, test))]
pub(crate) fn is_password_edit(class: &str, style: u32) -> bool {
    const ES_PASSWORD: u32 = 0x20;
    let lower = class.to_ascii_lowercase();
    let edit = lower == "edit" || lower.starts_with("richedit");
    edit && style & ES_PASSWORD != 0
}

/// Outcome of a UIA read of the focused element's `IsPassword`. `None` means the property
/// could not be read.
#[cfg(any(windows, test))]
pub(crate) fn uia_password_outcome(is_password: Option<bool>) -> SafetyOutcome {
    match is_password {
        Some(true) => SafetyOutcome::Found(SafetyReason::PasswordFieldFocused),
        Some(false) => SafetyOutcome::Clear,
        None => SafetyOutcome::Unavailable(GapKind::ReadFailed("UIA IsPassword")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear(check: SafetyCheck) -> SafetyEvidence {
        SafetyEvidence::new(check, SafetyOutcome::Clear)
    }
    fn found(check: SafetyCheck, r: SafetyReason) -> SafetyEvidence {
        SafetyEvidence::new(check, SafetyOutcome::Found(r))
    }
    fn gap(check: SafetyCheck, k: GapKind) -> SafetyEvidence {
        SafetyEvidence::new(check, SafetyOutcome::Unavailable(k))
    }

    #[test]
    fn no_evidence_is_unknown_never_safe() {
        match classify(&[]) {
            CaptureSafety::Unknown(g) => assert_eq!(g[0].kind, GapKind::NoEvidence),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn all_clear_is_safe() {
        let e = [
            clear(SafetyCheck::SecureEventInput),
            clear(SafetyCheck::WindowCapturable),
        ];
        assert_eq!(classify(&e), CaptureSafety::Safe);
        assert!(classify(&e).is_safe());
    }

    #[test]
    fn one_gap_makes_unknown() {
        let e = [
            clear(SafetyCheck::SecureEventInput),
            gap(
                SafetyCheck::FocusedSecureField,
                GapKind::PermissionMissing("Accessibility"),
            ),
        ];
        match classify(&e) {
            CaptureSafety::Unknown(g) => {
                assert_eq!(g.len(), 1);
                assert_eq!(g[0].check, SafetyCheck::FocusedSecureField);
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn a_finding_beats_gaps_and_clears() {
        let e = [
            clear(SafetyCheck::SecureEventInput),
            gap(SafetyCheck::FocusedSecureField, GapKind::FocusOpaque),
            found(
                SafetyCheck::WindowCapturable,
                SafetyReason::WindowSharingNone { window_id: 7 },
            ),
        ];
        assert_eq!(
            classify(&e),
            CaptureSafety::Unsafe(vec![SafetyReason::WindowSharingNone { window_id: 7 }])
        );
    }

    #[test]
    fn duplicate_findings_are_listed_once() {
        let r = SafetyReason::SecureEventInput;
        let e = [
            found(SafetyCheck::SecureEventInput, r.clone()),
            found(SafetyCheck::SecureEventInput, r.clone()),
        ];
        assert_eq!(classify(&e), CaptureSafety::Unsafe(vec![r]));
    }

    #[test]
    fn sharing_state_mapping() {
        assert_eq!(
            sharing_outcome(3, Some(0)),
            SafetyOutcome::Found(SafetyReason::WindowSharingNone { window_id: 3 })
        );
        assert_eq!(sharing_outcome(3, Some(1)), SafetyOutcome::Clear);
        assert_eq!(sharing_outcome(3, Some(2)), SafetyOutcome::Clear);
        assert_eq!(
            sharing_outcome(3, Some(9)),
            SafetyOutcome::Unavailable(GapKind::UnrecognisedValue(9))
        );
        assert!(matches!(
            sharing_outcome(3, None),
            SafetyOutcome::Unavailable(GapKind::ReadFailed(_))
        ));
    }

    fn row(window_id: u64, pid: i32, on_screen: bool, s: Option<i64>) -> WindowRow {
        WindowRow {
            window_id,
            pid,
            on_screen,
            sharing_state: s,
        }
    }

    #[test]
    fn named_window_missing_or_foreign_is_a_gap() {
        let rows = [row(10, 100, true, Some(2)), row(11, 200, true, Some(2))];
        let missing = sharing_evidence(&rows, &Target::window(100, 99));
        assert_eq!(
            missing[0].outcome,
            SafetyOutcome::Unavailable(GapKind::TargetWindowNotFound)
        );
        // Window 11 exists but belongs to pid 200, not 100.
        let foreign = sharing_evidence(&rows, &Target::window(100, 11));
        assert_eq!(
            foreign[0].outcome,
            SafetyOutcome::Unavailable(GapKind::TargetWindowNotFound)
        );
    }

    #[test]
    fn named_window_is_checked_even_off_screen() {
        let rows = [row(10, 100, false, Some(0))];
        let ev = sharing_evidence(&rows, &Target::window(100, 10));
        assert_eq!(
            ev[0].outcome,
            SafetyOutcome::Found(SafetyReason::WindowSharingNone { window_id: 10 })
        );
    }

    #[test]
    fn app_target_checks_its_on_screen_windows_only() {
        let rows = [
            row(10, 100, true, Some(2)),
            row(11, 100, true, Some(0)),
            row(12, 100, false, Some(0)),
            row(13, 200, true, Some(0)),
        ];
        let ev = sharing_evidence(&rows, &Target::app(100));
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].outcome, SafetyOutcome::Clear);
        assert_eq!(
            ev[1].outcome,
            SafetyOutcome::Found(SafetyReason::WindowSharingNone { window_id: 11 })
        );
    }

    #[test]
    fn app_with_no_visible_window_is_a_gap() {
        let rows = [row(12, 100, false, Some(2))];
        let ev = sharing_evidence(&rows, &Target::app(100));
        assert_eq!(
            ev[0].outcome,
            SafetyOutcome::Unavailable(GapKind::NoVisibleWindow)
        );
    }

    #[test]
    fn ax_secure_by_role_or_subrole() {
        let secure = SafetyOutcome::Found(SafetyReason::SecureTextFieldFocused);
        assert_eq!(ax_focus_outcome(Some("AXSecureTextField"), None), secure);
        assert_eq!(
            ax_focus_outcome(Some("AXTextField"), Some("AXSecureTextField")),
            secure
        );
    }

    #[test]
    fn ax_plain_field_is_clear_and_containers_are_opaque() {
        assert_eq!(
            ax_focus_outcome(Some("AXTextField"), None),
            SafetyOutcome::Clear
        );
        assert_eq!(
            ax_focus_outcome(Some("AXButton"), None),
            SafetyOutcome::Clear
        );
        for role in ["AXWebArea", "AXGroup", "AXScrollArea", "AXWindow"] {
            assert_eq!(
                ax_focus_outcome(Some(role), None),
                SafetyOutcome::Unavailable(GapKind::FocusOpaque),
                "{role}"
            );
        }
        assert!(matches!(
            ax_focus_outcome(None, None),
            SafetyOutcome::Unavailable(GapKind::ReadFailed(_))
        ));
    }

    #[test]
    fn affinity_mapping() {
        assert_eq!(affinity_outcome(5, Some(0)), SafetyOutcome::Clear);
        for a in [0x1u32, 0x11] {
            assert_eq!(
                affinity_outcome(5, Some(a)),
                SafetyOutcome::Found(SafetyReason::WindowDisplayAffinity {
                    window_id: 5,
                    affinity: a
                })
            );
        }
        assert_eq!(
            affinity_outcome(5, Some(0x2)),
            SafetyOutcome::Unavailable(GapKind::UnrecognisedValue(2))
        );
        assert!(matches!(
            affinity_outcome(5, None),
            SafetyOutcome::Unavailable(GapKind::ReadFailed(_))
        ));
    }

    #[test]
    fn es_password_counts_only_on_edit_classes() {
        assert!(is_password_edit("Edit", 0x20 | 0x4000_0000));
        assert!(is_password_edit("RICHEDIT50W", 0x20));
        assert!(!is_password_edit("Edit", 0x4000_0000));
        // 0x20 is another style on a button or a list box.
        assert!(!is_password_edit("Button", 0x20));
        assert!(!is_password_edit("ListBox", 0x20));
    }

    #[test]
    fn uia_password_mapping() {
        assert_eq!(
            uia_password_outcome(Some(true)),
            SafetyOutcome::Found(SafetyReason::PasswordFieldFocused)
        );
        assert_eq!(uia_password_outcome(Some(false)), SafetyOutcome::Clear);
        assert!(matches!(
            uia_password_outcome(None),
            SafetyOutcome::Unavailable(_)
        ));
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    #[test]
    fn other_platforms_are_unknown() {
        let r = probe_capture_safety(&Target::app(1));
        assert!(matches!(r.verdict, CaptureSafety::Unknown(_)));
    }
}
