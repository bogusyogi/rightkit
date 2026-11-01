//! COM worker side: the persistent MTA read-only loop, the detached mutation
//! thread, request dispatch, and every UIA probe/mutation implementation.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::uia_text::{TerminalLimits, TextRead};
use crate::win::uia::{self, El};
use crate::Action;
use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
use windows_sys::Win32::{
    Foundation::FILETIME,
    System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
};

use rightkit_transcript_library::delivery::TargetSnapshot;

use super::protocol::{
    FocusedElementIdentity, Request, RetainedStopTarget, StopTarget, TextProof,
    MAX_PROOF_UTF16_UNITS, MAX_PROOF_UTF8_BYTES, MAX_RETAINED_STOP_TARGETS, NEXT_STOP_TOKEN,
    RETAINED_STOP_TARGET_TTL, SET_VALUE_IN_FLIGHT,
};

// The VS Code terminal static-label rule lives with RightKit's UIA text rules.
pub(super) use crate::uia_text::is_static_terminal_label;

pub(super) fn worker(request: Request, mutation: bool) {
    // RightKit joins this thread to the multithreaded apartment (idempotent).
    uia::ensure_com();
    let mut retained = HashMap::new();
    process_request(request, &mut retained);
    if mutation {
        SET_VALUE_IN_FLIGHT.store(false, Ordering::Release);
    }
}

pub(super) fn worker_loop(rx: mpsc::Receiver<Request>) {
    uia::ensure_com();
    let mut retained = HashMap::new();
    while let Ok(request) = rx.recv() {
        process_request(request, &mut retained);
    }
}

fn process_request(request: Request, retained: &mut HashMap<u64, RetainedStopTarget>) {
    prune_retained(retained);
    match request {
        Request::Editable { deadline, reply } => {
            let _ = reply.send(editable(deadline));
        }
        Request::Password { deadline, reply } => {
            let _ = reply.send(password(deadline));
        }
        Request::NativeCapability { deadline, reply } => {
            let _ = reply.send(native_capability(deadline));
        }
        Request::Identity {
            deadline,
            process_id,
            control_handle,
            window_handle,
            reply,
        } => {
            let value = if expired(deadline) {
                None
            } else {
                identity(process_id, control_handle, window_handle)
            };
            let _ = reply.send(value);
        }
        Request::ReadText {
            deadline,
            target,
            expected,
            reply,
        } => {
            let value = if expired(deadline) {
                None
            } else {
                read_text(&target, &expected, deadline)
            };
            let _ = reply.send(value);
        }
        Request::ReadStopText {
            deadline,
            stop,
            reply,
        } => {
            let value = if expired(deadline) {
                None
            } else {
                read_stop_text(retained, &stop, deadline)
            };
            let _ = reply.send(value);
        }
        Request::SetValue {
            deadline,
            text,
            target,
            expected,
            reply,
        } => {
            let value = if expired(deadline) {
                None
            } else {
                set_value_inner(deadline, &text, &target, &expected)
            };
            let _ = reply.send(value);
        }
        Request::CaptureStopTarget {
            deadline,
            target,
            reply,
        } => {
            let value = if expired(deadline) {
                None
            } else {
                capture_stop_target_inner(deadline, target, retained)
            };
            let _ = reply.send(value);
        }
        Request::ValidateStopTarget {
            deadline,
            stop,
            reply,
        } => {
            let value = !expired(deadline) && validate_stop_target_inner(deadline, &stop);
            let _ = reply.send(value);
        }
        Request::StopTargetIsGone {
            deadline,
            stop,
            reply,
        } => {
            let value = if expired(deadline) {
                None
            } else {
                stop_target_is_gone(retained, &stop)
            };
            let _ = reply.send(value);
        }
        Request::RestoreStopTarget {
            deadline,
            stop,
            reply,
        } => {
            let value = !expired(deadline) && restore_stop_target_inner(deadline, &stop, retained);
            let _ = reply.send(value);
        }
        Request::WebContainerNamed {
            deadline,
            stop,
            hotkey_control,
            window_handle,
            reply,
        } => {
            let value = !expired(deadline)
                && web_container_named(deadline, retained, &stop, hotkey_control, window_handle);
            let _ = reply.send(value);
        }
        Request::ReleaseStopTarget { token, reply } => {
            retained.remove(&token);
            let _ = reply.send(());
        }
    }
}

