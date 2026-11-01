//! Context-client e2e (MEM lane): shell output keeps a durable full copy with
//! a recovery handle, and the context-off turn carries no generic memory
//! block alongside (absent) Membrane federation.

use crate::support::e2e_cli;
use e2e_cli::*;

fn tool_result_texts(req: &Value) -> Vec<String> {
    messages(req, "tool")
        .iter()
        .filter_map(|m| match &m["content"] {
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        })
        .collect()
}

#[test]
fn shell_output_reduction_keeps_full_copy_with_recovery_handle() {
    // 600 numbered lines: far past the head+tail line budget.
    let big: String = (0..600).map(|i| format!("line-{i:04}\n")).collect();
    let provider = Provider::start(vec![
        Chat::tool("big-1", "shell", json!({"command": "type big.txt"})).into(),
        Chat::text("done").into(),
    ]);
    let cli = Cli::with_config(&provider, "", &[("big.txt", big.as_str())]);
    let out = cli.run(&["--bypass-approvals"], "print big.txt");
    assert_eq!(out.code, Some(0), "{}", out.diag());

    let requests = provider.requests();
    let second = requests
        .get(1)
        .expect("tool result was sent back to the model");
    let result = tool_result_texts(second)
        .into_iter()
        .find(|text| text.contains("lines elided"))
        .unwrap_or_else(|| panic!("reduced shell result missing: {second}"));
    // The middle is omitted from the model's view...
    assert!(
        !result.contains("line-0300"),
        "middle must be elided inline"
    );
    // ...but a recovery handle points at the durable full copy.
    let marker = "stored) at ";
    let start = result.find(marker).expect("recovery handle present") + marker.len();
    let end = result[start..]
        .find("; read that file")
        .expect("handle names how to read it")
        + start;
    let path = std::path::PathBuf::from(&result[start..end]);
    let full = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("recovery copy {} unreadable: {e}", path.display()));
    assert!(full.contains("line-0000") && full.contains("line-0300") && full.contains("line-0599"));
    // The spill lives in CodeRight's own store, never the user's workspace.
    let ws = cli.ws().canonicalize().expect("workspace");
    let spill = path.canonicalize().expect("spill path");
    assert!(
        !spill.starts_with(&ws),
        "handle {spill:?} is inside workspace {ws:?}"
    );
    assert!(
        spill.starts_with(cli.home.path().canonicalize().unwrap()),
        "spill under the isolated suite root"
    );
    let status = rightkit_child_process::command_std("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(&ws)
        .output()
        .expect("git status");
    assert_eq!(
        String::from_utf8_lossy(&status.stdout).trim(),
        "",
        "workspace must stay clean"
    );
}

#[test]
fn context_off_turn_injects_no_generic_memory_block() {
    let provider = Provider::start(vec![Chat::text("ok").into()]);
    let cli = Cli::new(&provider);
    let out = cli.run(&["--bypass-approvals"], "say hi");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let requests = provider.requests();
    let first = requests.first().expect("one provider request");
    let system = system_text(first);
    assert!(
        !system.contains("<workspace_memory>"),
        "no turn-0 memory block may be injected into the system prompt: {system}"
    );
    assert!(
        !first
            .to_string()
            .contains("Relevant durable memory from earlier runs"),
        "no memory reminder when context is off"
    );
}
