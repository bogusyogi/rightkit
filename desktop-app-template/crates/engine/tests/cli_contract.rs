//! Black-box contract: the real binary, both surfaces (CLI words and rightkit-jobs stdio frames) agree.
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_rightapp-engine");

fn data_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rightapp-engine-{tag}-{}", std::process::id()))
}

#[test]
fn cli_and_sidecar_share_one_command_table() {
    let data = data_dir("contract");
    let cli = Command::new(BIN).env("RIGHTAPP_APP_DATA", &data).args(["echo", "--input", r#"{"a":1}"#]).output().unwrap();
    assert!(cli.status.success(), "{}", String::from_utf8_lossy(&cli.stderr));
    let cli_json: serde_json::Value = serde_json::from_slice(&cli.stdout).unwrap();
    assert_eq!(cli_json, serde_json::json!({"a": 1}));

    let mut child = Command::new(BIN)
        .env("RIGHTAPP_APP_DATA", &data)
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let hello: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["engine"], "rightapp-engine");

    writeln!(stdin, r#"{{"type":"request","id":7,"method":"echo","params":{{"a":1}}}}"#).unwrap();
    writeln!(stdin, r#"{{"type":"request","id":8,"method":"nope","params":{{}}}}"#).unwrap();
    drop(stdin); // closing stdin is the graceful shutdown signal
    line.clear();
    stdout.read_line(&mut line).unwrap();
    let ok: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!((ok["type"].clone(), ok["id"].clone(), ok["ok"].clone()), ("response".into(), 7.into(), true.into()));
    assert_eq!(ok["result"], cli_json);
    line.clear();
    stdout.read_line(&mut line).unwrap();
    let err: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(err["ok"], false);
    assert_eq!(err["error"]["code"], "unknown_method");
    assert!(child.wait().unwrap().success(), "engine exits cleanly when stdin closes");
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn bad_input_and_unknown_command_exit_codes() {
    let data = data_dir("exit");
    let bad = Command::new(BIN).env("RIGHTAPP_APP_DATA", &data).args(["echo", "--input", "{not json"]).output().unwrap();
    assert_eq!(bad.status.code(), Some(2));
    let unknown = Command::new(BIN).env("RIGHTAPP_APP_DATA", &data).arg("frobnicate").output().unwrap();
    assert_eq!(unknown.status.code(), Some(3));
    let _ = std::fs::remove_dir_all(&data);
}
