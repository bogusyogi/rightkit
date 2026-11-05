//! Focus tracking — snapshots stop-time destination & optional live current
//! focus for delivery validation and polish context.

use rightkit_dictation_core::engine::RecordingContext;
#[cfg(target_os = "windows")]
pub(crate) use rightkit_transcript_library::delivery::windows_terminal_target;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use rightkit_transcript_library::delivery::ForegroundTarget;
use rightkit_transcript_library::delivery::TargetSnapshot;
#[cfg(target_os = "macos")]
use std::hash::{Hash, Hasher};

/// Accessibility snapshot of the focused editable field. `Some(Self)` means
/// the platform accessibility probe reached a non-password field; the text
/// values may still be `None` when that field is empty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FocusedFieldContext {
    pub window_title: Option<String>,
    pub field_text: Option<String>,
    pub selected_text: Option<String>,
}

/// Content fingerprint for selected-text replacement. It intentionally uses
/// both selected text and surrounding field text: a same-looking selection in
/// a changed editor is not proof that replacement is still safe.
#[cfg(target_os = "macos")]
pub(crate) fn selection_fingerprint(field: &FocusedFieldContext) -> Option<u64> {
    let selected = field
        .selected_text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    selected.hash(&mut hasher);
    field
        .field_text
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .hash(&mut hasher);
    Some(hasher.finish())
}

pub struct FocusTracker {
    captured: Option<TargetSnapshot>,
    current: Option<TargetSnapshot>,
    start_target: Option<TargetSnapshot>,
    start_context: Option<RecordingContext>,
    /// Stop-time AX snapshot used for macOS AI polish context.
    #[cfg(target_os = "macos")]
    captured_field: Option<FocusedFieldContext>,
    #[cfg(target_os = "macos")]
    stop_field_rx: Option<std::sync::mpsc::Receiver<Option<FocusedFieldContext>>>,
    #[cfg(target_os = "macos")]
    polish_ax_generation: Option<u64>,
}

/// Cap for captured field text — enough to ground tone/names/continuity
/// without shipping the whole document into the prompt.
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) const FIELD_CONTEXT_MAX_CHARS: usize =
    rightkit_dictation_text_pipeline::focus::FIELD_CONTEXT_MAX_CHARS;

impl FocusTracker {
    pub fn new() -> Self {
        // Windows keeps its process-lifetime observer. macOS starts none here:
        // its continuity observer is on demand and lives only in the shell
        // (see `rightkit_automation::desktop::begin_focus_coverage`), so an idle engine
        // sidecar runs no focus thread.
        #[cfg(target_os = "windows")]
        rightkit_automation::desktop::start_focus_observer();
        Self {
            captured: None,
            current: None,
            start_target: None,
            start_context: None,
            #[cfg(target_os = "macos")]
            captured_field: None,
            #[cfg(target_os = "macos")]
            stop_field_rx: None,
            #[cfg(target_os = "macos")]
            polish_ax_generation: None,
        }
    }

    /// Capture the foreground destination at recording stop. Start-time focus
    /// is intentionally never used for delivery or polish context: the caller
    /// invokes this before draining capture tail or beginning final ASR.
    pub fn snapshot_at_stop(&mut self) {
        // Stop boundary is first-write-wins. Manual stop and automatic-stop
        // races must preserve exact destination observed by earliest path.
        if self.captured.is_some() {
            return;
        }
        #[cfg_attr(not(target_os = "windows"), allow(unused_mut))]
        let mut target = current_focus_at_stop();
        // macOS: no continuity token here. The shell freezes its own stop
        // destination and token from its own observer and never adopts this
        // snapshot (`accept_sidecar_transcribing_started`), and the engine's
        // only macOS delivery (`repaste_last`) validates a live snapshot that
        // carries none.
        #[cfg(target_os = "windows")]
        {
            let (captured_at, process_instance) = target
                .process_id
                .zip(target.window_handle)
                .and_then(|(pid, window)| {
                    rightkit_automation::desktop::capture_focus_continuity(pid, window)
                })
                .unwrap_or((0, 0));
            target.stop_continuity = Some(rightkit_transcript_library::delivery::StopContinuity {
                captured_at,
                process_instance,
                native_control: target.focused_control_handle,
            });
        }
        self.current = Some(target.clone());
        self.captured = Some(target);
    }

