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
            Some("initialize") => {
                if let Some(path) = std::env::var_os("RK_MCP_INIT_MARKER") {
                    std::fs::write(path, b"ready").unwrap();
                }
                if std::env::var_os("RK_MCP_STALL_INIT").is_some() {
                    continue;
                }
                if let Ok(delay) = std::env::var("RK_MCP_INIT_DELAY") {
                    std::thread::sleep(Duration::from_millis(delay.parse().unwrap()));
                }
                let response = json!({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":message["params"]["protocolVersion"],"capabilities":{}}});
                let prefix = if std::env::var_os("RK_MCP_LARGE_PREFIX").is_some() {
                    "x".repeat(64 * 1024 + 1)
                } else {
                    "fixture banner without newline".into()
                };
                // Reproduce libtest's banner glued directly to first JSON-RPC frame.
                println!("{prefix}{response}");
                std::io::stdout().flush().unwrap();
            }
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
            Some("ping") => match message.pointer("/params/mode").and_then(Value::as_str) {
                Some("missing-version") => send(json!({"id":message["id"],"result":null})),
                Some("wrong-version") => {
                    send(json!({"jsonrpc":"1.0","id":message["id"],"result":null}))
                }
                Some("missing-result") => send(json!({"jsonrpc":"2.0","id":message["id"]})),
                Some("null-result") => {
                    send(json!({"jsonrpc":"2.0","id":message["id"],"result":null}))
                }
                Some("notify") => {
                    send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}));
                    send(
                        json!({"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"hi"}}),
                    );
                    send(json!({"jsonrpc":"2.0","id":message["id"],"result":"after notifications"}));
                }
                Some("late-prefix") => {
                    let bogus =
                        json!({"jsonrpc":"2.0","id":message["id"],"result":"must be ignored"});
                    println!("later banner{bogus}");
                    send(json!({"jsonrpc":"2.0","id":message["id"],"result":"strict response"}));
                }
                _ => send(
                    json!({"jsonrpc":"2.0","id":message["id"],"result":{"cancelled":cancelled}}),
                ),
            },
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

fn with_fixture_env(mut spec: ServerSpec, key: &str, value: impl Into<String>) -> ServerSpec {
    let ServerSpec::Stdio { env, .. } = &mut spec else {
        unreachable!()
    };
    env.insert(key.into(), value.into());
    spec
}

fn tracked_extensions(stopped: Arc<Mutex<Vec<&'static str>>>) -> ClientExtensions {
    ClientExtensions {
        launcher: Some(Arc::new(
            move |request: &LaunchRequest| -> Result<LaunchedProcess> {
                let process = DefaultProcessLauncher.launch(request)?;
                Ok(LaunchedProcess {
                    child: Box::new(WrappedChild {
                        child: process.child,
                        stopped: stopped.clone(),
                    }),
                    stdin: process.stdin,
                    stdout: process.stdout,
                })
            },
        )),
        ..Default::default()
    }
}

fn wait_for_marker(marker: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(marker.exists(), "initialize must reach child");
}

#[test]
fn stdio_startup_recovers_glued_banner_but_does_not_recover_later_prefixes() {
    let client = McpClient::connect(
        "fixture",
        &rightkit_mcp::ServerSource::User,
        &rightkit_mcp::TrustStore::new(),
        stdio_spec(None),
        ClientOptions::default(),
    )
    .unwrap();
    assert_eq!(
        client
            .request_with_options(
                "ping",
                json!({"mode":"late-prefix"}),
                &CallOptions::default()
            )
            .unwrap(),
        "strict response",
    );
    client.close();
}

#[test]
fn stdio_startup_prefix_over_budget_is_typed_and_reaps_child() {
    let stopped = Arc::new(Mutex::new(Vec::new()));
    let result = McpClient::connect_with_extensions(
        "fixture",
        &rightkit_mcp::ServerSource::User,
        &rightkit_mcp::TrustStore::new(),
        with_fixture_env(stdio_spec(None), "RK_MCP_LARGE_PREFIX", "1"),
        ClientOptions::default(),
        tracked_extensions(stopped.clone()),
    );
    assert!(matches!(
        result,
        Err(McpError::StdioPrefixTooLarge { limit: 65536 })
    ));
    assert_eq!(*stopped.lock().unwrap(), vec!["kill", "wait"]);
}

#[test]
fn cold_stdio_handshake_cancel_reaps_child_without_request_timeout_wait() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().canonicalize().unwrap().join("initialize");
    let spec = with_fixture_env(
        with_fixture_env(stdio_spec(None), "RK_MCP_STALL_INIT", "1"),
        "RK_MCP_INIT_MARKER",
        marker.to_string_lossy().into_owned(),
    );
    let stopped = Arc::new(Mutex::new(Vec::new()));
    let extensions = tracked_extensions(stopped.clone());
    let token = CancellationToken::new();
    let cancel = token.clone();
    let call = std::thread::spawn(move || {
        McpClient::connect_with_extensions_cancellable(
            "fixture",
            &rightkit_mcp::ServerSource::User,
            &rightkit_mcp::TrustStore::new(),
            spec,
            ClientOptions::default(),
            extensions,
            &cancel,
        )
    });
    wait_for_marker(&marker);
    let started = Instant::now();
    token.cancel();
    assert!(matches!(call.join().unwrap(), Err(McpError::Cancelled)));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(*stopped.lock().unwrap(), vec!["kill", "wait"]);
}

