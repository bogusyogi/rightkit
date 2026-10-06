//! Black-box contract: the real binary, both surfaces (CLI words and framed stdio) agree.
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_rightapp-engine");

#[test]
fn cli_and_sidecar_share_one_command_table() {
    let cli = Command::new(BIN).args(["echo", "--input", r#"{"a":1}"#]).output().unwrap();
    assert!(cli.status.success());
    let cli_json: serde_json::Value = serde_json::from_slice(&cli.stdout).unwrap();
    assert_eq!(cli_json, serde_json::json!({"a": 1}));

    let mut child = Command::new(BIN).arg("--stdio").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    writeln!(stdin, r#"{{"id":7,"cmd":"echo","input":{{"a":1}}}}"#).unwrap();
    writeln!(stdin, r#"{{"id":8,"cmd":"nope"}}"#).unwrap();
    drop(stdin); // closing stdin is the graceful shutdown signal
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let ok: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!((ok["id"].clone(), ok["result"].clone()), (7.into(), cli_json));
    line.clear();
    stdout.read_line(&mut line).unwrap();
    let err: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(err["error"]["code"], "unknown_command");
    assert!(child.wait().unwrap().success(), "engine exits cleanly when stdin closes");
}

#[test]
fn bad_input_and_unknown_command_exit_codes() {
    let bad = Command::new(BIN).args(["echo", "--input", "{not json"]).output().unwrap();
    assert_eq!(bad.status.code(), Some(2));
    let unknown = Command::new(BIN).arg("frobnicate").output().unwrap();
    assert_eq!(unknown.status.code(), Some(3));
}
