use super::*;

#[test]
fn deny_globs_block_secret_and_vcs_paths_even_in_bypass() {
    let e = engine();

    let dotenv = e.evaluate(
        CapabilityClass::WriteFile,
        PermissionMode::Bypass,
        WorkspaceTrust::Trusted,
        Some("config/.env"),
        false,
    );
    assert_eq!(dotenv.decision, PermissionDecision::Deny);
    assert_eq!(dotenv.policy_source, "deny-glob:write");

    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
            Some(".git/config"),
            false
        ),
        PermissionDecision::Deny
    );
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::AcceptEdits,
            WorkspaceTrust::Trusted,
            Some("src/node_modules/x.js"),
            false
        ),
        PermissionDecision::Deny
    );
    // A normal source path is fine.
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::AcceptEdits,
            WorkspaceTrust::Trusted,
            Some("src/app/main.cs"),
            false
        ),
        PermissionDecision::Allow
    );
}

#[test]
fn deny_glob_precedes_test_file_protection() {
    let e = engine();
    let verdict = e.evaluate(
        CapabilityClass::WriteFile,
        PermissionMode::AcceptEdits,
        WorkspaceTrust::Trusted,
        Some("tests/.env"),
        false,
    );

    assert_eq!(verdict.decision, PermissionDecision::Deny);
    assert_eq!(verdict.policy_source, "deny-glob:write");
}

#[test]
fn custom_deny_globs_are_used_by_engine() {
    let deny = DenyGlobSet::with_patterns(
        vec!["private/**".to_string()],
        vec!["generated/**".to_string()],
    )
    .unwrap();
    let e = PermissionEngine::with_deny_globs(deny);

    assert_eq!(
        d(
            &e,
            CapabilityClass::ReadFile,
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
            Some("private/token.txt"),
            false
        ),
        PermissionDecision::Deny
    );
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::AcceptEdits,
            WorkspaceTrust::Trusted,
            Some("generated/out.txt"),
            false
        ),
        PermissionDecision::Deny
    );
    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::AcceptEdits,
            WorkspaceTrust::Trusted,
            Some(".env"),
            false
        ),
        PermissionDecision::Allow
    );
}

#[test]
fn empty_target_path_skips_deny_globs() {
    let e = engine();

    assert_eq!(
        d(
            &e,
            CapabilityClass::WriteFile,
            PermissionMode::AcceptEdits,
            WorkspaceTrust::Trusted,
            Some(""),
            false
        ),
        PermissionDecision::Allow
    );
}
