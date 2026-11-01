use super::*;

#[test]
fn plan_mode_denies_mutations_allows_reads() {
    let e = engine();
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
            None,
            false
        ),
        PermissionDecision::Deny
    );
    assert_eq!(
        d(
            &e,
            CapabilityClass::Shell,
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
            None,
            false
        ),
        PermissionDecision::Deny
    );
    assert_eq!(
        d(
            &e,
            CapabilityClass::Mcp,
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
            None,
            false
        ),
        PermissionDecision::Deny
    );
    assert_eq!(
        d(
            &e,
            CapabilityClass::ReadFile,
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
            None,
            false
        ),
        PermissionDecision::Allow
    );
}

#[test]
fn ask_mode_asks_mutations_allows_reads() {
    let e = engine();
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::Ask,
            WorkspaceTrust::Trusted,
            None,
            false
        ),
        PermissionDecision::Ask
    );
    assert_eq!(
        d(
            &e,
            CapabilityClass::ReadFile,
            PermissionMode::Ask,
            WorkspaceTrust::Trusted,
            None,
            false
        ),
        PermissionDecision::Allow
    );
}

#[test]
fn accept_edits_allows_file_writes_but_asks_shell() {
    let e = engine();
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::AcceptEdits,
            WorkspaceTrust::Trusted,
            None,
            false
        ),
        PermissionDecision::Allow
    );
    assert_eq!(
        d(
            &e,
            CapabilityClass::Shell,
            PermissionMode::AcceptEdits,
            WorkspaceTrust::Trusted,
            None,
            false
        ),
        PermissionDecision::Ask
    );
}

#[test]
fn auto_safe_allows_writes_but_destructive_shell_escalates_to_ask() {
    let e = engine();
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::AutoSafe,
            WorkspaceTrust::Trusted,
            None,
            false
        ),
        PermissionDecision::Allow
    );
    assert_eq!(
        d(
            &e,
            CapabilityClass::Shell,
            PermissionMode::AutoSafe,
            WorkspaceTrust::Trusted,
            None,
            true
        ),
        PermissionDecision::Ask
    );
}

#[test]
fn destructive_auto_allow_reports_escalation_source() {
    let e = engine();
    let verdict = e.evaluate(
        CapabilityClass::Shell,
        PermissionMode::AutoSafe,
        WorkspaceTrust::Trusted,
        None,
        true,
    );

    assert_eq!(verdict.decision, PermissionDecision::Ask);
    assert_eq!(verdict.policy_source, "destructive:escalated");
}

#[test]
fn untrusted_workspace_clamps_an_auto_allow_to_ask() {
    let e = engine();
    // AutoSafe would auto-allow a write in Trusted; Untrusted forces approval.
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::AutoSafe,
            WorkspaceTrust::Trusted,
            None,
            false
        ),
        PermissionDecision::Allow
    );
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::AutoSafe,
            WorkspaceTrust::Untrusted,
            None,
            false
        ),
        PermissionDecision::Ask
    );
    // Reads are still fine when untrusted.
    assert_eq!(
        d(
            &e,
            CapabilityClass::ReadFile,
            PermissionMode::AutoSafe,
            WorkspaceTrust::Untrusted,
            None,
            false
        ),
        PermissionDecision::Allow
    );
}

#[test]
fn locked_denies_everything_including_reads() {
    let e = engine();
    assert_eq!(
        d(
            &e,
            CapabilityClass::ReadFile,
            PermissionMode::Bypass,
            WorkspaceTrust::Locked,
            None,
            false
        ),
        PermissionDecision::Deny
    );
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::Bypass,
            WorkspaceTrust::Locked,
            None,
            false
        ),
        PermissionDecision::Deny
    );
}

#[test]
fn bypass_allows_mutations() {
    let e = engine();
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
            None,
            false
        ),
        PermissionDecision::Allow
    );
    // Bypass overrides destructive escalation.
    assert_eq!(
        d(
            &e,
            CapabilityClass::Shell,
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
            None,
            true
        ),
        PermissionDecision::Allow
    );
    // Bypass overrides the untrusted trust clamp.
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::Bypass,
            WorkspaceTrust::Untrusted,
            None,
            false
        ),
        PermissionDecision::Allow
    );
}
