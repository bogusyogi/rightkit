// Typed standalone-command dispatcher — entry point.
//
// Receives `CommandAction` from the recognition layer
// and executes it via Win32 APIs (SendInput, ShellExecuteW, mouse_event,
// shutdown helpers, clipboard). Boundary rule: TYPED actions only, no raw
// shell strings over the IPC.
//
// Each variant is rejected at the protocol boundary if malformed:
//   - KeySequence: every chord must parse into known modifiers + a known key
//   - LaunchApp: name is alias-normalized upstream; we call ShellExecuteW
//     (never shell-out to cmd.exe)
//   - Mouse: action / button / direction validated against fixed sets
//   - Power: enum-only (lock/sleep/hibernate/restart/shutdown_pc/cancel_shutdown/signout)
//   - Special: enum-only (backspace_last/clear_clipboard/always_on_top)
//   - LastPasteTransform: undo + clipboard rewrite + paste

use rightkit_voice_commands::command::CommandAction;

use super::keys::dispatch_key_sequence;
use super::keys::dispatch_launch_app;
use super::mouse::dispatch_mouse;
use super::power::dispatch_power;
use super::power::dispatch_run_shortcut;
use super::transforms::dispatch_last_paste_transform;
use super::transforms::dispatch_special;

#[derive(Debug)]
pub struct DispatchError {
    pub code: &'static str,
    pub message: String,
}

impl DispatchError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

pub type DispatchResult = Result<DispatchOutcome, DispatchError>;

#[derive(Debug, Clone)]
pub struct DispatchOutcome {
    pub action: &'static str,
    pub detail: String,
}

impl DispatchOutcome {
    pub fn new(action: &'static str, detail: impl Into<String>) -> Self {
        Self {
            action,
            detail: detail.into(),
        }
    }
}

/// Dispatch the requested command action. Returns success outcome or a
/// structured error suitable for logging + UI toast.
pub fn dispatch(action: &CommandAction) -> DispatchResult {
    dispatch_with_last_text(action, None)
}

/// Dispatch with optional previous-paste context. Last-paste transforms need
/// the previous delivered transcript; other commands ignore the context.
pub fn dispatch_with_last_text(action: &CommandAction, last_text: Option<&str>) -> DispatchResult {
    let detail = match action {
        CommandAction::KeySequence { chords, .. } => format!("key_sequence[{}]", chords.join(",")),
        CommandAction::LaunchApp { name } => format!("launch_app[{name}]"),
        CommandAction::RunShortcut { name } => format!("run_shortcut[{name}]"),
        CommandAction::Mouse { action, .. } => format!("mouse[{action}]"),
        CommandAction::Power { op, .. } => format!("power[{op}]"),
        CommandAction::LastPasteTransform { transform } => format!("transform[{transform}]"),
        CommandAction::Special { op } => format!("special[{op}]"),
    };
    let result = dispatch_inner(action, last_text);
    // events.jsonl: exactly which command + chord fired and whether injection
    // succeeded — so a "screenshot opened Start menu"-style regression is
    // diagnosable from the log instead of guessed at.
    let payload = match &result {
        Ok(o) => {
            serde_json::json!({ "event": "command_dispatched", "action": detail, "ok": true, "outcome": format!("{o:?}") })
        }
        Err(e) => {
            serde_json::json!({ "event": "command_dispatch_failed", "action": detail, "ok": false, "code": e.code, "message": e.message })
        }
    };
    if crate::settings::diagnostics_enabled() {
        let payload = rightkit_dictation_core::redact_diagnostic_event(payload);
        append_command_dispatch_event(&payload);
        tracing::info!(target: "command_dispatch", "{}", payload);
    }
    result
}

fn dispatch_inner(action: &CommandAction, last_text: Option<&str>) -> DispatchResult {
    match action {
        CommandAction::KeySequence {
            chords,
            description,
        } => dispatch_key_sequence(chords, description.as_deref()),
        CommandAction::LaunchApp { name } => dispatch_launch_app(name),
        CommandAction::RunShortcut { name } => dispatch_run_shortcut(name),
        CommandAction::Mouse {
            action,
            button,
            clicks,
            direction,
            page,
        } => dispatch_mouse(
            action,
            button.as_deref(),
            *clicks,
            direction.as_deref(),
            *page,
        ),
        CommandAction::Power {
            op,
            requires_confirm,
        } => {
            if *requires_confirm {
                // Confirmation must happen at the UI layer before dispatch.
                // This is a safety gate; the dispatch layer trusts that
                // confirmation was obtained if this function is called.
                tracing::info!(op = %op, "power dispatch confirmed");
            }
            dispatch_power(op)
        }
        CommandAction::LastPasteTransform { transform } => {
            dispatch_last_paste_transform(transform, last_text)
        }
        CommandAction::Special { op } => dispatch_special(op, last_text),
    }
}

const EVENTS_JSONL_MAX_BYTES: u64 = 10 * 1024 * 1024;

pub(in crate::command_dispatch) fn append_command_dispatch_event(payload: &serde_json::Value) {
    if !crate::settings::diagnostics_enabled() {
        return;
    }
    // Command dispatch runs in app process while probe diagnostics run in
    // sidecar process. Keep their appenders on separate files so sidecar
    // buffering/rotation cannot move an app-process record under its feet.
    let path = crate::settings::app_data_root().join("engine-command-events.jsonl");
    let payload = rightkit_dictation_core::redact_diagnostic_event(payload.clone());
    // `JsonlStore` creates the directory, rotates to `<name>.1` at the size cap and appends.
    if let Err(err) =
        rightkit_logs::JsonlStore::new(&path, EVENTS_JSONL_MAX_BYTES).append_value(&payload)
    {
        tracing::warn!(
            path = %path.display(),
            error = %err,
            "command dispatch telemetry append failed"
        );
    }
}
