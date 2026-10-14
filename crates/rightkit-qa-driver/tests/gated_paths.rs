//! EFF-001 on every CLI and raw macOS path: each effectful entry point settles
//! through the gate, and a deny hook causes no side effect.
#![cfg(target_os = "macos")]
use rightkit_qa_driver::admission::{
    AdmissionDecision, EffectGate, EffectKind, EffectRequest, SettleOutcome,
};
use rightkit_qa_driver::cli;
use rightkit_qa_driver::mac::{Gated, Node};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

const NO_PID: &str = "999999";

fn deny_all(_: &EffectRequest) -> AdmissionDecision {
    AdmissionDecision::deny("test policy denies everything")
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rkc-gated-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// Every effectful subcommand, with arguments that would act if admitted.
fn effectful_commands(shot: &str) -> Vec<(Vec<String>, EffectKind, &'static str)> {
    vec![
        (
            argv(&["launch", "/nonexistent/Fixture.app", "fixture-exe", "A=B"]),
            EffectKind::Process,
            "launch",
        ),
        (
            argv(&["click", NO_PID, "10", "20"]),
            EffectKind::Input,
            "click",
        ),
        (
            argv(&["hover", NO_PID, "10", "20"]),
            EffectKind::Input,
            "hover",
        ),
        (
            argv(&["drag", NO_PID, "1", "2", "3", "4"]),
            EffectKind::Input,
            "drag",
        ),
        (
            argv(&["scroll", NO_PID, "1", "2", "-3"]),
            EffectKind::Input,
            "scroll",
        ),
        (
            argv(&["click-text", NO_PID, "OK"]),
            EffectKind::Input,
            "click_text",
        ),
        (argv(&["press", NO_PID, "OK"]), EffectKind::Action, "press"),
        (
            argv(&["keywin", NO_PID, "AXMain"]),
            EffectKind::Action,
            "make_key",
        ),
        (
            argv(&["keyraw", NO_PID]),
            EffectKind::Action,
            "key_without_raise",
        ),
        (
            argv(&["type", NO_PID, "secret text"]),
            EffectKind::Input,
            "type",
        ),
        (argv(&["key", NO_PID, "cmd+a"]), EffectKind::Input, "key"),
        (
            argv(&["shot", NO_PID, shot]),
            EffectKind::Capture,
            "capture",
        ),
    ]
}

#[test]
fn every_cli_effect_is_denied_before_any_side_effect() {
    let dir = scratch("cli");
    let shot = dir.join("shot.png");
    let shot = shot.to_str().unwrap();
    let gate = Arc::new(EffectGate::new(deny_all));
    let cmds = effectful_commands(shot);
    for (args, _, _) in &cmds {
        assert_eq!(
            cli::run(args, gate.clone()),
            3,
            "{args:?} must exit as denied"
        );
    }
    let settled = gate.settlements();
    assert_eq!(
        settled.len(),
        cmds.len(),
        "every command settled exactly once"
    );
    for (s, (args, kind, method)) in settled.iter().zip(&cmds) {
        assert_eq!(s.outcome, SettleOutcome::Denied, "{args:?}");
        assert!(!s.executed, "{args:?} executed despite denial");
        assert_eq!((s.kind, s.method.as_str()), (*kind, *method), "{args:?}");
        assert!(s.reason.as_deref().unwrap_or("").contains("denies"));
    }
    assert!(
        !std::path::Path::new(shot).exists(),
        "denied capture wrote nothing"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn read_only_commands_are_not_effects() {
    let gate = Arc::new(EffectGate::new(deny_all));
    for args in [
        argv(&["trusted"]),
        argv(&["windows", NO_PID]),
        argv(&["ax", NO_PID, "1"]),
    ] {
        assert_eq!(cli::run(&args, gate.clone()), 0, "{args:?}");
    }
    assert!(gate.settlements().is_empty());
    assert_eq!(cli::run(&argv(&["bogus"]), gate.clone()), 2);
}

#[test]
fn allow_all_cli_still_settles_through_the_gate() {
    // pid 999999 has no window: admitted, executed, failed — and settled.
    let gate = Arc::new(EffectGate::allow_all());
    assert_eq!(
        cli::run(&argv(&["click", NO_PID, "1", "1"]), gate.clone()),
        1
    );
    let s = gate.settlements();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].outcome, SettleOutcome::Failed);
    assert!(s[0].executed);
    assert!(s[0].reason.as_deref().unwrap().contains("no window"));
}

#[test]
fn every_gated_raw_primitive_is_denied_before_execution() {
    let gate = Arc::new(EffectGate::new(deny_all));
    let g = Gated::new(gate.clone());
    // A null AX element: executing any node action on it would be invalid,
    // so a passing test also proves the executor never ran.
    let node = Node {
        el: 0,
        role: "AXButton".into(),
        text: "OK".into(),
        frame: Some((0.0, 0.0, 10.0, 10.0)),
    };
    let dir = scratch("raw");
    let out = dir.join("never.png");
    let pid = 999_999;
    let outcomes = [
        g.click(pid, 1, 1.0, 1.0).settlement,
        g.hover(pid, 1, 1.0, 1.0).settlement,
        g.drag(pid, 1, (0.0, 0.0), (5.0, 5.0), 3).settlement,
        g.scroll(pid, 1, 1.0, 1.0, 2).settlement,
        g.type_text(pid, "secret").settlement,
        g.key(pid, 0, 0).settlement,
        g.ax_press(&node).settlement,
        g.click_node(pid, &node).settlement,
        g.launch_hidden("/nonexistent/Fixture.app", "fixture-exe", &[])
            .settlement,
        g.capture_window(1, out.to_str().unwrap()).settlement,
        g.ax_make_key(pid, &["AXMain"]).settlement,
        g.key_without_raise(pid, 1).settlement,
    ];
    for s in &outcomes {
        assert_eq!(s.outcome, SettleOutcome::Denied, "{}", s.method);
        assert!(!s.executed, "{}", s.method);
    }
    assert_eq!(gate.settlements().len(), outcomes.len());
    assert!(!out.exists());
    let _ = std::fs::remove_dir_all(dir);
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
