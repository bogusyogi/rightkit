//! `right-ctl demo` against a suite with no host running. Built only with `--features demo`.
#![cfg(feature = "demo")]

use std::process::Command;

fn right_ctl(args: &[&str]) -> std::process::Output {
    let dir = tempfile::tempdir().expect("tempdir");
    Command::new(env!("CARGO_BIN_EXE_right-ctl"))
        .arg("--suite-root")
        .arg(dir.path())
        .args(args)
        .output()
        .expect("run right-ctl")
}

#[test]
fn tools_lists_the_six_demo_tools_and_nothing_else() {
    let out = right_ctl(&["demo", "--tools"]);
    assert!(out.status.success(), "{:?}", out);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let mut names: Vec<String> = v
        .as_array()
        .expect("array")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "demo.act",
            "demo.apps",
            "demo.batch",
            "demo.end",
            "demo.request",
            "demo.snapshot"
        ]
    );
}

#[test]
fn an_unknown_demo_verb_is_refused_before_any_connection() {
    let out = right_ctl(&["demo", "eval", "js=1"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("unknown_tool") || err.contains("tool_not_allowed"),
        "{err}"
    );
}

#[test]
fn a_call_with_no_host_running_fails_with_a_typed_error() {
    let out = right_ctl(&["demo", "apps"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("\"error\""), "{err}");
}

#[test]
fn a_batch_over_the_step_limit_is_refused_locally() {
    let steps: Vec<serde_json::Value> = (0..21)
        .map(|_| serde_json::json!({"ref": "e", "op": "click"}))
        .collect();
    let args = serde_json::json!({"token": "t", "steps": steps}).to_string();
    let out = right_ctl(&["demo", "batch", &args]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("invalid_arguments"), "{err}");
}
