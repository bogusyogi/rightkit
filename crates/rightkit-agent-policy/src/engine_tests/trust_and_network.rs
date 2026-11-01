use super::*;

#[test]
fn sandboxed_trust_clamps_effectful_auto_allow_to_ask() {
    let e = engine();
    // AcceptEdits auto-allows file writes — Sandboxed clamps it.
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::AcceptEdits,
            WorkspaceTrust::Sandboxed,
            None,
            false
        ),
        PermissionDecision::Ask
    );
    // Reads are unaffected.
    assert_eq!(
        d(
            &e,
            CapabilityClass::ReadFile,
            PermissionMode::AcceptEdits,
            WorkspaceTrust::Sandboxed,
            None,
            false
        ),
        PermissionDecision::Allow
    );
}

#[test]
fn plan_mode_asks_network() {
    let e = engine();
    assert_eq!(
        d(
            &e,
            CapabilityClass::Network,
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
            None,
            false
        ),
        PermissionDecision::Ask
    );
}

#[test]
fn auto_safe_asks_destructive_and_browser() {
    let e = engine();
    assert_eq!(
        d(
            &e,
            CapabilityClass::Destructive,
            PermissionMode::AutoSafe,
            WorkspaceTrust::Trusted,
            None,
            false
        ),
        PermissionDecision::Ask
    );
    assert_eq!(
        d(
            &e,
            CapabilityClass::Browser,
            PermissionMode::AutoSafe,
            WorkspaceTrust::Trusted,
            None,
            false
        ),
        PermissionDecision::Ask
    );
    assert_eq!(
        d(
            &e,
            CapabilityClass::ProcessControl,
            PermissionMode::AutoSafe,
            WorkspaceTrust::Trusted,
            None,
            false
        ),
        PermissionDecision::Ask
    );
}

#[test]
fn verdict_policy_source_for_locked_workspace() {
    let e = engine();
    let v = e.evaluate(
        CapabilityClass::ReadFile,
        PermissionMode::Ask,
        WorkspaceTrust::Locked,
        None,
        false,
    );
    assert_eq!(v.decision, PermissionDecision::Deny);
    assert_eq!(v.policy_source, "trust:locked");
}
