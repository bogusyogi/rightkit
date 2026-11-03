//! Runtime-only exact destination (`FocusPin`) captured at an accepted
//! recording stop, and its capture/validation lifecycle: system focus
//! resolution, process-instance identity, and read-only re-validation that a
//! pinned destination is still current.
//!
//! Split out of the former single-file `macos/focus.rs`
//! (docs/agent-rules.md: pure move, no logic changes).

use core_foundation::base::{CFHash, CFType, TCFType};

use super::super::ffi::{
    ax_app_element, ax_copy, ax_copy_with_error, ax_element_pid, ax_set_bool, ax_string,
    ax_system_element, kAXTrustedCheckOptionPrompt, log_ax_query_failure, proc_pidinfo,
    AXIsProcessTrusted, AXIsProcessTrustedWithOptions, AXUIElementRef,
    AXUIElementSetMessagingTimeout, ProcBsdInfo, AX_MESSAGING_TIMEOUT_SECONDS, PROC_BSD_INFO_SIZE,
    PROC_PIDTBSDINFO,
};
use super::super::frontmost::frontmost_pid;
use super::super::observation::{
    begin_accessibility_observation, release_pending_accessibility_observation,
    running_application_identity_detailed, same_running_application, AccessibilityObservationLease,
    RunningApplicationIdentityMiss, PENDING_OBSERVATION_HOLD,
};
use super::context::{focused_descendant, focused_window_for_pid, secure_state_for_roles};

pub(in crate::desktop::macos) fn ax_raise_app(pid: i32) -> bool {
    unsafe {
        let Some(element) = ax_app_element(pid) else {
            return false;
        };
        ax_set_bool(
            element.as_CFTypeRef() as AXUIElementRef,
            "AXFrontmost",
            true,
        )
    }
}

pub fn accessibility_trusted(prompt: bool) -> bool {
    unsafe {
        if !prompt {
            return AXIsProcessTrusted() != 0;
        }
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let val = CFBoolean::true_value();
        let options = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), val.as_CFType())]);
        AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) != 0
    }
}

/// Runtime-only exact destination captured at an accepted recording stop.
///
/// The AX references stay private so this cannot be serialized, logged, or
/// reused by a later recording. Keeping both references matters for WebKit:
/// activating its app/window alone does not restore its internal editor focus.
pub struct FocusPin {
    pub(in crate::desktop::macos) pid: i32,
    pub(in crate::desktop::macos) process_instance: u64,
    pub(in crate::desktop::macos) bundle_identifier: String,
    pub(in crate::desktop::macos) bundle_path: String,
    pub(in crate::desktop::macos) element: CFType,
    pub(in crate::desktop::macos) window: CFType,
    _accessibility_observation: Option<AccessibilityObservationLease>,
}

impl FocusPin {
    pub fn pid(&self) -> i32 {
        self.pid
    }

    pub fn bundle_identifier(&self) -> &str {
        &self.bundle_identifier
    }

    pub fn window_identity(&self) -> isize {
        unsafe { CFHash(self.window.as_CFTypeRef()) as isize }
    }

    pub fn editor_identity(&self) -> isize {
        let window = self.window_identity() as u64;
        let editor = unsafe { CFHash(self.element.as_CFTypeRef()) as u64 };
        (window.rotate_left(17) ^ editor) as isize
    }

    /// True when `identity` names this pin's editor OR one of its ancestors.
    ///
    /// The focus observer hashes the RAW focused element; this pin resolves the
    /// editable control. Chromium apps sometimes report a container as focused:
    /// Claude can report an inner AXWebArea while the caret is in
    /// its composer (a descendant), so the two identities never matched and
    /// every such dictation went to the clipboard. A container that holds the
    /// pinned editor is the same place; an unrelated element is not. Moves
    /// between boxes inside that container are still caught by continuity.
    pub fn editor_identity_is_self_or_ancestor(&self, identity: isize) -> bool {
        if self.editor_identity() == identity {
            return true;
        }
        // Only a WEB AREA container counts (the observed Chromium behaviour), the
        // walk never reaches the window or application element (that would turn
        // the identity into "same window"), and it is bounded in hops and time.
        let window = (self.window_identity() as u64).rotate_left(17);
        let started = std::time::Instant::now();
        let mut current = self.element.clone();
        for _ in 0..40 {
            if started.elapsed() > std::time::Duration::from_millis(400) {
                return false;
            }
            let parent = unsafe {
                let current_ref = current.as_CFTypeRef() as AXUIElementRef;
                let _ = AXUIElementSetMessagingTimeout(current_ref, AX_MESSAGING_TIMEOUT_SECONDS);
                match ax_copy(current_ref, "AXParent") {
                    Some(parent) => CFType::wrap_under_create_rule(parent),
                    None => return false,
                }
            };
            let role = unsafe { ax_string(parent.as_CFTypeRef() as AXUIElementRef, "AXRole") };
            match role.as_deref() {
                Some("AXWindow") | Some("AXApplication") | None => return false,
                Some("AXWebArea") => {
                    let hash = unsafe { CFHash(parent.as_CFTypeRef()) as u64 };
                    if (window ^ hash) as isize == identity {
                        return true;
                    }
                }
                Some(_) => {}
            }
            current = parent;
        }
        false
    }