fn prune_retained(retained: &mut HashMap<u64, RetainedStopTarget>) {
    let now = Instant::now();
    retained.retain(|_, entry| now.duration_since(entry.captured_at) < RETAINED_STOP_TARGET_TTL);
    while retained.len() >= MAX_RETAINED_STOP_TARGETS {
        let oldest = retained
            .iter()
            .min_by_key(|(_, entry)| entry.captured_at)
            .map(|(token, _)| *token);
        let Some(token) = oldest else { break };
        retained.remove(&token);
    }
}

fn expired(deadline: Instant) -> bool {
    Instant::now() >= deadline
}

fn foreground_window() -> isize {
    // SAFETY: GetForegroundWindow has no preconditions.
    unsafe { GetForegroundWindow() as isize }
}

fn focused() -> Option<El> {
    uia::focused_element()
}

/// Native window handle of an element; 0 when it has none (RightKit reports
/// "no handle" as `None`, which is how UIA reports a virtual/web element).
fn native_handle(element: &El) -> isize {
    element.native_hwnd().unwrap_or(0)
}

fn editable_once() -> Option<bool> {
    focused()?.is_editable()
}

fn editable(deadline: Instant) -> Option<bool> {
    let first = editable_once();
    if first != Some(false) || expired(deadline) {
        return first;
    }
    let retry_at = Instant::now() + Duration::from_millis(140);
    if retry_at >= deadline {
        return first;
    }
    std::thread::sleep(Duration::from_millis(140));
    if expired(deadline) {
        first
    } else {
        editable_once()
    }
}

fn password(deadline: Instant) -> Option<bool> {
    let probe = || focused()?.password_state();
    let first = probe();
    if first.is_some() || expired(deadline) {
        return first;
    }
    let retry_at = Instant::now() + Duration::from_millis(140);
    if retry_at >= deadline {
        return first;
    }
    std::thread::sleep(Duration::from_millis(140));
    if expired(deadline) {
        None
    } else {
        probe()
    }
}

/// Chromium and Internet Explorer hosts do not take a UIA `SetValue` (the page
/// never sees an input event). RightKit does not expose UIA `FrameworkId`, so
/// the host is recognised the way those frameworks present: web content has no
/// native window handle of its own, and the hosting window classes are
/// `Chrome_*` / `Internet Explorer_Server`. An element with no native window
/// handle is refused, which also refuses XAML/WinUI fields (fail closed).
fn framework_takes_native_set_value(element: &El) -> bool {
    if element.native_hwnd().is_none() {
        return false;
    }
    let class = uia::read_node(element)
        .and_then(|node| node.subrole)
        .unwrap_or_default();
    !(class.starts_with("Chrome")
        || class.eq_ignore_ascii_case("Internet Explorer_Server")
        || class.starts_with("Internet Explorer"))
}

fn writable_value_pattern(element: &El) -> Option<bool> {
    let node = uia::read_node(element)?;
    Some(node.caps.iter().any(|cap| cap == "set_value"))
}

fn native_capability(deadline: Instant) -> Option<bool> {
    if expired(deadline) {
        return None;
    }
    let element = focused()?;
    if element.password_state()? {
        return Some(false);
    }
    if !framework_takes_native_set_value(&element) {
        return Some(false);
    }
    if !writable_value_pattern(&element)? {
        return Some(false);
    }
    // `None` (no ValuePattern, or the read failed) stays unknown.
    element.value_is_empty()
}

fn identity(
    expected_process_id: i32,
    expected_control_handle: isize,
    expected_window_handle: isize,
) -> Option<FocusedElementIdentity> {
    let element = focused()?;
    let process_id = element.pid()?;
    let native_window_handle = native_handle(&element);
    if process_id != expected_process_id
        || (native_window_handle != 0
            && expected_control_handle != 0
            && native_window_handle != expected_control_handle
            && native_window_handle != expected_window_handle)
        || !element.has_focus()
        || foreground_window() != expected_window_handle
    {
        return None;
    }
    Some(FocusedElementIdentity {
        process_id,
        native_window_handle,
        runtime_id: element.runtime_id()?,
    })
}

