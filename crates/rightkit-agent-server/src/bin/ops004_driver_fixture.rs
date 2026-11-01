//! OPS-004 driver-fixture executable.
//!
//! The exhaustive route-acceptance driver (`tests/ops004_route_contract.rs`)
//! exercises the real driver chain: binary discovery, attestation readiness,
//! capability-lease issuance, governed process spawn, capability gateway
//! headers, and prompt/interrupt/stop lifecycle. That chain requires a
//! persistent stream-json process; relying on an installed vendor CLI would
//! make the qualification depend on machine-local auth state. This binary is
//! a real executable the daemon discovers, hashes, attests, and spawns under
//! the governed launcher exactly like a vendor CLI:
//!
//! - `--version` prints a version line and exits (discovery probe contract).
//! - Any other invocation emits one stream-json `system/init` frame and then
//!   holds stdin open so the daemon's supervisor sees a live binding until
//!   `stop` shuts it down.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--probe-governed-git") {
        probe_governed_git();
        return;
    }
    if args.iter().any(|arg| arg == "--version") {
        println!("ops004-driver-fixture 1.0.0");
        return;
    }
    // `claude auth status` is the daemon's auth probe: it must answer and
    // exit, not block on stdin like the stream-json session does.
    if args.windows(2).any(|pair| pair == ["auth", "status"]) {
        println!("{{\"loggedIn\":true}}");
        return;
    }
    if std::env::var_os("RIGHTKIT_VENDOR_APPROVAL_FIXTURE").is_some() {
        vendor_approval_fixture(&args);
        return;
    }
    println!(
        "{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"ops004-vendor-session\",\"cwd\":\"\",\"tools\":[],\"mcp_servers\":[],\"model\":\"ops004-fixture\",\"permissionMode\":\"default\"}}"
    );
    let _ = std::io::Write::flush(&mut std::io::stdout());
    // Confined Windows launches forward explicit environment entries only.
    // Select this fixture inside its isolated, readable workspace instead of
    // relying on an ambient daemon flag reaching the vendor process.
    if std::path::Path::new(".pl005-gateway-fixture").is_file()
        || std::env::var_os("RIGHTKIT_DRIVER_GATEWAY_FIXTURE").is_some()
    {
        gateway_fixture(&args);
        return;
    }
    use std::io::BufRead;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        if line.is_err() {
            break;
        }
    }
}

// PL-005 uses actual scoped MCP transport, not a fabricated readiness echo.
// This fixture-only child guard also reaps on an assertion panic.
struct FixtureMcpChild(std::process::Child);

impl Drop for FixtureMcpChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn gateway_fixture(args: &[String]) {
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Write};

    let config = args
        .windows(2)
        .find(|pair| pair[0] == "--mcp-config")
        .expect("scoped MCP launch config");
    let config: Value = serde_json::from_str(&config[1]).expect("MCP JSON");
    let server = &config["mcpServers"]["coderight"];
    let mut command =
        rightkit_child_process::command_std(server["command"].as_str().expect("MCP command"));
    for arg in server["args"].as_array().expect("MCP argv") {
        command.arg(arg.as_str().expect("MCP argument"));
    }
    let mut child = FixtureMcpChild(
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("scoped MCP child"),
    );
    let mut input = child.0.stdin.take().expect("MCP stdin");
    let mut output = BufReader::new(child.0.stdout.take().expect("MCP stdout"));
    let mut next_id = 0u32;
    let mut frame = Vec::new();
    let mut rpc = |method: &str, params: Value| -> Value {
        next_id += 1;
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","id":next_id,"method":method,"params":params})
        )
        .unwrap();
        input.flush().unwrap();
        let response: Value =
            rightkit_sidecar::read_json_line(&mut output, &mut frame, rightkit_sidecar::UNCAPPED)
                .expect("MCP response frame")
                .expect("MCP response before EOF");
        assert_eq!(response["id"], next_id, "correlated MCP response");
        assert!(
            response.get("error").is_none(),
            "MCP request failed: {response}"
        );
        response
    };
    rpc(
        "initialize",
        json!({"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"pl005-fixture","version":"1"}}),
    );
    let tools = rpc("tools/list", json!({}));
    assert!(
        tools["result"]["tools"]
            .as_array()
            .expect("leased tools")
            .iter()
            .any(|tool| tool["name"] == "read_file"),
        "read_file is leased"
    );
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let frame: Value =
            serde_json::from_str(&line.expect("driver input")).expect("driver stream JSON");
        if frame["type"] != "user" {
            continue;
        }
        let response = rpc(
            "tools/call",
            json!({"name":"read_file","arguments":{"path":"README.md"}}),
        );
        assert_eq!(
            response["result"]["isError"], false,
            "real gateway read succeeds: {response}"
        );
        writeln!(stdout, "{}", json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"gateway-read","name":"mcp__coderight__read_file","input":{"path":"README.md"}}]}})).unwrap();
        writeln!(stdout, "{}", json!({"type":"assistant","message":{"content":[{"type":"text","text":"gateway-read-ok"}]}})).unwrap();
        writeln!(
            stdout,
            "{}",
            json!({"type":"result","subtype":"success","result":"gateway-read-ok"})
        )
        .unwrap();
        stdout.flush().unwrap();
    }
    drop(rpc);
    drop(input);
    drop(output);
    drop(child);
}