    /// Capture field/window state at record stop while final transcription
    /// runs. Enqueue only; caller never waits for AX.
    #[cfg(target_os = "macos")]
    pub fn snapshot_at_stop_prefetch(&mut self) {
        if !crate::l3_cleanup::cleanup_enabled() || self.stop_field_rx.is_some() {
            return;
        }
        let pid = self
            .start_target
            .as_ref()
            .or(self.captured.as_ref())
            .or(self.current.as_ref())
            .and_then(|target| target.process_id)
            .map(|pid| pid as i32);
        let Some(pid) = pid else { return };
        let generation = *self.polish_ax_generation.get_or_insert_with(
            rightkit_automation::desktop::macos::new_polish_accessibility_generation,
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("hr-macos-polish-context-stop".to_string())
            .spawn(move || {
                let result = macos_polish_field_context_for_pid_at_stop(
                    pid,
                    generation,
                    FIELD_CONTEXT_MAX_CHARS,
                );
                let _ = tx.send(result);
            });
        if spawned.is_ok() {
            self.stop_field_rx = Some(rx);
        } else {
            tracing::warn!("macos_polish_stop_context_thread_spawn_failed");
        }
    }

    /// Await only detached stop-time AX work after final ASR has already run.
    /// Recording, KWS, pill, & audio threads never wait here.
    #[cfg(target_os = "macos")]
    pub fn captured_field_for_polish(&mut self) -> Option<FocusedFieldContext> {
        // Stop is authoritative. Its probe started before final ASR, so this
        // bounded receive normally returns immediately after transcription.
        let stop_field = self.stop_field_rx.take().and_then(|receiver| {
            receiver
                .recv_timeout(std::time::Duration::from_millis(450))
                .ok()
                .flatten()
        });
        if stop_field.is_some() {
            self.captured_field = stop_field;
        }
        self.captured_field.clone()
    }

    /// Re-read macOS AX immediately before a selected transform is handed to
    /// the shell. This proves same process/target plus the same non-empty
    /// selection fingerprint; a missing probe or changed field fails closed.
    #[cfg(target_os = "macos")]
    pub fn captured_selection_matches_live(&self, expected: &FocusedFieldContext) -> bool {
        // Content-free reason on every refusal: without it, this check fails
        // closed for every selected-text transform and gives no cause.
        let refuse = |reason: &'static str| {
            tracing::warn!(reason, "selected_transform_live_check_refused");
            false
        };
        let Some(expected_fingerprint) = selection_fingerprint(expected) else {
            return refuse("no_expected_selection");
        };
        // This sidecar's own NSWorkspace snapshots are frozen (no AppKit run
        // loop), so comparing two of them proved nothing and, because one
        // carries a control handle and the other does not, refused every
        // transform. With a shell-provided start target the proof is the AX
        // re-read below against THAT process: field text + selection must be
        // byte-identical to the start capture. The shell still owns where the
        // result is delivered and re-proves that destination before pasting.
        if self
            .start_target
            .as_ref()
            .and_then(|target| target.process_id)
            .is_none()
        {
            let Some(captured_target) = self.captured.as_ref() else {
                return refuse("no_captured_target");
            };
            let live_target = current_focus();
            if live_target.process_id != captured_target.process_id
                || !live_target.same_stable_target(captured_target)
            {
                return refuse("target_identity_changed");
            }
        }
        let Some(live_field) = self.focused_field_context_for_captured_target() else {
            return refuse("live_field_unreadable");
        };
        if selection_fingerprint(&live_field) != Some(expected_fingerprint) {
            return refuse(if live_field.selected_text.is_none() {
                "live_selection_missing"
            } else {
                "fingerprint_mismatch"
            });
        }
        true
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub fn focused_field_context_for_captured_target(&self) -> Option<FocusedFieldContext> {
        #[cfg(target_os = "windows")]
        {
            focused_field_context(FIELD_CONTEXT_MAX_CHARS)
        }
        #[cfg(target_os = "macos")]
        {
            // The shell's hotkey-time target is authoritative. This sidecar
            // has no AppKit run loop, so its own NSWorkspace frontmost lookup
            // is frozen at the process that launched it (the shell): ~100 of
            // 100 stop-time reads since 0.1.170 targeted the app itself.
            let pid = self
                .start_target
                .as_ref()
                .and_then(|target| target.process_id)
                .or_else(|| self.captured.as_ref()?.process_id)? as i32;
            focused_field_context_for_pid(pid, FIELD_CONTEXT_MAX_CHARS)
        }
    }

    pub fn current_target(&self) -> Option<TargetSnapshot> {
        self.current.clone()
    }

    pub fn captured_target(&self) -> Option<TargetSnapshot> {
        self.captured.clone()
    }

    pub fn set_recording_start_context(&mut self, context: Option<RecordingContext>) {
        self.start_context = context;
    }

    pub fn set_recording_start_target(&mut self, target: Option<TargetSnapshot>) {
        self.start_target = target;
    }

    pub fn recording_start_target(&self) -> Option<TargetSnapshot> {
        self.start_target.clone()
    }

    pub fn recording_start_context(&self) -> Option<RecordingContext> {
        self.start_context.clone()
    }

    pub fn refresh_current(&mut self) {
        self.current = Some(current_focus());
    }

    pub fn update_current(&mut self, target: TargetSnapshot) {
        self.current = Some(target);
    }

    pub fn reset(&mut self) {
        // Release any AXEnhancedUserInterface activation this turn triggered
        // before dropping the pid it was keyed on — otherwise a Chromium app
        // that needed escalation (ChatGPT, WhatsApp) stays in enhanced-
        // accessibility mode indefinitely, and a later pid reuse by the OS
        // would inherit a stale "already activated" cache entry.
        #[cfg(target_os = "macos")]
        if let (Some(pid), Some(generation)) = (
            self.captured
                .as_ref()
                .or(self.current.as_ref())
                .and_then(|target| target.process_id)
                .map(|pid| pid as i32),
            self.polish_ax_generation,
        ) {
            rightkit_automation::desktop::macos::release_polish_accessibility_for_pid(
                pid, generation,
            );
        }
        self.captured = None;
        self.current = None;
        self.start_target = None;
        self.start_context = None;
        #[cfg(target_os = "macos")]
        {
            self.captured_field = None;
        }
        #[cfg(target_os = "macos")]
        {
            self.stop_field_rx = None;
            self.polish_ax_generation = None;
        }
    }
}

/// Readback of the focused field's text through the platform AX (macOS) or
/// UIA (Windows) helper, for verifying the last paste before an undo.
/// `None` means unreadable (secure field, no permission, probe failure).
#[cfg(target_os = "windows")]
pub(crate) fn focused_field_text_for_undo() -> Option<String> {
    focused_field_context(FIELD_CONTEXT_MAX_CHARS)?.field_text
}

#[cfg(target_os = "macos")]
pub(crate) fn focused_field_text_for_undo() -> Option<String> {
    let pid = crate::macos_input::frontmost_pid()?;
    focused_field_context_for_pid(pid, FIELD_CONTEXT_MAX_CHARS)?.field_text
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn truncate_middle(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    let head = max_chars * 2 / 3;
    let tail = max_chars - head;
    let mut out: String = chars[..head].iter().collect();
    out.push_str(" […] ");
    out.extend(chars[chars.len() - tail..].iter());
    out
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn normalize_context_text(value: Option<&str>, max_chars: usize) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| truncate_middle(value, max_chars))
        .filter(|value| !value.is_empty())
}

/// Remove the transform source from captured field context so selected text is
/// sent exactly once: as the L2/L3 input, never again as untrusted reference.
pub fn field_text_without_selection(
    field_text: Option<&str>,
    selected_text: Option<&str>,
) -> Option<String> {
    let field_text = field_text?.trim();
    if field_text.is_empty() {
        return None;
    }
    let Some(selected_text) = selected_text.map(str::trim).filter(|text| !text.is_empty()) else {
        return Some(field_text.to_string());
    };
    let Some(start) = field_text.find(selected_text) else {
        return Some(field_text.to_string());
    };
    let end = start + selected_text.len();
    let before = field_text[..start].trim_end();
    let after = field_text[end..].trim_start();
    let remaining = match (before.is_empty(), after.is_empty()) {
        (true, true) => String::new(),
        (true, false) => after.to_string(),
        (false, true) => before.to_string(),
        (false, false) => format!("{before} {after}"),
    };
    (!remaining.is_empty()).then_some(remaining)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn focused_field_context_from_values(
    is_password: bool,
    window_title: Option<&str>,
    field_text: Option<&str>,
    selected_text: Option<&str>,
    max_chars: usize,
) -> Option<FocusedFieldContext> {
    if is_password {
        return None;
    }
    Some(FocusedFieldContext {
        window_title: normalize_context_text(window_title, max_chars),
        field_text: normalize_context_text(field_text, max_chars),
        selected_text: normalize_context_text(selected_text, max_chars),
    })
}

#[cfg(target_os = "macos")]
fn focused_field_context_for_pid(pid: i32, max_chars: usize) -> Option<FocusedFieldContext> {
    let field = rightkit_automation::desktop::macos::focused_field_context_for_pid(pid, max_chars)?;
    focused_field_context_from_values(
        false,
        field.window_title.as_deref(),
        field.field_text.as_deref(),
        field.selected_text.as_deref(),
        max_chars,
    )
}

#[cfg(target_os = "macos")]
fn macos_polish_field_context_for_pid_at_stop(
    pid: i32,
    generation: u64,
    max_chars: usize,
) -> Option<FocusedFieldContext> {
    let field = rightkit_automation::desktop::macos::polish_field_context_for_pid_at_stop(
        pid, generation, max_chars,
    )?;
    Some(FocusedFieldContext {
        window_title: field.window_title,
        field_text: field.field_text,
        selected_text: field.selected_text,
    })
}

/// Tri-state password evidence -> ordinary-paste veto (spec 3.3). Only POSITIVE
/// password evidence (`Some(true)`) vetoes; `None` (timeout, saturated lane,
/// no element, COM failure) is Unknown, which is neither a veto nor proof of
/// safety. Never fold Unknown into `false` via `unwrap_or(false)`.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn password_state_vetoes_paste(state: Option<bool>) -> bool {
    state == Some(true)
}

/// Tri-state password evidence -> Send+Enter authority. Submitting needs
/// POSITIVE proof the destination is not a password field (`Some(false)`);
/// Unknown (`None`) is unproven and must not submit.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn password_state_allows_submit(state: Option<bool>) -> bool {
    state == Some(false)
}

