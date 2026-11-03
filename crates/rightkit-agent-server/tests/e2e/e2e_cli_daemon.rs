//! CLI-visible daemon lifecycle that `daemon_lifecycle.rs` does not cover:
//! client discovery through the home port file (no env URL), `coderight stop`,
//! `status` before/after, and `serve --db` ownership + persistence across a
//! hard restart. Every daemon is a real child process killed on drop.
//!
//! Retires the discovery/startup-info unit tests in `daemon/*`,
//! `daemon_state.rs`, `startup.rs`, `cli_presence_tests.rs` and
//! `main_runtime_server*` that parse port-file records and lifecycle verbs.

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

fn text(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn diag(out: &std::process::Output) -> String {
    format!(
        "code={:?}\nstdout={}\nstderr={}",
        out.status.code(),
        text(out),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Spawn `coderight serve ... <extra>` in `home` and wait for its startup line.
fn start_with_args(home: &Path, extra: &[&str]) -> daemon_process::TestDaemon {
    let mut cmd = daemon_process::isolated_command(home, &[]);
    cmd.args(extra);
    let mut child = cmd.spawn().expect("spawn coderight serve");
    let stdout = child.stdout.take().expect("startup pipe");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let n = BufReader::new(stdout).read_line(&mut line).unwrap_or(0);
        let _ = tx.send((n, line));
    });
    let (n, line) = match rx.recv_timeout(daemon_process::STARTUP_TIMEOUT) {
        Ok(v) => v,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("daemon never printed its startup contract: {e}");
        }
    };
    if n == 0 {
        let _ = child.wait();
        panic!("daemon exited before publishing a startup contract");
    }
    let startup: daemon_process::StartupContract =
        serde_json::from_str(line.trim()).expect("startup contract JSON");
    let daemon = daemon_process::TestDaemon { child, startup };
    daemon_process::wait_for_health(&daemon);
    daemon
}

