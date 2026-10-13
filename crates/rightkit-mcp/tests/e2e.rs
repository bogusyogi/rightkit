//! End-to-end: real processes, real sockets, validated by content.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use rightkit_mcp::*;
use serde_json::json;

fn example_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let p = dir.join(if cfg!(windows) {
        "example_server.exe"
    } else {
        "example_server"
    });
    assert!(p.exists(), "example binary missing at {p:?}");
    p
}

fn stdio_spec(env: &[(&str, &str)]) -> ServerSpec {
    ServerSpec::Stdio {
        command: example_bin().to_string_lossy().into(),
        args: vec!["stdio".into()],
        env: env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
        cwd: None,
    }
}

/// Test auth provider returning a fixed value (stands in for the app's OAuth/keychain layer).
struct Fixed(&'static str);
impl AuthProvider for Fixed {
    fn bearer(&self, _s: &str) -> Result<Option<Secret>> {
        Ok(Some(Secret::new(self.0)))
    }
    fn on_unauthorized(&self, _s: &str, _w: Option<&str>) -> Result<bool> {
        Ok(false)
    }
}

fn opts_with(auth: Option<Arc<dyn AuthProvider>>) -> ClientOptions {
    ClientOptions {
        request_timeout: Duration::from_secs(20),
        auth,
        ..ClientOptions::default()
    }
}

fn assert_surface(c: &McpClient) {
    let names: Vec<String> = c
        .list_tools()
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(
        names,
        ["echo", "add", "write_marker", "die"],
        "secret_tool must be hidden by policy"
    );
    let r = c
        .call_tool("echo", json!({"message": "hello mcp"}))
        .unwrap();
    assert!(!r.is_error);
    assert_eq!(r.text_content(), "echo: hello mcp");
    let r = c.call_tool("add", json!({"a": 2, "b": 3.5})).unwrap();
    assert_eq!(r.structured.unwrap()["sum"], 5.5);
    assert_eq!(
        c.read_resource("demo://readme").unwrap().text.as_deref(),
        Some("hello resource")
    );
    assert_eq!(c.list_resources().unwrap()[0].uri, "demo://readme");
    assert_eq!(
        c.get_prompt("greet", json!({"who": "Ada"})).unwrap()[0].text,
        "Hello Ada"
    );
    assert_eq!(c.list_prompts().unwrap()[0].arguments[0].name, "who");
}

#[test]
fn stdio_client_server_roundtrip() {
    let c = McpClient::connect(
        "demo",
        &rightkit_mcp::ServerSource::User,
        &rightkit_mcp::TrustStore::new(),
        stdio_spec(&[]),
        opts_with(None),
    )
    .unwrap();
    assert_eq!(c.protocol_version(), "2025-03-26");
    assert_surface(&c);
    c.ping().unwrap();
}

struct HttpChild(Child, String);
impl Drop for HttpChild {
    fn drop(&mut self) {
        drop(self.0.stdin.take()); // EOF stops the example
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_http(secret: &str) -> HttpChild {
    let mut child = Command::new(example_bin())
        .arg("http")
        // The example refuses env tokens on managed build hosts (unit-tested there); this
        // test supplies its own throwaway token, so run the child as an unmanaged host.
        .env_remove("RIGHTKIT_BUILD_BROKER_SOCKET")
        .env("EXAMPLE_TOKEN", secret)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // Skip any output before the announcement; report it if none arrives.
    let mut reader = BufReader::new(child.stdout.as_mut().unwrap());
    let mut seen = Vec::new();
    let url = loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).unwrap();
        if let Some(url) = line.trim().strip_prefix("LISTENING ") {
            break url.to_string();
        }
        assert!(
            n > 0 && seen.len() < 20,
            "no announce line; example printed: {seen:?}"
        );
        seen.push(line);
    };
    HttpChild(child, url)
}

fn port_of(url: &str) -> String {
    url.split(':')
        .nth(2)
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .to_string()
}

fn raw(port: &str, request: String) -> String {
    let mut s = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    s.write_all(request.as_bytes()).unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    resp
}

#[test]
fn http_loopback_roundtrip_and_gates() {
    let h = start_http("secret-e2e-123");
    let c = McpClient::connect(
        "demo",
        &rightkit_mcp::ServerSource::User,
        &rightkit_mcp::TrustStore::new(),
        ServerSpec::Http { url: h.1.clone() },
        opts_with(Some(Arc::new(Fixed("secret-e2e-123")))),
    )
    .unwrap();
    assert_surface(&c);

    // Wrong or missing credential is refused.
    let bad = McpClient::connect(
        "demo",
        &rightkit_mcp::ServerSource::User,
        &rightkit_mcp::TrustStore::new(),
        ServerSpec::Http { url: h.1.clone() },
        opts_with(Some(Arc::new(Fixed("nope")))),
    );
    assert!(
        matches!(bad, Err(McpError::Unauthorized)),
        "{:?}",
        bad.err()
    );
    let none = McpClient::connect(
        "demo",
        &rightkit_mcp::ServerSource::User,
        &rightkit_mcp::TrustStore::new(),
        ServerSpec::Http { url: h.1.clone() },
        opts_with(None),
    );
    assert!(matches!(none, Err(McpError::Unauthorized)));

    // A browser-origin / rebinding request carrying the right credential is still refused.
    let port = port_of(&h.1);
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    for (extra, want) in [
        (
            "Host: 127.0.0.1\r\nOrigin: http://localhost.evil.com\r\n",
            "403",
        ),
        ("Host: evil.com\r\n", "403"),
        (
            "Host: 127.0.0.1\r\nOrigin: http://localhost:3000\r\n",
            "200",
        ),
    ] {
        let req = format!(
            "POST /mcp HTTP/1.1\r\n{extra}Authorization: Bearer secret-e2e-123\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let resp = raw(&port, req);
        assert!(
            resp.starts_with(&format!("HTTP/1.1 {want}")),
            "{extra:?} -> {resp}"
        );
    }
}

#[test]
fn http_session_loss_reinitialises() {
    let h = start_http("s2");
    let c = McpClient::connect(
        "demo",
        &rightkit_mcp::ServerSource::User,
        &rightkit_mcp::TrustStore::new(),
        ServerSpec::Http { url: h.1.clone() },
        opts_with(Some(Arc::new(Fixed("s2")))),
    )
    .unwrap();
    assert_eq!(
        c.call_tool("echo", json!({"message": "a"}))
            .unwrap()
            .text_content(),
        "echo: a"
    );
    // A stale session id must be answered 404 by the server (spec), proving the reconnect trigger is real.
    let stale = raw(
        &port_of(&h.1),
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer s2\r\nMcp-Session-Id: stale\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".into(),
    );
    assert!(stale.starts_with("HTTP/1.1 404"), "{stale}");
    // Dropping the client's connection state forces a fresh initialize and a new session; calls keep working.
    c.close();
    assert_eq!(
        c.call_tool("echo", json!({"message": "b"}))
            .unwrap()
            .text_content(),
        "echo: b"
    );
}

#[test]
fn trust_gate_blocks_spawn_until_approved() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("spawned.marker");
    // If this wrapper ever runs, it leaves proof on disk before exec'ing the real server.
    #[cfg(not(windows))]
    let (shell, args) = (
        "/bin/sh",
        vec![
            "-c".to_string(),
            format!(
                "touch '{}'; exec '{}' stdio",
                marker.display(),
                example_bin().display()
            ),
        ],
    );
    // cmd does not understand Rust's \" argument escaping, so pass each word separately (unquoted).
    #[cfg(windows)]
    let (shell, args) = (
        "cmd",
        ["/C", "type", "nul", ">"]
            .iter()
            .map(|s| s.to_string())
            .chain([
                marker.display().to_string(),
                "&&".into(),
                example_bin().display().to_string(),
                "stdio".into(),
            ])
            .collect::<Vec<_>>(),
    );
    let spec = ServerSpec::Stdio {
        command: shell.into(),
        args,
        env: BTreeMap::new(),
        cwd: None,
    };
    let entry = ServerEntry {
        name: "ws-server".into(),
        spec: spec.clone(),
        source: ServerSource::Workspace("/proj".into()),
    };
    let pool = ConnectionPool::new(
        PoolConfig {
            max_connections: 4,
            options: opts_with(None),
        },
        TrustStore::new(),
    );

    let err = pool.get(&entry).err().expect("must refuse");
    let McpError::NotApproved(p) = err else {
        panic!("expected NotApproved")
    };
    assert_eq!(p.command, shell);
    assert_eq!(p.fingerprint, fingerprint(&spec));
    std::thread::sleep(Duration::from_millis(400));
    assert!(!marker.exists(), "process must not start before approval");
    assert_eq!(
        pool.catalog(std::slice::from_ref(&entry))
            .unwrap()
            .skipped
            .len(),
        1
    );
    assert!(!marker.exists());

    pool.trust().approve("/proj", "ws-server", &spec);
    let c = pool.get(&entry).unwrap();
    assert_eq!(
        c.call_tool("echo", json!({"message": "gated"}))
            .unwrap()
            .text_content(),
        "echo: gated"
    );
    assert!(marker.exists());
    assert!(
        Arc::ptr_eq(&c, &pool.get(&entry).unwrap()),
        "same server shares one connection"
    );

    // Persisted approval survives reload; a changed command line is Pending again.
    let store_path = dir.path().join("trust.json");
    pool.trust().save(&store_path).unwrap();
    let reloaded = TrustStore::try_load(&store_path).unwrap();
    assert_eq!(
        reloaded.decide("/proj", "ws-server", &spec),
        TrustDecision::Approved
    );
    let ServerSpec::Stdio {
        command,
        args,
        env,
        cwd,
    } = spec.clone()
    else {
        unreachable!()
    };
    let mut args2 = args;
    args2[1].push_str(" # edited");
    let changed = ServerSpec::Stdio {
        command,
        args: args2,
        env,
        cwd,
    };
    assert_eq!(
        reloaded.decide("/proj", "ws-server", &changed),
        TrustDecision::Pending
    );

    // Explicit denial is final for that fingerprint.
    pool.trust().deny("/proj", "ws-server", &spec);
    pool.close_all();
    assert!(matches!(pool.get(&entry), Err(McpError::Denied(_))));
}

#[test]
fn admission_denies_before_any_side_effect() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("write.marker");
    let args = json!({"path": marker.to_string_lossy(), "content": "payload"});

    let c = McpClient::connect(
        "demo",
        &rightkit_mcp::ServerSource::User,
        &rightkit_mcp::TrustStore::new(),
        stdio_spec(&[]),
        opts_with(None),
    )
    .unwrap();
    let r = c.call_tool("write_marker", args.clone()).unwrap();
    assert!(r.is_error);
    assert!(
        r.text_content().starts_with("denied:"),
        "{}",
        r.text_content()
    );
    assert_eq!(r.structured.as_ref().unwrap()["denied"], true);
    assert!(!marker.exists(), "provider must not run on denial");
    let hidden = c.call_tool("secret_tool", json!({})).unwrap();
    assert!(
        hidden.is_error && hidden.text_content().contains("not exposed"),
        "{}",
        hidden.text_content()
    );

    // Same server with mutation enabled: the hook (not the tool) was what stopped it.
    let c2 = McpClient::connect(
        "demo",
        &rightkit_mcp::ServerSource::User,
        &rightkit_mcp::TrustStore::new(),
        stdio_spec(&[("EXAMPLE_ALLOW_MUTATE", "1")]),
        opts_with(None),
    )
    .unwrap();
    let r = c2.call_tool("write_marker", args).unwrap();
    assert!(!r.is_error);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "payload");
    // Unknown tools are protocol errors, not silent successes.
    assert!(matches!(
        c2.call_tool("nope", json!({})),
        Err(McpError::Rpc { code: -32602, .. })
    ));
}

