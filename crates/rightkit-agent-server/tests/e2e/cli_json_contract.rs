use std::io::Write;
use std::process::Stdio;

fn run_with_stdin(args: &[&str], input: &[u8]) -> std::process::Output {
    // Fresh isolated suite root: a test-mode binary with no root would use the real data folder.
    let suite = tempfile::tempdir().expect("temp suite root");
    let mut child = rightkit_child_process::command_std(env!("CARGO_BIN_EXE_coderight"))
        .envs([crate::common::suite_env(suite.path())])
        .args(args)
        .env_remove("RIGHTKIT_DAEMON_URL")
        .env_remove("RIGHTKIT_DAEMON_TOKEN")
        .env("RIGHTKIT_NO_AUTOSTART", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn coderight");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input)
        .expect("write child stdin");
    child.wait_with_output().expect("wait for coderight")
}

fn assert_single_jsonl_record(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "stdout must contain one JSONL record: {stdout:?}"
    );
    serde_json::from_str(lines[0]).expect("stdout record is JSON")
}

#[test]
fn info_json_has_stable_cli_schema_version() {
    let suite = tempfile::tempdir().expect("temp suite root");
    let output = rightkit_child_process::command_std(env!("CARGO_BIN_EXE_coderight"))
        .envs([crate::common::suite_env(suite.path())])
        .args(["info", "--json"])
        .output()
        .expect("run coderight info --json");

    assert!(
        output.status.success(),
        "info --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("info --json stdout is JSON");

    assert_eq!(value["schema_version"], "coderight.cli.info.v1");
    assert!(value["version"].as_str().is_some());
    assert!(value["daemon"]["running"].as_bool().is_some());
}

#[test]
fn doctor_json_has_stable_cli_schema_version() {
    let suite = tempfile::tempdir().expect("temp suite root");
    let output = rightkit_child_process::command_std(env!("CARGO_BIN_EXE_coderight"))
        .envs([crate::common::suite_env(suite.path())])
        .args(["doctor", "--json"])
        .output()
        .expect("run coderight doctor --json");

    assert!(
        matches!(output.status.code(), Some(0..=2)),
        "doctor --json had unexpected exit: {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor --json stdout is JSON");

    assert_eq!(value["schema_version"], "coderight.cli.doctor.v1");
    assert!(matches!(
        value["verdict"].as_str(),
        Some("pass" | "warn" | "block")
    ));
    assert!(value["checks"].as_array().is_some());
}

#[test]
fn stream_json_input_keeps_stdout_pure_jsonl() {
    let output = run_with_stdin(
        &["run", "--input-format", "stream-json"],
        br#"{"type":"control","action":"ping"}
"#,
    );

    assert_eq!(output.status.code(), Some(2));
    let value = assert_single_jsonl_record(&output);
    assert_eq!(value["schema_version"], "coderight.cli.error.v1");
    assert_eq!(value["event"], "error");
}

#[test]
fn pipe_machine_failure_keeps_stdout_single_line_json() {
    let output = run_with_stdin(&["run", "--pipe", "--output-format", "json"], b"\n");

    assert_eq!(output.status.code(), Some(2));
    let value = assert_single_jsonl_record(&output);
    assert_eq!(value["schema_version"], "coderight.cli.error.v1");
    assert_eq!(value["error"]["class"], "usage");
}
