//! Real right-ctl -> MCP stdio -> rightkit-service control-service round trip.

use rightkit_agent_tools::CTL_CALLER;
use rightkit_service::{host, Allowlist, CallContext, Emit, Handler, ServiceConfig, ServiceError, Suite};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

struct Demo;

impl Handler for Demo {
    fn call(&self, _cx: &CallContext, method: &str, params: &Value, _emit: &Emit) -> Result<Value, ServiceError> {
        match method {
            "health" => Ok(json!({"ok": true, "app": "demo"})),
            "command" => Ok(json!({"command": params["name"], "args": params["args"]})),
            _ => Err(ServiceError::new("unknown_method", method)),
        }
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn stdout_lines(stdout: ChildStdout) -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    rx
}

fn request(stdin: &mut impl Write, output: &Receiver<String>, id: u64, method: &str, params: Value) -> Value {
    writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap();
    stdin.flush().unwrap();
    let line = output.recv_timeout(Duration::from_secs(10)).expect("MCP response timeout");
    serde_json::from_str(&line).unwrap()
}

fn wait_bounded(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return child.wait().unwrap();
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn right_ctl_mcp_drives_service_app() {
    let temp = tempfile::tempdir().unwrap();
    let suite = Suite::at(temp.path().join("suite")).unwrap();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let host_suite = suite.clone();
    let host_thread = thread::spawn(move || {
        let allow = Allowlist::new().allow(CTL_CALLER, ["health", "command"]);
        let cfg = ServiceConfig::new("control-demo", "test", 0, "e2e", allow);
        let hosted = host(&host_suite, cfg, Arc::new(Demo)).unwrap();
        let _ = stop_rx.recv();
        drop(hosted);
    });

    let deadline = Instant::now() + Duration::from_secs(5);
    while rightkit_service::read_live_record(&suite, "control-demo").is_none() {
        assert!(Instant::now() < deadline, "service host did not publish discovery");
        thread::sleep(Duration::from_millis(20));
    }

    let mut child = ChildGuard(Command::new(env!("CARGO_BIN_EXE_right-ctl"))
        .args(["--suite-root", suite.root().to_str().unwrap(), "mcp-serve", "demo"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap());
    let mut stdin = child.0.stdin.take().unwrap();
    let output = stdout_lines(child.0.stdout.take().unwrap());
    let mut stderr = child.0.stderr.take().unwrap();

    let init = request(&mut stdin, &output, 1, "initialize", json!({"protocolVersion": "2025-03-26"}));
    assert_eq!(init["result"]["serverInfo"]["name"], "right-ctl");
    writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).unwrap();
    let listed = request(&mut stdin, &output, 2, "tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "demo.health"));
    assert!(tools.iter().any(|t| t["name"] == "demo.command"));

    let health = request(&mut stdin, &output, 3, "tools/call", json!({"name": "demo.health", "arguments": {}}));
    assert_eq!(health["result"]["isError"], false);
    assert!(health["result"]["structuredContent"]["ok"].as_bool().unwrap());
    let command = request(&mut stdin, &output, 4, "tools/call", json!({"name": "demo.command", "arguments": {"name": "echo", "args": "hello"}}));
    assert_eq!(command["result"]["isError"], false);
    assert_eq!(command["result"]["structuredContent"]["command"], "echo");

    drop(stdin);
    let status = wait_bounded(&mut child.0);
    let mut stderr_bytes = Vec::new();
    stderr.read_to_end(&mut stderr_bytes).unwrap();
    assert!(status.success(), "right-ctl stderr: {}", String::from_utf8_lossy(&stderr_bytes));
    let _ = stop_tx.send(());
    host_thread.join().unwrap();
}