#[test]
fn precancelled_connect_never_launches_child() {
    let token = CancellationToken::new();
    token.cancel();
    let extensions = ClientExtensions {
        launcher: Some(Arc::new(|_: &LaunchRequest| -> Result<LaunchedProcess> {
            panic!("pre-cancelled connect must not launch");
        })),
        ..Default::default()
    };
    assert!(matches!(
        McpClient::connect_with_extensions_cancellable(
            "fixture",
            &rightkit_mcp::ServerSource::User,
            &rightkit_mcp::TrustStore::new(),
            stdio_spec(None),
            ClientOptions::default(),
            extensions,
            &token,
        ),
        Err(McpError::Cancelled)
    ));
}

#[test]
fn abandoned_slow_launch_returns_promptly_and_reaps_late_child() {
    for cancel_connect in [true, false] {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let stopped = Arc::new(Mutex::new(Vec::new()));
        let recorded = stopped.clone();
        let extensions = ClientExtensions {
            launcher: Some(Arc::new(
                move |request: &LaunchRequest| -> Result<LaunchedProcess> {
                    started_tx.send(()).unwrap();
                    release_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(3))
                        .unwrap();
                    let process = DefaultProcessLauncher.launch(request)?;
                    Ok(LaunchedProcess {
                        child: Box::new(WrappedChild {
                            child: process.child,
                            stopped: recorded.clone(),
                        }),
                        stdin: process.stdin,
                        stdout: process.stdout,
                    })
                },
            )),
            ..Default::default()
        };
        let token = CancellationToken::new();
        let cancelled = token.clone();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let call = std::thread::spawn(move || {
            let result = McpClient::connect_with_extensions_cancellable(
                "fixture",
                &rightkit_mcp::ServerSource::User,
                &rightkit_mcp::TrustStore::new(),
                stdio_spec(None),
                ClientOptions::default().connect_timeout(Duration::from_millis(500)),
                extensions,
                &cancelled,
            );
            finished_tx.send(result).ok();
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        if cancel_connect {
            token.cancel();
        }
        let result = finished_rx.recv_timeout(Duration::from_secs(1));
        // Always release launcher before asserting, so a failed regression cannot strand it.
        release_tx.send(()).unwrap();
        let result = result.expect("connect must stop while launcher is blocked");
        if cancel_connect {
            assert!(matches!(result, Err(McpError::Cancelled)));
        } else {
            assert!(matches!(result, Err(McpError::Timeout)));
        }
        call.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while stopped.lock().unwrap().len() != 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*stopped.lock().unwrap(), vec!["kill", "wait"]);
    }
}