    /// True only when the retained element no longer exists in the app
    /// (`kAXErrorInvalidUIElement`). A slow or failed read is NOT "gone".
    pub fn element_is_gone(&self) -> bool {
        unsafe {
            let element = self.element.as_CFTypeRef() as AXUIElementRef;
            let _ = AXUIElementSetMessagingTimeout(element, AX_MESSAGING_TIMEOUT_SECONDS);
            matches!(ax_copy_with_error(element, "AXRole"), Err(-25202))
        }
    }
}

// AXUIElement references are CoreFoundation objects and are retained by this
// value. They are only used behind the runtime's existing synchronization;
// marking the opaque pin transferable lets the detached delivery worker carry
// its per-session state without exposing raw pointers to callers.

unsafe impl Send for FocusPin {}

unsafe impl Sync for FocusPin {}

/// App-level `AXFocusedUIElement` for `pid`, verified to belong to `pid`.
pub(in crate::desktop::macos) fn app_focused_element_for_pid(pid: i32) -> Option<CFType> {
    unsafe {
        let app = ax_app_element(pid)?;
        let started = std::time::Instant::now();
        let focused =
            ax_copy_with_error(app.as_CFTypeRef() as AXUIElementRef, "AXFocusedUIElement");
        drop(app);
        let focused = match focused {
            Ok(focused) => CFType::wrap_under_create_rule(focused),
            Err(error) => {
                log_ax_query_failure("application", "AXFocusedUIElement", error, started);
                return None;
            }
        };
        let focused_ref = focused.as_CFTypeRef() as AXUIElementRef;
        if ax_element_pid(focused_ref) != Some(pid) {
            return None;
        }
        let _ = AXUIElementSetMessagingTimeout(focused_ref, AX_MESSAGING_TIMEOUT_SECONDS);
        Some(focused)
    }
}

pub(in crate::desktop::macos) fn system_focused_element() -> Option<CFType> {
    unsafe {
        let system = ax_system_element()?;
        let system_ref = system.as_CFTypeRef() as AXUIElementRef;
        let started = std::time::Instant::now();
        let focused = match ax_copy_with_error(system_ref, "AXFocusedUIElement") {
            Ok(focused) => focused,
            Err(error) => {
                log_ax_query_failure("system", "AXFocusedUIElement", error, started);
                return None;
            }
        };
        let focused = CFType::wrap_under_create_rule(focused);
        let _ = AXUIElementSetMessagingTimeout(
            focused.as_CFTypeRef() as AXUIElementRef,
            AX_MESSAGING_TIMEOUT_SECONDS,
        );
        Some(focused)
    }
}

/// Why `capture_focus_pin_detailed` returned no pin. Carries only which
/// internal check failed — never AX content (no titles, no text, no bundle
/// identifiers) — so it is safe to fold into telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPinMiss {
    NotTrusted,
    NoFrontmostPid,
    NoAppIdentity {
        attempted_pid: i32,
        cause: RunningApplicationIdentityMiss,
    },
    NoProcessInstance {
        attempted_pid: i32,
        cause: RunningProcessInstanceMiss,
    },
    ObservationFailed,
    NoSystemFocusedElement,
    DescendantDeadline,
    ElementPidMismatch,
    SecureOrUnknownRole,
    NoWindow,
}