fn probe_governed_git() {
    use coderight_execution::{CommandLaunchSpec, ProcessLaunchAdapter, ProcessLaunchContext};
    use coderight_tools::process_resource_authority::{
        install_process_resource_profile, GovernedProcessLaunchAdapter, ResourceKind,
        ResourceProfile,
    };
    use tokio::io::AsyncReadExt;
    let governor = install_process_resource_profile(ResourceProfile::Practical).unwrap();
    let launcher = GovernedProcessLaunchAdapter::new(
        coderight_tools::shell::NativeCommandSandboxAdapter,
        governor,
        ResourceKind::ChildProcess,
        true,
    );
    let root = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    #[cfg(windows)]
    {
        let canonical = std::fs::canonicalize(&root).unwrap();
        let mut hash = 0xcbf29ce484222325u64;
        for unit in canonical
            .to_string_lossy()
            .to_ascii_lowercase()
            .encode_utf16()
        {
            for byte in unit.to_le_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x100000001b3);
            }
        }
        println!("governed git: before profile");
        let profile = coderight_tools::sandbox::AppContainerProfile::ensure(
            &format!("coderight.sandbox.workspace.{hash:016x}"),
            "RightKit workspace sandbox",
        )
        .unwrap();
        println!("governed git: profile ready; before workspace ACL");
        profile.grant_path_access(&canonical, true).unwrap();
        println!("governed git: workspace ACL ready");
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        println!("governed git: before spawn");
        let mut child = launcher
            .spawn(CommandLaunchSpec {
                program: "git".into(),
                argv: vec!["rev-parse".into(), "HEAD".into()],
                cwd: root.clone(),
                context: ProcessLaunchContext {
                    workspace_root: root,
                    sandbox_profile: Some("workspace".into()),
                    sandbox_required: true,
                    network_required: false,
                },
                env: vec![],
            })
            .await
            .unwrap();
        println!("governed git: spawned");
        let mut stdout = child.take_stdout().unwrap();
        let mut stderr = child.take_stderr().unwrap();
        let (out, err) = tokio::join!(
            async {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).await
            },
            async {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).await
            }
        );
        println!("governed git: streams completed: {out:?} {err:?}");
        println!("governed git: exit {:?}", child.wait().await);
    });
}

// Protocol-only installed TUI acceptance. This fixture never contacts a
// vendor or model; it consumes the real launch contract and MCP transport.
fn vendor_approval_fixture(args: &[String]) {
    use serde_json::{json, Value};
    use std::io::{BufReader, Write};
    let path = std::env::var("RIGHTKIT_VENDOR_FIXTURE_PATH").expect("isolated fixture output path");
    let trace_path = format!("{path}.trace");
    let trace = |stage: &str| {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&trace_path)
            .expect("fixture trace");
        writeln!(file, "{stage}").unwrap();
    };
    let config = args
        .windows(2)
        .find(|pair| pair[0] == "--mcp-config")
        .expect("MCP config");
    assert!(args.windows(2).any(|pair| pair
        == [
            "--permission-prompt-tool",
            "mcp__coderight__permission_prompt"
        ]));
    let config: Value = serde_json::from_str(&config[1]).expect("MCP JSON");
    let server = &config["mcpServers"]["coderight"];
    let mut command = rightkit_child_process::command_std(server["command"].as_str().unwrap());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    for arg in server["args"].as_array().unwrap() {
        command.arg(arg.as_str().unwrap());
    }
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("installed MCP child");
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut frame = Vec::new();
    let mut rpc = |id: u32, method: &str, params: Value| -> Value {
        trace(&format!("rpc {id} {method}: sent"));
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .unwrap();
        input.flush().unwrap();
        let response: Value =
            rightkit_sidecar::read_json_line(&mut output, &mut frame, rightkit_sidecar::UNCAPPED)
                .expect("MCP response frame")
                .expect("MCP response before EOF");
        trace(&format!("rpc {id} {method}: received"));
        response
    };
    println!(
        "{}",
        json!({"type":"system","subtype":"init","session_id":"approval-fixture"})
    );
    std::io::stdout().flush().unwrap();
    rpc(1, "initialize", json!({}));
    let tools = rpc(2, "tools/list", json!({}));
    assert!(tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "write_file"));
    let arguments = json!({"path":path,"content":"RightKit installed approval fixture\n"});
    let response = rpc(
        3,
        "tools/call",
        json!({"name":"permission_prompt","arguments":{
            "tool_name":"mcp__coderight__write_file","input":arguments
        }}),
    );
    let decision: Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    trace(if decision["behavior"] == "allow" {
        "decision: allow"
    } else {
        "decision: deny"
    });
    let text = if decision["behavior"] == "allow" {
        let response = rpc(
            4,
            "tools/call",
            json!({"name":"write_file","arguments":arguments}),
        );
        if response["result"]["isError"] == false {
            "Approval fixture: allow; write succeeded"
        } else {
            trace(&format!(
                "write error: {}",
                response["result"]["content"][0]["text"]
            ));
            "Approval fixture: allow; write failed"
        }
    } else {
        "Approval fixture: deny; no write"
    };
    trace(text);
    println!(
        "{}",
        json!({"type":"assistant","message":{"content":[{"type":"text","text":text}]}})
    );
    println!(
        "{}",
        json!({"type":"result","subtype":"success","result":text})
    );
    std::io::stdout().flush().unwrap();
    drop(rpc);
    drop(input);
    let _ = child.wait();
}
