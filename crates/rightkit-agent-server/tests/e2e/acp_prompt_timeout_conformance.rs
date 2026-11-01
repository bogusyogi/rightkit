//! S:P8 actual daemon/provider/ACP timeout qualification, with no shortened deadline.
#![allow(dead_code)]

use crate::support::daemon_process;
use crate::support::mock_provider;

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct AcpChild(Child);
impl Drop for AcpChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn request(child: &mut Child, receiver: &mpsc::Receiver<Value>, request: Value) -> Value {
    let stdin = child.stdin.as_mut().expect("ACP stdin");
    writeln!(stdin, "{request}").expect("write ACP request");
    stdin.flush().expect("flush ACP request");
    let deadline = Instant::now() + Duration::from_secs(150);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let value = receiver
            .recv_timeout(remaining)
            .expect("bounded ACP response");
        if value["id"] == request["id"] {
            return value;
        }
    }
}

#[test]
#[ignore = "real production timeout takes 120 seconds; run explicitly for S:P8"]
fn stalled_provider_reaches_real_acp_timeout_golden() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let acp_home = tempfile::tempdir().unwrap();
    let provider = mock_provider::MockProvider::start_stalled();
    // Must name an embedded-catalog slug: the CRA-10 capacity gate resolves
    // the request model against loaded model metadata before dispatch.
    daemon_process::write_local_provider_route_config(
        home.path(),
        &provider.base_url(),
        "deepseek/deepseek-v4-pro",
    );
    let mut daemon = daemon_process::start_daemon(home.path(), &[]);
    let mut acp = AcpChild(
        rightkit_child_process::command_std(daemon_process::executable())
            .arg("acp-server")
            .arg("--port")
            .arg(daemon.startup.port.to_string())
            .arg("--token")
            .arg(&daemon.startup.token)
            .envs([crate::common::suite_env(acp_home.path())])
            .env("HOME", acp_home.path())
            .env("USERPROFILE", acp_home.path())
            .env("XDG_CONFIG_HOME", acp_home.path().join("config"))
            .env("XDG_DATA_HOME", acp_home.path().join("data"))
            .env("RIGHTKIT_DISABLE_OS_KEYCHAIN", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn real ACP bridge"),
    );
    let stdout = acp.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if sender.send(value).is_err() {
                    break;
                }
            }
        }
    });
    let initialized = request(
        &mut acp.0,
        &receiver,
        json!({"jsonrpc":"2.0","id":1,
        "method":"initialize","params":{"protocolVersion":1}}),
    );
    assert_eq!(initialized["result"]["protocolVersion"], 1);
    let created = request(
        &mut acp.0,
        &receiver,
        json!({"jsonrpc":"2.0","id":2,
        "method":"session/new","params":{"cwd":workspace.path()}}),
    );
    let session = created["result"]["sessionId"]
        .as_str()
        .expect("new session ID");
    let started = Instant::now();
    let response = request(
        &mut acp.0,
        &receiver,
        json!({"jsonrpc":"2.0","id":3,
        "method":"session/prompt","params":{"sessionId":session,
        "prompt":[{"type":"text","text":"stall provider"}]}}),
    );
    let golden = include_str!("../../../../fixtures/machine-contract/acp-negotiation.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["case"] == "prompt-timeout")
        .unwrap();
    assert_eq!(
        response["error"]["code"], golden["error"]["code"],
        "{response}"
    );
    for field in ["code", "retryable", "effectState"] {
        assert_eq!(
            response["error"]["data"][field],
            golden["error"]["data"][field]
        );
    }
    assert_eq!(response["error"]["data"]["sessionId"], session);
    assert!(response["error"]["data"]["afterSeq"].is_i64());
    assert!(started.elapsed() >= Duration::from_secs(120));
    assert_eq!(
        provider.dispatches(),
        1,
        "independent provider request count"
    );
    assert!(daemon.is_running());
    drop(acp);
    reader.join().expect("ACP output reader exits");
}