#[test]
fn connect_timeout_covers_launch_and_initialize_together() {
    let stopped = Arc::new(Mutex::new(Vec::new()));
    let recorded = stopped.clone();
    let extensions = ClientExtensions {
        launcher: Some(Arc::new(
            move |request: &LaunchRequest| -> Result<LaunchedProcess> {
                std::thread::sleep(Duration::from_millis(300));
                let process = DefaultProcessLauncher.launch(request)?;
                Ok(LaunchedProcess {
                    child: Box::new(WrappedChild {
                        child: process.child,
                        stopped: recorded.clone(),
                    }),
                    stdin: process.stdin,
                    stdout: process.stdout,
                })
            },
        )),
        ..Default::default()
    };
    assert!(matches!(
        McpClient::connect_with_extensions(
            "fixture",
            &rightkit_mcp::ServerSource::User,
            &rightkit_mcp::TrustStore::new(),
            with_fixture_env(stdio_spec(None), "RK_MCP_INIT_DELAY", "300"),
            ClientOptions::default().connect_timeout(Duration::from_millis(500)),
            extensions,
        ),
        Err(McpError::Timeout)
    ));
    assert_eq!(*stopped.lock().unwrap(), vec!["kill", "wait"]);
}

#[test]
fn stdio_connect_timeout_is_independent_of_request_timeout() {
    let options = ClientOptions {
        request_timeout: Duration::from_millis(100),
        ..Default::default()
    }
    .connect_timeout(Duration::from_secs(5));
    let client = McpClient::connect_with_extensions(
        "fixture",
        &rightkit_mcp::ServerSource::User,
        &rightkit_mcp::TrustStore::new(),
        with_fixture_env(stdio_spec(None), "RK_MCP_INIT_DELAY", "250"),
        options,
        ClientExtensions::default(),
    )
    .unwrap();
    assert!(matches!(
        client.request_with_options("wait", json!({}), &CallOptions::default(),),
        Err(McpError::Timeout)
    ));
    client.ping().unwrap();
    client.close();

    let stopped = Arc::new(Mutex::new(Vec::new()));
    assert!(matches!(
        McpClient::connect_with_extensions(
            "fixture",
            &rightkit_mcp::ServerSource::User,
            &rightkit_mcp::TrustStore::new(),
            with_fixture_env(stdio_spec(None), "RK_MCP_STALL_INIT", "1"),
            ClientOptions::default().connect_timeout(Duration::from_millis(200)),
            tracked_extensions(stopped.clone()),
        ),
        Err(McpError::Timeout)
    ));
    assert_eq!(*stopped.lock().unwrap(), vec!["kill", "wait"]);
    assert!(ClientOptions::default().connect_timeout.is_none());
}

#[test]
fn stdio_response_requires_jsonrpc_and_result_but_allows_null() {
    let client = McpClient::connect(
        "fixture",
        &rightkit_mcp::ServerSource::User,
        &rightkit_mcp::TrustStore::new(),
        stdio_spec(None),
        ClientOptions::default(),
    )
    .unwrap();
    for mode in ["missing-version", "wrong-version", "missing-result"] {
        assert!(
            matches!(
                client.request_with_options("ping", json!({"mode":mode}), &CallOptions::default(),),
                Err(McpError::Protocol(_))
            ),
            "mode: {mode}"
        );
    }
    assert_eq!(
        client
            .request_with_options(
                "ping",
                json!({"mode":"null-result"}),
                &CallOptions::default(),
            )
            .unwrap(),
        Value::Null
    );
    client.close();
}

#[test]
fn insecure_origin_opt_in_allows_other_origins_but_still_validates_urls() {
    let policy = EndpointOriginPolicy::allow_any_origin_insecure();
    for endpoint in [
        "https://evil.test/messages",
        "http://example.com:444/messages",
        "//other.test/post",
    ] {
        assert!(policy
            .resolve_endpoint("https://example.com/sse", endpoint)
            .is_ok());
        assert!(matches!(
            EndpointOriginPolicy::default().resolve_endpoint("https://example.com/sse", endpoint,),
            Err(McpError::CrossOriginEndpoint { .. })
        ));
    }
    for endpoint in [
        "ftp://evil.test/post",
        "https://user:secret@evil.test/post",
        "https://evil.test/post#fragment",
    ] {
        assert!(matches!(
            policy.resolve_endpoint("https://example.com/sse", endpoint),
            Err(McpError::Protocol(_))
        ));
    }
}

