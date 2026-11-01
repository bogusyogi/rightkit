// RIGHTKIT-SERIOUS: host-side hook runner (spawn command hooks, chain, matchers, fail-closed PreToolUse, destructive guard) has no published rightkit-hooks equivalent; rightkit-hooks 0.2.0 only parses Claude/Codex hook payloads inside a hook process (report S-HOOKS-RUNNER)
//! `host_hooks` — lifecycle hook bus, destructive-action guard, and
//! runtime hook registration helpers.

/// A legacy hook env-var name, present only where hooks may be configured from the
/// environment (`host-env` and tests). The mobile build carries no such name.
#[cfg(any(test, feature = "agent_hooks-host-env"))]
macro_rules! hook_var {
    ($name:literal) => {
        Some($name)
    };
}
#[cfg(not(any(test, feature = "agent_hooks-host-env")))]
macro_rules! hook_var {
    ($name:literal) => {
        None::<&'static str>
    };
}

pub mod bus;
pub mod chain;
pub mod guard;
pub mod registry;

pub use bus::{
    HookBus, HookEvent, HookOutcome, HookSubscription, PreToolUseOutcome, PreToolUseReport,
    ProcessLaunchAdapter,
};
pub use chain::{ChainReport, HookDecision, HookResult, HookSpec};
pub use guard::{DestructiveGuard, DestructiveGuardDecision};
#[cfg(test)]
pub use registry::dry_run_hook;
pub use registry::{dry_run_hook_with_adapter, HookDryRunReport, HookRegistry};

/// Read one operator-supplied hook command / hook-config variable.
///
/// Hook commands arrive from the environment only in desktop, daemon and test
/// builds (`host-env`). A build without that feature (the iOS mobile runtime)
/// reads no environment variable here and runs no environment-selected command.
#[cfg(any(test, feature = "agent_hooks-host-env"))]
pub(crate) fn hook_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(not(any(test, feature = "agent_hooks-host-env")))]
pub(crate) fn hook_env(_name: &str) -> Option<String> {
    None
}