fn capture_stop_target_inner(
    deadline: Instant,
    target: TargetSnapshot,
    retained: &mut HashMap<u64, RetainedStopTarget>,
) -> Option<StopTarget> {
    let element = focused()?;
    let process_id = target.process_id? as i32;
    let window_handle = target.window_handle?;
    if expired(deadline)
        || element.pid()? != process_id
        || !element.has_focus()
        || foreground_window() != window_handle
    {
        return None;
    }
    let captured_process_creation_time = process_creation_time(process_id as u32)?;
    let native_window_handle = native_handle(&element);
    let identity = FocusedElementIdentity {
        process_id,
        native_window_handle,
        runtime_id: element.runtime_id()?,
    };
    let focused_text_input = element.is_editable();
    let is_password = element.password_state();
    // UIA reads above may yield to another app. Reject a capture that did not
    // finish against the same foreground window and focused runtime element.
    let final_element = focused()?;
    if expired(deadline)
        || foreground_window() != window_handle
        || process_creation_time(process_id as u32) != Some(captured_process_creation_time)
        || final_element.pid()? != process_id
        || !final_element.has_focus()
        || final_element.runtime_id()? != identity.runtime_id
    {
        return None;
    }
    let token = NEXT_STOP_TOKEN.fetch_add(1, Ordering::Relaxed).max(1);
    retained.insert(
        token,
        RetainedStopTarget {
            element: final_element,
            captured_at: Instant::now(),
        },
    );
    let mut target = target;
    target.focused_text_input = focused_text_input;
    Some(StopTarget {
        target,
        process_creation_time: captured_process_creation_time,
        identity,
        focused_text_input,
        is_password,
        element_token: token,
    })
}

fn validate_stop_target_inner(deadline: Instant, stop: &StopTarget) -> bool {
    let Some(element) = focused() else {
        return false;
    };
    if expired(deadline)
        || stop.target.window_handle != Some(foreground_window())
        || element.pid() != Some(stop.identity.process_id)
        || !element.has_focus()
    {
        return false;
    }
    if process_creation_time(stop.identity.process_id as u32) != Some(stop.process_creation_time) {
        return false;
    }
    let native = native_handle(&element);
    (stop.identity.native_window_handle == 0
        || native == 0
        || native == stop.identity.native_window_handle)
        && element
            .runtime_id()
            .is_some_and(|id| id == stop.identity.runtime_id)
}

/// Chromium reports a Document/web-area container as focused while the caret
/// is in an inner editor whose own HWND is 0. Accept the hotkey proof as
/// naming the retained element only when: the element is still the captured
/// one, positively non-password, keyboard-focusable and either has a writable
/// Value pattern, and a bounded parent walk reaches a same-process
/// Document whose native window is the proof's control, never reaching the
/// top-level window or a Window control first.
///
/// RightKit exposes no TextEdit-pattern probe on its opaque element, so the
/// TextEdit-only contenteditable case is not recognised here and fails closed
/// (the writable-Value case still binds).
fn web_container_named(
    deadline: Instant,
    retained: &HashMap<u64, RetainedStopTarget>,
    stop: &StopTarget,
    hotkey_control: isize,
    window_handle: isize,
) -> bool {
    if hotkey_control == 0 || hotkey_control == window_handle {
        return false;
    }
    let Some(entry) = retained.get(&stop.element_token) else {
        return false;
    };
    let element = &entry.element;
    if element.pid() != Some(stop.identity.process_id)
        || element.runtime_id().as_ref() != Some(&stop.identity.runtime_id)
        || element.password_state() != Some(false)
    {
        return false;
    }
    let writable_value = writable_value_pattern(element).unwrap_or(false);
    // A plain page-level Document (no writable Value / TextEdit) never binds.
    if !rightkit_transcript_library::delivery::web_area_editor_is_bindable(
        Some(false),
        element.is_keyboard_focusable().unwrap_or(false),
        writable_value,
        false,
    ) {
        return false;
    }
    let stop_at = deadline.min(Instant::now() + Duration::from_millis(400));
    let mut current = element.clone();
    for _ in 0..40 {
        if Instant::now() >= stop_at {
            return false;
        }
        let Some(parent) = current.parent() else {
            return false;
        };
        if parent.pid() != Some(stop.identity.process_id) {
            return false;
        }
        let handle = parent.native_hwnd();
        let role = uia::read_node(&parent).map(|node| node.native_role);
        let is_window = role.as_deref() == Some("UIA_Window");
        let is_document = role.as_deref() == Some("UIA_Document");
        if handle == Some(window_handle) || is_window {
            return false;
        }
        if is_document && handle == Some(hotkey_control) {
            return true;
        }
        current = parent;
    }
    false
}