#[test]
fn stdio_child_death_reconnects_and_pool_is_bounded() {
    let c = McpClient::connect(
        "demo",
        &rightkit_mcp::ServerSource::User,
        &rightkit_mcp::TrustStore::new(),
        stdio_spec(&[]),
        opts_with(None),
    )
    .unwrap();
    // The tool kills the server mid-call; the client reconnects and retries once, which kills it again.
    assert!(c.call_tool("die", json!({})).is_err());
    // A later call transparently re-spawns.
    assert_eq!(
        c.call_tool("echo", json!({"message": "back"}))
            .unwrap()
            .text_content(),
        "echo: back"
    );

    let a = ServerEntry {
        name: "a".into(),
        spec: stdio_spec(&[]),
        source: ServerSource::User,
    };
    let b = ServerEntry {
        name: "b".into(),
        spec: stdio_spec(&[]),
        source: ServerSource::User,
    };
    let pool = ConnectionPool::new(
        PoolConfig {
            max_connections: 1,
            options: opts_with(None),
        },
        TrustStore::new(),
    );
    let held = pool.get(&a).unwrap();
    assert!(matches!(pool.get(&b), Err(McpError::PoolFull(1))));
    drop(held);
    pool.get(&b).unwrap(); // idle `a` evicted
    assert_eq!(pool.len(), 1);

    // Catalog namespacing against a live server.
    let pool2 = ConnectionPool::new(PoolConfig::default(), TrustStore::new());
    let cat = pool2
        .catalog(&[ServerEntry {
            name: "My-Server".into(),
            ..a.clone()
        }])
        .unwrap();
    assert!(cat
        .tools
        .iter()
        .any(|t| t.qualified == "mcp__my_server__echo"));
    assert_eq!(
        cat.call(&pool2, "mcp__my_server__echo", json!({"message": "q"}))
            .unwrap()
            .text_content(),
        "echo: q"
    );
}

