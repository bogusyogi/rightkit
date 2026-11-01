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

#[cfg(all(target_os = "windows", not(test)))]
fn cancel_pending_shutdown() -> DispatchResult {
    spawn_owned_detached("shutdown.exe", &["/a"], false)
        .map(|_| DispatchOutcome::new("power", "cancel pending shutdown"))
        .map_err(|e| DispatchError::new("E_CANCEL_FAILED", e.to_string()))
}

#[cfg(all(target_os = "windows", test))]
fn cancel_pending_shutdown() -> DispatchResult {
    Ok(DispatchOutcome::new(
        "power",
        "cancel pending shutdown (test no-op)",
    ))
}

// Switch class S2 (authority). Compiled only in test-mode builds; a release build never reads an
// environment value to enable destructive power commands, and the variable's name is not in it.
#[cfg(all(any(target_os = "windows", target_os = "macos"), feature = "test-mode"))]
fn destructive_power_env_consent() -> bool {
    std::env::var("RIGHTKIT_ENABLE_POWER_COMMANDS")
        .map(|value| value == "I_UNDERSTAND")
        .unwrap_or(false)
}

#[cfg(all(
    any(target_os = "windows", target_os = "macos"),
    feature = "test-mode",
    not(test)
))]
pub(super) fn destructive_power_enabled() -> bool {
    destructive_power_env_consent()
}

#[cfg(all(
    any(target_os = "windows", target_os = "macos"),
    any(test, not(feature = "test-mode"))
))]
pub(super) fn destructive_power_enabled() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    #[test]
    fn power_spawns_use_owned_detached_launch() {
        let source = include_str!("power.rs");
        let production = source.split("mod tests").next().unwrap();
        assert!(
            !production.contains("command_std("),
            "raw std spawn left in power.rs"
        );
        // The owner's `OwnedCommand::new(` is the required launcher; only a bare `Command::new(` is raw.
        let raw = production.replace("OwnedCommand::new(", "");
        assert!(
            !raw.contains("Command::new("),
            "raw Command::new left in power.rs"
        );
        assert!(production.contains("rightkit_child_process::OwnedCommand::new("));
        assert!(production.contains("spawn_uncontained_detached()"));
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn spawn_owned_detached_reports_a_missing_program() {
        assert!(spawn_owned_detached("definitely-not-a-real-program-xyz", &[], true).is_err());
    }

    #[test]
    fn dispatch_power_unknown_op_is_bad_power_op() {
        let result = dispatch_power("teleport_to_mars");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "E_BAD_POWER_OP");
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn destructive_power_disabled_by_default() {
        let _guard = env_lock();
        std::env::remove_var("RIGHTKIT_ENABLE_POWER_COMMANDS");
        assert!(!destructive_power_enabled());
    }

    #[cfg(all(any(target_os = "windows", target_os = "macos"), feature = "test-mode"))]
    #[test]
    fn destructive_power_requires_exact_consent_value() {
        let _guard = env_lock();
        std::env::set_var("RIGHTKIT_ENABLE_POWER_COMMANDS", "yes");
        assert!(
            !destructive_power_env_consent(),
            "wrong value must not enable"
        );
        std::env::set_var("RIGHTKIT_ENABLE_POWER_COMMANDS", "I_UNDERSTAND");
        assert!(destructive_power_env_consent(), "exact value must enable");
        assert!(
            !destructive_power_enabled(),
            "unit tests must never enable real destructive power dispatch"
        );
        std::env::remove_var("RIGHTKIT_ENABLE_POWER_COMMANDS");
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn destructive_power_disabled_ops_rejected_without_consent() {
        let _guard = env_lock();
        std::env::remove_var("RIGHTKIT_ENABLE_POWER_COMMANDS");
        for op in ["sleep", "restart", "shutdown_pc", "signout"] {
            let result = dispatch_power(op);
            assert!(
                result.is_err(),
                "op {op} should be rejected without consent"
            );
            assert_eq!(result.unwrap_err().code, "E_POWER_DISABLED");
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn cancel_shutdown_always_allowed_without_consent() {
        let _guard = env_lock();
        std::env::remove_var("RIGHTKIT_ENABLE_POWER_COMMANDS");
        // Test builds must not invoke shutdown.exe, but the operation should
        // remain available and outside the destructive-power gate.
        let result = dispatch_power("cancel_shutdown");
        if let Err(e) = &result {
            assert_ne!(e.code, "E_POWER_DISABLED");
            assert_ne!(e.code, "E_BAD_POWER_OP");
        }
    }
}
