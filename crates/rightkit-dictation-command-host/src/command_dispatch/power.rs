// Power dispatch (typed enum, NOT raw shell) + macOS Shortcuts runner.

use super::entry::{DispatchError, DispatchOutcome, DispatchResult};

/// Launch a fire-and-forget system command through RightKit process ownership. The child must
/// outlive this process (`shutdown /s /t 60`, `pmset`, a long `shortcuts run`), so it is launched
/// uncontained and detached; the owner reaps it. `null_stdio` detaches all three standard handles.
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn spawn_owned_detached(program: &str, args: &[&str], null_stdio: bool) -> std::io::Result<()> {
    let mut command = rightkit_child_process::OwnedCommand::new(program);
    command.command_mut().args(args);
    if null_stdio {
        use std::process::Stdio;
        command
            .command_mut()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    }
    command.windows_hide();
    command.spawn_uncontained_detached().map(|_| ())
}

// ---------------------------------------------------------------------------
// Run Shortcut (macOS Shortcuts app) — resolved name, no shell interpolation
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
pub(super) fn dispatch_run_shortcut(name: &str) -> DispatchResult {
    // `name` is an installed Shortcut's exact name, resolved by
    // app_launch::resolve_shortcut. `shortcuts run` takes the name as a single
    // arg (no shell) and fails harmlessly (exit 1) on a bad name.
    //
    // CRITICAL: detach all three stdio handles. `shortcuts run` BLOCKS for the
    // whole shortcut and reads/writes stdin+stdout — if it inherits the sidecar's
    // handles (which ARE the JSON-RPC IPC pipes to the shell) the shortcut breaks
    // (garbage stdin / SIGPIPE) AND the IPC channel gets corrupted. `open -a`
    // doesn't hit this because it returns instantly without touching stdio. We
    // don't wait() — a shortcut may run for many seconds; this is fire-and-forget.
    spawn_owned_detached("/usr/bin/shortcuts", &["run", name], true)
        .map(|_| DispatchOutcome::new("run_shortcut", format!("name={name:?}")))
        .map_err(|e| {
            DispatchError::new("E_SHORTCUT_FAILED", format!("shortcuts run {name:?}: {e}"))
        })
}

#[cfg(not(any(target_os = "macos")))]
pub(super) fn dispatch_run_shortcut(name: &str) -> DispatchResult {
    Err(DispatchError::new(
        "E_UNSUPPORTED",
        format!("RunShortcut not implemented for this OS: {name}"),
    ))
}

// ---------------------------------------------------------------------------
// Power — typed enum, NOT raw shell
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
pub(super) fn dispatch_power(op: &str) -> DispatchResult {
    match op {
        "lock" => super::keys::send_chord("windows+l")
            .map(|_| DispatchOutcome::new("power", "lock"))
            .map_err(|e| DispatchError::new("E_LOCK_FAILED", e.message)),
        "sleep" if destructive_power_enabled() => {
            // Use rundll32 SetSuspendState approach to avoid raw shell strings;
            // we still spawn rundll32.exe which is the standard Windows path.
            spawn_owned_detached(
                "rundll32.exe",
                &["powrprof.dll,SetSuspendState", "0,1,0"],
                false,
            )
            .map(|_| DispatchOutcome::new("power", "sleep"))
            .map_err(|e| DispatchError::new("E_SLEEP_FAILED", e.to_string()))
        }
        "hibernate" if destructive_power_enabled() => {
            spawn_owned_detached("shutdown.exe", &["/h"], false)
                .map(|_| DispatchOutcome::new("power", "hibernate"))
                .map_err(|e| DispatchError::new("E_HIBERNATE_FAILED", e.to_string()))
        }
        "restart" if destructive_power_enabled() => {
            spawn_owned_detached("shutdown.exe", &["/r", "/t", "60"], false)
                .map(|_| {
                    DispatchOutcome::new(
                        "power",
                        "restart (60s grace, say 'cancel shutdown' to abort)",
                    )
                })
                .map_err(|e| DispatchError::new("E_RESTART_FAILED", e.to_string()))
        }
        "shutdown_pc" if destructive_power_enabled() => {
            spawn_owned_detached("shutdown.exe", &["/s", "/t", "60"], false)
                .map(|_| DispatchOutcome::new("power", "shutdown (60s grace)"))
                .map_err(|e| DispatchError::new("E_SHUTDOWN_FAILED", e.to_string()))
        }
        "cancel_shutdown" => cancel_pending_shutdown(),
        "signout" if destructive_power_enabled() => {
            spawn_owned_detached("shutdown.exe", &["/l"], false)
                .map(|_| DispatchOutcome::new("power", "sign out"))
                .map_err(|e| DispatchError::new("E_SIGNOUT_FAILED", e.to_string()))
        }
        "sleep" | "hibernate" | "restart" | "shutdown_pc" | "signout" => Err(DispatchError::new(
            "E_POWER_DISABLED",
            format!("destructive power command {:?} is disabled", op),
        )),
        other => Err(DispatchError::new(
            "E_BAD_POWER_OP",
            format!("unknown power op {:?}", other),
        )),
    }
}

