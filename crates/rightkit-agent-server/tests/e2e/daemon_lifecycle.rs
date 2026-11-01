//! Real-process daemon lifecycle acceptance (DL-015).
//!
//! These tests deliberately exercise the `coderight serve` executable rather
//! than constructing daemon structs. Every daemon gets a throwaway
//! `RIGHTKIT_SUITE_ROOT`, database, config directory, and data directory; no test
//! can discover or mutate an installed-app profile, journal, database, or OS
//! keychain belonging to the operator.

use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Deserialize)]
struct StartupContract {
    #[serde(rename = "Port")]
    port: u16,
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DatabasePath")]
    database_path: String,
    #[serde(rename = "Pid")]
    pid: u32,
    #[serde(rename = "BootEpochMs")]
    boot_epoch_ms: u128,
    #[serde(rename = "ProtocolVersion")]
    protocol_version: u16,
}

struct TestDaemon {
    child: Child,
    startup: StartupContract,
}

impl TestDaemon {
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.startup.port)
    }

    fn is_running(&mut self) -> bool {
        self.child
            .try_wait()
            .expect("probe daemon process")
            .is_none()
    }

    fn kill_and_reap(&mut self) {
        if self.is_running() {
            self.child.kill().expect("kill daemon");
        }
        let _ = self.child.wait().expect("reap daemon");
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn executable() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_coderight")
        .map(PathBuf::from)
        .expect("Cargo must provide CARGO_BIN_EXE_coderight for this integration test")
}

fn isolated_command(home: &Path, db: Option<&Path>) -> Command {
    let mut command = rightkit_child_process::command_std(executable());
    // Direct context mode keeps Membrane availability outside CodeRight's
    // ServiceHost lifecycle acceptance; the signed-/health binding path is
    // qualified separately as a Membrane integration concern.
    command
        .arg("serve")
        .env("RIGHTKIT_CONTEXT", "off")
        .arg("--port")
        .arg("0");
    if let Some(db) = db {
        command.arg("--db").arg(db);
    }
    // RIGHTKIT_SUITE_ROOT is the daemon's authoritative override. The additional
    // variables cover libraries that resolve config/data through their normal
    // platform home locations.
    command
        .envs([crate::common::suite_env(home)])
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("RIGHTKIT_DISABLE_OS_KEYCHAIN", "1")
        .env_remove("RIGHTKIT_DAEMON_URL")
        .env_remove("RIGHTKIT_DAEMON_TOKEN")
        .env_remove("RIGHTKIT_SERVE_FORCE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

fn start_daemon(home: &Path, db: Option<&Path>) -> TestDaemon {
    let mut child = isolated_command(home, db)
        .spawn()
        .expect("spawn real coderight daemon");
    let stdout = child.stdout.take().expect("daemon startup stdout pipe");
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send(match result {
            Ok(0) => Err("daemon exited before its startup contract".to_string()),
            Ok(_) => serde_json::from_str::<StartupContract>(line.trim())
                .map_err(|error| format!("invalid daemon startup contract: {error}")),
            Err(error) => Err(format!("could not read daemon startup contract: {error}")),
        });
    });

    let startup = match receiver.recv_timeout(STARTUP_TIMEOUT) {
        Ok(Ok(startup)) => startup,
        Ok(Err(error)) => {
            let status = child.try_wait().ok().flatten();
            let _ = child.kill();
            let _ = child.wait();
            panic!("real daemon failed to publish startup contract ({error}); status={status:?}");
        }
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("timed out waiting for real daemon startup contract: {error}");
        }
    };
    assert_eq!(startup.protocol_version, 1);
    assert!(startup.port > 0);
    assert!(!startup.token.is_empty());
    assert!(startup.pid > 0);
    assert!(startup.boot_epoch_ms > 0);

    let daemon = TestDaemon { child, startup };
    wait_for_health(&daemon);
    daemon
}