#[test]
fn stdio_elicitation_exposes_wire_id_and_originating_call_cancellation() {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (observed_tx, observed_rx) = std::sync::mpsc::channel();
    let client = Arc::new(
        McpClient::connect_with_extensions(
            "fixture",
            &rightkit_mcp::ServerSource::User,
            &rightkit_mcp::TrustStore::new(),
            stdio_spec(None),
            ClientOptions::default(),
            ClientExtensions {
                elicitation: Some(Arc::new(move |context, _| {
                    assert_eq!(context.request_id(), &json!("elicitation-2"));
                    let signal = context.cancellation().expect("originating call signal");
                    assert!(!signal.is_cancelled());
                    ready_tx.send(()).unwrap();
                    let deadline = Instant::now() + Duration::from_secs(3);
                    while !signal.is_cancelled() && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    observed_tx.send(signal.is_cancelled()).unwrap();
                    Ok(json!({"action":"cancel"}))
                })),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let token = CancellationToken::new();
    let cancelled = token.clone();
    let caller = client.clone();
    let call = std::thread::spawn(move || {
        caller.call_tool_with_options(
            "fixture",
            json!({"label":"cancel"}),
            &CallOptions {
                cancellation: Some(cancelled),
                ..Default::default()
            },
        )
    });
    ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    token.cancel();
    assert!(matches!(call.join().unwrap(), Err(McpError::Cancelled)));
    assert!(observed_rx.recv_timeout(Duration::from_secs(3)).unwrap());
    client.ping().unwrap();
    client.close();
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
        &rightkit_mcp::ServerSource::User,
        &rightkit_mcp::TrustStore::new(),
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
            &rightkit_mcp::ServerSource::User,
            &rightkit_mcp::TrustStore::new(),
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
            &rightkit_mcp::ServerSource::User,
            &rightkit_mcp::TrustStore::new(),
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

#[test]
fn stdio_notifications_mid_request_reach_subscriber_and_response_resolves() {
    let seen = Arc::new(Mutex::new(Vec::<ServerNotification>::new()));
    let sink = seen.clone();
    let options = ClientOptions::default()
        .on_notification(move |n| sink.lock().unwrap().push(n.clone()));
    let client = McpClient::connect_with_extensions(
        "fixture",
        &rightkit_mcp::ServerSource::User,
        &rightkit_mcp::TrustStore::new(),
        stdio_spec(None),
        options,
        ClientExtensions::default(),
    )
    .unwrap();
    let result = client
        .request_with_options("ping", json!({"mode":"notify"}), &CallOptions::default())
        .unwrap();
    assert_eq!(result, json!("after notifications"));
    // Notifications precede the response on the wire, so the reader has delivered both.
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].method, "notifications/tools/list_changed");
    assert_eq!(seen[0].server, "fixture");
    assert_eq!(seen[0].params, Value::Null);
    assert_eq!(seen[1].method, "notifications/message");
    assert_eq!(seen[1].params["data"], "hi");
    client.close();
}

#[test]
fn stdio_notifications_without_subscriber_leave_behaviour_unchanged() {
    let client = McpClient::connect_with_extensions(
        "fixture",
        &rightkit_mcp::ServerSource::User,
        &rightkit_mcp::TrustStore::new(),
        stdio_spec(None),
        ClientOptions::default(),
        ClientExtensions::default(),
    )
    .unwrap();
    let result = client
        .request_with_options("ping", json!({"mode":"notify"}), &CallOptions::default())
        .unwrap();
    assert_eq!(result, json!("after notifications"));
    client.ping().unwrap();
    client.close();
}

#[test]
fn stdio_notification_handler_panic_does_not_break_the_connection() {
    let options = ClientOptions::default().on_notification(|_| panic!("handler bug"));
    let client = McpClient::connect_with_extensions(
        "fixture",
        &rightkit_mcp::ServerSource::User,
        &rightkit_mcp::TrustStore::new(),
        stdio_spec(None),
        options,
        ClientExtensions::default(),
    )
    .unwrap();
    let result = client
        .request_with_options("ping", json!({"mode":"notify"}), &CallOptions::default())
        .unwrap();
    assert_eq!(result, json!("after notifications"));
    client.close();
}
