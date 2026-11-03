//! Isolated daemon-backed MCP CLI journey.

use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

struct DaemonFixture {
    child: Child,
    stdout_drain: Option<std::thread::JoinHandle<()>>,
    port: u16,
    token: String,
    home: std::path::PathBuf,
    db: std::path::PathBuf,
}

impl Drop for DaemonFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(stdout_drain) = self.stdout_drain.take() {
            finish_stdout_drain(stdout_drain);
        }
        cleanup_fixture_files(&self.db, &self.home);
    }
}

fn finish_stdout_drain(stdout_drain: std::thread::JoinHandle<()>) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while !stdout_drain.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if stdout_drain.is_finished() {
        let _ = stdout_drain.join();
    }
}

fn cleanup_fixture_files(db: &std::path::Path, home: &std::path::Path) {
    let _ = std::fs::remove_file(db);
    let _ = std::fs::remove_file(db.with_extension("db-shm"));
    let _ = std::fs::remove_file(db.with_extension("db-wal"));
    let _ = std::fs::remove_dir_all(home);
}

fn unique_fixture_path(label: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "coderight-mcp-cli-{label}-{}-{nonce}",
        std::process::id()
    ))
}

fn spawn_daemon() -> DaemonFixture {
    let exe = env!("CARGO_BIN_EXE_coderight");
    let engine_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("resolve engine root");
    let home = unique_fixture_path("home");
    std::fs::create_dir_all(&home).expect("create isolated home");
    let db = unique_fixture_path("state").with_extension("db");
    let db_arg = db.to_string_lossy().to_string();

    let mut child = rightkit_child_process::command_std(exe)
        .env("RIGHTKIT_CONTEXT", "off")
        .args(["serve", "--port", "0", "--db", &db_arg])
        .current_dir(engine_root)
        .envs([crate::common::suite_env(&home)])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn coderight serve");

    let stdout = child.stdout.take().expect("daemon stdout");
    let (startup_tx, startup_rx) = std::sync::mpsc::sync_channel(1);
    let stdout_drain = std::thread::spawn(move || {
        const MAX_STARTUP_LINES: usize = 64;

        let mut reader = BufReader::new(stdout);
        let mut observed = Vec::new();
        for _ in 0..MAX_STARTUP_LINES {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    let _ = startup_tx.send(Err(format!(
                        "daemon stdout closed before startup JSON; observed={observed:?}"
                    )));
                    return;
                }
                Ok(_) => {
                    let parsed = serde_json::from_str::<serde_json::Value>(line.trim()).ok();
                    let startup = parsed.and_then(|value| {
                        let port = value
                            .get("Port")?
                            .as_u64()
                            .and_then(|port| u16::try_from(port).ok())
                            .filter(|port| *port != 0)?;
                        let token = value
                            .get("Token")?
                            .as_str()
                            .filter(|token| !token.is_empty())?
                            .to_string();
                        Some((port, token))
                    });
                    if let Some(startup) = startup {
                        let _ = startup_tx.send(Ok(startup));
                        let _ = std::io::copy(&mut reader, &mut std::io::sink());
                        return;
                    }
                    observed.push(line.trim().chars().take(240).collect::<String>());
                }
                Err(error) => {
                    let _ = startup_tx.send(Err(format!(
                        "failed reading daemon startup output: {error}; observed={observed:?}"
                    )));
                    return;
                }
            }
        }
        let _ = startup_tx.send(Err(format!(
            "daemon startup JSON not found within {MAX_STARTUP_LINES} lines; observed={observed:?}"
        )));
    });

    let (port, token) = match startup_rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(startup)) => startup,
        Ok(Err(error)) => {
            let _ = child.kill();
            let _ = child.wait();
            finish_stdout_drain(stdout_drain);
            cleanup_fixture_files(&db, &home);
            panic!("{error}");
        }
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            finish_stdout_drain(stdout_drain);
            cleanup_fixture_files(&db, &home);
            panic!("timed out waiting for daemon startup JSON: {error}");
        }
    };

    let mut fixture = DaemonFixture {
        child,
        stdout_drain: Some(stdout_drain),
        port,
        token,
        home,
        db,
    };
    let health_url = format!("http://127.0.0.1:{port}/health");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = fixture.child.try_wait().expect("poll daemon") {
            panic!("daemon exited before readiness: {status}");
        }
        match crate::support::daemon_process::send(&rightkit_http::Request::get(&health_url)) {
            Ok(response) if crate::support::daemon_process::is_success(response.status) => break,
            _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => panic!("daemon did not become ready: {health_url}"),
        }
    }
    fixture
}

fn run_cli(fixture: &DaemonFixture, args: &[&str]) -> std::process::Output {
    rightkit_child_process::command_std(env!("CARGO_BIN_EXE_coderight"))
        .args(args)
        .env(
            "RIGHTKIT_DAEMON_URL",
            format!("http://127.0.0.1:{}", fixture.port),
        )
        .env("RIGHTKIT_DAEMON_TOKEN", &fixture.token)
        .envs([crate::common::suite_env(&fixture.home)])
        .env("HOME", &fixture.home)
        .env("USERPROFILE", &fixture.home)
        .output()
        .unwrap_or_else(|error| panic!("run coderight {args:?}: {error}"))
}

fn json_output(fixture: &DaemonFixture, args: &[&str]) -> serde_json::Value {
    let output = run_cli(fixture, args);
    assert!(
        output.status.success(),
        "coderight {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "coderight {args:?} did not print JSON: {error}; stdout={:?}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn mcp_cli_status_catalog_install_list_persists_filesystem_server() {
    let fixture = spawn_daemon();

    let status = json_output(&fixture, &["status"]);
    assert_eq!(status["running"], true);
    assert_eq!(status["status"], "healthy");
    assert_eq!(status["health"]["service"], "coderight-daemon");

    let catalog = json_output(&fixture, &["mcp", "catalog", "--json"]);
    let filesystem = catalog["catalog"]
        .as_array()
        .and_then(|entries| entries.iter().find(|entry| entry["name"] == "filesystem"))
        .expect("filesystem catalog entry");
    assert_eq!(filesystem["config"]["type"], "stdio");
    assert_eq!(filesystem["config"]["command"], "npx");

    let install = json_output(&fixture, &["mcp", "install", "filesystem"]);
    assert_eq!(install["saved"], true);
    assert_eq!(install["scope"], "user");
    assert_eq!(install["name"], "filesystem");
    let config_path = install["path"].as_str().expect("install config path");
    assert!(std::path::Path::new(config_path).starts_with(&fixture.home));

    let list = json_output(&fixture, &["mcp", "list", "--json"]);
    let row = list["servers"]
        .as_array()
        .and_then(|servers| {
            servers
                .iter()
                .find(|server| server["name"] == "filesystem" && server["scope"] == "user")
        })
        .expect("persisted filesystem server row");
    assert_eq!(row["config"]["type"], "stdio");
    assert_eq!(row["config"]["command"], filesystem["config"]["command"]);
    assert_eq!(row["config"]["args"], filesystem["config"]["args"]);
    assert!(std::path::Path::new(config_path).is_file());

    let persisted: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(config_path).expect("read persisted MCP config"),
    )
    .expect("parse persisted MCP config");
    assert_eq!(persisted["mcpServers"]["filesystem"], row["config"]);
}
