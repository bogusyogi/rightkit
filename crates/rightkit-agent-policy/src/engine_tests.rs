use super::*;
use crate::capabilities::CapabilityClass;
use crate::types::{PermissionDecision, PermissionMode, WorkspaceTrust};

fn engine() -> PermissionEngine {
    PermissionEngine::new()
}

fn d(
    e: &PermissionEngine,
    cls: CapabilityClass,
    mode: PermissionMode,
    trust: WorkspaceTrust,
    path: Option<&str>,
    destructive: bool,
) -> PermissionDecision {
    e.evaluate(cls, mode, trust, path, destructive).decision
}

#[test]
fn media_is_independent_from_shell_and_requires_auto_safe_approval() {
    let engine = engine();
    assert_eq!(
        d(
            &engine,
            CapabilityClass::Media,
            PermissionMode::AutoSafe,
            WorkspaceTrust::Trusted,
            None,
            false,
        ),
        PermissionDecision::Ask
    );
    assert_eq!(
        d(
            &engine,
            CapabilityClass::Media,
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
            None,
            false,
        ),
        PermissionDecision::Allow
    );
}

#[path = "engine_tests/abac.rs"]
mod abac;
#[path = "engine_tests/deny_globs.rs"]
mod deny_globs;
#[path = "engine_tests/modes.rs"]
mod modes;
#[path = "engine_tests/test_file_protection.rs"]
mod test_file_protection;
#[path = "engine_tests/trust_and_network.rs"]
mod trust_and_network;
