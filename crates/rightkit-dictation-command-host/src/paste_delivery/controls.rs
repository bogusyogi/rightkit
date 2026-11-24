//! Fresh control identity for the evidence gate: reading the focused control now, and building
//! the binding a delivery compares it with.
//!
//! The gate compares at the bound control's [`IdentityLevel`]: UIA runtime id where UIA resolves
//! one (Windows), the native focus handle / AX focused element otherwise, and the top-level
//! window only for the documented targets that cannot expose a control (an opaque app listed by
//! the host, and a WezTerm pane whose own probe returned `PanePreflight::Current`). A target
//! with no usable binding at all has none, and the gate refuses it.

#[allow(unused_imports)]
use super::*;

#[allow(unused_imports)]
use crate::evidence_gate::{
    classify_observation, control_match, recheck_observation, AppIdentity, AppObservation,
    AppRefusal, AppUse, ControlId, ControlMatch, IdentityLevel, SecureEvidence,
};

/// The foreground application as the operating system reports it now, tri-state. macOS: the
/// automation crate's `desktop::macos::frontmost()` (the AX focused-application pid, with
/// `NSWorkspace.frontmostApplication` as the fallback, resolved to `NSRunningApplication`:
/// process id, bundle identifier, bundle path). The identity is never a window title, a process
/// name or a label the target supplies. `Unknown` when the OS names no frontmost application or
/// it has no bundle identifier; `Error` for a malformed identity. Every other platform: `Error`
/// (nothing here observes an application identity there).
#[cfg(target_os = "macos")]
pub fn foreground_app_observation() -> AppObservation {
    match rightkit_automation::desktop::macos::frontmost() {
        None => AppObservation::Unknown,
        Some(app) => {
            AppObservation::from_parts(app.pid, app.bundle.as_deref(), app.bundle_path.as_deref())
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn foreground_app_observation() -> AppObservation {
    AppObservation::Error
}

/// macOS paste and Return, capture: observe the foreground application and classify it for an
/// automatic paste under the host's policy. A terminal refuses (there is no pane confirmation on
/// macOS); an unclassified application follows `UnclassifiedPaste`; unknown and error refuse. The
/// observed process must be the delivery's target process, otherwise the destination moved.
#[cfg(target_os = "macos")]
pub fn macos_capture_app(
    host: &DeliveryHost,
    target: &TargetSnapshot,
) -> Result<AppIdentity, AppRefusal> {
    let identity = classify_observation(
        host.config.macos_app_policy,
        foreground_app_observation(),
        AppUse::Paste,
    )?;
    if target.process_id != Some(identity.pid()) {
        return Err(AppRefusal::IdentityChanged);
    }
    Ok(identity)
}

/// [`macos_control_match`] followed by a fresh application observation: the application must pass
/// the paste classification again and be the identity bound at capture. This is the control
/// sampler of the gate, so the observation sits after the control read and before the modifier
/// sample, which stays last.
#[cfg(target_os = "macos")]
pub fn macos_control_match_with_app(
    host: &DeliveryHost,
    bound: Option<&ControlId>,
    bound_app: &AppIdentity,
    target: &TargetSnapshot,
    require_editable: bool,
    pin: Option<&rightkit_automation::desktop::macos::FocusPin>,
) -> ControlMatch {
    let matched = macos_control_match(host, bound, target, require_editable, pin);
    if matched != ControlMatch::Same {
        return matched;
    }
    match recheck_observation(
        host.config.macos_app_policy,
        bound_app,
        foreground_app_observation(),
        AppUse::Paste,
    ) {
        Ok(()) => ControlMatch::Same,
        Err(refusal) => ControlMatch::App(refusal),
    }
}

/// A fresh read of the focused control, as strong as `want` where the platform can give it.
/// `None` when the foreground window or its process cannot be read.
#[cfg(target_os = "windows")]
pub fn focused_control_id(host: &DeliveryHost, want: IdentityLevel) -> Option<ControlId> {
    use crate::evidence_gate::ElementKey;
    use rightkit_automation::desktop::uia_worker::NativeTarget;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };

    let (process_id, window) = unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut process_id = 0u32;
        GetWindowThreadProcessId(hwnd, &mut process_id);
        if process_id == 0 {
            return None;
        }
        (process_id, hwnd as isize)
    };
    let control = windows_focused_control_handle().map(|handle| handle as isize);
    let element = if want == IdentityLevel::Element {
        windows_uia(host)
            .focused_element_identity(&NativeTarget {
                process_id: Some(process_id),
                window_handle: Some(window),
                focused_control_handle: control,
            })
            .map(|identity| ElementKey {
                native_window: identity.native_window_handle,
                runtime_id: identity.runtime_id,
            })
    } else {
        None
    };
    Some(ControlId::new(process_id, window, control, element))
}

/// Windows: the caret of `control` right now, as UI Automation TextPattern reports it: the text
/// from the start of the document to the caret and whether the selection is collapsed.
///
/// The read is made on a short-lived helper thread (UIA calls into the target can hang), waited for
/// at most `uia_text_read_ms`, and only one such read is in flight at a time; a read that is still
/// running refuses the next. Every answer is `None` unless ALL of these hold: the focused element
/// is the bound one (process id, native window handle and UIA runtime id equal the binding's, and
/// it has keyboard focus), it is not a password field, TextPattern gave the document text AND a
/// selection range (a ValuePattern-only control has no selection and so gives `None`), and the
/// caret offset lies inside the document text on a character boundary.
#[cfg(target_os = "windows")]
pub fn focused_control_caret(
    host: &DeliveryHost,
    control: &ControlId,
) -> Option<crate::streaming::CaretRead> {
    use std::sync::atomic::{AtomicBool, Ordering};

    static BUSY: AtomicBool = AtomicBool::new(false);

    let element = control.element.as_ref()?;
    let process_id = i32::try_from(control.process_id).ok()?;
    let native_window = element.native_window;
    let runtime_id = element.runtime_id.clone();
    if BUSY.swap(true, Ordering::SeqCst) {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("streaming-caret-read".into())
        .spawn(move || {
            let read = read_bound_caret(process_id, native_window, &runtime_id);
            BUSY.store(false, Ordering::SeqCst);
            let _ = tx.send(read);
        });
    if spawned.is_err() {
        BUSY.store(false, Ordering::SeqCst);
        return None;
    }
    rx.recv_timeout(std::time::Duration::from_millis(
        host.config.uia_text_read_ms,
    ))
    .ok()
    .flatten()
}

#[cfg(target_os = "windows")]
fn read_bound_caret(
    process_id: i32,
    native_window: isize,
    runtime_id: &[i32],
) -> Option<crate::streaming::CaretRead> {
    use rightkit_automation::win::uia;

    let element = uia::focused_element()?;
    if element.pid()? != process_id
        || element.native_hwnd().unwrap_or(0) != native_window
        || element.runtime_id()?.as_slice() != runtime_id
        || !element.has_focus()
    {
        return None;
    }
    // `field_text` gives a selection only from TextPattern: (UTF-16 offset of the selection start
    // from the document start, UTF-16 length of the selection). A collapsed caret has length 0.
    let field = uia::field_text(&element)?;
    let (caret, selected) = field.selection?;
    let units: Vec<u16> = field.text.encode_utf16().collect();
    let before = units.get(..caret)?;
    Some(crate::streaming::CaretRead {
        before_caret: String::from_utf16(before).ok()?,
        collapsed: selected == 0,
    })
}

/// macOS: the caret of `control` right now, from the focused AX element: `AXValue` (the text) and
/// `AXSelectedTextRange` (UTF-16 location and length). Every answer is `None` unless ALL hold: the
/// host's focused-target identity (focused element hash mixed with its window hash, the same value
/// the binding was built from) equals the binding's, and still does after the read; the focused
/// element belongs to the bound process; it is not a secure text field; `AXValue` is a string and
/// `AXSelectedTextRange` is present and inside it. Editors that publish text only through text
/// markers (some Chromium and Electron fields) give no `AXValue` or no range and therefore `None`:
/// streaming does not type into them (fail closed). The AX messaging timeout is 100 ms per call
/// (`ax::system_wide_with` sets it as the process default, as the host's own AX code does).
#[cfg(target_os = "macos")]
pub fn focused_control_caret(
    host: &DeliveryHost,
    control: &ControlId,
) -> Option<crate::streaming::CaretRead> {
    use rightkit_automation::macos::ax::{self, AxOptions};
    use std::time::Duration;

    let want = control.control?;
    if (host.focused_target_identity)() != Some(want) {
        return None;
    }
    let timeout = Duration::from_millis(100);
    let system = ax::system_wide_with(&AxOptions::with_timeout(timeout)).ok()?;
    let focused = system.el_attr("AXFocusedUIElement")?;
    focused.set_timeout(timeout);
    if u32::try_from(focused.pid()?).ok()? != control.process_id || focused.is_secure() {
        return None;
    }
    let value = focused.value_text()?;
    let (location, length) = focused.selected_range()?;
    // The same element must still be the bound one after the reads.
    if (host.focused_target_identity)() != Some(want) {
        return None;
    }
    let units: Vec<u16> = value.encode_utf16().collect();
    let before = units.get(..location)?;
    Some(crate::streaming::CaretRead {
        before_caret: String::from_utf16(before).ok()?,
        collapsed: length == 0,
    })
}

/// Other platforms have no caret readback: `None`, never "the caret is at the end".
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn focused_control_caret(
    _host: &DeliveryHost,
    _control: &ControlId,
) -> Option<crate::streaming::CaretRead> {
    None
}

#[cfg(target_os = "macos")]
pub fn focused_control_id(host: &DeliveryHost, _want: IdentityLevel) -> Option<ControlId> {
    let pid = rightkit_automation::desktop::macos::frontmost_pid()?;
    let window = rightkit_automation::desktop::macos::frontmost_window_identity(pid)?;
    let control = (host.focused_target_identity)();
    Some(ControlId::new(pid as u32, window, control, None))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn focused_control_id(_host: &DeliveryHost, _want: IdentityLevel) -> Option<ControlId> {
    None
}

/// The control a Windows paste is bound to. `None` (the gate then refuses) when the target has
/// neither a UIA element identity nor a native focus handle and is not one of the two documented
/// window-level targets.
#[cfg(target_os = "windows")]
pub fn windows_bound_control(
    host: &DeliveryHost,
    target: &TargetSnapshot,
    stop_target: Option<&WindowsStopTarget>,
    identity: Option<&WindowsFocusedElementIdentity>,
) -> Option<ControlId> {
    use crate::evidence_gate::ElementKey;

    let process_id = target.process_id?;
    let window = target.window_handle?;
    // WezTerm: only `PanePreflight::Current` reaches the gate (checked by the caller before the
    // gate), and a terminal grid exposes no control. An opaque app is listed by the host
    // because its focused child changes between probes.
    if stop_target.is_some_and(WindowsStopTarget::is_wezterm) || windows_opaque_target(host, target)
    {
        return Some(ControlId::window_level(process_id, window));
    }
    let element = identity.map(|identity| ElementKey {
        native_window: identity.native_window_handle,
        runtime_id: identity.runtime_id.clone(),
    });
    let control = target.focused_control_handle;
    if element.is_none() && control.is_none() {
        return None;
    }
    Some(ControlId::new(process_id, window, control, element))
}

/// Windows: is `window` (a top-level window handle) owned by a process on `names` (the host's
/// terminal list)? Tri-state: `None` when the owning process or its name cannot be read, which is
/// never "not a terminal". Callers that decide a write refuse on `None`.
#[cfg(target_os = "windows")]
pub fn window_terminal_state(names: &[&str], window: isize) -> Option<bool> {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

    let mut process_id = 0u32;
    unsafe {
        GetWindowThreadProcessId(window as _, &mut process_id);
    }
    if process_id == 0 {
        return None;
    }
    let name = rightkit_automation::desktop::windows::process_name_for_pid(process_id)?;
    Some(names.iter().any(|listed| name.eq_ignore_ascii_case(listed)))
}

/// Windows: is the process named in `target` on the host's terminal list? `None` when the
/// snapshot has no process name (the gate then identifies the live foreground window instead).
#[cfg(target_os = "windows")]
pub fn target_terminal_state(host: &DeliveryHost, target: &TargetSnapshot) -> Option<bool> {
    let name = target.process_name.as_deref()?;
    Some(
        host.config
            .wezterm_gui_process_names
            .iter()
            .any(|listed| name.eq_ignore_ascii_case(listed)),
    )
}

/// The secure-field input of a Windows write: the UIA password probe for an ordinary target, and
/// for a terminal pane either its own evidence (`wezterm_secure_state`: none exists, `None`) under
/// `TerminalPaneSecurity::RequireEvidence`, or the named `NotApplicableByHostPolicy` under
/// `TerminalPaneSecurity::NotApplicableWhenPaneConfirmed`. Only a target bound to a WezTerm pane
/// pin can receive the exception; the pane itself is confirmed by `windows_control_match`, which
/// the gate runs right after this sample and which a write must pass as `Same`.
#[cfg(target_os = "windows")]
pub fn windows_secure_evidence(
    host: &DeliveryHost,
    stop_target: Option<&WindowsStopTarget>,
) -> SecureEvidence {
    match stop_target {
        Some(stop) if stop.is_wezterm() => match host.config.terminal_pane_security {
            TerminalPaneSecurity::RequireEvidence => {
                SecureEvidence::Probed(wezterm_secure_state(host, stop))
            }
            TerminalPaneSecurity::NotApplicableWhenPaneConfirmed => {
                SecureEvidence::NotApplicableByHostPolicy
            }
        },
        _ => SecureEvidence::Probed(windows_uia(host).focused_field_is_password()),
    }
}

/// Fresh control comparison for a Windows write: the existing target-ready check (which for a
/// retained stop target validates the pin) must hold, AND the freshly read control must equal
/// the bound one at the bound level.
///
/// Terminal identification happens here, on the FRESH foreground window, whatever entry point the
/// delivery came through: the window's process is matched against the host's terminal list
/// (`wezterm_gui_process_names`). A terminal window is accepted only for a delivery bound to a
/// WezTerm pane pin (`TerminalUnverified` otherwise, under every policy); a pane pin whose fresh
/// foreground window is not a terminal is a changed control; an unreadable process is unknown.
///
/// For a WezTerm pane the window binding cannot tell two panes of one window apart, so the bound
/// pane is re-resolved HERE, now, and must be `PanePreflight::Current` (a fresh `wezterm cli`
/// pane check, budget `wezterm_dispatch_probe_ms`). This function is the control sampler of the
/// evidence gate, so it runs once immediately before the paste chord and once immediately before
/// Return, inside each authorization; an earlier pane check carries no weight. `Moved` is a
/// changed control; `Unknown` (budget exhausted or still resolving) is an unobservable one.
#[cfg(target_os = "windows")]
pub fn windows_control_match(
    host: &DeliveryHost,
    bound: Option<&ControlId>,
    target: &TargetSnapshot,
    require_editable: bool,
    stop_target: Option<&WindowsStopTarget>,
) -> ControlMatch {
    use rightkit_automation::desktop::wezterm::PanePreflight;

    let Some(bound) = bound else {
        return ControlMatch::Unknown;
    };
    if !windows_target_ready_for_input(host, target, require_editable, stop_target) {
        return ControlMatch::Changed;
    }
    let pane = stop_target.filter(|stop| stop.is_wezterm());
    if let Some(stop) = pane {
        let state = stop.wezterm_pane_now(std::time::Duration::from_millis(
            host.config.wezterm_dispatch_probe_ms,
        ));
        host.log("delivery_wezterm_dispatch_pane", &format!("pane={state:?}"));
        match state {
            Some(PanePreflight::Current) => {}
            Some(PanePreflight::Moved) => return ControlMatch::Changed,
            Some(PanePreflight::Unknown) | None => return ControlMatch::Unknown,
        }
    }
    // The control read is last, so nothing slower sits between it and the write.
    let fresh = focused_control_id(host, bound.level);
    let Some(fresh_control) = fresh.as_ref() else {
        return ControlMatch::Unknown;
    };
    match (
        window_terminal_state(host.config.wezterm_gui_process_names, fresh_control.window),
        pane.is_some(),
    ) {
        (None, _) => return ControlMatch::Unknown,
        (Some(true), false) => return ControlMatch::TerminalUnverified,
        (Some(false), true) => return ControlMatch::Changed,
        (Some(true), true) | (Some(false), false) => {}
    }
    control_match(bound, fresh.as_ref())
}

/// Windows: is `window` (a top-level window handle) owned by a process the host lists as a
/// WezTerm GUI? Unknown (an unreadable process) is `false` here, so this is for hosts that only
/// want a quick yes; the delivery gate and streaming use [`window_terminal_state`] and refuse on
/// unknown. Streaming insertion never types into a terminal pane.
#[cfg(target_os = "windows")]
pub fn window_is_wezterm(host: &DeliveryHost, window: isize) -> bool {
    window_terminal_state(host.config.wezterm_gui_process_names, window) == Some(true)
}

/// Security state of the field focused in a streaming destination, for
/// `StreamingHost::destination_security`: `Some(false)` only when positively known non-secure,
/// `Some(true)` and `None` both refuse.
///
/// Windows: `destination` is the bound top-level window handle. A window of a process the host
/// lists as a terminal (`DeliveryConfig::wezterm_gui_process_names`) answers `None` (streaming
/// into a terminal pane stays refused; the shared streaming code refuses a terminal window itself,
/// this is the second, host-side answer); otherwise the UIA password state of the focused field.
/// macOS: `destination` is the bound process id; the AX secure-field state with the delivery's
/// retry (`focused_field_security_with_retry_macos`). Other platforms: `None`.
#[cfg(target_os = "windows")]
pub fn streaming_destination_security(host: &DeliveryHost, destination: isize) -> Option<bool> {
    if window_is_wezterm(host, destination) {
        return None;
    }
    windows_uia(host).focused_field_is_password()
}

#[cfg(target_os = "macos")]
pub fn streaming_destination_security(host: &DeliveryHost, destination: isize) -> Option<bool> {
    focused_field_security_with_retry_macos(host, destination as i32)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn streaming_destination_security(_host: &DeliveryHost, _destination: isize) -> Option<bool> {
    None
}

/// The control a macOS paste is bound to. With a retained focus pin the pin itself is the
/// control evidence (its currency is read in `macos_control_match`), so the binding is the
/// window; without one the live snapshot's AX focused element is required.
#[cfg(target_os = "macos")]
pub fn macos_bound_control(target: &TargetSnapshot, pinned: bool) -> Option<ControlId> {
    let process_id = target.process_id?;
    let window = target.window_handle?;
    if pinned {
        return Some(ControlId::window_level(process_id, window));
    }
    let control = target.focused_control_handle?;
    Some(ControlId::new(process_id, window, Some(control), None))
}

/// Fresh control comparison for a macOS write: the existing target-ready check (which for a pin
/// reads `stop_focus_pin_is_current`) must hold AND the fresh control must equal the bound one.
#[cfg(target_os = "macos")]
pub fn macos_control_match(
    host: &DeliveryHost,
    bound: Option<&ControlId>,
    target: &TargetSnapshot,
    require_editable: bool,
    pin: Option<&rightkit_automation::desktop::macos::FocusPin>,
) -> ControlMatch {
    let Some(bound) = bound else {
        return ControlMatch::Unknown;
    };
    if !macos_target_ready_for_input(host, target, require_editable, pin) {
        return ControlMatch::Changed;
    }
    control_match(bound, focused_control_id(host, bound.level).as_ref())
}
