//! End to end: a host that renames the environment prefix (`set_env_prefix("CODERIGHT")`) gets
//! every hook variable exactly once under its own prefix, whether it hands the bus the default
//! spelling (`RIGHTKIT_HOOK_STATUS`) or its own (`CODERIGHT_HOOK_ROLE`). Before 0.2.12 the second
//! form reached the hook as `CODERIGHTCODERIGHT_HOOK_ROLE` and the hook never saw its role.
#![cfg(all(unix, feature = "agent_hooks-host-env"))]

use rightkit_hooks::agent_hooks::{set_env_prefix, HookBus};

#[tokio::test]
async fn a_renamed_prefix_reaches_the_hook_once_for_both_spellings() {
    assert!(set_env_prefix("CODERIGHT"));
    let dir = std::env::temp_dir().join(format!("rk-hooks-prefix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("env.txt");
    let config =
        serde_json::json!({ "SessionEnd": [{ "command": format!("env > {}", out.display()) }] });
    // SAFETY: this test binary has one test, so nothing else reads the environment meanwhile.
    unsafe { std::env::set_var("CODERIGHT_HOOKS_JSON", config.to_string()) };

    let bus = HookBus::new(8);
    let report = bus
        .run_event_hooks(
            "SessionEnd",
            "session-1",
            None,
            None,
            &[
                ("CODERIGHT_HOOK_ROLE", "coder".to_string()),
                ("RIGHTKIT_HOOK_STATUS", "ok".to_string()),
            ],
        )
        .await;
    assert_eq!(report.ran, 1, "the hook did not run: {:?}", report.errors);
    let env = std::fs::read_to_string(&out).expect("the hook wrote its environment");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        env.lines().any(|l| l == "CODERIGHT_HOOK_ROLE=coder"),
        "no CODERIGHT_HOOK_ROLE in {env}"
    );
    assert!(
        env.lines().any(|l| l == "CODERIGHT_HOOK_STATUS=ok"),
        "no CODERIGHT_HOOK_STATUS in {env}"
    );
    assert!(
        env.lines().any(|l| l == "CODERIGHT_HOOK_EVENT=SessionEnd"),
        "no CODERIGHT_HOOK_EVENT in {env}"
    );
    assert!(
        !env.contains("CODERIGHTCODERIGHT_"),
        "a variable was prefixed twice: {env}"
    );
    assert!(
        !env.contains("RIGHTKIT_HOOK_"),
        "a variable kept the default prefix: {env}"
    );
}