/// Hard deadline for a Windows UIA probe. RightKit's `win::with_deadline`
/// runs each probe on its own COM-initialised worker, returns `None` at this
/// deadline, and refuses new work while an earlier call is still stuck, so a
/// hung target UIA server cannot wedge `StopDictation`/`CancelDictation`/
/// `GetState` or pile up threads. Password checks use the separate
/// `with_deadline_security` worker so a hung content probe cannot starve them.
#[cfg(target_os = "windows")]
const UIA_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(600);

#[cfg(target_os = "windows")]
fn bounded_uia_probe<T, F>(label: &'static str, probe: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> Option<T> + Send + 'static,
{
    let result = rightkit_automation::win::with_deadline(UIA_PROBE_TIMEOUT, |_| probe());
    if result.is_none() {
        tracing::warn!(probe = label, "uia_probe_unavailable_or_timed_out");
    }
    result.flatten()
}

#[cfg(target_os = "windows")]
fn bounded_password_uia_probe<T, F>(label: &'static str, probe: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> Option<T> + Send + 'static,
{
    let result = rightkit_automation::win::with_deadline_security(UIA_PROBE_TIMEOUT, |_| probe());
    if result.is_none() {
        tracing::warn!(probe = label, "uia_probe_unavailable_or_timed_out");
    }
    result.flatten()
}

/// Best-effort UIA probe: is the focused control a password / credential field?
///   `Some(true)`  — `CurrentIsPassword` is set. Never paste a transcript here.
///   `Some(false)` — a focused element that is definitively NOT a password field.
///   `None`        — no focused element, or UIA/COM was unavailable.
///
/// Delivery vetoes only `Some(true)` (HR-DELIVERY-004, HR-CONTROL-011).
/// `None` means UIA could not answer; per spec §3.3 it leaves an otherwise
/// valid ordinary paste eligible. Mirrors the shell twin
/// `windows_focused_field_is_password` in `src-tauri/src/delivery/backend.rs`.
#[cfg(target_os = "windows")]
pub(crate) fn windows_focused_element_is_password() -> Option<bool> {
    bounded_password_uia_probe("focused_element_is_password", || {
        rightkit_automation::win::uia::focused_element_is_password()
    })
}

/// Read the focused Windows field through UI Automation. TextPattern provides
/// both the current selection and document text for rich/web editors;
/// ValuePattern is the single-line input fallback. Password elements are
/// rejected before either value is read.
#[cfg(target_os = "windows")]
pub(crate) fn focused_field_context(max_chars: usize) -> Option<FocusedFieldContext> {
    use rightkit_automation::win::uia::focused_field_context as read;

    // RightKit's probe refuses password and non-editable elements and bounds
    // both text paths to `max_chars`. Chromium materializes its accessibility
    // tree lazily, so the first probe is also the activation poke: retry once
    // after 140 ms, inside the same bounded worker call.
    bounded_uia_probe("focused_field_context", move || {
        let attempt = |max_chars: usize| {
            let text = read(max_chars)?;
            focused_field_context_from_values(
                false,
                None,
                text.field_text.as_deref(),
                text.selected_text.as_deref(),
                max_chars,
            )
        };
        let mut context = attempt(max_chars);
        if context.is_none() {
            std::thread::sleep(std::time::Duration::from_millis(140));
            context = attempt(max_chars);
        }
        context
    })
}

/// Untruncated, unnormalized read of the focused element's text — used ONLY
/// for delivery insertion-attribution proof (HR-DELIVERY-006), never for
/// polish/context capture. `focused_field_context` above is unsuitable for
/// that job: it routes every value through `normalize_context_text`, which
/// trims and middle-truncates, either of which can corrupt an exact
/// before/after occurrence-count delta
/// (`rightkit_transcript_library::delivery::insertion_attributable`) — a trimmed edge or
/// a truncated middle can silently change or hide the count. This function
/// uses RightKit's `focused_field_raw_text(None)` (TextPattern first,
/// ValuePattern fallback, password fields refused), with no editable gate
/// and no normalize step, bounded the same way.
#[cfg(target_os = "windows")]
pub(crate) fn focused_field_raw_text_for_proof() -> Option<String> {
    // Uncapped (`None`), no editable gate, no normalisation; password
    // elements give `None`.
    bounded_uia_probe("focused_field_raw_text_for_proof", || {
        rightkit_automation::win::uia::focused_field_raw_text(None)
    })
}

/// Engine focus probe note: there is no native (non-clipboard) insertion
/// path here (spec DESKTOP_PASTE_AND_MEDIA_SPEC section 4). UIA remains
/// read-only identity/security/readback evidence.
impl Default for FocusTracker {
    fn default() -> Self {
        Self::new()
    }
}

pub fn snapshot_current_target() -> TargetSnapshot {
    current_focus()
}

#[cfg(target_os = "windows")]
fn current_focus() -> TargetSnapshot {
    current_focus_with_editability(true)
}

#[cfg(target_os = "windows")]
fn current_focus_at_stop() -> TargetSnapshot {
    // Stop boundary must not wait on UIA. Target identity (foreground HWND,
    // PID, title, control HWND) is sufficient; shell performs delivery-time
    // editability validation against this stop snapshot.
    current_focus_with_editability(false)
}

#[cfg(target_os = "macos")]
fn current_focus_at_stop() -> TargetSnapshot {
    crate::macos_input::frontmost_fast()
        .map(|app| macos_target_snapshot(app, false))
        .unwrap_or_else(empty_target)
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn current_focus_at_stop() -> TargetSnapshot {
    current_focus()
}

#[cfg(target_os = "windows")]
fn current_focus_with_editability(probe_editability: bool) -> TargetSnapshot {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, GetWindowLongPtrW,
        GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, GUITHREADINFO, GWL_EXSTYLE,
    };

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return empty_target();
        }

        // The app's own overlays (the pill, the recent-transcripts popup) host their
        // UI in a WebView2/Chromium widget, which the chrome_* rule below would misread
        // as a text input — making us paste into our own pill and SKIP the no-text-field
        // fallback popup. Our overlays are the only windows we mark WS_EX_NOACTIVATE, so
        // that flag identifies them: never treat a non-activating overlay as a target.
        let is_own_overlay = (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32
            & rightkit_overlay::ex_style::NOACTIVATE)
            != 0;

        let mut process_id = 0u32;
        GetWindowThreadProcessId(hwnd, &mut process_id);
        let process_name = rightkit_automation::desktop::windows::process_name_for_pid(process_id);
        let exe_path = rightkit_automation::desktop::windows::process_path_for_pid(process_id);

        let len = GetWindowTextLengthW(hwnd);
        let window_title = if len > 0 {
            let mut buffer = vec![0u16; len as usize + 1];
            let copied = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
            Some(String::from_utf16_lossy(&buffer[..copied.max(0) as usize]))
        } else {
            None
        };

        let thread_id = GetWindowThreadProcessId(hwnd, std::ptr::null_mut());
        let mut focused_control_handle = None;
        let mut focused_text_input = None;
        if thread_id != 0 {
            let mut info: GUITHREADINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
            if GetGUIThreadInfo(thread_id, &mut info) != 0 && !info.hwndFocus.is_null() {
                focused_control_handle = Some(info.hwndFocus as isize);
                let mut class_buf = [0u16; 128];
                let copied = GetClassNameW(
                    info.hwndFocus,
                    class_buf.as_mut_ptr(),
                    class_buf.len() as i32,
                );
                if copied > 0 {
                    let class = String::from_utf16_lossy(&class_buf[..copied as usize])
                        .to_ascii_lowercase();
                    let is_native_edit = class == "edit"
                        || class.contains("richedit")
                        || class.contains("text")
                        || class.contains("scintilla");
                    // Electron / Chrome / Claude host their text inputs in a Chromium
                    // render-widget HWND ("Chrome_RenderWidgetHostHWND",
                    // "Chrome_WidgetWin_1"), Edge WebView2 in "WebView2" — the SAME class
                    // whether or not a text box has focus. Blindly treating the class as a
                    // text input would paste into the void when the user wasn't in a field
                    // and skip the fallback popup, silently losing the transcript.
                    // So for the Chromium case, ask UI Automation for the
                    // ACTUAL focused element (the Windows analog of the macOS AX probe):
                    // editable element → paste; non-editable → popup. UIA
                    // unavailability leaves editability unknown, so ordinary
                    // paste remains eligible under the §3.3 delivery policy.
                    let is_chromium = class.starts_with("chrome_")
                        || class.contains("chromium")
                        || class == "webview2";
                    let is_terminal = windows_terminal_target(
                        process_name.as_deref(),
                        window_title.as_deref(),
                        Some(&class),
                    );
                    focused_text_input = if is_native_edit || is_terminal {
                        Some(true)
                    } else if is_chromium && probe_editability {
                        // A timed-out/hung UIA tree is Unknown, not a proven
                        // non-editable target. Retain that uncertainty:
                        // ordinary paste may proceed, while Send+Enter still
                        // requires positive destination readiness.
                        windows_focused_element_editable()
                    } else if is_chromium {
                        None
                    } else {
                        Some(false)
                    };
                }
            }
        }

        // Force our own overlays to non-text so delivery falls to the captured target
        // or the copy-fallback popup — never types/pastes into the pill itself.
        // The HUB isn't WS_EX_NOACTIVATE, but its WebView2 matches the chrome_*
        // text-input heuristic — dictating with the hub focused could paste into
        // the app itself and skip the fallback popup entirely. Any window of
        // the app family (shell or engine sidecar) is never a paste target.
        let is_own_process = process_name
            .as_deref()
            .is_some_and(crate::host::is_own_process);
        if is_own_overlay || is_own_process {
            focused_text_input = Some(false);
        }

        TargetSnapshot {
            process_id: Some(process_id).filter(|id| *id != 0),
            process_name,
            window_title,
            window_handle: Some(hwnd as isize),
            focused_control_handle,
            foreground_target: Some(ForegroundTarget::WindowHandle {
                handle: hwnd as isize,
            }),
            focused_text_input,
            is_elevated: None,
            stop_continuity: None,
            exe_path,
        }
    }
}

