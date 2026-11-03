//! The real `rightkit-qa-driver` binary admits every effectful command through its configured
//! policy: a deny policy causes no side effect, a missing policy fails closed.
#![cfg(target_os = "macos")]
use std::path::PathBuf;
use std::process::Command;

const NO_PID: &str = "999999";

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rkc-gated-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn binary_admits_through_configured_policy() {
    let dir = scratch("bin");
    let policy = dir.join("policy.txt");
    std::fs::write(&policy, "default allow\ndeny input\ndeny capture\n").unwrap();
    let shot = dir.join("shot.png");
    let bin = env!("CARGO_BIN_EXE_rightkit-qa-driver");

    let run = |args: &[&str], env: Option<&PathBuf>| {
        let mut c = Command::new(bin);
        c.args(args).env_remove("RIGHTKIT_CONTROL_POLICY");
        if let Some(p) = env {
            c.env("RIGHTKIT_CONTROL_POLICY", p);
        }
        c.output().unwrap()
    };

    let o = run(
        &[
            "--policy",
            policy.to_str().unwrap(),
            "--events",
            "type",
            NO_PID,
            "hi",
        ],
        None,
    );
    assert_eq!(o.status.code(), Some(3));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("denied: policy denies input type"), "{err}");
    assert!(err.contains("EffectDenied"), "events on stderr: {err}");
    assert!(!err.contains("hi\""), "events are content-free: {err}");

    let o = run(&["shot", NO_PID, shot.to_str().unwrap()], Some(&policy));
    assert_eq!(o.status.code(), Some(3), "policy from the environment");
    assert!(!shot.exists());

    // Policy allows reads to proceed; a missing policy fails closed.
    assert_eq!(
        run(&["--policy", policy.to_str().unwrap(), "trusted"], None)
            .status
            .code(),
        Some(0)
    );
    let missing = dir.join("missing.txt");
    let o = run(
        &[
            "--policy",
            missing.to_str().unwrap(),
            "click",
            NO_PID,
            "1",
            "1",
        ],
        None,
    );
    assert_eq!(o.status.code(), Some(2));
    // Without a policy: allow-all, admitted and executed (fails: no window).
    let o = run(&["click", NO_PID, "1", "1"], None);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("no window"));
    let _ = std::fs::remove_dir_all(dir);
}