#[cfg(target_os = "macos")]
pub(super) fn dispatch_power(op: &str) -> DispatchResult {
    // Run an AppleScript via System Events (may prompt for the Automation
    // permission on first use). Used for the destructive ops.
    fn system_events(verb: &str) -> Result<(), String> {
        spawn_owned_detached(
            "osascript",
            &[
                "-e",
                &format!("tell application \"System Events\" to {verb}"),
            ],
            false,
        )
        .map_err(|e| e.to_string())
    }

    match op {
        // Lock screen — fast user-switching suspend (no extra permission).
        "lock" => spawn_owned_detached(
            "/System/Library/CoreServices/Menu Extras/User.menu/Contents/Resources/CGSession",
            &["-suspend"],
            false,
        )
        .map(|_| DispatchOutcome::new("power", "lock"))
        .map_err(|e| DispatchError::new("E_LOCK_FAILED", e.to_string())),
        "sleep" if destructive_power_enabled() => {
            spawn_owned_detached("pmset", &["sleepnow"], false)
                .map(|_| DispatchOutcome::new("power", "sleep"))
                .map_err(|e| DispatchError::new("E_SLEEP_FAILED", e.to_string()))
        }
        "restart" if destructive_power_enabled() => system_events("restart")
            .map(|_| DispatchOutcome::new("power", "restart"))
            .map_err(|e| DispatchError::new("E_RESTART_FAILED", e)),
        "shutdown_pc" if destructive_power_enabled() => system_events("shut down")
            .map(|_| DispatchOutcome::new("power", "shutdown"))
            .map_err(|e| DispatchError::new("E_SHUTDOWN_FAILED", e)),
        "signout" if destructive_power_enabled() => system_events("log out")
            .map(|_| DispatchOutcome::new("power", "sign out"))
            .map_err(|e| DispatchError::new("E_SIGNOUT_FAILED", e)),
        "sleep" | "restart" | "shutdown_pc" | "signout" => Err(DispatchError::new(
            "E_POWER_DISABLED",
            format!("destructive power command {:?} is disabled", op),
        )),
        "hibernate" | "cancel_shutdown" => Err(DispatchError::new(
            "E_UNSUPPORTED",
            format!("power op {:?} has no macOS equivalent", op),
        )),
        other => Err(DispatchError::new(
            "E_BAD_POWER_OP",
            format!("unknown power op {:?}", other),
        )),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(super) fn dispatch_power(op: &str) -> DispatchResult {
    Err(DispatchError::new(
        "E_UNSUPPORTED",
        format!("power op not supported on this OS: {}", op),
    ))
}

#[cfg(target_os = "windows")]
fn cancel_pending_shutdown() -> DispatchResult {
    spawn_owned_detached("shutdown.exe", &["/a"], false)
        .map(|_| DispatchOutcome::new("power", "cancel pending shutdown"))
        .map_err(|e| DispatchError::new("E_CANCEL_FAILED", e.to_string()))
}

// Switch class S2 (authority): destructive power commands are never enabled in a build.
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(super) fn destructive_power_enabled() -> bool {
    false
}
