use rightkit_mcp::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct WrappedChild {
    child: Box<dyn ProcessChild>,
    stopped: Arc<Mutex<Vec<&'static str>>>,
}
impl ProcessChild for WrappedChild {
    fn id(&self) -> u32 {
        self.child.id()
    }
    fn kill(&mut self) -> std::io::Result<()> {
        self.stopped.lock().unwrap().push("kill");
        self.child.kill()
    }
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }
    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.stopped.lock().unwrap().push("wait");
        self.child.wait()
    }
}

#[test]
fn endpoint_origin_policy_normalizes_and_fails_closed() {
    let policy = EndpointOriginPolicy::default();
    assert_eq!(
        policy
            .resolve_endpoint("https://EXAMPLE.com:443/sse", "/messages?sid=1")
            .unwrap(),
        "https://example.com/messages?sid=1"
    );
    assert_eq!(
        policy
            .resolve_endpoint("https://example.com/dir/sse", "../messages")
            .unwrap(),
        "https://example.com/messages"
    );
    for endpoint in [
        "https://evil.test/messages",
        "https://example.com:444/messages",
        "http://example.com/messages",
        "//evil.test/messages",
    ] {
        assert!(matches!(
            policy.resolve_endpoint("https://example.com/sse", endpoint),
            Err(McpError::CrossOriginEndpoint { .. })
        ));
    }
    for endpoint in [
        "ftp://example.com/messages",
        "https://user:secret@example.com/messages",
        "https://example.com/messages#fragment",
    ] {
        assert!(matches!(
            policy.resolve_endpoint("https://example.com/sse", endpoint),
            Err(McpError::Protocol(_))
        ));
    }
    let policy = EndpointOriginPolicy {
        allowed_origins: vec!["https://OTHER.test:443".into()],
    };
    assert_eq!(
        policy
            .resolve_endpoint("https://example.com/sse", "https://other.test/messages")
            .unwrap(),
        "https://other.test/messages"
    );
    assert!(matches!(
        policy.resolve_endpoint("https://example.com/sse", "https://other.test:444/messages"),
        Err(McpError::CrossOriginEndpoint { .. })
    ));
}

#[tokio::test]
async fn cancellation_is_sticky_and_wakes_all_waiters() {
    let token = CancellationToken::new();
    let first = token.clone();
    let second = token.clone();
    let tasks = [
        tokio::spawn(async move { first.cancelled().await }),
        tokio::spawn(async move { second.cancelled().await }),
    ];
    token.cancel();
    for task in tasks {
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(1), token.cancelled())
        .await
        .unwrap();
    assert!(token.is_cancelled());
}