// `windows_terminal_target` now lives once in `rightkit_transcript_library::delivery`
// (shared with the shell, so it no longer needs a per-crate copy) — see the `use` above.

/// Ask UI Automation whether the element with keyboard focus is editable — the
/// Windows analog of the macOS AX probe. Chromium/Electron expose their UIA tree
/// lazily; `GetFocusedElement` reads the ACTUAL focused element, so a Claude
/// window that ISN'T in a text box returns `Some(false)` (→ popup) instead of the
/// window-class guess that blind-pasted into the void. `Some(true)` = editable
/// (Edit/Document control, or a non-read-only ValuePattern — the settable-value
/// fallback that catches web/contenteditable inputs). `None` = probe failed;
/// callers retain unknown editability and apply delivery-intent policy.
#[cfg(target_os = "windows")]
fn windows_focused_element_editable() -> Option<bool> {
    // F2: bounded like every other UIA probe here (RightKit worker + deadline).
    bounded_uia_probe("focused_element_editable", || {
        // Chromium/Electron builds its UIA tree LAZILY: the FIRST probe
        // doubles as the activation poke. On a non-editable read, give the
        // tree one beat to materialize and re-probe once. RightKit's
        // `is_editable` is the shared rule (Edit/Document that can take
        // focus, writable ValuePattern, or the TextEdit pattern Chromium
        // composers expose).
        let probe = rightkit_automation::win::uia::focused_element_editable;
        let mut editable = probe();
        if editable == Some(false) {
            std::thread::sleep(std::time::Duration::from_millis(140));
            editable = probe();
        }
        editable
    })
}