/// `Some(true)` when the retained element is provably gone or replaced.
/// RightKit reports an unavailable element and a failed read the same way
/// (`pid()` is `None`), so an unreadable element stays unknown (`None`)
/// rather than "gone".
fn stop_target_is_gone(
    retained: &HashMap<u64, RetainedStopTarget>,
    stop: &StopTarget,
) -> Option<bool> {
    let element = &retained.get(&stop.element_token)?.element;
    let process_id = element.pid()?;
    if process_id != stop.identity.process_id {
        return Some(true);
    }
    element
        .runtime_id()
        .map(|id| id != stop.identity.runtime_id)
}

fn restore_stop_target_inner(
    deadline: Instant,
    stop: &StopTarget,
    retained: &HashMap<u64, RetainedStopTarget>,
) -> bool {
    if expired(deadline)
        || stop.target.window_handle != Some(foreground_window())
        || process_creation_time(stop.identity.process_id as u32)
            != Some(stop.process_creation_time)
    {
        return false;
    }
    let Some(retained) = retained.get(&stop.element_token) else {
        return false;
    };
    if retained.element.pid() != Some(stop.identity.process_id)
        || retained.element.runtime_id().as_ref() != Some(&stop.identity.runtime_id)
    {
        return false;
    }
    // Focus executes on the persistent MTA, preserving UIA apartment affinity
    // for virtual browser/chat editors with no stable child HWND. RightKit's
    // Focus refuses unless the element is in the already-foreground app.
    if uia::semantic(&retained.element, &Action::Focus).is_err() {
        return false;
    }
    std::thread::sleep(Duration::from_millis(5));
    !expired(deadline) && validate_stop_target_inner(deadline, stop)
}

fn process_creation_time(process_id: u32) -> Option<u64> {
    // SAFETY: plain Win32 process-time query on a handle opened and closed here.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id);
        if handle.is_null() {
            return None;
        }
        let mut creation: FILETIME = std::mem::zeroed();
        let mut exit: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        let ok = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
        let _ = windows_sys::Win32::Foundation::CloseHandle(handle);
        (ok != 0)
            .then_some(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
    }
}

fn matching_element(target: &TargetSnapshot, expected: &FocusedElementIdentity) -> Option<El> {
    let element = focused()?;
    let native = native_handle(&element);
    if element.pid()? != expected.process_id
        || (expected.native_window_handle != 0
            && native != 0
            && native != expected.native_window_handle)
        || element.runtime_id()? != expected.runtime_id
        || !element.has_focus()
        || element.password_state()? // unknown password state fails closed
        || target.window_handle != Some(foreground_window())
    {
        return None;
    }
    Some(element)
}

fn read_stop_text(
    retained: &HashMap<u64, RetainedStopTarget>,
    stop: &StopTarget,
    deadline: Instant,
) -> Option<TextProof> {
    let retained = retained.get(&stop.element_token)?;
    if retained.element.pid()? != stop.identity.process_id
        || process_creation_time(stop.identity.process_id as u32)
            != Some(stop.process_creation_time)
        || retained.element.runtime_id()? != stop.identity.runtime_id
    {
        return None;
    }
    // Recheck sensitive role on retained element immediately before any
    // context/fingerprint read; unknown password state fails closed.
    if retained.element.password_state()? {
        return None;
    }
    read_text_element(
        &retained.element,
        stop.identity.process_id,
        stop.target.window_handle?,
        deadline,
    )
}

fn read_text(
    target: &TargetSnapshot,
    expected: &FocusedElementIdentity,
    deadline: Instant,
) -> Option<TextProof> {
    let element = matching_element(target, expected)?;
    read_text_element(
        &element,
        expected.process_id,
        target.window_handle?,
        deadline,
    )
}

/// One bounded UIA text representation. `failed` is true when the pattern
/// answered but the text was over the proof budget (a missing pattern is not
/// a failure).
fn bounded_text(read: TextRead) -> (Option<String>, bool) {
    match read {
        TextRead::Absent => (None, false),
        TextRead::TooLarge => (None, true),
        TextRead::Text(text) if text.len() <= MAX_PROOF_UTF8_BYTES => (Some(text), false),
        TextRead::Text(_) => (None, true),
    }
}