impl FocusPinMiss {
    pub fn label(self) -> &'static str {
        match self {
            FocusPinMiss::NotTrusted => "not_trusted",
            FocusPinMiss::NoFrontmostPid => "no_frontmost_pid",
            FocusPinMiss::NoAppIdentity { .. } => "no_app_identity",
            FocusPinMiss::NoProcessInstance { .. } => "no_process_instance",
            FocusPinMiss::ObservationFailed => "observation_failed",
            FocusPinMiss::NoSystemFocusedElement => "no_system_focused_element",
            FocusPinMiss::DescendantDeadline => "descendant_deadline",
            FocusPinMiss::ElementPidMismatch => "element_pid_mismatch",
            FocusPinMiss::SecureOrUnknownRole => "secure_or_unknown_role",
            FocusPinMiss::NoWindow => "no_window",
        }
    }

    /// Content-free cause for diagnostics. The existing app-identity label is
    /// preserved while process-instance failures are now separated.
    pub fn detail_label(self) -> &'static str {
        match self {
            FocusPinMiss::NoAppIdentity { cause, .. } => cause.label(),
            FocusPinMiss::NoProcessInstance { cause, .. } => cause.label(),
            other => other.label(),
        }
    }

    pub fn attempted_pid(self) -> Option<i32> {
        match self {
            FocusPinMiss::NoAppIdentity { attempted_pid, .. }
            | FocusPinMiss::NoProcessInstance { attempted_pid, .. } => Some(attempted_pid),
            _ => None,
        }
    }
}

/// Capture exact system focus, its owning window, and owning process at a
/// recording-stop boundary. Unknown or secure roles are rejected so delivery
/// cannot write to a destination whose security state is unproven.
///
/// Capture exact destination at recording stop. The caller must invoke this
/// only after the first accepted stop, never from recording-start handling.
pub fn capture_stop_focus_pin() -> Result<FocusPin, FocusPinMiss> {
    capture_focus_pin_detailed()
}