#[test]
fn public_connect_refuses_untrusted_workspace_server_before_launch() {
    // A command that cannot exist: if the gate let the call through we would see a transport/launch error,
    // never NotApproved/Denied.
    let spec = ServerSpec::Stdio {
        command: "/nonexistent/rightkit-untrusted-server".into(),
        args: vec![],
        env: BTreeMap::new(),
        cwd: None,
    };
    let source = ServerSource::Workspace("ws-1".into());
    let mut trust = TrustStore::new();

    let pending = McpClient::connect(
        "evil",
        &source,
        &trust,
        spec.clone(),
        ClientOptions::default(),
    );
    assert!(matches!(pending, Err(McpError::NotApproved(_))));

    let pending_ext = McpClient::connect_with_extensions(
        "evil",
        &source,
        &trust,
        spec.clone(),
        ClientOptions::default(),
        ClientExtensions::default(),
    );
    assert!(matches!(pending_ext, Err(McpError::NotApproved(_))));

    trust.deny("ws-1", "evil", &spec);
    let denied = McpClient::connect(
        "evil",
        &source,
        &trust,
        spec.clone(),
        ClientOptions::default(),
    );
    assert!(matches!(denied, Err(McpError::Denied(_))));

    // Once approved the gate passes, so the failure is the launch itself, not the trust gate.
    trust.approve("ws-1", "evil", &spec);
    let approved = McpClient::connect("evil", &source, &trust, spec, ClientOptions::default());
    assert!(!matches!(
        approved,
        Err(McpError::NotApproved(_)) | Err(McpError::Denied(_))
    ));
}
