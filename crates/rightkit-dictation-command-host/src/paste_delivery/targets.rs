#[allow(unused_imports)]
use super::*;

/// Put `text` on the clipboard and paste it into the focused field (a host's
/// "repaste" action).
#[cfg(target_os = "windows")]
pub fn paste_text(host: &'static DeliveryHost, text: &str) -> Result<(), DeliveryError> {
    let target = current_target_snapshot(host);
    let method = (host.delivery_method)();
    let mut steps = Vec::new();
    insert_transcript(
        host,
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
pub fn paste_text(host: &'static DeliveryHost, text: &str) -> Result<(), DeliveryError> {
    let target = current_target_snapshot(host);
    let method = (host.delivery_method)();
    let mut steps = Vec::new();
    insert_transcript(
        host,
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
pub fn paste_text(_host: &DeliveryHost, _text: &str) -> Result<(), DeliveryError> {
    Err(DeliveryError::new(
        "E_UNSUPPORTED",
        "paste is unsupported on this platform",
    ))
}

// ---- Stop-time destination capture + read-only delivery validation ----

#[cfg(target_os = "windows")]
use rightkit_automation::desktop::{uia_worker, wezterm};

/// The UIA stop pin, carrying the host's own persisted target snapshot.
#[cfg(target_os = "windows")]
pub type WindowsUiaStopTarget = uia_worker::StopTarget<TargetSnapshot>;

/// The UIA worker, configured from `DeliveryConfig` (deadlines, proof bounds,
/// supervision) and the fail-closed rule functions of
/// `rightkit_transcript_library::delivery` plus
/// `rightkit_automation::desktop::collapse_line_break_runs`. The rules are
/// fixed here on purpose: a host cannot pass a weaker one. One handle for the
/// process; the first host to ask configures it.
#[cfg(target_os = "windows")]
static WINDOWS_UIA: std::sync::OnceLock<uia_worker::Uia> = std::sync::OnceLock::new();

/// Bounded teardown of the UIA worker on app exit. The handle is a static and
/// is never dropped, so the exit path must call this. Does nothing when
/// delivery never started the worker.
#[cfg(target_os = "windows")]
pub fn windows_uia_shutdown(budget: std::time::Duration) {
    if let Some(uia) = WINDOWS_UIA.get() {
        let _ = uia.shutdown(budget);
    }
}

#[cfg(target_os = "windows")]
pub fn windows_uia(host: &DeliveryHost) -> &'static uia_worker::Uia {
    WINDOWS_UIA.get_or_init(|| {
        let mut config = uia_worker::Config::new(uia_worker::Rules {
            collapse_line_break_runs: rightkit_automation::desktop::collapse_line_break_runs,
            insertion_attributable: rightkit_transcript_library::delivery::insertion_attributable,
            terminal_grid_insertion_attributable:
                rightkit_transcript_library::delivery::terminal_grid_insertion_attributable,
            web_area_editor_is_bindable:
                rightkit_transcript_library::delivery::web_area_editor_is_bindable,
        });
        let values = &host.config;
        config.request_timeout = std::time::Duration::from_millis(values.uia_request_timeout_ms);
        config.max_proof_utf16_units = values.uia_max_proof_utf16_units;
        config.max_proof_utf8_bytes = values.uia_max_proof_utf8_bytes;
        config.max_retained_stop_targets = values.uia_max_retained_stop_targets;
        config.retained_stop_target_ttl =
            std::time::Duration::from_millis(values.uia_retained_stop_target_ttl_ms);
        config.transient_retry_delay =
            std::time::Duration::from_millis(values.uia_transient_retry_delay_ms);
        config.web_walk_budget = std::time::Duration::from_millis(values.uia_web_walk_budget_ms);
        config.web_walk_max_hops = values.uia_web_walk_max_hops;
        let hops = values.uia_membership_max_hops.clamp(
            *super::host::UIA_MEMBERSHIP_MAX_HOPS_RANGE.start(),
            *super::host::UIA_MEMBERSHIP_MAX_HOPS_RANGE.end(),
        );
        if hops != values.uia_membership_max_hops {
            host.log("delivery_config_clamped", "field=uia_membership_max_hops");
        }
        config.membership_max_hops = hops;
        config.terminal_max_rows = values.uia_terminal_max_rows;
        config.max_abandoned_workers = values.uia_max_abandoned_workers;
        config.abandon_grace = std::time::Duration::from_millis(values.uia_abandon_grace_ms);
        config.provider_connection_timeout =
            std::time::Duration::from_millis(values.uia_provider_connection_timeout_ms);
        config.provider_transaction_timeout =
            std::time::Duration::from_millis(values.uia_provider_transaction_timeout_ms);
        config.event_sink = Some(host.event_sink);
        uia_worker::Uia::new(config)
    })
}

/// WezTerm pane resolution with the host's budget, output cap, profile
/// resolver and event sink.
#[cfg(target_os = "windows")]
fn windows_wezterm_policy(host: &DeliveryHost) -> wezterm::WezTermPolicy {
    let mut policy = wezterm::WezTermPolicy::new(host.profile_dir);
    policy.pin_resolve_budget =
        std::time::Duration::from_millis(host.config.wezterm_pin_resolve_budget_ms);
    policy.max_output_bytes = host.config.wezterm_max_output_bytes;
    policy.event_sink = Some(host.event_sink);
    policy
}

#[cfg(target_os = "windows")]
pub fn windows_native_target(target: &TargetSnapshot) -> uia_worker::NativeTarget {
    uia_worker::NativeTarget {
        process_id: target.process_id,
        window_handle: target.window_handle,
        focused_control_handle: target.focused_control_handle,
    }
}

#[cfg(target_os = "windows")]
pub enum WindowsStopTarget {
    Uia(WindowsUiaStopTarget),
    /// Native stop identity remains usable when an editor exposes no UIA pin.
    /// Continuity proves the destination; it does not prove insertion.
    Window {
        target: TargetSnapshot,
    },
    WezTerm {
        binding: wezterm::WezTermPin,
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

    pub fn uia_identity(&self) -> Option<&uia_worker::FocusedElementIdentity> {
        match self {
            Self::Uia(stop) => Some(&stop.identity),
            Self::Window { .. } => None,
            Self::WezTerm { .. } => None,
        }
    }

    /// `None` when unknown (never "gone"). The UIA handle is process-wide and
    /// exists whenever a `Uia` pin does; without it the answer is unknown.
    pub fn is_gone(&self) -> Option<bool> {
        match self {
            Self::Uia(stop) => WINDOWS_UIA.get()?.stop_target_is_gone(stop),
            Self::Window { .. } => None,
            Self::WezTerm { .. } => None,
        }
    }

    pub fn text_snapshot(&self, timeout: std::time::Duration) -> Option<uia_worker::TextProof> {
        match self {
            Self::Uia(stop) if stop.is_password == Some(true) => None,
            Self::Uia(stop) => WINDOWS_UIA.get()?.stop_target_text(stop, timeout),
            Self::Window { .. } => None,
            Self::WezTerm { binding, .. } => {
                let started = std::time::Instant::now();
                let wezterm::PinView::Ready(ready) = binding.view(timeout) else {
                    return None;
                };
                let read = wezterm::read_bound_pane_text(
                    &ready.binding,
                    timeout.saturating_sub(started.elapsed()),
                )
                .ok()?;
                Some(wezterm::text_proof(&ready.binding, read))
            }
        }
    }

    /// WezTerm only: the pane text captured in the background right after Stop. Spawns nothing
    /// and checks nothing about the pane; it is readback context, never evidence. `None` for
    /// other targets, while the pin still resolves, or when no baseline was read.
    pub fn wezterm_baseline(&self) -> Option<uia_worker::TextProof> {
        use wezterm::PinView;
        let Self::WezTerm { binding, .. } = self else {
            return None;
        };
        match binding.view(std::time::Duration::ZERO) {
            PinView::Ready(ready) => ready
                .baseline
                .clone()
                .map(|read| wezterm::text_proof(&ready.binding, read)),
            PinView::Resolving | PinView::Failed => None,
        }
    }

    /// WezTerm only: a FRESH pane check, run now, inside `budget`. `Current` only when the bound
    /// pane is the focused client's active pane right now (`wezterm cli list-clients` and `list`,
    /// run concurrently; this spawns the CLI). The evidence gate calls it inside its sampling,
    /// immediately before the paste chord and again before Return; nothing earlier in the
    /// pipeline counts. `None` for other targets.
    pub fn wezterm_pane_now(&self, budget: std::time::Duration) -> Option<wezterm::PanePreflight> {
        use wezterm::{PanePreflight, PinView};
        let Self::WezTerm { binding, .. } = self else {
            return None;
        };
        Some(match binding.view(std::time::Duration::ZERO) {
            // Resolution failed (ambiguous or changed pane): never write blind.
            PinView::Failed => PanePreflight::Moved,
            // Still resolving after transcription: the pane is unconfirmed.
            PinView::Resolving => PanePreflight::Unknown,
            PinView::Ready(ready) => wezterm::prepaste_probe(&ready.binding, budget, false).0,
        })
    }

    pub fn is_wezterm(&self) -> bool {
        matches!(self, Self::WezTerm { .. })
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsStopTarget {
    fn drop(&mut self) {
        // The handle is process-wide and exists whenever a `Uia` pin does (the
        // pin came from it); `get` never starts a worker just to release.
        if let Self::Uia(stop) = self {
            if let Some(uia) = WINDOWS_UIA.get() {
                uia.release_stop_target(stop.element_token);
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub fn capture_stop_target_windows(
    host: &DeliveryHost,
) -> (TargetSnapshot, Option<WindowsStopTarget>) {
    let capture_started = std::time::Instant::now();
    // Audio stop has already been dispatched by the caller. Capture the exact
    // UIA editor on the persistent MTA worker now; a missing proof means
    // recovery, never a later arbitrary foreground target.
    let mut snapshot = current_target_snapshot_windows(host, false, None);
    snapshot.stop_continuity = snapshot
        .process_id
        .zip(snapshot.window_handle)
        .and_then(|(pid, window)| {
            rightkit_automation::desktop::capture_focus_continuity(pid, window)
        })
        .map(|(captured_at, process_instance)| StopContinuity {
            captured_at,
            process_instance,
            native_control: snapshot.focused_control_handle,
        });
    let retained = if snapshot.process_name.as_deref().is_some_and(|name| {
        host.config
            .wezterm_gui_process_names
            .iter()
            .any(|wezterm| name.eq_ignore_ascii_case(wezterm))
    }) {
        // Win32-only and bounded: the mux pane resolution and baseline read
        // continue on a background thread (`WezTermPin`) and are reused at
        // delivery, so no `wezterm cli` launch runs on the Stop critical path.
        let binding = snapshot
            .window_handle
            .and_then(|hwnd| {
                let (pid, creation) = wezterm::capture_process_identity(hwnd).ok()?;
                if snapshot.process_id != Some(pid) {
                    return None;
                }
                match wezterm::WezTermPin::begin(&windows_wezterm_policy(host), hwnd, pid, creation)
                {
                    Ok(pin) => Some(pin),
                    Err(error) => {
                        host.log(
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
        windows_uia(host)
            .capture_stop_target(
                windows_native_target(&snapshot),
                snapshot.clone(),
                |target, editable| target.focused_text_input = editable,
            )
            .map(WindowsStopTarget::Uia)
            .or_else(|| {
                let target = WindowsStopTarget::Window {
                    target: snapshot.clone(),
                };
                validate_stop_target(host, &target).then_some(target)
            })
    };
    host.log(
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
    host: &DeliveryHost,
    initial: TargetSnapshot,
) -> (
    TargetSnapshot,
    Option<WindowsStopTarget>,
    rightkit_dictation_core::engine::RecordingContext,
    Option<u64>,
) {
    let (target, retained) = capture_stop_target_windows(host);
    let continuity_ok = initial
        .stop_continuity
        .as_ref()
        .zip(initial.process_id.zip(initial.window_handle))
        .is_some_and(|(token, (pid, window))| {
            rightkit_automation::desktop::validate_focus_continuity(
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
                    windows_uia(host).stop_target_names_web_container(uia, control, window)
                }
                _ => false,
            };
        if names_container {
            host.log(
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
        host.log(
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
        .and_then(|stop| {
            stop.text_snapshot(std::time::Duration::from_millis(
                host.config.uia_text_read_ms,
            ))
        })
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
            rightkit_automation::desktop::validate_focus_continuity(
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
pub fn validate_stop_target(host: &DeliveryHost, target: &WindowsStopTarget) -> bool {
    match target {
        WindowsStopTarget::Uia(stop) => {
            stop.target.process_id != Some(std::process::id())
                && (stop.focused_text_input != Some(false)
                    || windows_opaque_target(host, &stop.target))
                && windows_uia(host).validate_stop_target(stop)
        }
        WindowsStopTarget::Window { target } => {
            target.process_id != Some(std::process::id())
                && target.focused_control_handle.is_some()
                && target
                    .stop_continuity
                    .as_ref()
                    .zip(target.process_id.zip(target.window_handle))
                    .is_some_and(|(proof, (pid, window))| {
                        rightkit_automation::desktop::validate_focus_continuity(
                            pid,
                            window,
                            proof.captured_at,
                            proof.process_instance,
                            proof.native_control,
                        )
                    })
                && target_ready_for_input(host, target, false)
        }
        WindowsStopTarget::WezTerm { binding, .. } => {
            let started = std::time::Instant::now();
            match binding.view(std::time::Duration::from_millis(
                host.config.wezterm_validate_budget_ms,
            )) {
                wezterm::PinView::Ready(ready) => wezterm::validate_bound_pane(
                    &ready.binding,
                    std::time::Duration::from_millis(host.config.wezterm_validate_budget_ms)
                        .saturating_sub(started.elapsed()),
                )
                .is_ok(),
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
pub fn validate_stop_target_for_delivery(host: &DeliveryHost, target: &WindowsStopTarget) -> bool {
    match target {
        WindowsStopTarget::WezTerm { binding, .. } => {
            wezterm::validate_bound_window(binding.window_binding()).is_ok()
        }
        other => validate_stop_target(host, other),
    }
}

/// Secure-field state of a WezTerm pane: `Some(false)` only on affirmative evidence that the pane
/// is not a password prompt. There is none, so this is always `None`. Under
/// `TerminalPaneSecurity::RequireEvidence` every automatic write into a pane is therefore refused
/// (explicit recovery: the clipboard is not touched and the host offers Copy); under
/// `NotApplicableWhenPaneConfirmed` the host declares the check not applicable instead
/// (`windows_secure_evidence`), and this function still returns `None`: it never invents evidence.
///
/// What was looked at (`rightkit_automation::desktop::wezterm`, the proposed module): the pane
/// binding carries `hwnd`, `pid`, `window_id`, `pane_id` and paths; the CLI answers parsed there
/// are `list` (`window_id`, `pane_id`, `is_active`) and `list-clients` (`pid`,
/// `focused_pane_id`); `get-text` returns the screen text. None of them says whether the
/// terminal is echoing input. UIA exposes no text pattern for WezTerm. Screen text can only be
/// negative evidence (a visible "password" prompt would be a positive secure signal); the absence
/// of such a prompt does not show a pane is safe, so no heuristic is substituted for evidence.
/// A real source (for example a WezTerm-documented prompt indicator or tty echo state) goes
/// here, reads fresh for the bound pane, and returns `Some(false)` only when it affirms.
#[cfg(target_os = "windows")]
pub fn wezterm_secure_state(_host: &DeliveryHost, _stop: &WindowsStopTarget) -> Option<bool> {
    None
}

/// Stop-time recovery for an unbound start: is the frozen stop target the same,
/// non-secure, continuous editor? The decision is the shared
/// `rightkit_transcript_library::delivery::unbound_stop_editor_is_safe`; this supplies the
/// native facts (UIA RuntimeId/HWND element match, live password probe, hotkey
/// continuity evaluated before and after that probe, live pin currency).
#[cfg(target_os = "windows")]
pub fn stop_editor_is_safe_for_recovery(
    host: &DeliveryHost,
    start: Option<&TargetSnapshot>,
    stop: Option<&TargetSnapshot>,
    pin: Option<&WindowsStopTarget>,
) -> bool {
    let (Some(start), Some(stop), Some(pin)) = (start, stop, pin) else {
        host.log(
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
                rightkit_automation::desktop::validate_focus_continuity(
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
        host.log(
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
        // No hard-coded answer: a pane's secure state is its own evidence (none exists: `None`).
        // The terminal-pane policy is NOT consulted here: stop-time recovery of an unbound start
        // stays refused for a pane under both policies.
        WindowsStopTarget::WezTerm { .. } => wezterm_secure_state(host, pin),
        _ => windows_uia(host).focused_field_is_password(),
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
        // A pane exposes no editability evidence either: `None`, never a hard-coded `Some(true)`.
        WindowsStopTarget::WezTerm { .. } => (false, live_secure.map(|secure| !secure), None),
    };
    let current = validate_stop_target(host, pin);
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
    host.log(
        "start_context_stop_recovery",
        &format!(
            "platform=windows safe={safe} same_place={same_place} continuous_before={before} continuous_after={after} same_element={same_element} non_secure={non_secure:?} editable={editable:?} current={current}"
        ),
    );
    safe
}

#[cfg(target_os = "windows")]
pub fn restore_stop_target_windows(
    host: &DeliveryHost,
    target: &WindowsStopTarget,
) -> Result<(), DeliveryError> {
    // Bound delivery restores exact start window before editor verification.
    // Any failure remains a recovery path; current foreground never acquires
    // authority through this branch.
    //
    // This moves focus to the bound target and writes no content into any application; the
    // paste or keystroke that may follow passes the evidence gate against fresh evidence
    // (see HANDOFF "Evidence gate" audit, row F1). A restore that was issued but not
    // verified may have moved focus: it is reported as its own error and never as "focus did
    // not move".
    crate::delivery::restore_and_verify(target.snapshot())?;
    match target {
        WindowsStopTarget::Uia(stop) => match windows_uia(host).restore_stop_target_outcome(stop) {
            uia_worker::RestoreOutcome::Restored => Ok(()),
            uia_worker::RestoreOutcome::NotIssued => Err(DeliveryError::new(
                "E_FOCUS_RESTORE",
                "could not restore captured UIA editor focus",
            )),
            uia_worker::RestoreOutcome::IssuedUnconfirmed => {
                host.log(
                    "delivery_focus_restore_issued_unconfirmed",
                    "kind=uia_editor focus_may_have_changed=true",
                );
                Err(DeliveryError::new(
                    "E_FOCUS_RESTORE_UNCONFIRMED",
                    "captured UIA editor focus was requested but not confirmed; focus may have moved",
                ))
            }
        },
        WindowsStopTarget::WezTerm { .. } => Ok(()),
        WindowsStopTarget::Window { .. } => Err(DeliveryError::new(
            "E_FOCUS_RESTORE",
            "native stop target cannot restore an editor",
        )),
    }
}

/// Public snapshot of current foreground target for non-recording operations.
pub fn snapshot_target(host: &DeliveryHost) -> TargetSnapshot {
    #[cfg(target_os = "windows")]
    {
        current_target_snapshot_windows(host, false, None)
    }
    #[cfg(not(target_os = "windows"))]
    current_target_snapshot(host)
}

/// Capture destination on the first accepted recording stop. Native
/// app/window identity is captured without any AX traversal. Exact editor
/// enrichment is intentionally left to a later worker: a stop handler cannot
/// prove that an editor observed after its boundary is still the same editor.
/// Never recapture or redirect to a newer focus.
#[cfg(target_os = "macos")]
pub fn capture_stop_target_macos(
    host: &DeliveryHost,
) -> (
    TargetSnapshot,
    Option<rightkit_automation::desktop::macos::FocusPin>,
) {
    let capture_started = std::time::Instant::now();
    // Cheap PID/window identity is sampled first, then exact AX editor capture
    // runs only after audio stop dispatch. An empty destination stays missing
    // rather than becoming a vacuous target that could pass validation.
    let snapshot = current_target_snapshot_for_stop().unwrap_or_else(TargetSnapshot::empty);
    let (pin, pin_result, attempted_frontmost_pid, miss_detail) =
        match rightkit_automation::desktop::macos::capture_stop_focus_pin() {
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
    host.log(
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
    host: &DeliveryHost,
    initial: TargetSnapshot,
) -> (
    TargetSnapshot,
    Option<rightkit_automation::desktop::macos::FocusPin>,
    rightkit_dictation_core::engine::RecordingContext,
    Option<u64>,
) {
    use rightkit_automation::desktop::FocusContinuityStatus;
    let mut target = initial;
    let capture_started = std::time::Instant::now();
    let capture_budget = std::time::Duration::from_millis(host.config.start_capture_budget_ms);
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
            proof_miss_reason =
                rightkit_automation::desktop::focus_continuity_capture_miss_reason(pid, window);
            while capture_started.elapsed()
                < std::time::Duration::from_millis(host.config.start_proof_wait_ms)
            {
                if let Some((captured_at, process_instance, editor)) =
                    rightkit_automation::desktop::capture_focus_continuity_with_editor(pid, window)
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
                std::thread::sleep(std::time::Duration::from_millis(
                    host.config.start_proof_poll_ms,
                ));
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
                rightkit_automation::desktop::wait_for_focus_continuity(
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
    let before_status = continuity_status(std::time::Duration::from_millis(
        host.config.start_continuity_wait_ms,
    ));
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
    let (candidate, after_status) = rightkit_automation::desktop::capture_continuous_candidate(
        before_status,
        identity_stable,
        || {
            // Chromium can expose its focused editor shortly after renderer
            // observation starts. Retry only these transient AX misses while
            // the original continuity interval remains available for the
            // post-capture validation below.
            let raw_pin = loop {
                pin_attempts += 1;
                let result = rightkit_automation::desktop::macos::capture_stop_focus_pin();
                let transient = matches!(
                    &result,
                    Err(
                        rightkit_automation::desktop::macos::FocusPinMiss::NoSystemFocusedElement
                            | rightkit_automation::desktop::macos::FocusPinMiss::DescendantDeadline
                    )
                );
                if !transient
                    || pin_attempts >= host.config.pin_retry_max_attempts
                    || capture_started.elapsed() >= capture_budget
                {
                    break result;
                }
                std::thread::sleep(std::time::Duration::from_millis(
                    host.config.pin_retry_interval_ms,
                ));
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
                    host.log(
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
                let pin_current =
                    rightkit_automation::desktop::macos::stop_focus_pin_is_current(pin);
                if same_pid && !same_editor {
                    if let Some(proof_editor) = proof_editor {
                        host.log(
                            "recording_start_editor_mismatch",
                            &format!(
                                "proof_editor={proof_editor} pin_editor={} observer={}",
                                pin.editor_identity(),
                                rightkit_automation::desktop::focus_observer_debug_snapshot()
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
                .map(rightkit_automation::desktop::macos::focused_field_is_secure_for_pin);
            let captured = pin.as_ref().filter(|_| secure == Some(Some(false))).and_then(|pin| {
                loop {
                    read_attempts += 1;
                    let read = match rightkit_automation::desktop::macos::read_editor_context_for_pin(
                        pin,
                        host.config.editor_read_max_chars,
                    ) {
                        Ok(context) => Some(context),
                        Err(rightkit_automation::desktop::macos::EditorContextReadFailure::UnsupportedTerminal) => {
                            context_unsupported = true;
                            break None;
                        }
                        Err(rightkit_automation::desktop::macos::EditorContextReadFailure::Unreadable) => None,
                    };
                    if read.is_some() || read_attempts >= host.config.editor_read_max_attempts || capture_started.elapsed() >= capture_budget {
                        break read;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(
                        host.config.editor_read_retry_interval_ms,
                    ));
                }
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
    host.log(
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
    let app = rightkit_automation::desktop::macos::frontmost_fast()?;
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
    snapshot.window_handle =
        rightkit_automation::desktop::macos::frontmost_window_identity(app.pid);
    snapshot.foreground_target = snapshot
        .window_handle
        .map(|handle| ForegroundTarget::WindowHandle { handle });
    Some(snapshot)
}

#[cfg(target_os = "windows")]
pub fn current_target_snapshot(host: &DeliveryHost) -> TargetSnapshot {
    current_target_snapshot_windows(host, true, None)
}

#[cfg(target_os = "windows")]
pub fn current_target_snapshot_for_revalidation(
    host: &DeliveryHost,
    original: &TargetSnapshot,
) -> TargetSnapshot {
    current_target_snapshot_windows(host, true, Some(original))
}

#[cfg(target_os = "windows")]
pub fn current_target_snapshot_windows(
    host: &DeliveryHost,
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
            .or_else(|| rightkit_automation::desktop::windows::process_name_for_pid(process_id));
        let exe_path = known_target
            .filter(|target| target.process_id == Some(process_id))
            .and_then(|target| target.exe_path.clone())
            .or_else(|| rightkit_automation::desktop::windows::process_path_for_pid(process_id));
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
            windows_focused_text_input(host)
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
pub fn windows_focused_text_input(host: &DeliveryHost) -> Option<bool> {
    match windows_uia(host).focused_text_input() {
        editable @ Some(_) => {
            host.log(
                "focused_text_input",
                &format!("platform=windows editable={editable:?}"),
            );
            editable
        }
        None => {
            host.log(
                "focused_text_input_probe_timeout",
                &format!(
                    "platform=windows timeout_ms={}",
                    host.config.uia_request_timeout_ms
                ),
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
pub fn current_target_snapshot(host: &DeliveryHost) -> TargetSnapshot {
    match rightkit_automation::desktop::macos::frontmost() {
        Some(app) => {
            // Read editability independently; native window identity remains
            // the continuity fallback for opaque editors.
            let focused_text_input = (host.focused_text_input)();
            let window_title = (host.focused_window_title)(app.pid);
            let window_handle =
                rightkit_automation::desktop::macos::frontmost_window_identity(app.pid);
            let focused_control_handle = (host.focused_target_identity)();
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
pub fn current_target_snapshot(_host: &DeliveryHost) -> TargetSnapshot {
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

/// Retry a transient read of the retained editor without treating missing
/// content as either a match or a user edit.
#[cfg(target_os = "macos")]
pub fn read_bound_editor_content_macos(
    host: &DeliveryHost,
    pin: Option<&rightkit_automation::desktop::macos::FocusPin>,
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
    let budget = std::time::Duration::from_millis(host.config.bound_read_budget_ms);
    let attempts = host.config.bound_read_attempts.max(1);
    for attempt in 1..=attempts {
        let observed =
            rightkit_automation::desktop::macos::focused_field_context_and_fingerprint_for_pin(
                pin,
                host.config.editor_read_max_chars,
            )
            .and_then(|(_, fingerprint)| fingerprint);
        let status = editor_content_status(
            expected,
            observed,
            observed.is_none() && pin.element_is_gone(),
        );
        if status != EditorContentStatus::Unreadable
            || attempt == attempts
            || started.elapsed() >= budget
        {
            if attempt > 1 || status != EditorContentStatus::Unchanged {
                host.log(
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
            std::time::Duration::from_millis(host.config.bound_read_retry_interval_ms)
                .min(budget.saturating_sub(started.elapsed())),
        );
    }
    unreachable!("bounded read always returns on its last attempt")
}