// Same integration-test executable serves as a portable stdio fixture. No shell,
// Node, Python, extra build, or fixed port is needed.
#[test]
fn stdio_fixture() {
    if std::env::var_os("RK_MCP_STDIO_FIXTURE").is_none() {
        return;
    }
    let mut waiting = HashMap::<String, Value>::new();
    let mut cancelled = Vec::<Value>::new();
    let send = |value: Value| {
        // Separate harness's "test ..." text from JSON-RPC output.
        println!("\n{value}");
        std::io::stdout().flush().unwrap();
    };
    for line in std::io::stdin().lock().lines() {
        let message: Value = match serde_json::from_str(&line.unwrap()) {
            Ok(message) => message,
            Err(_) => continue,
        };
        match message.get("method").and_then(Value::as_str) {
            Some("initialize") => send(
                json!({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":message["params"]["protocolVersion"],"capabilities":{}}}),
            ),
            Some("tools/call") => {
                let callback_id = format!("elicitation-{}", message["id"]);
                waiting.insert(callback_id.clone(), message.clone());
                send(
                    json!({"jsonrpc":"2.0","id":callback_id,"method":"elicitation/create","params":{"message":message["params"]["arguments"]["label"],"requestedSchema":{"type":"object"},"_meta":{"relatedRequestId":message["id"]}}}),
                );
            }
            Some("wait") => {
                if let Some(path) = message.pointer("/params/marker").and_then(Value::as_str) {
                    std::fs::write(path, b"ready").unwrap();
                }
            }
            Some("notifications/cancelled") => {
                cancelled.push(message["params"]["requestId"].clone())
            }
            Some("ping") => {
                send(json!({"jsonrpc":"2.0","id":message["id"],"result":{"cancelled":cancelled}}))
            }
            Some(_) => {}
            None => {
                if let Some(original) = message
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(|id| waiting.remove(id))
                {
                    send(
                        json!({"jsonrpc":"2.0","id":original["id"],"result":{"elicitationResult":message.get("result"),"elicitationError":message.get("error")}}),
                    );
                }
            }
        }
    }
}

fn stdio_spec(cwd: Option<std::path::PathBuf>) -> ServerSpec {
    ServerSpec::Stdio {
        command: std::env::current_exe()
            .unwrap()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        args: vec![
            "--exact".into(),
            "stdio_fixture".into(),
            "--nocapture".into(),
        ],
        env: BTreeMap::from([
            ("RK_MCP_STDIO_FIXTURE".into(), "1".into()),
            ("RK_EXACT_ENV".into(), "spaces & symbols".into()),
        ]),
        cwd,
    }
}

#[test]
fn launcher_receives_exact_inputs_and_supplies_working_stdio() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path().canonicalize().unwrap();
    let spec = stdio_spec(Some(cwd.clone()));
    let ServerSpec::Stdio {
        command,
        args,
        env,
        cwd,
    } = &spec
    else {
        unreachable!()
    };
    let expected = LaunchRequest {
        program: command.clone(),
        args: args.clone(),
        env: env.clone(),
        cwd: cwd.clone(),
    };
    let recorded = Arc::new(Mutex::new(Vec::<LaunchRequest>::new()));
    let output = recorded.clone();
    let stopped = Arc::new(Mutex::new(Vec::new()));
    let stopped_by_launcher = stopped.clone();
    let launcher = move |request: &LaunchRequest| -> Result<LaunchedProcess> {
        output.lock().unwrap().push(request.clone());
        let process = DefaultProcessLauncher.launch(request)?;
        Ok(LaunchedProcess {
            child: Box::new(WrappedChild {
                child: process.child,
                stopped: stopped_by_launcher.clone(),
            }),
            stdin: process.stdin,
            stdout: process.stdout,
        })
    };
    let client = McpClient::connect_with_extensions(
        "fixture",
        spec,
        ClientOptions {
            request_timeout: Duration::from_secs(5),
            ..Default::default()
        },
        ClientExtensions {
            launcher: Some(Arc::new(launcher)),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(*recorded.lock().unwrap(), vec![expected]);
    client.ping().unwrap();
    client.close();
    assert_eq!(*stopped.lock().unwrap(), vec!["kill", "wait"]);
}

#[test]
fn stdio_elicitation_routes_related_calls_to_exact_logical_sessions() {
    let client = Arc::new(
        McpClient::connect_with_extensions(
            "fixture",
            stdio_spec(None),
            ClientOptions {
                request_timeout: Duration::from_secs(5),
                ..Default::default()
            },
            ClientExtensions::default(),
        )
        .unwrap(),
    );
    let seen = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
    for session in ["session-one", "session-two"] {
        let output = seen.clone();
        let expected = session.to_string();
        client
            .set_elicitation_handler(
                session,
                Arc::new(move |context, params| {
                    assert_eq!(context.session_id, expected);
                    assert_eq!(params["message"], expected);
                    output
                        .lock()
                        .unwrap()
                        .push((context.session_id.clone(), params.clone()));
                    Ok(json!({"action":"accept","content":{"session":context.session_id}}))
                }),
            )
            .unwrap();
    }
    let calls: Vec<_> = ["session-one", "session-two"]
        .into_iter()
        .map(|session| {
            let client = client.clone();
            std::thread::spawn(move || {
                client
                    .request_with_options(
                        "tools/call",
                        json!({"name":"fixture","arguments":{"label":session}}),
                        &CallOptions {
                            session_id: Some(session.into()),
                            ..Default::default()
                        },
                    )
                    .unwrap()
            })
        })
        .collect();
    for (call, expected) in calls.into_iter().zip(["session-one", "session-two"]) {
        assert_eq!(
            call.join().unwrap()["elicitationResult"]["content"]["session"],
            expected
        );
    }
    assert_eq!(seen.lock().unwrap().len(), 2);
    client.clear_elicitation_handler("session-one").unwrap();
    let result = client
        .request_with_options(
            "tools/call",
            json!({"name":"fixture","arguments":{"label":"session-one"}}),
            &CallOptions {
                session_id: Some("session-one".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result["elicitationError"]["code"], -32000);
    assert_eq!(seen.lock().unwrap().len(), 2);
    client.close();
}

#[test]
fn stdio_cancel_is_prompt_and_sends_original_request_id() {
    let client = Arc::new(
        McpClient::connect(
            "fixture",
            stdio_spec(None),
            ClientOptions {
                request_timeout: Duration::from_secs(5),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let token = CancellationToken::new();
    let cancelled = token.clone();
    let caller = client.clone();
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().canonicalize().unwrap().join("received");
    let request_marker = marker.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let call = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        caller.request_with_options(
            "wait",
            json!({"marker":request_marker}),
            &CallOptions {
                cancellation: Some(cancelled),
                ..Default::default()
            },
        )
    });
    ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        marker.exists(),
        "request must reach child before cancellation"
    );
    let start = Instant::now();
    token.cancel();
    assert!(matches!(call.join().unwrap(), Err(McpError::Cancelled)));
    assert!(start.elapsed() < Duration::from_secs(1));
    let result = client
        .request_with_options("ping", json!({}), &CallOptions::default())
        .unwrap();
    assert_eq!(result["cancelled"], json!([2]));
    client.close();
}