#[cfg(target_os = "macos")]
fn current_focus() -> TargetSnapshot {
    crate::macos_input::frontmost()
        .map(|app| macos_target_snapshot(app, true))
        .unwrap_or_else(empty_target)
}

#[cfg(target_os = "macos")]
fn macos_target_snapshot(
    app: rightkit_automation::desktop::macos::FrontApp,
    include_editor_identity: bool,
) -> TargetSnapshot {
    let window_handle = crate::macos_input::frontmost_window_identity(app.pid);
    let focused_control_handle = include_editor_identity
        .then(|| crate::macos_input::focused_target_identity_for_pid(app.pid))
        .flatten();
    let exe_path = app.bundle_path.clone();
    TargetSnapshot {
        process_id: Some(app.pid as u32),
        process_name: app.name.or(app.bundle),
        window_title: None,
        window_handle,
        focused_control_handle,
        foreground_target: window_handle.map(|handle| ForegroundTarget::WindowHandle { handle }),
        // Target identity is captured cheaply at stop; editability/context
        // probes run only in explicit delivery or stop-polish paths.
        focused_text_input: None,
        is_elevated: None,
        stop_continuity: None,
        exe_path,
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn current_focus() -> TargetSnapshot {
    empty_target()
}

fn empty_target() -> TargetSnapshot {
    TargetSnapshot {
        process_id: None,
        process_name: None,
        window_title: None,
        window_handle: None,
        focused_control_handle: None,
        foreground_target: None,
        focused_text_input: None,
        is_elevated: None,
        stop_continuity: None,
        exe_path: None,
    }
}
