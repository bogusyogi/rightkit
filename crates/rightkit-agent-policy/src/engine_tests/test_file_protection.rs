use super::*;

#[test]
fn is_test_file_matches_common_patterns() {
    assert!(is_test_file("src/foo.test.ts"));
    assert!(is_test_file("src/foo.spec.js"));
    assert!(is_test_file("pkg/handler_test.go"));
    assert!(is_test_file("crates/x/src/lib_test.rs"));
    assert!(is_test_file("app/tests/integration.rs"));
    assert!(is_test_file("a/__tests__/b.jsx"));
    assert!(is_test_file("test_models.py"));
    assert!(is_test_file("spec/widget_spec.rb"));
    // Not tests:
    assert!(!is_test_file("src/main.rs"));
    assert!(!is_test_file("src/contest.rs"));
    assert!(!is_test_file("docs/testing.md"));
}

#[test]
fn test_file_write_escalates_under_accept_edits() {
    let e = engine();
    // A normal file auto-applies under AcceptEdits.
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::AcceptEdits,
            WorkspaceTrust::Trusted,
            Some("src/main.rs"),
            false
        ),
        PermissionDecision::Allow
    );
    // A test file must escalate to Ask instead of silently applying.
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::AcceptEdits,
            WorkspaceTrust::Trusted,
            Some("src/main.test.ts"),
            false
        ),
        PermissionDecision::Ask
    );
}

#[test]
fn test_file_write_reports_protection_source() {
    let e = engine();
    let verdict = e.evaluate(
        CapabilityClass::WriteFile,
        PermissionMode::AcceptEdits,
        WorkspaceTrust::Trusted,
        Some("src/main.test.ts"),
        false,
    );

    assert_eq!(verdict.decision, PermissionDecision::Ask);
    assert_eq!(verdict.policy_source, "test-file:protected");
}

#[test]
fn test_file_write_still_allowed_under_bypass() {
    let e = engine();
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
            Some("src/main.test.ts"),
            false
        ),
        PermissionDecision::Allow
    );
}

// ── Ported from C# PermissionEngineTests ──────────────────────────────────
