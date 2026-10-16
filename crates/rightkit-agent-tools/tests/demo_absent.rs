//! A default build (no `demo` feature) has no demo tools, no demo subcommand and no fixture id.
//! Built only without `--features demo`; the feature-on side is covered by `demo_cli.rs`.
#![cfg(not(feature = "demo"))]

use std::process::Command;

const DEMO_TOOLS: [&str; 6] = [
    "demo.request",
    "demo.apps",
    "demo.snapshot",
    "demo.act",
    "demo.batch",
    "demo.end",
];

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
fn default_build_lists_none_of_the_demo_tools() {
    // In a default build `demo` is an ordinary app name, so `--tools` lists control tools only.
    let out = right_ctl(&["demo", "--tools"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    for name in DEMO_TOOLS {
        assert!(
            !stdout.contains(&format!("\"{name}\"")),
            "{name} in default build: {stdout}"
        );
    }
}

#[test]
fn default_build_never_names_the_fixture_app() {
    let out = right_ctl(&[
        "demo",
        "request",
        "app=app.orthiclabs.demo-fixture",
        "agent_name=x",
    ]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!text.contains("demo-fixture"), "{text}");
    assert!(!text.contains("rightkit.agent.demo"), "{text}");
}