/// Same capture as [`capture_stop_focus_pin`], but on failure reports which check
/// rejected the focus so callers can log the cause instead of inferring it.
pub fn capture_focus_pin_detailed() -> Result<FocusPin, FocusPinMiss> {
    unsafe {
        if AXIsProcessTrusted() == 0 {
            return Err(FocusPinMiss::NotTrusted);
        }
        // Resolve app identity and acquire renderer observation before the
        // first focused-element read. This is what wakes dormant Chromium /
        // Electron trees in time for an exact focus pin.
        let pid = frontmost_pid().ok_or(FocusPinMiss::NoFrontmostPid)?;
        let identity = running_application_identity_detailed(pid).map_err(|cause| {
            FocusPinMiss::NoAppIdentity {
                attempted_pid: pid,
                cause,
            }
        })?;
        // A new capture supersedes any hold parked by the previous recording.
        release_pending_accessibility_observation();
        let accessibility_observation = match begin_accessibility_observation(&identity) {
            Ok(lease) => lease,
            Err(()) => return Err(FocusPinMiss::ObservationFailed),
        };
        // From here on a miss must not drop a freshly granted lease: the
        // tree may still be waking (ChatGPT/Codex: ~2.5 s), and the
        // delivery-time probe needs it awake. Park it instead.
        let park =
            |lease: Option<AccessibilityObservationLease>, miss: FocusPinMiss| -> FocusPinMiss {
                if let Some(lease) = lease {
                    if let Ok(mut slot) = PENDING_OBSERVATION_HOLD.lock() {
                        *slot = Some(lease);
                    }
                }
                miss
            };
        // System-wide focus follows the key window. An app that is frontmost
        // but whose window is not yet key (measured on ChatGPT/Codex after a
        // scripted activation) reports its focused control only at the app
        // level, which is what the delivery-time probe already reads. Use
        // the same fallback here so the pin and the probe agree.
        let root = match system_focused_element().or_else(|| app_focused_element_for_pid(pid)) {
            Some(root) => root,
            None => {
                return Err(park(
                    accessibility_observation,
                    FocusPinMiss::NoSystemFocusedElement,
                ))
            }
        };
        let root_ref = root.as_CFTypeRef() as AXUIElementRef;
        let root_role = ax_string(root_ref, "AXRole");
        // Chromium may keep system focus at its AXWebArea until renderer
        // observation is enabled above. Resolve exact focused editor from newly
        // available tree before pinning; root web area is never an input pin.
        let element = if matches!(
            root_role.as_deref(),
            Some("AXWebArea" | "AXGroup" | "AXDocument")
        ) {
            match focused_descendant(
                &root,
                std::time::Instant::now() + std::time::Duration::from_millis(350),
            ) {
                Some(descendant) => descendant,
                None => {
                    // Content-free: role string only, never window titles or text.
                    eprintln!(
                        "focus_pin_descendant_deadline root_role={}",
                        root_role.as_deref().unwrap_or("unknown")
                    );
                    return Err(park(
                        accessibility_observation,
                        FocusPinMiss::DescendantDeadline,
                    ));
                }
            }
        } else {
            root
        };
        let element_ref = element.as_CFTypeRef() as AXUIElementRef;
        let _ = AXUIElementSetMessagingTimeout(element_ref, AX_MESSAGING_TIMEOUT_SECONDS);
        if ax_element_pid(element_ref) != Some(identity.pid) {
            return Err(park(
                accessibility_observation,
                FocusPinMiss::ElementPidMismatch,
            ));
        }
        let element_role = ax_string(element_ref, "AXRole");
        let element_subrole = ax_string(element_ref, "AXSubrole");
        if secure_state_for_roles(element_role.as_deref(), element_subrole.as_deref())
            != Some(false)
        {
            // Content-free: role strings only, never titles or text.
            eprintln!(
                "focus_pin_role_rejected root_role={} role={} subrole={}",
                root_role.as_deref().unwrap_or("unknown"),
                element_role.as_deref().unwrap_or("unknown"),
                element_subrole.as_deref().unwrap_or("none")
            );
            return Err(park(
                accessibility_observation,
                FocusPinMiss::SecureOrUnknownRole,
            ));
        }
        let window = match ax_copy(element_ref, "AXWindow")
            .map(|window| CFType::wrap_under_create_rule(window))
            .or_else(|| focused_window_for_pid(pid))
        {
            Some(window) => window,
            None => return Err(park(accessibility_observation, FocusPinMiss::NoWindow)),
        };
        let _ = AXUIElementSetMessagingTimeout(
            window.as_CFTypeRef() as AXUIElementRef,
            AX_MESSAGING_TIMEOUT_SECONDS,
        );
        if ax_element_pid(window.as_CFTypeRef() as AXUIElementRef) != Some(pid) {
            return Err(park(accessibility_observation, FocusPinMiss::NoWindow));
        }
        let process_instance = running_process_instance_detailed(pid).map_err(|cause| {
            FocusPinMiss::NoProcessInstance {
                attempted_pid: pid,
                cause,
            }
        })?;
        Ok(FocusPin {
            pid,
            process_instance,
            bundle_identifier: identity.bundle_identifier,
            bundle_path: identity.bundle_path,
            element,
            window,
            _accessibility_observation: accessibility_observation,
        })
    }
}

pub(in crate::desktop::macos) unsafe fn resolved_focused_editor(pid: i32) -> Option<CFType> {
    let root = system_focused_element().or_else(|| app_focused_element_for_pid(pid))?;
    let element = root.as_CFTypeRef() as AXUIElementRef;
    if ax_element_pid(element) != Some(pid) {
        return None;
    }
    if matches!(
        ax_string(element, "AXRole").as_deref(),
        Some("AXWebArea" | "AXGroup" | "AXDocument")
    ) {
        focused_descendant(
            &root,
            std::time::Instant::now() + Duration::from_millis(100),
        )
    } else {
        Some(root)
    }
}

pub(in crate::desktop::macos) unsafe fn ax_window_for_element(
    element: AXUIElementRef,
    pid: i32,
) -> Option<CFType> {
    ax_copy(element, "AXWindow")
        .map(|window| CFType::wrap_under_create_rule(window))
        .or_else(|| focused_window_for_pid(pid))
}

pub(in crate::desktop::macos) unsafe fn focus_pin_window_matches(
    element: AXUIElementRef,
    pid: i32,
    window: &CFType,
) -> bool {
    let _ = AXUIElementSetMessagingTimeout(element, AX_MESSAGING_TIMEOUT_SECONDS);
    ax_window_for_element(element, pid)
        .map(|current| current == *window)
        .unwrap_or(false)
}