fn read_text_element(
    element: &El,
    expected_process_id: i32,
    expected_window_handle: isize,
    deadline: Instant,
) -> Option<TextProof> {
    // Keep both independent representations. ValuePattern can expose an empty
    // or placeholder value while TextPattern has the document, and selecting
    // whichever API answered first loses useful proof evidence. Both reads
    // are bounded by UIA units and fail closed on an unknown password state.
    let (value_text, value_read_failed) =
        bounded_text(element.read_value_checked(MAX_PROOF_UTF16_UNITS));
    let (document_text, document_read_failed) =
        bounded_text(element.read_document_text_checked(MAX_PROOF_UTF16_UNITS));
    let total = value_text
        .as_ref()
        .map_or(0, String::len)
        .saturating_add(document_text.as_ref().map_or(0, String::len));
    let (mut value_text, mut document_text) = if total <= MAX_PROOF_UTF8_BYTES {
        (value_text, document_text)
    } else {
        // Preserve one complete representation when retaining both would
        // exceed the transaction budget. Prefer document text over an empty
        // ValuePattern placeholder, matching the macOS proof policy.
        match (value_text, document_text) {
            (Some(value), Some(document)) if value.is_empty() && !document.is_empty() => {
                (None, Some(document))
            }
            (Some(value), _) => (Some(value), None),
            (None, document) => (None, document),
        }
    };
    // VS Code's terminal edit exposes a static label through TextPattern and
    // an empty ValuePattern. The actual draft is exposed as ListItem rows
    // below the terminal container, so retain that representation separately.
    if value_text.as_deref().is_some_and(is_static_terminal_label) {
        value_text = None;
    }
    if document_text
        .as_deref()
        .is_some_and(is_static_terminal_label)
    {
        document_text = None;
    }
    let terminal = element
        .terminal_rows(
            expected_process_id,
            expected_window_handle,
            &TerminalLimits {
                max_rows: 256,
                max_row_utf16: MAX_PROOF_UTF16_UNITS,
                max_total_bytes: MAX_PROOF_UTF8_BYTES,
            },
            deadline,
        )
        .map(|rows| {
            (
                rows.scope_runtime_id,
                rows.scope_native_window_handle,
                rows.text,
            )
        });
    let (terminal_rows, terminal_scope_runtime_id, terminal_scope_native_window_handle) = terminal
        .map_or((None, None, None), |(id, hwnd, text)| {
            (Some(text), Some(id), Some(hwnd))
        });
    if (value_read_failed
        && document_text.as_deref().is_none_or(|text| text.is_empty())
        && terminal_rows.as_deref().is_none_or(|text| text.is_empty()))
        || (document_read_failed
            && value_text.as_deref().is_none_or(|text| text.is_empty())
            && terminal_rows.as_deref().is_none_or(|text| text.is_empty()))
    {
        return None;
    }
    if value_text.is_none()
        && document_text.is_none()
        && terminal_rows.as_deref().is_none_or(|text| text.is_empty())
    {
        return None;
    }
    let all_text_bytes = value_text
        .as_ref()
        .map_or(0, String::len)
        .saturating_add(document_text.as_ref().map_or(0, String::len))
        .saturating_add(terminal_rows.as_ref().map_or(0, String::len));
    if all_text_bytes > MAX_PROOF_UTF8_BYTES && terminal_rows.is_some() {
        // Terminal rows are the only authoritative representation for this
        // VS Code target; discard incidental editor label/value text when
        // retaining both would exceed the proof transaction budget.
        value_text = None;
        document_text = None;
    }
    let proof = TextProof {
        value: value_text,
        document: document_text,
        terminal_rows,
        terminal_scope_runtime_id,
        terminal_scope_native_window_handle,
    };
    // A readable empty field is a proven state, not a missing capture. The
    // caller distinguishes this from UIA/read failures by the outer Option.
    Some(proof)
}

fn set_value_inner(
    deadline: Instant,
    text: &str,
    target: &TargetSnapshot,
    expected: &FocusedElementIdentity,
) -> Option<bool> {
    let element = matching_element(target, expected)?;
    // Writes only when positively not a password field, writable and empty.
    element.set_value_if_empty_until(text, Some(deadline))
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