#[test]
fn cli_discovers_the_daemon_through_the_home_port_file_without_any_environment_url() {
    let rig = Rig::start(vec![]);
    // The port file is the discovery record every client reads.
    let port_file = crate::common::app_root(rig.home.path()).join("daemon.port");
    let raw = std::fs::read_to_string(&port_file).expect("daemon publishes its discovery record");
    let record: Value = serde_json::from_str(raw.trim()).expect("discovery record is JSON");
    assert!(
        raw.contains(&rig.daemon.startup.port.to_string()),
        "record names the port: {raw}"
    );
    assert!(
        raw.contains(&rig.daemon.startup.pid.to_string()),
        "record names the owner pid: {raw}"
    );
    assert!(record.is_object());

    // No RIGHTKIT_DAEMON_URL/TOKEN: only RIGHTKIT_SUITE_ROOT points at the record.
    let out = isolated_cli(rig.home.path(), rig.ws())
        .arg("status")
        .output()
        .expect("run status");
    assert_eq!(out.status.code(), Some(0), "{}", diag(&out));
    let status = stdout_json(&out);
    assert_eq!(status["running"], true);
    assert_eq!(status["status"], "healthy");
    assert_eq!(status["daemonUrl"], rig.url());
    assert_eq!(status["health"]["service"], "coderight-daemon");
    assert_eq!(status["health"]["port"], rig.daemon.startup.port);

    // Session verbs find it the same way (and authenticate with the recorded token).
    rig.session("disc1", json!({"title": "Discovered"}));
    let ps = isolated_cli(rig.home.path(), rig.ws())
        .arg("ps")
        .output()
        .expect("run ps");
    assert_eq!(ps.status.code(), Some(0), "{}", diag(&ps));
    let rows = stdout_json(&ps);
    assert!(
        rows.as_array().unwrap().iter().any(|r| r["id"] == "disc1"),
        "{rows}"
    );

    // `info` mirrors the same discovery.
    let info = stdout_json(
        &isolated_cli(rig.home.path(), rig.ws())
            .args(["info", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(info["daemon"]["running"], true);
    assert_eq!(info["daemon"]["baseUrl"], rig.url());
}

#[test]
fn stop_shuts_the_daemon_down_and_status_then_reports_stopped() {
    let mut rig = Rig::start(vec![]);
    let stop = isolated_cli(rig.home.path(), rig.ws())
        .arg("stop")
        .output()
        .expect("run stop");
    assert_eq!(stop.status.code(), Some(0), "{}", diag(&stop));
    assert!(
        !text(&stop).trim().is_empty(),
        "stop reports what it did: {}",
        diag(&stop)
    );

    daemon_process::wait_for_exit(&mut rig.daemon.child, Duration::from_secs(20));
    assert!(
        !rig.daemon.is_running(),
        "the daemon process is gone after stop"
    );
    assert!(
        daemon_process::get_json(&rig.url(), "/health", &rig.daemon.startup.token).is_none(),
        "the HTTP port is closed"
    );

    // A client no longer sees a running daemon (and never autostarts one here).
    let status = stdout_json(
        &isolated_cli(rig.home.path(), rig.ws())
            .arg("status")
            .output()
            .unwrap(),
    );
    assert_eq!(status["running"], false, "{status}");
    assert_ne!(status["status"], "healthy");
    let again = isolated_cli(rig.home.path(), rig.ws())
        .arg("stop")
        .output()
        .expect("run stop again");
    assert!(
        again.status.code().is_some(),
        "stopping an absent daemon terminates promptly"
    );
}

#[test]
fn serve_with_an_explicit_db_owns_that_database_and_keeps_sessions_across_a_hard_restart() {
    let home = tempfile::TempDir::new().unwrap();
    let workspace = tempfile::TempDir::new().unwrap();
    let db_dir = home.path().join("custom-db");
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = db_dir.join("explicit.db");
    let db_arg = db.to_string_lossy().to_string();

    let mut first = start_with_args(home.path(), &["--db", &db_arg]);
    assert!(
        first
            .startup
            .database_path
            .replace('\\', "/")
            .ends_with("custom-db/explicit.db"),
        "startup contract names the requested database: {}",
        first.startup.database_path
    );
    let health = daemon_process::wait_for_health(&first);
    assert_eq!(health["databasePath"], first.startup.database_path);
    assert!(db.is_file(), "the requested database file exists");

    let (status, created) = daemon_process::post_json(
        &first.url(),
        "/sessions",
        &first.startup.token,
        &json!({"id": "persist-db-1", "workspaceRoot": workspace.path().to_string_lossy(), "model": MODEL, "title": "Survives restart"}),
    );
    assert_eq!(status, 200, "{created}");

    // Hard kill (no graceful shutdown), then a new daemon on the same database.
    let _ = first.child.kill();
    let _ = first.child.wait();
    let second = start_with_args(home.path(), &["--db", &db_arg]);
    assert_ne!(second.startup.pid, first.startup.pid);
    assert_ne!(
        second.startup.token, first.startup.token,
        "every boot mints a fresh bearer token"
    );
    let rows = daemon_process::get_json(&second.url(), "/sessions", &second.startup.token)
        .expect("list sessions");
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "persist-db-1")
        .unwrap_or_else(|| panic!("session survived the restart: {rows}"));
    assert_eq!(row["title"], "Survives restart");
    assert!(row["status"].is_string(), "{row}");
}

#[test]
fn old_bearer_tokens_stop_working_after_a_takeover_replacement() {
    let home = tempfile::TempDir::new().unwrap();
    let mut old = start_with_args(home.path(), &[]);
    let old_token = old.startup.token.clone();
    let new = start_with_args(home.path(), &[]);
    daemon_process::wait_for_exit(&mut old.child, Duration::from_secs(15));

    // The replacement rejects the predecessor's credential and accepts its own.
    let denied = crate::support::daemon_process::send(
        &rightkit_http::Request::get(format!("{}/sessions", new.url())).bearer(&old_token),
    )
    .expect("request reaches the replacement");
    assert_eq!(denied.status, 401);
    let allowed = crate::support::daemon_process::send(
        &rightkit_http::Request::get(format!("{}/sessions", new.url())).bearer(&new.startup.token),
    )
    .unwrap();
    assert_eq!(allowed.status, 200);
}
