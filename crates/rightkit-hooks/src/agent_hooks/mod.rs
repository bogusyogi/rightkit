// RIGHTKIT-SERIOUS: host-side hook runner (spawn command hooks, chain, matchers, fail-closed PreToolUse, destructive guard) has no published rightkit-hooks equivalent; rightkit-hooks 0.2.0 only parses Claude/Codex hook payloads inside a hook process (report S-HOOKS-RUNNER)
//! `host_hooks` — lifecycle hook bus, destructive-action guard, and
//! runtime hook registration helpers.

pub mod bus;
pub mod chain;
pub mod guard;
pub mod registry;

pub use bus::{
    HookBus, HookEvent, HookOutcome, HookSubscription, PreToolUseOutcome, PreToolUseReport,
    ProcessLaunchAdapter,
};
pub use chain::{ChainReport, ConfiguredHooks, HookDecision, HookKind, HookResult, HookSpec};
pub use guard::{DestructiveGuard, DestructiveGuardDecision};
pub use registry::{dry_run_hook_with_adapter, HookDryRunReport, HookRegistry, IdentifiedHook};

/// Environment-variable prefix of the variables hooks read and set (`<prefix>_HOOKS_JSON`,
/// `<prefix>_HOOK_EVENT`, `<prefix>_HOOK_REASON`, ...) until the host calls [`set_env_prefix`].
pub const DEFAULT_ENV_PREFIX: &str = "RIGHTKIT";
/// Workspace state folder holding the project hook file until the host calls
/// [`set_state_dir_name`].
pub const DEFAULT_STATE_DIR_NAME: &str = ".rightkit";

static ENV_PREFIX: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static STATE_DIR_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Sets the environment-variable prefix, upper case without the trailing `_` (for example
/// `MYAPP`). Call once at start-up; returns `false` when one was already set.
pub fn set_env_prefix(prefix: &str) -> bool {
    ENV_PREFIX.set(prefix.to_string()).is_ok()
}

/// The environment-variable prefix in force.
pub fn env_prefix() -> &'static str {
    ENV_PREFIX
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_ENV_PREFIX)
}

/// Sets the workspace state folder name with its leading dot (for example `.myapp`). Call once at
/// start-up; returns `false` when one was already set.
pub fn set_state_dir_name(name: &str) -> bool {
    STATE_DIR_NAME.set(name.to_string()).is_ok()
}

/// The workspace state folder name in force.
pub fn state_dir_name() -> &'static str {
    STATE_DIR_NAME
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_STATE_DIR_NAME)
}

/// `name` (written with the default `RIGHTKIT_` prefix) under the host's prefix. The renamed
/// strings are interned once each, so a call allocates nothing after the first.
pub(crate) fn host_env_name(name: &'static str) -> &'static str {
    let prefix = env_prefix();
    if prefix == DEFAULT_ENV_PREFIX {
        return name;
    }
    static NAMES: std::sync::Mutex<Option<std::collections::HashMap<&'static str, &'static str>>> =
        std::sync::Mutex::new(None);
    let mut guard = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(Default::default)
        .entry(name)
        .or_insert_with(|| {
            let rest = name.strip_prefix("RIGHTKIT").unwrap_or(name);
            Box::leak(format!("{prefix}{rest}").into_boxed_str())
        })
}

/// Read the JSON-form hook configuration variable (`RIGHTKIT_HOOKS_JSON`).
///
/// Hook commands arrive from the environment only in desktop and daemon
/// builds (`host-env`). A build without that feature (the iOS mobile runtime)
/// reads no environment variable here and runs no environment-selected command.
#[cfg(feature = "agent_hooks-host-env")]
pub(crate) fn hook_env(name: &'static str) -> Option<String> {
    std::env::var(host_env_name(name))
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(not(feature = "agent_hooks-host-env"))]
pub(crate) fn hook_env(_name: &'static str) -> Option<String> {
    None
}