fn wait_for_health(daemon: &TestDaemon) -> Value {
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    loop {
        if let Some(value) = get_json(&daemon.url(), "/health", &daemon.startup.token) {
            assert_eq!(
                value["ok"], true,
                "health must report a live daemon: {value}"
            );
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "real daemon did not become healthy at {}",
            daemon.url()
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn client() -> rightkit_http::Client {
    let timeout = Duration::from_millis(500);
    rightkit_http::Client::new(rightkit_http::ClientConfig {
        connect_timeout: Some(timeout),
        request_timeout: timeout,
        response_timeout: Some(timeout),
        retry: rightkit_http::RetryPolicy::none(),
        address_policy: crate::support::daemon_process::loopback_policy(),
        ..rightkit_http::ClientConfig::default()
    })
}

fn get_json(base_url: &str, path: &str, token: &str) -> Option<Value> {
    let response = client()
        .send_any(&rightkit_http::Request::get(format!("{base_url}{path}")).bearer(token))
        .ok()?;
    if !crate::support::daemon_process::is_success(response.status) {
        return None;
    }
    response.json().ok()
}

fn post_json(base_url: &str, path: &str, token: &str, body: Value) -> rightkit_http::Response {
    let request = rightkit_http::Request::post(format!("{base_url}{path}"))
        .bearer(token)
        .json(&body)
        .expect("daemon POST body");
    client().send_any(&request).expect("bounded daemon POST")
}

fn delete_path(base_url: &str, path: &str, token: &str) -> rightkit_http::Response {
    client()
        .send_any(&rightkit_http::Request::delete(format!("{base_url}{path}")).bearer(token))
        .expect("bounded daemon DELETE")
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("probe expected-exit process") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("process did not exit within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn port_file(home: &Path) -> PathBuf {
    crate::common::app_root(&home).join("daemon.port")
}

fn primary_db(home: &Path) -> PathBuf {
    crate::common::app_root(&home).join("coderight.db")
}

#[test]
fn singleton_enforcement_survives_a_missing_discovery_file() {
    let home = TempDir::new().unwrap();
    let mut first = start_daemon(home.path(), None);
    std::fs::remove_file(port_file(home.path())).expect("remove only the QA discovery file");

    // The discovery file is not the singleton authority. Removing it while
    // the first real process is alive must still leave the database lifetime
    // lock to refuse the second process.
    let mut second = isolated_command(home.path(), None)
        .spawn()
        .expect("spawn contending real daemon");
    let status = wait_for_exit(&mut second, Duration::from_secs(5));
    assert!(
        !status.success(),
        "a second daemon must not acquire the live DB"
    );
    assert!(first.is_running(), "the original daemon must remain owner");
}

#[test]
fn production_machine_info_proves_published_owner_identity() {
    let home = TempDir::new().unwrap();
    let daemon = start_daemon(home.path(), None);
    let machine = get_json(&daemon.url(), "/machine/info", &daemon.startup.token)
        .expect("production machine info must be reachable");

    assert_eq!(machine["service"], "coderight-daemon");
    assert_eq!(machine["pid"], daemon.startup.pid);
    assert_eq!(machine["port"], daemon.startup.port);
    assert_eq!(
        machine["databasePath"], daemon.startup.database_path,
        "machine info must describe same owned database as startup contract"
    );
}

#[test]
fn live_owner_takeover_signals_and_reaps_predecessor_before_replacement() {
    let home = TempDir::new().unwrap();
    let mut predecessor = start_daemon(home.path(), None);
    let predecessor_pid = predecessor.startup.pid;
    let predecessor_token = predecessor.startup.token.clone();

    // A second real process must use the held ownership lock's takeover path,
    // pinning predecessor identity before graceful/forceful signaling. It may
    // publish only after predecessor has released ownership.
    let replacement = start_daemon(home.path(), None);
    assert_ne!(replacement.startup.pid, predecessor_pid);
    assert_ne!(replacement.startup.token, predecessor_token);
    let status = wait_for_exit(&mut predecessor.child, Duration::from_secs(5));
    assert!(
        status.success(),
        "predecessor should exit through takeover: {status}"
    );
    assert!(!predecessor.is_running(), "takeover must reap predecessor");
    assert_eq!(
        get_json(&replacement.url(), "/health", &replacement.startup.token).unwrap()["ok"],
        true
    );
}

#[test]
fn stale_owner_is_replaced_after_kill_and_reap() {
    let home = TempDir::new().unwrap();
    let mut old = start_daemon(home.path(), None);
    let old_pid = old.startup.pid;
    old.kill_and_reap();
    assert!(
        !old.is_running(),
        "the killed daemon must be reaped by its owner"
    );

    // The stale daemon.port is intentionally left behind by a hard kill. A
    // fresh real daemon must classify the dead endpoint as stale and publish
    // its own process identity rather than inheriting the old record.
    let replacement = start_daemon(home.path(), None);
    assert_ne!(replacement.startup.pid, old_pid);
    assert_ne!(replacement.startup.token, old.startup.token);
    let health = wait_for_health(&replacement);
    assert_eq!(health["databasePath"], replacement.startup.database_path);
}

#[test]
fn foreign_pid_record_is_not_adopted() {
    let target_home = TempDir::new().unwrap();
    let foreign_home = TempDir::new().unwrap();
    let foreign_db = foreign_home.path().join("foreign.sqlite");
    let foreign = start_daemon(foreign_home.path(), Some(&foreign_db));

    std::fs::create_dir_all(crate::common::app_root(target_home.path())).unwrap();
    // The record names the target database but its live endpoint answers with
    // the foreign database. A real target daemon must validate the live
    // identity and start itself; it must not adopt a foreign PID/token pair.
    let forged = json!({
        "Port": foreign.startup.port,
        "Token": foreign.startup.token,
        "DatabasePath": primary_db(target_home.path()),
        "Pid": foreign.startup.pid,
        "BootEpochMs": foreign.startup.boot_epoch_ms,
        "ProtocolVersion": foreign.startup.protocol_version
    });
    std::fs::write(
        port_file(target_home.path()),
        serde_json::to_vec(&forged).unwrap(),
    )
    .unwrap();

    let target = start_daemon(target_home.path(), None);
    assert_ne!(target.startup.pid, foreign.startup.pid);
    assert_ne!(target.startup.token, foreign.startup.token);
}

#[test]
fn malformed_pidfile_is_recovered_by_a_real_start() {
    let home = TempDir::new().unwrap();
    std::fs::create_dir_all(crate::common::app_root(home.path())).unwrap();
    std::fs::write(port_file(home.path()), b"not-json\n").unwrap();

    let daemon = start_daemon(home.path(), None);
    let published: StartupContract = serde_json::from_slice(
        &std::fs::read(port_file(home.path())).expect("real daemon must republish daemon.port"),
    )
    .unwrap();
    assert_eq!(published.pid, daemon.startup.pid);
    assert_eq!(published.port, daemon.startup.port);
    assert_ne!(published.token, "attacker-token");
}

#[cfg(unix)]
#[test]
fn symlinked_pidfile_does_not_modify_the_attacker_target() {
    // Unix-only: creating the symlink is the attack under test; Windows
    // requires a separately provisioned privilege for arbitrary symlink
    // creation, so this exact pidfile attack is not silently reported there.
    use std::os::unix::fs::symlink;

    let home = TempDir::new().unwrap();
    let dir = crate::common::app_root(home.path());
    std::fs::create_dir_all(&dir).unwrap();
    let attacker_target = home.path().join("attacker-record");
    std::fs::write(&attacker_target, b"attacker-owned-content").unwrap();
    symlink(&attacker_target, port_file(home.path())).unwrap();

    let daemon = start_daemon(home.path(), None);
    assert_eq!(
        std::fs::read(&attacker_target).unwrap(),
        b"attacker-owned-content",
        "daemon discovery publication must not follow a pidfile symlink"
    );
    assert!(!std::fs::symlink_metadata(port_file(home.path()))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        get_json(&daemon.url(), "/health", &daemon.startup.token).unwrap()["ok"],
        true
    );
}

#[cfg(unix)]
#[test]
fn graceful_old_process_does_not_remove_a_replacement_pidfile() {
    // Unix-only: SIGTERM is the portable way to exercise the daemon's
    // graceful cleanup path. A successor record is kept when the old process
    // exits, which is the replacement-safe endpoint invariant.
    let old_home = TempDir::new().unwrap();
    let foreign_home = TempDir::new().unwrap();
    let foreign_db = foreign_home.path().join("replacement.sqlite");
    let foreign = start_daemon(foreign_home.path(), Some(&foreign_db));
    let mut old = start_daemon(old_home.path(), None);

    let replacement = json!({
        "Port": foreign.startup.port,
        "Token": foreign.startup.token,
        "DatabasePath": primary_db(old_home.path()),
        "Pid": foreign.startup.pid,
        "BootEpochMs": foreign.startup.boot_epoch_ms,
        "ProtocolVersion": foreign.startup.protocol_version
    });
    let replacement_bytes = serde_json::to_vec(&replacement).unwrap();
    std::fs::write(port_file(old_home.path()), &replacement_bytes).unwrap();

    // The old real daemon is still alive, but it no longer owns the record.
    // Its graceful cleanup must not blindly unlink the successor.
    unsafe {
        libc::kill(old.startup.pid as libc::pid_t, libc::SIGTERM);
    }
    let status = wait_for_exit(&mut old.child, Duration::from_secs(15));
    assert!(
        status.success(),
        "old daemon should gracefully stop: {status}"
    );
    assert_eq!(
        std::fs::read(port_file(old_home.path())).unwrap(),
        replacement_bytes,
        "old daemon cleanup must preserve a successor pidfile"
    );
}

#[test]
fn pidfile_directory_attack_fails_closed_without_replacing_the_directory() {
    let home = TempDir::new().unwrap();
    let dir = crate::common::app_root(home.path());
    std::fs::create_dir_all(port_file(home.path())).unwrap();

    let mut child = isolated_command(home.path(), None)
        .spawn()
        .expect("spawn real daemon against directory pidfile");
    let status = wait_for_exit(&mut child, Duration::from_secs(15));
    assert!(
        !status.success(),
        "a directory cannot become daemon discovery"
    );
    assert!(
        port_file(home.path()).is_dir(),
        "the attack directory must survive"
    );
    assert!(dir.is_dir());
}

#[test]
fn process_replacement_preserves_a_persisted_session_for_reconnect() {
    let home = TempDir::new().unwrap();
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut old = start_daemon(home.path(), None);
    let session_id = "qa-reconnect-session";
    let response = post_json(
        &old.url(),
        "/sessions",
        &old.startup.token,
        json!({
            "id": session_id,
            "workspaceRoot": workspace,
            "title": "reconnect continuity"
        }),
    );
    assert!(
        crate::support::daemon_process::is_success(response.status),
        "session creation failed: {}",
        response.status
    );
    old.kill_and_reap();

    let replacement = start_daemon(home.path(), None);
    let deadline = Instant::now() + Duration::from_secs(5);
    let sessions = loop {
        if let Some(value) = get_json(&replacement.url(), "/sessions", &replacement.startup.token) {
            if value
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == session_id))
            {
                break value;
            }
        }
        assert!(
            Instant::now() < deadline,
            "persisted session did not reconnect"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert!(sessions
        .as_array()
        .unwrap()
        .iter()
        .any(|row| { row["id"] == session_id && row["title"] == "reconnect continuity" }));
}

#[test]
fn idle_daemon_exits_only_after_a_real_client_detaches_and_lingers() {
    let home = TempDir::new().unwrap();
    let mut daemon = start_daemon(home.path(), None);
    thread::sleep(Duration::from_millis(500));
    assert!(
        daemon.is_running(),
        "a never-attached daemon must not self-exit"
    );

    let attached = post_json(
        &daemon.url(),
        "/clients",
        &daemon.startup.token,
        json!({"kind":"cli","pid":std::process::id(),"label":"dl-015"}),
    );
    assert!(
        crate::support::daemon_process::is_success(attached.status),
        "client attach failed: {}",
        attached.status
    );
    let client_id = attached.json::<Value>().unwrap()["clientId"]
        .as_str()
        .expect("attach response clientId")
        .to_string();
    let detached = delete_path(
        &daemon.url(),
        &format!("/clients/{client_id}"),
        &daemon.startup.token,
    );
    assert!(
        crate::support::daemon_process::is_success(detached.status),
        "client detach failed: {}",
        detached.status
    );

    let deadline = Instant::now() + Duration::from_secs(27);
    while daemon.is_running() {
        assert!(
            Instant::now() < deadline,
            "idle daemon did not exit after client linger"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(unix)]
#[test]
fn daemon_survives_parent_death_and_remains_reachable() {
    // Unix-only: this acceptance relies on `sh` background-process and
    // parent-death semantics. Windows has no equivalent POSIX shell contract;
    // its process-tree behavior belongs in a native job-object acceptance lane.
    let home = TempDir::new().unwrap();
    let db = home.path().join("parent-death.sqlite");
    let startup_path = home.path().join("startup.json");
    let command = format!(
        "{} serve --port 0 --db {} > {} 2>/dev/null &",
        shell_quote(&executable()),
        shell_quote(&db),
        shell_quote(&startup_path)
    );
    let mut parent = rightkit_child_process::command_std("sh");
    parent
        .arg("-c")
        .arg(command)
        .envs([crate::common::suite_env(home.path())])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("RIGHTKIT_DISABLE_OS_KEYCHAIN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = parent
        .spawn()
        .expect("spawn POSIX daemon parent")
        .wait()
        .unwrap();
    assert!(
        status.success(),
        "launcher parent must exit cleanly: {status}"
    );

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let startup = loop {
        if let Ok(contents) = std::fs::read_to_string(&startup_path) {
            if let Ok(startup) = serde_json::from_str::<StartupContract>(contents.trim()) {
                break startup;
            }
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not start after parent death"
        );
        thread::sleep(Duration::from_millis(50));
    };
    let base_url = format!("http://127.0.0.1:{}", startup.port);
    let health_deadline = Instant::now() + HEALTH_TIMEOUT;
    while get_json(&base_url, "/health", &startup.token).is_none() {
        assert!(
            Instant::now() < health_deadline,
            "orphaned daemon did not remain reachable"
        );
        thread::sleep(Duration::from_millis(50));
    }

    // The daemon is no longer a child of this test process, so a signal is the
    // cleanup primitive rather than Child::kill + wait. This is deliberately
    // after the assertion that parent death did not kill the daemon.
    unsafe {
        libc::kill(startup.pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}
