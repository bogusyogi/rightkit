//! Canon closure journeys for the kernel host atoms, driven through the real binary.
//!
//! Evidence map (docs/canon/daemon-master-canon-atoms.md, ledger docs/pending/canon-closure-ledger.md):
//! - KRN-001 / KRN-003 / KRN-004: a clean local one-shot turn runs InlineHost and leaves
//!   no daemon, discovery file or lease behind.
//!   `inline_one_shot_run_starts_no_daemon_and_publishes_no_discovery_file`
//! - KRN-002: InlineHost and ServiceHost expose the same semantic facts for one
//!   representative recorded turn.
//!   `inline_and_service_host_emit_the_same_semantic_fact_sequence_for_one_turn`

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::e2e_cli;
use crate::support::scripted_provider;

use e2e_cli::{Chat, Cli, Provider};
use scripted_provider::Step;
use serde_json::{json, Value};

/// Durable semantic facts both hosts must report, in order. Transport metadata,
/// live deltas and host-specific bookkeeping events are intentionally excluded.
const SEMANTIC: [&str; 3] = [
    "toolCallStarted",
    "toolResultAdded",
    "assistantMessageCompleted",
];

#[test]
fn inline_one_shot_run_starts_no_daemon_and_publishes_no_discovery_file() {
    let p = Provider::start(vec![Chat::text("inline answer").into()]);
    let cli = Cli::new(&p);

    // Autostart is deliberately ENABLED here (the shared harness disables it): a
    // clean local one-shot turn must not need, and must not start, a daemon.
    let mut c = cli.command(&["--stream-json"]);
    c.env_remove("RIGHTKIT_NO_AUTOSTART");
    c.arg("say hello");
    let out = e2e_cli::finish(c, 90);

    let port_file = crate::common::app_root(cli.home.path()).join("daemon.port");
    let leaked_discovery = port_file.exists();
    // Best-effort cleanup so a failing assertion cannot leak a resident process.
    let _ = cli.exec(&["stop"], &[]);

    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert!(
        out.kinds().iter().any(|k| k == "assistantMessageCompleted"),
        "the turn completed through the inline kernel: {}",
        out.diag()
    );
    assert!(
        !leaked_discovery,
        "an inline one-shot turn must not publish daemon discovery state ({})",
        port_file.display()
    );
    assert_eq!(p.count(), 1, "exactly one provider call for one turn");

    let status = cli.exec(&["status"], &[]);
    assert_eq!(status.code, Some(0), "{}", status.diag());
    let v: Value = serde_json::from_str(status.stdout.trim()).expect("status is one JSON record");
    assert_eq!(v["running"], false, "no daemon exists after the run: {v}");
}

fn semantic_only(kinds: Vec<String>) -> Vec<String> {
    kinds
        .into_iter()
        .filter(|k| SEMANTIC.contains(&k.as_str()))
        .collect()
}

fn tool_message_text(req: &Value) -> String {
    req["messages"]
        .as_array()
        .and_then(|m| m.iter().find(|x| x["role"] == "tool"))
        .map(|m| m["content"].to_string())
        .unwrap_or_default()
        .replace("\\\\", "/")
}

#[test]
fn inline_and_service_host_emit_the_same_semantic_fact_sequence_for_one_turn() {
    let args = json!({"path": "parity.txt", "content": "PARITY-CONTENT"});

    // InlineHost: `coderight run` owns the kernel in-process.
    let p = Provider::start(vec![
        Chat::tool("c1", "write_file", args.clone()).into(),
        Chat::text("parity done").into(),
    ]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json", "--bypass-approvals"], "write the file");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let inline = semantic_only(out.kinds());

    // ServiceHost: a real `coderight serve` daemon, same script, same prompt.
    let rig = e2e_api::Rig::start(vec![
        Step::tool("c1", "write_file", args),
        Step::text("parity done"),
    ]);
    rig.session("parity", json!({"permissionMode": "bypass"}));
    assert_eq!(rig.send("parity", "write the file").0, 200);
    rig.wait_assistant("parity", "parity done");
    let service = semantic_only(rig.kinds("parity"));

    assert_eq!(
        SEMANTIC.to_vec(),
        inline,
        "InlineHost must emit the full semantic fact sequence: {}",
        out.stdout
    );
    assert_eq!(
        inline, service,
        "InlineHost and ServiceHost must expose equivalent semantic facts"
    );

    // Same effect reached the model on both hosts.
    let inline_tool = tool_message_text(&p.requests()[1]);
    let service_tool = tool_message_text(&rig.provider.requests()[1]);
    assert!(
        inline_tool.contains("parity.txt") && service_tool.contains("parity.txt"),
        "inline={inline_tool} service={service_tool}"
    );
    assert!(
        inline_tool.contains("Created") && service_tool.contains("Created"),
        "inline={inline_tool} service={service_tool}"
    );
}
