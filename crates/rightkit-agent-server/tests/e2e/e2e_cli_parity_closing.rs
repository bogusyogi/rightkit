//! CLI parity journeys for options that must reach the shared session runtime.

use crate::support::e2e_cli;
use e2e_cli::*;

use std::io::Write;
use std::process::{Command, Stdio};

fn finish_with_stdin(mut command: Command, input: &str) -> Out {
    use std::io::Read;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn coderight");
    {
        let mut stdin = child.stdin.take().expect("child stdin");
        stdin
            .write_all(input.as_bytes())
            .expect("write JSONL input");
    }
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut value = String::new();
        let _ = stdout.read_to_string(&mut value);
        value
    });
    let err = std::thread::spawn(move || {
        let mut value = String::new();
        let _ = stderr.read_to_string(&mut value);
        value
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break Some(-999);
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    };
    Out {
        code,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}

#[test]
fn stream_json_accepts_live_followup_messages_and_image_attachment() {
    let provider = Provider::start(vec![
        Chat::text("first response").into(),
        Chat::text("second response").into(),
    ]);
    let cli = Cli::new(&provider);
    let image = cli.ws().join("input.png");
    let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
    std::fs::write(&image, png).expect("write fixture image");
    let mut command = cli.command(&["--input-format", "stream-json", "--bypass-approvals"]);
    command.arg("--image").arg(&image);
    let out = finish_with_stdin(
        command,
        "{\"type\":\"user_message\",\"text\":\"first live turn\"}\n{\"type\":\"user_message\",\"text\":\"second live turn\"}\n",
    );
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let requests = provider.requests();
    assert_eq!(requests.len(), 2, "each user_message gets a provider turn");
    assert!(requests[0].to_string().contains("first live turn"));
    assert!(
        requests[0].to_string().contains("image/png"),
        "image attachment reaches provider: {}",
        requests[0]
    );
    assert!(requests[1].to_string().contains("second live turn"));
}

#[test]
fn output_last_message_writes_final_answer_and_ephemeral_run_completes() {
    let provider = Provider::start(vec![Chat::text("saved final answer").into()]);
    let cli = Cli::new(&provider);
    let output = cli.ws().join("last-message.txt");
    let mut command = cli.command(&["-p", "--ephemeral", "--bypass-approvals"]);
    command
        .arg("--output-last-message")
        .arg(&output)
        .arg("write it");
    let result = finish(command, 90);
    assert_eq!(result.code, Some(0), "{}", result.diag());
    assert_eq!(
        std::fs::read_to_string(output).unwrap(),
        "saved final answer"
    );
    assert_eq!(provider.count(), 1);
}

#[test]
fn run_limits_and_prompt_context_flags_are_consumed_by_session_runtime() {
    let provider = Provider::start(vec![Chat::text("options consumed").into()]);
    let cli = Cli::with_config(
        &provider,
        "",
        &[
            ("extra-instructions/AGENTS.md", "ADDED-DIR-INSTRUCTION"),
            ("mcp.json", "{\"mcpServers\":{}}"),
        ],
    );
    let out = cli.run(
        &[
            "--max-turns",
            "2",
            "--max-budget-usd",
            "1.25",
            "--mcp-config",
            "mcp.json",
            "--system-prompt",
            "CLI-SYSTEM-PROMPT",
            "--append-system-prompt",
            "CLI-APPENDED-PROMPT",
            "--add-dir",
            "extra-instructions",
            "--bypass-approvals",
        ],
        "check options",
    );
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let request = provider.requests()[0].to_string();
    assert!(
        request.contains("CLI-SYSTEM-PROMPT"),
        "replacement prompt reaches provider: {request}"
    );
    assert!(
        request.contains("CLI-APPENDED-PROMPT"),
        "append prompt reaches provider: {request}"
    );
    assert!(
        request.contains("ADDED-DIR-INSTRUCTION"),
        "add-dir instructions reach provider: {request}"
    );
}

#[test]
fn bare_suppresses_project_instructions_while_system_prompt_remains_explicit() {
    let provider = Provider::start(vec![Chat::text("bare prompt").into()]);
    let cli = Cli::with_config(&provider, "", &[("AGENTS.md", "PROJECT-ONLY-INSTRUCTION")]);
    let out = cli.run(
        &[
            "--bare",
            "--system-prompt",
            "EXPLICIT-BARE-SYSTEM",
            "--bypass-approvals",
        ],
        "check bare",
    );
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let request = provider.requests()[0].to_string();
    assert!(
        request.contains("EXPLICIT-BARE-SYSTEM"),
        "explicit prompt still applies: {request}"
    );
    assert!(
        !request.contains("PROJECT-ONLY-INSTRUCTION"),
        "bare suppresses project instructions: {request}"
    );
}

#[test]
fn include_exclude_tool_globs_and_no_mcp_reach_provider_tool_contract() {
    let provider = Provider::start(vec![Chat::text("filtered tools").into()]);
    let cli = Cli::new(&provider);
    let out = cli.run(
        &[
            "--tools",
            "*_file",
            "--exclude-tools",
            "write_*",
            "--no-mcp",
            "--bypass-approvals",
        ],
        "inspect tools",
    );
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let requests = provider.requests();
    let request = &requests[0];
    let tool_names = request["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect::<Vec<_>>();
    assert!(
        tool_names.contains(&"read_file"),
        "include glob selects read_file: {tool_names:?}"
    );
    assert!(
        !tool_names.contains(&"write_file"),
        "exclude glob removes write_file: {tool_names:?}"
    );
    assert!(
        !tool_names.contains(&"mcp_call"),
        "--no-mcp removes MCP dispatch: {tool_names:?}"
    );
}

#[test]
fn color_always_reaches_text_event_projection() {
    let provider = Provider::start(vec![
        Chat::tool("call-color", "read_file", json!({"path":"README.md"})).into(),
        Chat::text("colored result").into(),
    ]);
    let cli = Cli::new(&provider);
    let out = cli.run(&["--color", "always", "--bypass-approvals"], "read README");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert!(
        out.stderr.contains("\u{1b}[36m[tool]"),
        "forced ANSI tool label: {:?}",
        out.stderr
    );
}

#[test]
fn bare_continue_print_launch_resumes_most_recent_session() {
    let provider = Provider::start(vec![
        Chat::text("first persisted response").into(),
        Chat::text("continued response").into(),
        Chat::text("resumed response").into(),
    ]);
    let cli = Cli::new(&provider);
    let first = cli.run(
        &["-p", "--session-id", "continue-latest"],
        "persist this turn",
    );
    assert_eq!(first.code, Some(0), "{}", first.diag());
    let continued = cli.exec(&["--continue", "-p", "continue this session"], &[]);
    assert_eq!(continued.code, Some(0), "{}", continued.diag());
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].to_string().contains("persist this turn"));
    assert!(requests[1].to_string().contains("continue this session"));
    let resumed = cli.exec(
        &[
            "--resume",
            "continue-latest",
            "-p",
            "resume explicit session",
        ],
        &[],
    );
    assert_eq!(resumed.code, Some(0), "{}", resumed.diag());
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].to_string().contains("persist this turn"));
    assert!(requests[2].to_string().contains("resume explicit session"));
}

#[test]
fn bare_positional_prompt_starts_first_interactive_fallback_turn() {
    let provider = Provider::start(vec![Chat::text("bare prompt response").into()]);
    let cli = Cli::new(&provider);
    let out = cli.exec(&["bare positional prompt"], &[]);
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert!(provider.requests()[0]
        .to_string()
        .contains("bare positional prompt"));
}

#[test]
fn launch_provider_effort_and_workspace_apply_to_positional_prompt() {
    let provider = Provider::start(vec![Chat::text("launch route response").into()]);
    let cli = Cli::new(&provider);
    let workspace = cli.ws();
    let out = cli.exec(
        &[
            "--provider",
            "qual-fixture",
            "--effort",
            "medium",
            "--workspace",
            workspace.to_str().unwrap(),
            "launch routed prompt",
        ],
        &[],
    );
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert!(provider.requests()[0]
        .to_string()
        .contains("launch routed prompt"));
}
