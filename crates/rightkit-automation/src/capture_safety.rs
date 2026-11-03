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
#[cfg(target_os = "macos")]
pub(crate) fn sharing_outcome(window_id: u64, state: Option<i64>) -> SafetyOutcome {
    match state {
        Some(0) => SafetyOutcome::Found(SafetyReason::WindowSharingNone { window_id }),
        Some(1) | Some(2) => SafetyOutcome::Clear,
        Some(other) => SafetyOutcome::Unavailable(GapKind::UnrecognisedValue(other)),
        None => SafetyOutcome::Unavailable(GapKind::ReadFailed("kCGWindowSharingState")),
    }
}

/// One row of the macOS window list.
#[cfg(target_os = "macos")]
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
#[cfg(target_os = "macos")]
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
#[cfg(target_os = "macos")]
const OPAQUE_AX_ROLES: [&str; 5] = [
    "AXWebArea",
    "AXGroup",
    "AXScrollArea",
    "AXWindow",
    "AXApplication",
];

/// Classifies the deepest focused AX element by role and subrole. `None` role means the
/// element could not be read.
#[cfg(target_os = "macos")]
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
#[cfg(windows)]
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
#[cfg(windows)]
pub(crate) fn is_password_edit(class: &str, style: u32) -> bool {
    const ES_PASSWORD: u32 = 0x20;
    let lower = class.to_ascii_lowercase();
    let edit = lower == "edit" || lower.starts_with("richedit");
    edit && style & ES_PASSWORD != 0
}

/// Outcome of a UIA read of the focused element's `IsPassword`. `None` means the property
/// could not be read.
#[cfg(windows)]
pub(crate) fn uia_password_outcome(is_password: Option<bool>) -> SafetyOutcome {
    match is_password {
        Some(true) => SafetyOutcome::Found(SafetyReason::PasswordFieldFocused),
        Some(false) => SafetyOutcome::Clear,
        None => SafetyOutcome::Unavailable(GapKind::ReadFailed("UIA IsPassword")),
    }
}