pub(in crate::desktop::macos) fn focus_pin_is_current(pin: &FocusPin) -> bool {
    unsafe {
        if !focus_pin_running_identity_matches(pin) || frontmost_pid() != Some(pin.pid) {
            return false;
        }
        // Same fallback as capture: a frontmost app whose window is not key
        // reports its focused control at the app level only.
        let Some(current) = resolved_focused_editor(pin.pid) else {
            return false;
        };
        let current_ref = current.as_CFTypeRef() as AXUIElementRef;
        let _ = AXUIElementSetMessagingTimeout(current_ref, AX_MESSAGING_TIMEOUT_SECONDS);
        if ax_element_pid(current_ref) != Some(pin.pid) || current != pin.element {
            return false;
        }
        if secure_state_for_roles(
            ax_string(current_ref, "AXRole").as_deref(),
            ax_string(current_ref, "AXSubrole").as_deref(),
        ) != Some(false)
        {
            return false;
        }
        focus_pin_window_matches(current_ref, pin.pid, &pin.window)
    }
}

/// Read-only validation for a stop-time destination. It never activates,
/// raises, or focuses an app, so a user changing fields during transcription
/// is respected and cannot be redirected by delivery.
pub fn stop_focus_pin_is_current(pin: &FocusPin) -> bool {
    focus_pin_is_current(pin)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunningProcessInstanceMiss {
    InvalidPid,
    KernelQueryFailed,
    KernelShortRead,
    KernelPidMismatch,
    KernelZeroStartSeconds,
    KernelInvalidStartMicroseconds,
    KernelStartTimeOverflow,
}

impl RunningProcessInstanceMiss {
    pub fn label(self) -> &'static str {
        match self {
            Self::InvalidPid => "invalid_pid",
            Self::KernelQueryFailed => "kernel_query_failed",
            Self::KernelShortRead => "kernel_short_read",
            Self::KernelPidMismatch => "kernel_pid_mismatch",
            Self::KernelZeroStartSeconds => "kernel_zero_start_seconds",
            Self::KernelInvalidStartMicroseconds => "kernel_invalid_start_microseconds",
            Self::KernelStartTimeOverflow => "kernel_start_time_overflow",
        }
    }
}

pub(in crate::desktop::macos) fn process_instance_from_kernel_info(
    pid: i32,
    info: &ProcBsdInfo,
) -> Result<u64, RunningProcessInstanceMiss> {
    if pid <= 0 {
        return Err(RunningProcessInstanceMiss::InvalidPid);
    }
    if info.pbi_pid != pid as u32 {
        return Err(RunningProcessInstanceMiss::KernelPidMismatch);
    }
    if info.pbi_start_tvsec == 0 {
        return Err(RunningProcessInstanceMiss::KernelZeroStartSeconds);
    }
    if info.pbi_start_tvusec >= 1_000_000 {
        return Err(RunningProcessInstanceMiss::KernelInvalidStartMicroseconds);
    }
    let seconds = info
        .pbi_start_tvsec
        .checked_mul(1_000_000_000)
        .ok_or(RunningProcessInstanceMiss::KernelStartTimeOverflow)?;
    let micros = info
        .pbi_start_tvusec
        .checked_mul(1_000)
        .ok_or(RunningProcessInstanceMiss::KernelStartTimeOverflow)?;
    seconds
        .checked_add(micros)
        .ok_or(RunningProcessInstanceMiss::KernelStartTimeOverflow)
}

pub(in crate::desktop::macos) fn running_process_instance_detailed(
    pid: i32,
) -> Result<u64, RunningProcessInstanceMiss> {
    if pid <= 0 {
        return Err(RunningProcessInstanceMiss::InvalidPid);
    }
    let mut info = unsafe { std::mem::zeroed::<ProcBsdInfo>() };
    let returned = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut ProcBsdInfo).cast(),
            PROC_BSD_INFO_SIZE,
        )
    };
    if returned <= 0 {
        return Err(RunningProcessInstanceMiss::KernelQueryFailed);
    }
    if returned != PROC_BSD_INFO_SIZE {
        return Err(RunningProcessInstanceMiss::KernelShortRead);
    }
    process_instance_from_kernel_info(pid, &info)
}

/// Compatibility wrapper for callers that only need a process instance.
pub(crate) fn running_process_instance(pid: i32) -> Option<u64> {
    running_process_instance_detailed(pid).ok()
}

pub(in crate::desktop::macos) fn focus_pin_running_identity_matches(pin: &FocusPin) -> bool {
    running_process_instance(pin.pid) == Some(pin.process_instance)
        && same_running_application(pin.pid, &pin.bundle_identifier, &pin.bundle_path)
}
