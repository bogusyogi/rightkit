#[allow(unused_imports)]
use super::*;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn send_enter_keystroke() -> Result<(), DeliveryError> {
    Err(DeliveryError::new(
        "E_UNSUPPORTED",
        "send Enter is unsupported on this platform",
    ))
}

/// Put `text` on the clipboard and paste it into the focused field. Used by the
/// pill's last-5 popover for per-row "repaste". Windows-only.
#[cfg(target_os = "windows")]
pub fn paste_text(text: &str) -> Result<(), DeliveryError> {
    let target = current_target_snapshot();
    let method = crate::desktop::delivery::delivery_method();
    let mut steps = Vec::new();
    insert_transcript(
        text,
        &target,
        &method,
        false,
        false,
        None,
        &mut steps,
        &mut || {},
    )
    .map_err(|reason| DeliveryError::new("E_PASTE", reason.to_string()))
}

#[cfg(target_os = "macos")]
pub fn paste_text(text: &str) -> Result<(), DeliveryError> {
    let target = current_target_snapshot();
    let method = crate::desktop::delivery::delivery_method();
    let mut steps = Vec::new();
    insert_transcript(
        text,
        &target,
        &method,
        false,
        false,
        None,
        false,
        &mut steps,
        &mut || {},
    )
    .map_err(|reason| DeliveryError::new("E_PASTE", reason.to_string()))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn paste_text(_text: &str) -> Result<(), DeliveryError> {
    Err(DeliveryError::new(
        "E_UNSUPPORTED",
        "paste is unsupported on this platform",
    ))
}

// ---- Stop-time destination capture + read-only delivery validation ----

#[cfg(target_os = "windows")]
pub enum WindowsStopTarget {
    Uia(crate::desktop::uia_win::StopTarget),
    /// Native stop identity remains usable when an editor exposes no UIA pin.
    /// Continuity proves the destination; it does not prove insertion.
    Window {
        target: TargetSnapshot,
    },
    WezTerm {
        binding: crate::desktop::wezterm_win::WezTermPin,
        target: TargetSnapshot,
    },
}

#[cfg(target_os = "windows")]
impl WindowsStopTarget {
    pub fn snapshot(&self) -> &TargetSnapshot {
        match self {
            Self::Uia(stop) => &stop.target,
            Self::Window { target } => target,
            Self::WezTerm { target, .. } => target,
        }
    }

    pub(crate) fn uia_identity(&self) -> Option<&crate::desktop::uia_win::FocusedElementIdentity> {
        match self {
            Self::Uia(stop) => Some(&stop.identity),
            Self::Window { .. } => None,
            Self::WezTerm { .. } => None,
        }
    }

    pub(crate) fn is_gone(&self) -> Option<bool> {
        match self {
            Self::Uia(stop) => crate::desktop::uia_win::stop_target_is_gone(stop),
            Self::Window { .. } => None,
            Self::WezTerm { .. } => None,
        }
    }

    pub(crate) fn text_snapshot(
        &self,
        timeout: std::time::Duration,
    ) -> Option<crate::desktop::uia_win::TextProof> {
        match self {
            Self::Uia(stop) if stop.is_password == Some(true) => None,
            Self::Uia(stop) => crate::desktop::uia_win::stop_target_text(stop, timeout),
            Self::Window { .. } => None,
            Self::WezTerm { binding, .. } => {
                let started = std::time::Instant::now();
                let crate::desktop::wezterm_win::PinView::Ready(ready) = binding.view(timeout)
                else {
                    return None;
                };
                let read = crate::desktop::wezterm_win::read_bound_pane_text(
                    &ready.binding,
                    timeout.saturating_sub(started.elapsed()),
                )
                .ok()?;
                Some(wezterm_text_proof(&ready.binding, read))
            }
        }
    }

    /// WezTerm only: bounded pre-paste pane check, run inside `budget`. The
    /// baseline is the one captured in the background after Stop; a fresh read
    /// is attempted only when that one is missing. `None` for other targets.
    pub(crate) fn wezterm_prepaste_probe(
        &self,
        budget: std::time::Duration,
    ) -> Option<(
        crate::desktop::wezterm_win::PanePreflight,
        Option<crate::desktop::uia_win::TextProof>,
    )> {
        use crate::desktop::wezterm_win::{PanePreflight, PinView};
        let Self::WezTerm { binding, .. } = self else {
            return None;
        };
        let started = std::time::Instant::now();
        Some(match binding.view(std::time::Duration::ZERO) {
            // Resolution failed (ambiguous or changed pane): never paste blind.
            PinView::Failed => (PanePreflight::Moved, None),
            // Still resolving after transcription: proceed on HWND/process
            // identity alone (already validated) and without a baseline.
            PinView::Resolving => (PanePreflight::Unknown, None),
            PinView::Ready(ready) => {
                let want_text = ready.baseline.is_none();
                let (preflight, fresh) = crate::desktop::wezterm_win::prepaste_probe(
                    &ready.binding,
                    budget.saturating_sub(started.elapsed()),
                    want_text,
                );
                let read = fresh.or_else(|| ready.baseline.clone());
                (
                    preflight,
                    read.map(|read| wezterm_text_proof(&ready.binding, read)),
                )
            }
        })
    }

    pub(crate) fn is_wezterm(&self) -> bool {
        matches!(self, Self::WezTerm { .. })
    }
}

#[cfg(target_os = "windows")]
fn wezterm_text_proof(
    binding: &crate::desktop::wezterm_win::WezTermPaneBinding,
    read: crate::desktop::wezterm_win::WezTermReadback,
) -> crate::desktop::uia_win::TextProof {
    crate::desktop::uia_win::TextProof {
        value: None,
        document: None,
        terminal_rows: Some(read.text),
        terminal_scope_runtime_id: Some(
            [
                u64_scope_parts(read.window_id),
                u64_scope_parts(read.pane_id),
            ]
            .into_iter()
            .flatten()
            .collect(),
        ),
        terminal_scope_native_window_handle: Some(binding.hwnd),
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsStopTarget {
    fn drop(&mut self) {
        if let Self::Uia(stop) = self {
            crate::desktop::uia_win::release_stop_target(stop.element_token);
        }
    }
}

#[cfg(target_os = "windows")]
pub fn capture_stop_target_windows() -> (TargetSnapshot, Option<WindowsStopTarget>) {
    let capture_started = std::time::Instant::now();
    // Audio stop has already been dispatched by the caller. Capture the exact
    // UIA editor on the persistent MTA worker now; a missing proof means
    // recovery, never a later arbitrary foreground target.
    let mut snapshot = current_target_snapshot_windows(false, None);
    snapshot.stop_continuity = snapshot
        .process_id
        .zip(snapshot.window_handle)
        .and_then(|(pid, window)| crate::desktop::capture_focus_continuity(pid, window))
        .map(|(captured_at, process_instance)| StopContinuity {
            captured_at,
            process_instance,
            native_control: snapshot.focused_control_handle,
        });
    let retained = if snapshot
        .process_name
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case("wezterm-gui.exe"))
    {
        // Win32-only and bounded: the mux pane resolution and baseline read
        // continue on a background thread (`WezTermPin`) and are reused at
        // delivery, so no `wezterm cli` launch runs on the Stop critical path.
        let binding = snapshot
            .window_handle
            .and_then(|hwnd| {
                let (pid, creation) =
                    crate::desktop::wezterm_win::capture_process_identity(hwnd).ok()?;
                if snapshot.process_id != Some(pid) {
                    return None;
                }
                match crate::desktop::wezterm_win::WezTermPin::begin(hwnd, pid, creation) {
                    Ok(pin) => Some(pin),
                    Err(error) => {
                        rightkit_dictation_telemetry::telemetry::log_message(
                            "windows_stop_target_capture_failed",
                            &format!(
                                "kind=wezterm pid={pid} hwnd={hwnd} elapsed_ms={} error={error:?}",
                                capture_started.elapsed().as_millis(),
                            ),
                        );
                        None
                    }
                }
            })
            .map(|binding| WindowsStopTarget::WezTerm {
                binding,
                target: snapshot.clone(),
            });
        binding
    } else {
        crate::desktop::uia_win::capture_stop_target(snapshot.clone())
            .map(WindowsStopTarget::Uia)
            .or_else(|| {
                let target = WindowsStopTarget::Window {
                    target: snapshot.clone(),
                };
                validate_stop_target(&target).then_some(target)
            })
    };
    rightkit_dictation_telemetry::telemetry::log_message(
        "windows_stop_target_capture",
        &format!(
            "pid={:?} hwnd={:?} pin={} elapsed_ms={}",
            snapshot.process_id,
            snapshot.window_handle,
            retained.is_some(),
            capture_started.elapsed().as_millis(),
        ),
    );
    (snapshot, retained)
}

#[cfg(target_os = "windows")]
pub fn capture_start_destination_windows(
    initial: TargetSnapshot,
) -> (
    TargetSnapshot,
    Option<WindowsStopTarget>,
    rightkit_dictation_core::engine::RecordingContext,
    Option<u64>,
) {
    let (target, retained) = capture_stop_target_windows();
    let continuity_ok = initial
        .stop_continuity
        .as_ref()
        .zip(initial.process_id.zip(initial.window_handle))
        .is_some_and(|(token, (pid, window))| {
            crate::desktop::validate_focus_continuity(
                pid,
                window,
                token.captured_at,
                token.process_instance,
                token.native_control,
            )
        });
    let retained = (continuity_ok
        && target.process_id == initial.process_id
        && target.window_handle == initial.window_handle
        && target.foreground_target == initial.foreground_target)
        .then_some(retained)
        .flatten();
    // A stop-time UIA capture can identify the current box, but it cannot
    // retroactively prove which editor had focus at the hotkey. Bind only
    // when the hotkey continuity token named the same native control.
    let hotkey_editor = initial
        .stop_continuity
        .as_ref()
        .and_then(|token| token.native_control);
    let bound = retained.as_ref().and_then(|stop| {
        let pinned_editor = stop.uia_identity()?.native_window_handle;
        let proof_ok = continuity_ok && hotkey_editor.is_some();
        // Chromium reports a web-area/document container as focused while the
        // editor has no HWND of its own: the proof may name an ancestor.
        let names_container = proof_ok
            && hotkey_editor != Some(pinned_editor)
            && match (stop, hotkey_editor, initial.window_handle) {
                (WindowsStopTarget::Uia(uia), Some(control), Some(window)) => {
                    crate::desktop::uia_win::stop_target_names_web_container(uia, control, window)
                }
                _ => false,
            };
        if names_container {
            rightkit_dictation_telemetry::telemetry::log_message(
                "recording_start_editor_container",
                "platform=windows proof_names_ancestor=true",
            );
        }
        rightkit_transcript_library::delivery::start_capture_may_bind_via_container(
            proof_ok,
            hotkey_editor,
            pinned_editor,
            names_container,
        )
        .then_some(())
    });
    if bound.is_none() {
        rightkit_dictation_telemetry::telemetry::log_message(
            "recording_start_capture_gates",
            &format!(
                "platform=windows continuity={} hotkey_editor_present={} bound=false",
                continuity_ok,
                hotkey_editor.is_some()
            ),
        );
    }
    let retained = bound.and(retained);
    let proof = retained
        .as_ref()
        .and_then(|stop| stop.text_snapshot(std::time::Duration::from_millis(600)))
        // Terminal rows/scrollback are rendered output, never box context.
        .filter(|proof| {
            proof.terminal_rows.is_none()
                && !windows_terminal_target(
                    target.process_name.as_deref(),
                    target.window_title.as_deref(),
                    None,
                )
        });
    let fingerprint = proof.as_ref().and_then(|proof| {
        use std::hash::{Hash, Hasher};
        let text = proof.authoritative_text()?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        Some(hasher.finish())
    });
    let context = proof
        .map(|proof| {
            let field_text = proof
                .authoritative_text()
                .filter(|text| !text.trim().is_empty())
                .map(str::to_string);
            rightkit_dictation_core::engine::RecordingContext {
                app_name: target.process_name.clone(),
                window_title: target.window_title.clone(),
                selected_text: None,
                state: if field_text.is_some() {
                    rightkit_dictation_core::engine::RecordingContextState::NonEmpty
                } else {
                    rightkit_dictation_core::engine::RecordingContextState::ConfirmedEmpty
                },
                field_text,
                request_attempted: false,
            }
        })
        .unwrap_or(rightkit_dictation_core::engine::RecordingContext {
            app_name: target.process_name.clone(),
            window_title: target.window_title.clone(),
            field_text: None,
            selected_text: None,
            state: rightkit_dictation_core::engine::RecordingContextState::Unreadable,
            request_attempted: false,
        });
    let continuity_after_read = initial
        .stop_continuity
        .as_ref()
        .zip(initial.process_id.zip(initial.window_handle))
        .is_some_and(|(token, (pid, window))| {
            crate::desktop::validate_focus_continuity(
                pid,
                window,
                token.captured_at,
                token.process_instance,
                token.native_control,
            )
        });
    if !continuity_after_read {
        return (
            target,
            None,
            rightkit_dictation_core::engine::RecordingContext {
                app_name: None,
                window_title: None,
                field_text: None,
                selected_text: None,
                state: rightkit_dictation_core::engine::RecordingContextState::Unreadable,
                request_attempted: false,
            },
            None,
        );
    }
    (target, retained, context, fingerprint)
}

#[cfg(target_os = "windows")]
pub fn validate_stop_target(target: &WindowsStopTarget) -> bool {
    match target {
        WindowsStopTarget::Uia(stop) => {
            stop.target.process_id != Some(std::process::id())
                && (stop.focused_text_input != Some(false) || windows_opaque_target(&stop.target))
                && crate::desktop::uia_win::validate_stop_target(stop)
        }
        WindowsStopTarget::Window { target } => {
            target.process_id != Some(std::process::id())
                && target.focused_control_handle.is_some()
                && target
                    .stop_continuity
                    .as_ref()
                    .zip(target.process_id.zip(target.window_handle))
                    .is_some_and(|(proof, (pid, window))| {
                        crate::desktop::validate_focus_continuity(
                            pid,
                            window,
                            proof.captured_at,
                            proof.process_instance,
                            proof.native_control,
                        )
                    })
                && target_ready_for_input(target, false)
        }
        WindowsStopTarget::WezTerm { binding, .. } => {
            let started = std::time::Instant::now();
            match binding.view(std::time::Duration::from_millis(600)) {
                crate::desktop::wezterm_win::PinView::Ready(ready) => {
                    crate::desktop::wezterm_win::validate_bound_pane(
                        &ready.binding,
                        std::time::Duration::from_millis(600).saturating_sub(started.elapsed()),
                    )
                    .is_ok()
                }
                _ => false,
            }
        }
    }
}

/// Delivery-path variant of `validate_stop_target`: identical for UIA and
/// native-window targets, but a WezTerm pin is checked by foreground HWND and
/// process identity only (no CLI spawns). The pane-level check runs once, in
/// parallel with the baseline read, inside `insert_transcript`; repeating it at
/// every gate cost ~300 ms each on the field Dell.
#[cfg(target_os = "windows")]
pub fn validate_stop_target_for_delivery(target: &WindowsStopTarget) -> bool {
    match target {
        WindowsStopTarget::WezTerm { binding, .. } => {
            crate::desktop::wezterm_win::validate_bound_window(binding.window_binding()).is_ok()
        }
        other => validate_stop_target(other),
    }
}

/// Stop-time recovery for an unbound start: is the frozen stop target the same,
/// non-secure, continuous editor? The decision is the shared
/// `rightkit_transcript_library::delivery::unbound_stop_editor_is_safe`; this supplies the
/// native facts (UIA RuntimeId/HWND element match, live password probe, hotkey
/// continuity evaluated before and after that probe, live pin currency).
#[cfg(target_os = "windows")]
pub fn stop_editor_is_safe_for_recovery(
    start: Option<&TargetSnapshot>,
    stop: Option<&TargetSnapshot>,
    pin: Option<&WindowsStopTarget>,
) -> bool {
    let (Some(start), Some(stop), Some(pin)) = (start, stop, pin) else {
        rightkit_dictation_telemetry::telemetry::log_message(
            "start_context_stop_recovery",
            &format!(
                "platform=windows safe=false early=missing_input start={} stop={} pin={}",
                start.is_some(),
                stop.is_some(),
                pin.is_some()
            ),
        );
        return false;
    };
    let same_place = start.process_id.is_some()
        && start.process_id == stop.process_id
        && start.window_handle.is_some()
        && start.window_handle == stop.window_handle
        && start.foreground_target == stop.foreground_target
        && stop.process_id != Some(std::process::id());
    let continuous = || {
        start
            .stop_continuity
            .as_ref()
            .zip(start.process_id.zip(start.window_handle))
            .is_some_and(|(token, (pid, window))| {
                crate::desktop::validate_focus_continuity(
                    pid,
                    window,
                    token.captured_at,
                    token.process_instance,
                    token.native_control,
                )
            })
    };
    let before = continuous();
    if !same_place || !before {
        rightkit_dictation_telemetry::telemetry::log_message(
            "start_context_stop_recovery",
            &format!(
                "platform=windows safe=false early=same_place_or_continuity same_place={same_place} continuous_before={before} start_token={} start_pid={:?} stop_pid={:?} start_hwnd={:?} stop_hwnd={:?}",
                start.stop_continuity.is_some(),
                start.process_id,
                stop.process_id,
                start.window_handle,
                stop.window_handle
            ),
        );
        return false;
    }
    let live_secure = match pin {
        WindowsStopTarget::WezTerm { .. } => Some(false),
        _ => crate::desktop::uia_win::focused_field_is_password(),
    };
    let (same_element, non_secure, editable) = match pin {
        WindowsStopTarget::Uia(uia) => (
            start
                .stop_continuity
                .as_ref()
                .and_then(|token| token.native_control)
                .is_some_and(|control| {
                    control != 0 && control == uia.identity.native_window_handle
                }),
            match (uia.is_password, live_secure) {
                (Some(false), Some(false)) => Some(true),
                (Some(true), _) | (_, Some(true)) => Some(false),
                _ => None,
            },
            uia.focused_text_input,
        ),
        WindowsStopTarget::Window { target } => (
            false,
            live_secure.map(|secure| !secure),
            target.focused_text_input,
        ),
        WindowsStopTarget::WezTerm { .. } => (false, Some(true), Some(true)),
    };
    let current = validate_stop_target(pin);
    let after = continuous();
    let safe = current
        && rightkit_transcript_library::delivery::unbound_stop_editor_is_safe(
            same_place,
            before,
            after,
            same_element,
            non_secure,
            editable,
        );
    rightkit_dictation_telemetry::telemetry::log_message(
        "start_context_stop_recovery",
        &format!(
            "platform=windows safe={safe} same_place={same_place} continuous_before={before} continuous_after={after} same_element={same_element} non_secure={non_secure:?} editable={editable:?} current={current}"
        ),
    );
    safe
}

#[cfg(target_os = "windows")]
pub fn restore_stop_target_windows(target: &WindowsStopTarget) -> Result<(), DeliveryError> {
    // Bound delivery restores exact start window before editor verification.
    // Any failure remains a recovery path; current foreground never acquires
    // authority through this branch.
    restore_and_verify(target.snapshot())?;
    match target {
        WindowsStopTarget::Uia(stop) => crate::desktop::uia_win::restore_stop_target(stop)
            .then_some(())
            .ok_or_else(|| {
                DeliveryError::new(
                    "E_FOCUS_RESTORE",
                    "could not restore captured UIA editor focus",
                )
            }),
        WindowsStopTarget::WezTerm { .. } => Ok(()),
        WindowsStopTarget::Window { .. } => Err(DeliveryError::new(
            "E_FOCUS_RESTORE",
            "native stop target cannot restore an editor",
        )),
    }
}

#[cfg(target_os = "windows")]
fn u64_scope_parts(value: u64) -> [i32; 2] {
    [value as u32 as i32, (value >> 32) as u32 as i32]
}

/// Public snapshot of current foreground target for non-recording operations.
pub fn snapshot_target() -> TargetSnapshot {
    #[cfg(target_os = "windows")]
    {
        current_target_snapshot_windows(false, None)
    }
    #[cfg(not(target_os = "windows"))]
    current_target_snapshot()
}

/// Capture destination on the first accepted recording stop. Native
/// app/window identity is captured without any AX traversal. Exact editor
/// enrichment is intentionally left to a later worker: a stop handler cannot
/// prove that an editor observed after its boundary is still the same editor.
/// Never recapture or redirect to a newer focus.
#[cfg(target_os = "macos")]
pub fn capture_stop_target_macos() -> (TargetSnapshot, Option<crate::desktop::macos::FocusPin>) {
    let capture_started = std::time::Instant::now();
    // Cheap PID/window identity is sampled first, then exact AX editor capture
    // runs only after audio stop dispatch. An empty destination stays missing
    // rather than becoming a vacuous target that could pass validation.
    let snapshot = current_target_snapshot_for_stop().unwrap_or_else(TargetSnapshot::empty);
    let (pin, pin_result, attempted_frontmost_pid, miss_detail) =
        match crate::desktop::macos::capture_stop_focus_pin() {
            Ok(pin) => {
                let attempted_frontmost_pid = Some(pin.pid());
                (Some(pin), "captured", attempted_frontmost_pid, "captured")
            }
            Err(miss) => (
                None,
                miss.label(),
                miss.attempted_pid(),
                miss.detail_label(),
            ),
        };
    let attempted_frontmost_pid_matches_snapshot = attempted_frontmost_pid
        .zip(snapshot.process_id.map(|pid| pid as i32))
        .map(|(attempted, captured)| attempted == captured);
    let after = current_target_snapshot_for_stop();
    // AX capture may yield. A pin from a different process/window must never
    // gain authority to restore over the first native stop snapshot.
    let pin_before_filter = pin.is_some();
    let pin = pin.filter(|pin| {
        snapshot.process_id == Some(pin.pid() as u32)
            && snapshot.window_handle.is_some()
            && after.as_ref().is_some_and(|after| {
                after.process_id == snapshot.process_id
                    && after.window_handle == snapshot.window_handle
                    && after.foreground_target == snapshot.foreground_target
            })
    });
    let result = if pin_before_filter && pin.is_none() {
        "identity_changed_during_capture"
    } else {
        pin_result
    };
    rightkit_dictation_telemetry::telemetry::log_message(
        "delivery_stop_target_capture",
        &format!(
            "elapsed_ms={} result={} detail={} snapshot_pid={:?} attempted_frontmost_pid={:?} attempted_frontmost_pid_matches_snapshot={:?} snapshot_window={:?} pin_pid={:?} pin_window={:?} pin_editor={:?}",
            capture_started.elapsed().as_millis(),
            result,
            miss_detail,
            snapshot.process_id,
            attempted_frontmost_pid,
            attempted_frontmost_pid_matches_snapshot,
            snapshot.window_handle,
            pin.as_ref().map(|value| value.pid()),
            pin.as_ref().map(|value| value.window_identity()),
            pin.as_ref().map(|value| value.editor_identity()),
        ),
    );
    (snapshot, pin)
}

#[cfg(target_os = "macos")]
pub fn capture_start_destination_macos(
    initial: TargetSnapshot,
) -> (
    TargetSnapshot,
    Option<crate::desktop::macos::FocusPin>,
    rightkit_dictation_core::engine::RecordingContext,
    Option<u64>,
) {
    use crate::desktop::FocusContinuityStatus;
    let mut target = initial;
    let capture_started = std::time::Instant::now();
    let capture_budget = std::time::Duration::from_millis(1_500);
    // The hotkey-time proof is ONE instantaneous, non-blocking read on the UI
    // thread; it misses whenever the observer is mid-cycle (measured 2 of 20
    // live fn+space dictations). Acquire it here instead, off the UI
    // thread, for the SAME process + window the hotkey froze. A late proof is
    // NOT evidence of which editor had focus at the hotkey, so it never binds
    // context (see below); it only lets stop-time recovery prove continuity.
    let mut proof_miss_reason = if target.stop_continuity.is_some() {
        "present_at_hotkey"
    } else {
        "missing_native_target"
    };
    let mut proof_late_ms = 0u128;
    if target.stop_continuity.is_none() {
        if let (Some(pid), Some(window)) = (target.process_id, target.window_handle) {
            proof_miss_reason = crate::desktop::focus_continuity_capture_miss_reason(pid, window);
            while capture_started.elapsed() < std::time::Duration::from_millis(400) {
                if let Some((captured_at, process_instance, editor)) =
                    crate::desktop::capture_focus_continuity_with_editor(pid, window)
                {
                    target.stop_continuity =
                        Some(rightkit_transcript_library::delivery::StopContinuity {
                            captured_at,
                            process_instance,
                            native_control: editor,
                        });
                    proof_late_ms = capture_started.elapsed().as_millis();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    // A pending observer pass is not evidence of a changed editor. Wait for
    // original continuity, then acquire that editor and revalidate after read.
    let continuity_status = |max_wait| {
        target
            .stop_continuity
            .as_ref()
            .zip(target.process_id.zip(target.window_handle))
            .map(|(token, (pid, window))| {
                crate::desktop::wait_for_focus_continuity(
                    pid,
                    window,
                    token.captured_at,
                    token.process_instance,
                    token.native_control,
                    max_wait,
                )
            })
            .unwrap_or(FocusContinuityStatus::Invalid("capture_proof_missing"))
    };
    let capture_proof_present = target.stop_continuity.is_some();
    let before_status = continuity_status(std::time::Duration::from_millis(150));
    let continuity_before = before_status.is_valid();
    let identity_stable = current_target_snapshot_for_stop().is_some_and(|after| {
        after.process_id == target.process_id
            && after.window_handle == target.window_handle
            && after.foreground_target == target.foreground_target
    });
    let mut pin_result = "not_attempted";
    let mut pin_filter = "kept";
    let mut secure = None;
    let mut read_attempts = 0u8;
    let mut pin_attempts = 0u8;
    let mut field_read = false;
    let mut context_unsupported = false;
    let (candidate, after_status) = crate::desktop::capture_continuous_candidate(
        before_status,
        identity_stable,
        || {
            // Chromium can expose its focused editor shortly after renderer
            // observation starts. Retry only these transient AX misses while
            // the original continuity interval remains available for the
            // post-capture validation below.
            let raw_pin = loop {
                pin_attempts += 1;
                let result = crate::desktop::macos::capture_stop_focus_pin();
                let transient = matches!(
                    &result,
                    Err(crate::desktop::macos::FocusPinMiss::NoSystemFocusedElement
                        | crate::desktop::macos::FocusPinMiss::DescendantDeadline)
                );
                if !transient || pin_attempts >= 4 || capture_started.elapsed() >= capture_budget {
                    break result;
                }
                std::thread::sleep(std::time::Duration::from_millis(120));
            };
            pin_result = match &raw_pin {
                Ok(pin) if target.process_id == Some(pin.pid() as u32) => "captured",
                Ok(_) => "pid_mismatch",
                Err(miss) => miss.label(),
            };
            let pin = raw_pin.ok().filter(|pin| {
                let proof_editor = target
                    .stop_continuity
                    .as_ref()
                    .and_then(|proof| proof.native_control);
                // Debug-only QA switch: reproduce an editor-less hotkey proof on demand.
                #[cfg(feature = "test-mode")]
                let proof_editor = proof_editor
                    .filter(|_| std::env::var_os("RIGHTKIT_QA_DROP_PROOF_EDITOR").is_none());
                // The observer may name a CONTAINER of the pinned editor (see
                // `editor_identity_is_self_or_ancestor`); that is the same place.
                let proof_names_ancestor = proof_editor.is_some_and(|editor| {
                    editor != pin.editor_identity()
                        && pin.editor_identity_is_self_or_ancestor(editor)
                });
                let proof_editor = if proof_names_ancestor {
                    Some(pin.editor_identity())
                } else {
                    proof_editor
                };
                if proof_names_ancestor {
                    rightkit_dictation_telemetry::telemetry::log_message(
                        "recording_start_editor_container",
                        "proof_names_ancestor=true",
                    );
                }
                let same_pid = target.process_id == Some(pin.pid() as u32);
                // An editor-less proof (the observer had not yet seen the app's
                // focused element) cannot contradict the pin; process, window,
                // continuity and pin currency still bind it. Rejecting it sent
                // every dictation to the clipboard until the next app switch.
                // Binding needs the editor named AT the hotkey (see
                // `start_capture_may_bind`). Anything else stays unbound and is
                // delivered by the stop-time path, never to the clipboard just
                // because the identity was missing.
                let same_editor = rightkit_transcript_library::delivery::start_capture_may_bind(
                    proof_late_ms == 0 && proof_miss_reason == "present_at_hotkey",
                    proof_editor,
                    pin.editor_identity(),
                );
                let live_same = current_target_snapshot_for_stop().is_some_and(|live| {
                    live.process_id == target.process_id
                        && live.window_handle == target.window_handle
                        && live.foreground_target == target.foreground_target
                });
                let pin_current = crate::desktop::macos::stop_focus_pin_is_current(pin);
                if same_pid && !same_editor {
                    if let Some(proof_editor) = proof_editor {
                        rightkit_dictation_telemetry::telemetry::log_message(
                            "recording_start_editor_mismatch",
                            &format!(
                                "proof_editor={proof_editor} pin_editor={} observer={}",
                                pin.editor_identity(),
                                crate::desktop::focus_observer_debug_snapshot()
                            ),
                        );
                    }
                }
                if !(same_pid && same_editor && live_same && pin_current) {
                    pin_filter = if !same_pid {
                        "pid_differs"
                    } else if proof_editor.is_none() {
                        "unbound_no_editor_at_hotkey"
                    } else if !same_editor {
                        "editor_differs_from_proof"
                    } else if !live_same {
                        "live_target_differs"
                    } else {
                        "pin_not_current"
                    };
                }
                same_pid && same_editor && live_same && pin_current
            });
            secure = pin
                .as_ref()
                .map(crate::desktop::macos::focused_field_is_secure_for_pin);
            let captured = pin
                .as_ref()
                .filter(|_| secure == Some(Some(false)))
                .and_then(|pin| loop {
                    read_attempts += 1;
                    let read = match crate::desktop::macos::read_editor_context_for_pin(pin, 1_500)
                    {
                        Ok(context) => Some(context),
                        Err(
                            crate::desktop::macos::EditorContextReadFailure::UnsupportedTerminal,
                        ) => {
                            context_unsupported = true;
                            break None;
                        }
                        Err(crate::desktop::macos::EditorContextReadFailure::Unreadable) => None,
                    };
                    if read.is_some()
                        || read_attempts >= 4
                        || capture_started.elapsed() >= capture_budget
                    {
                        break read;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(120));
                });
            field_read = captured.is_some();
            (pin, captured)
        },
        || continuity_status(capture_budget.saturating_sub(capture_started.elapsed())),
    );
    let continuity_after_read = after_status.is_valid();
    // A proof acquired AFTER the hotkey cannot say which editor had focus AT
    // the hotkey: the user may have moved to another box in the same window in
    // between. Never bind context or an "original" destination to it. The late
    // proof is kept only so the stop-time recovery can show that one editor
    // stayed focused from then until stop, which pastes where the cursor is.
    let (pin, captured) = if proof_late_ms > 0 || proof_miss_reason != "present_at_hotkey" {
        (None, None)
    } else {
        candidate.unwrap_or((None, None))
    };
    // Content-free: which gate turned a start capture into `Unreadable`.
    rightkit_dictation_telemetry::telemetry::log_message(
        "recording_start_capture_gates",
        &format!(
            "capture_proof_present={capture_proof_present} proof_at_hotkey={proof_miss_reason} proof_late_ms={proof_late_ms} target_pid={} target_window={} continuity_before={continuity_before} before_reason={} identity_stable={identity_stable} pin={pin_result} pin_attempts={pin_attempts} pin_filter={pin_filter} secure={secure:?} field_read={} context_unsupported={context_unsupported} read_attempts={read_attempts} continuity_after_read={continuity_after_read} after_reason={} elapsed_ms={}",
            target.process_id.is_some(), target.window_handle.is_some(),
            before_status.label(), field_read, after_status.label(), capture_started.elapsed().as_millis()
        ),
    );
    let (pin, captured) = if continuity_after_read {
        (pin, captured)
    } else {
        (None, None)
    };
    let context = captured
        .as_ref()
        .map(|(field, _)| {
            let nonempty = field
                .field_text
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
                || field
                    .selected_text
                    .as_deref()
                    .is_some_and(|text| !text.trim().is_empty());
            rightkit_dictation_core::engine::RecordingContext {
                app_name: target.process_name.clone(),
                window_title: field
                    .window_title
                    .clone()
                    .or_else(|| target.window_title.clone()),
                field_text: field.field_text.clone(),
                selected_text: field.selected_text.clone(),
                state: if nonempty {
                    rightkit_dictation_core::engine::RecordingContextState::NonEmpty
                } else {
                    rightkit_dictation_core::engine::RecordingContextState::ConfirmedEmpty
                },
                request_attempted: false,
            }
        })
        .unwrap_or(rightkit_dictation_core::engine::RecordingContext {
            app_name: target.process_name.clone(),
            window_title: target.window_title.clone(),
            field_text: None,
            selected_text: None,
            state: rightkit_dictation_core::engine::RecordingContextState::Unreadable,
            request_attempted: false,
        });
    let fingerprint = captured.and_then(|(_, fingerprint)| fingerprint);
    (target, pin, context, fingerprint)
}

/// Cheap snapshot of the frontmost destination at recording stop. PID and
/// bundle identity come from AppKit; window identity comes from the
/// CoreGraphics window server. No AX traversal runs on stop.
#[cfg(target_os = "macos")]
pub fn current_target_snapshot_for_stop() -> Option<TargetSnapshot> {
    let app = crate::desktop::input_macos::frontmost_fast()?;
    let mut snapshot = TargetSnapshot {
        process_id: Some(app.pid as u32),
        process_name: app.name.or(app.bundle.clone()),
        window_title: None,
        window_handle: None,
        focused_control_handle: None,
        foreground_target: None,
        focused_text_input: None,
        is_elevated: None,
        stop_continuity: None,
        exe_path: app.bundle_path.clone(),
    };
    snapshot.window_handle = crate::desktop::input_macos::frontmost_window_identity(app.pid);
    snapshot.foreground_target = snapshot
        .window_handle
        .map(|handle| ForegroundTarget::WindowHandle { handle });
    Some(snapshot)
}

/// Restore foreground to a previously-captured window and verify it took.
/// Returns Err if the target can't be made foreground (gone / UIPI / elevated)
/// — the caller must NOT paste blindly then (→ error state).
#[cfg(target_os = "windows")]
pub fn restore_and_verify(target: &TargetSnapshot) -> Result<(), DeliveryError> {
    use std::{thread, time::Duration};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};
    let Some(handle) = target.window_handle else {
        return Err(DeliveryError::new(
            "E_NO_TARGET",
            "no captured window to restore",
        ));
    };
    let hwnd = handle as *mut core::ffi::c_void;
    // Common case: the target never lost foreground (the pill is WS_EX_NOACTIVATE, so
    // it doesn't steal focus). Do NOT touch focus — the target's existing keyboard
    // focus, INCLUDING Chromium/Electron's internal web-input focus, is intact, and the
    // typed Unicode keystrokes land there. Calling SetFocus on an Electron top-level
    // window would move focus off the render widget and break delivery; the old
    // child-HWND verify also false-failed for Chromium (its focus isn't a child HWND).
    if unsafe { GetForegroundWindow() } == hwnd {
        return Ok(());
    }
    // Foreground genuinely changed (the user clicked elsewhere). Best-effort bring the
    // target back; if Windows' foreground lock refuses, we fall back to the popup.
    unsafe { SetForegroundWindow(hwnd) };
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(15));
        if unsafe { GetForegroundWindow() } == hwnd {
            return Ok(());
        }
    }
    Err(DeliveryError::new(
        "E_FOCUS_RESTORE",
        "could not bring the captured target window to the foreground",
    ))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn restore_and_verify(_target: &TargetSnapshot) -> Result<(), DeliveryError> {
    unsupported_focus_restore()
}

#[cfg(target_os = "windows")]
pub fn current_target_snapshot() -> TargetSnapshot {
    current_target_snapshot_windows(true, None)
}

#[cfg(target_os = "windows")]
pub fn current_target_snapshot_for_revalidation(original: &TargetSnapshot) -> TargetSnapshot {
    current_target_snapshot_windows(true, Some(original))
}

#[cfg(target_os = "windows")]
pub fn current_target_snapshot_windows(
    probe_editability: bool,
    known_target: Option<&TargetSnapshot>,
) -> TargetSnapshot {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    };

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return TargetSnapshot {
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
            };
        }

        let mut process_id = 0u32;
        GetWindowThreadProcessId(hwnd, &mut process_id);
        let process_name = known_target
            .filter(|target| target.process_id == Some(process_id))
            .and_then(|target| target.process_name.clone())
            .or_else(|| crate::desktop::windows::process_name_for_pid(process_id));
        let exe_path = known_target
            .filter(|target| target.process_id == Some(process_id))
            .and_then(|target| target.exe_path.clone())
            .or_else(|| crate::desktop::windows::process_path_for_pid(process_id));
        let window_title = known_target
            .filter(|target| target.window_handle == Some(hwnd as isize))
            .and_then(|target| target.window_title.clone())
            .or_else(|| {
                let len = GetWindowTextLengthW(hwnd);
                if len > 0 {
                    let mut buffer = vec![0u16; len as usize + 1];
                    let copied = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
                    Some(String::from_utf16_lossy(&buffer[..copied as usize]))
                } else {
                    None
                }
            });

        let focused_control_handle = windows_focused_control_handle().map(|hwnd| hwnd as isize);
        // Let UIA report actual capability. Terminal delivery is authorized by
        // attributable text readback, so the target classifier must not turn
        // a known terminal label into synthetic editability.
        let focused_text_input = if probe_editability {
            windows_focused_text_input()
        } else {
            None
        };
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

/// Whether the element with keyboard focus is a text-editable control, via UI
/// Automation `GetFocusedElement` (the Windows analog of macOS's AX probe).
/// Native Edit/Document controls, writable ValuePattern controls, and
/// TextEditPattern contenteditable controls are accepted. Chromium builds its
/// UIA tree lazily, so a first definitive `false` is retried once after 140 ms.
/// `None` remains unknown and follows the shell's existing paste-safe policy.
#[cfg(target_os = "windows")]
pub fn windows_focused_text_input() -> Option<bool> {
    match crate::desktop::uia_win::focused_text_input() {
        editable @ Some(_) => {
            rightkit_dictation_telemetry::telemetry::log_message(
                "focused_text_input",
                &format!("platform=windows editable={editable:?}"),
            );
            editable
        }
        None => {
            rightkit_dictation_telemetry::telemetry::log_message(
                "focused_text_input_probe_timeout",
                "platform=windows timeout_ms=600",
            );
            None
        }
    }
}

#[cfg(target_os = "windows")]
pub fn windows_focused_control_handle() -> Option<*mut core::ffi::c_void> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, GUITHREADINFO,
    };

    unsafe {
        let foreground = GetForegroundWindow();
        if foreground.is_null() {
            return None;
        }
        let thread_id = GetWindowThreadProcessId(foreground, std::ptr::null_mut());
        if thread_id == 0 {
            return None;
        }
        let mut info: GUITHREADINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        if GetGUIThreadInfo(thread_id, &mut info) == 0 || info.hwndFocus.is_null() {
            return None;
        }
        Some(info.hwndFocus)
    }
}

#[cfg(target_os = "macos")]
pub fn current_target_snapshot() -> TargetSnapshot {
    match crate::desktop::input_macos::frontmost() {
        Some(app) => {
            // Read editability independently; native window identity remains
            // the continuity fallback for opaque editors.
            let focused_text_input = crate::desktop::input_macos::focused_text_input();
            let window_title = crate::desktop::input_macos::focused_window_title(app.pid);
            let window_handle = crate::desktop::input_macos::frontmost_window_identity(app.pid);
            let focused_control_handle = crate::desktop::input_macos::focused_target_identity();
            let foreground_target =
                window_handle.map(|handle| ForegroundTarget::WindowHandle { handle });
            TargetSnapshot {
                process_id: Some(app.pid as u32),
                process_name: app.name.or(app.bundle.clone()),
                window_title,
                window_handle,
                focused_control_handle,
                foreground_target,
                focused_text_input,
                is_elevated: None,
                stop_continuity: None,
                exe_path: app.bundle_path.clone(),
            }
        }
        None => TargetSnapshot {
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
        },
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn current_target_snapshot() -> TargetSnapshot {
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

#[cfg(all(test, target_os = "macos"))]
mod stop_target_freeze_tests {
    use super::super::*;
    use super::*;

    fn cheap_snapshot(pid: u32, window: Option<isize>) -> TargetSnapshot {
        TargetSnapshot {
            process_id: Some(pid),
            process_name: Some(format!("cheap-app-{pid}")),
            window_title: None,
            window_handle: window,
            focused_control_handle: None,
            foreground_target: window.map(|handle| ForegroundTarget::WindowHandle { handle }),
            focused_text_input: Some(true),
            is_elevated: None,
            stop_continuity: None,
            exe_path: None,
        }
    }

    #[test]
    fn native_window_identity_proves_same_stop_destination() {
        let stopped = cheap_snapshot(100, Some(1234));
        let live = cheap_snapshot(100, Some(1234));
        assert!(targets_match_for_delivery(&live, &stopped, false));
    }

    #[test]
    fn changed_native_window_invalidates_stop_destination() {
        let stopped = cheap_snapshot(100, Some(1234));
        let live = cheap_snapshot(100, Some(5678));
        assert!(!targets_match_for_delivery(&live, &stopped, false));
    }

    #[test]
    fn missing_native_window_never_becomes_stop_proof() {
        let stopped = cheap_snapshot(100, None);
        assert!(!targets_match_for_delivery(&stopped, &stopped, false));
    }

    #[test]
    fn continuity_observation_is_not_a_delivery_veto() {
        let mut stopped = cheap_snapshot(100, Some(1234));
        stopped.stop_continuity = Some(rightkit_transcript_library::delivery::StopContinuity {
            captured_at: 0,
            process_instance: 0,
            native_control: None,
        });
        let mut live = cheap_snapshot(100, Some(1234));
        live.focused_control_handle = Some(9876);
        assert!(targets_match_for_delivery(&live, &stopped, false));
        assert!(macos_target_ready_for_input(&stopped, false, None));
    }

    #[test]
    fn later_editor_evidence_preserves_window_fallback_but_binds_send() {
        let stopped = cheap_snapshot(100, Some(1234));
        let mut live = stopped.clone();
        live.focused_control_handle = Some(9876);
        assert!(targets_match_for_delivery(&live, &stopped, false));
        let before_paste = live.clone();
        live.focused_control_handle = Some(9877);
        assert!(!targets_match_for_delivery(&live, &before_paste, false));
        live.focused_control_handle = None;
        assert!(!targets_match_for_delivery(&live, &before_paste, false));
    }
}

/// Retry a transient read of the retained editor without treating missing
/// content as either a match or a user edit.
#[cfg(target_os = "macos")]
pub fn read_bound_editor_content_macos(
    pin: Option<&crate::desktop::macos::FocusPin>,
    expected: Option<u64>,
) -> rightkit_transcript_library::delivery::EditorContentStatus {
    use rightkit_transcript_library::delivery::{editor_content_status, EditorContentStatus};
    let Some(pin) = pin else {
        return EditorContentStatus::Unreadable;
    };
    if expected.is_none() {
        return EditorContentStatus::MissingBaseline;
    }
    let started = std::time::Instant::now();
    let budget = std::time::Duration::from_millis(150);
    for attempt in 1..=3 {
        let observed =
            crate::desktop::macos::focused_field_context_and_fingerprint_for_pin(pin, 1_500)
                .and_then(|(_, fingerprint)| fingerprint);
        let status = editor_content_status(
            expected,
            observed,
            observed.is_none() && pin.element_is_gone(),
        );
        if status != EditorContentStatus::Unreadable || attempt == 3 || started.elapsed() >= budget
        {
            if attempt > 1 || status != EditorContentStatus::Unchanged {
                rightkit_dictation_telemetry::telemetry::log_message(
                    "bound_editor_content_read",
                    &format!(
                        "status={} attempts={attempt} elapsed_ms={}",
                        status.label(),
                        started.elapsed().as_millis()
                    ),
                );
            }
            return status;
        }
        std::thread::sleep(
            std::time::Duration::from_millis(25).min(budget.saturating_sub(started.elapsed())),
        );
    }
    unreachable!("bounded read always returns on its last attempt")
}
