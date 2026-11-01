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

/// Read the JSON-form hook configuration variable (`RIGHTKIT_HOOKS_JSON`).
///
/// It is read only in desktop, daemon and test builds (`host-env`). A build without that
/// feature (the iOS mobile runtime) reads no environment variable here.
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
