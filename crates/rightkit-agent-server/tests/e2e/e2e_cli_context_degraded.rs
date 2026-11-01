//! D18 CLI e2e: `coderight run` with Membrane unavailable still runs the
//! turn (no refusal, no shadow store) and emits a typed degraded event on the
//! stream-json contract; the removed `--context-mode` flag is accepted as a
//! deprecated no-op that warns.

use crate::support::e2e_cli;
use e2e_cli::*;

fn jsonl(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .collect()
}

#[test]
fn stream_json_reports_a_typed_degraded_event_and_the_turn_completes() {
    let provider = Provider::start(vec![Reply::Chat(Chat::text("degraded-cli-answer"))]);
    let cli = Cli::new(&provider);
    let redirect = tempfile::tempdir().expect("redirect root");
    let mut c = cli.base();
    c.env("RIGHTKIT_CONTEXT", "on")
        .env("LOCALAPPDATA", redirect.path())
        .env("APPDATA", redirect.path())
        .args(["run", "--json-event", "--bypass-approvals", "say hi"]);
    let out = finish(c, 120);
    let events = jsonl(&out.stdout);
    let degraded: Vec<&Value> = events
        .iter()
        .filter(|e| e["event"] == "contextDegraded")
        .collect();
    assert_eq!(
        degraded.len(),
        1,
        "one typed degraded event:\n{}\n{}",
        out.stdout,
        out.stderr
    );
    assert_eq!(
        degraded[0]["schema_version"],
        "coderight.cli.stream_event.v1"
    );
    assert!(
        degraded[0]["data"]["reason"]
            .as_str()
            .is_some_and(|r| !r.is_empty()),
        "degraded event names a reason: {}",
        degraded[0]
    );
    assert!(
        out.stdout.contains("degraded-cli-answer"),
        "the turn ran and answered:\n{}\n{}",
        out.stdout,
        out.stderr
    );
    assert_eq!(
        out.code,
        Some(0),
        "exit 0 despite degraded context:\n{}",
        out.stderr
    );
    assert_eq!(provider.count(), 1, "the model was called");
    assert!(
        std::fs::read_dir(redirect.path())
            .map(|mut d| d.next().is_none())
            .unwrap_or(true),
        "no installed-Membrane effect or shadow store was created"
    );
}

#[test]
fn removed_context_mode_flag_is_a_deprecated_no_op_with_a_warning() {
    let provider = Provider::start(vec![Reply::Chat(Chat::text("flag-still-works"))]);
    let cli = Cli::new(&provider);
    let out = cli.run(&["--context-mode", "direct", "-p"], "hello");
    assert_eq!(
        out.code,
        Some(0),
        "scripts passing the old flag keep working:\n{}",
        out.stderr
    );
    assert!(
        out.stdout.contains("flag-still-works"),
        "{}\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("--context-mode is deprecated"),
        "a one-line deprecation warning is printed:\n{}",
        out.stderr
    );
    let help = cli.exec(&["run", "--help"], &[]);
    assert!(
        !help.stdout.contains("--context-mode"),
        "the flag is hidden from help:\n{}",
        help.stdout
    );
}
