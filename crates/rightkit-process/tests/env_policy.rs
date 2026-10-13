use rightkit_process::{OwnedCommand, DEFAULT_ENV_ALLOWLIST};

#[test]
fn a_fresh_command_does_not_report_a_cleared_environment() {
    let cmd = OwnedCommand::new("program");
    assert!(!cmd.env_is_cleared());
}

#[test]
fn allowlist_with_defaults_clears_and_keeps_only_named_variables() {
    std::env::set_var("RK_M3_ENV_EXTRA", "kept");
    std::env::set_var("RK_M3_ENV_SECRET", "dropped");
    let mut cmd = OwnedCommand::new("program");
    cmd.env_allowlist_with_defaults(["RK_M3_ENV_EXTRA", "RK_M3_ENV_UNSET"]);
    assert!(cmd.env_is_cleared());
    let envs: Vec<(String, Option<String>)> = cmd
        .command_mut()
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    assert!(envs
        .iter()
        .any(|(k, v)| k == "RK_M3_ENV_EXTRA" && v.as_deref() == Some("kept")));
    assert!(
        !envs.iter().any(|(k, _)| k == "RK_M3_ENV_SECRET"),
        "a variable that is not named must not be passed: {envs:?}"
    );
    assert!(
        !envs.iter().any(|(k, _)| k == "RK_M3_ENV_UNSET"),
        "a named variable that is not set is not invented"
    );
    // Every key passed is either a default or the consumer's own.
    for (k, _) in &envs {
        assert!(DEFAULT_ENV_ALLOWLIST.contains(&k.as_str()) || k == "RK_M3_ENV_EXTRA");
    }
}
