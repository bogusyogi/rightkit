//! Real-daemon acceptance for receipt-bound performance publication (S:P10).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Deserialize)]
struct StartupContract {
    #[serde(rename = "Port")]
    port: u16,
    #[serde(rename = "Token")]
    token: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Fm189SoakRequest {
    run_id: String,
    scenarios: Vec<String>,
    cancelled_workers: Option<u64>,
    completed_workers: Option<u64>,
    ownership_violations: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    drive_real_workers: Option<RealWorkerDriveRequest>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RealWorkerDriveRequest {
    worker_count: usize,
    duration_secs: u64,
    workspace_root: String,
}

struct TestDaemon {
    child: Child,
    startup: StartupContract,
}

impl TestDaemon {
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.startup.port)
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
        .expect("Cargo must provide CARGO_BIN_EXE_coderight")
}

fn start_daemon(home: &Path) -> TestDaemon {
    let mut child = rightkit_child_process::command_std(executable())
        .arg("serve")
        .env("RIGHTKIT_CONTEXT", "off")
        .arg("--port")
        .arg("0")
        .envs([crate::common::suite_env(home)])
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("RIGHTKIT_DISABLE_OS_KEYCHAIN", "1")
        .env_remove("RIGHTKIT_DAEMON_URL")
        .env_remove("RIGHTKIT_DAEMON_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn isolated daemon");
    let stdout = child.stdout.take().expect("startup stdout");
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout)
            .read_line(&mut line)
            .map_err(|error| error.to_string())
            .and_then(|count| {
                if count == 0 {
                    Err("daemon exited before startup contract".to_string())
                } else {
                    serde_json::from_str(line.trim()).map_err(|error| error.to_string())
                }
            });
        let _ = sender.send(result);
    });
    let startup = receiver
        .recv_timeout(STARTUP_TIMEOUT)
        .expect("startup contract timeout")
        .expect("valid startup contract");
    let daemon = TestDaemon { child, startup };

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if crate::support::daemon_process::send(
            &rightkit_http::Request::get(format!("{}/health", daemon.url()))
                .bearer(&daemon.startup.token),
        )
        .map(|response| crate::support::daemon_process::is_success(response.status))
        .unwrap_or(false)
        {
            return daemon;
        }
        assert!(Instant::now() < deadline, "daemon health timeout");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn runtime_soak_publishes_receipts_and_digest_linked_rows() {
    let home = tempfile::tempdir().expect("isolated daemon home");
    let daemon = start_daemon(home.path());
    let request: Fm189SoakRequest =
        serde_json::from_str(include_str!("../fixtures/runtime-soak-fm189.v1.json"))
            .expect("FM-189 acceptance request fixture");
    let response = crate::support::daemon_process::send(
        &rightkit_http::Request::post(format!("{}/runtime/soak", daemon.url()))
            .bearer(&daemon.startup.token)
            .json(&request)
            .expect("JSON request body")
            .timeout(Duration::from_secs(30)),
    )
    .expect("POST runtime soak");
    assert!(
        crate::support::daemon_process::is_success(response.status),
        "runtime soak status: {}",
        response.status
    );
    let body: Value = response.json().expect("runtime soak JSON");
    assert_eq!(body["runId"].as_str(), Some(request.run_id.as_str()));
    let receipts = body["receipts"]
        .as_array()
        .expect("published receipt manifest");
    assert_eq!(receipts.len(), 2);
    assert_eq!(body["receiptCount"], 2);
    assert_eq!(body["observedReceipts"], 2);

    let markdown = body["performanceMarkdown"]
        .as_str()
        .expect("published Markdown table");
    assert!(markdown
        .lines()
        .next()
        .unwrap_or_default()
        .contains("receipt digest"));
    for receipt in receipts {
        let digest = receipt["receiptDigest"].as_str().expect("receipt digest");
        assert!(digest.starts_with("sha256:"));
        let scenario = receipt["scenario"]
            .as_str()
            .expect("receipt scenario")
            .to_ascii_uppercase();
        let row = markdown
            .lines()
            .find(|line| line.starts_with(&format!("| {scenario} |")))
            .expect("scenario publication row");
        assert!(
            row.contains(digest),
            "row must link its source receipt digest: {row}"
        );
        let observation = &receipt["observation"];
        assert_eq!(
            observation["cancelledWorkers"].as_u64(),
            request.cancelled_workers
        );
        assert_eq!(
            observation["completedWorkers"].as_u64(),
            request.completed_workers
        );
        assert_eq!(
            observation["ownershipViolations"].as_u64(),
            request.ownership_violations
        );
    }
}

/// FM-189: a short (bounded, seconds-scale) real soak drive against a
/// spawned daemon. `driveRealWorkers` tells `/runtime/soak` to spawn real
/// `LiveSession` worker trees (see `soak_driver::drive_soak`), complete some
/// normally, cancel the rest mid-flight, and count containment from live
/// runtime state -- so the receipt's worker counters are measured, not the
/// caller-supplied passthrough values exercised by the test above.
#[test]
fn runtime_soak_drives_real_workers_and_publishes_measured_counts() {
    let home = tempfile::tempdir().expect("isolated daemon home");
    let workspace = tempfile::tempdir().expect("isolated soak worker workspace");
    let daemon = start_daemon(home.path());

    let request = Fm189SoakRequest {
        run_id: "fm-189-real-worker-soak".to_string(),
        scenarios: vec!["S0".to_string()],
        // Ignored: driveRealWorkers is present, so these passthrough values
        // must not appear in the published receipt.
        cancelled_workers: Some(999),
        completed_workers: Some(999),
        ownership_violations: Some(999),
        drive_real_workers: Some(RealWorkerDriveRequest {
            worker_count: 4,
            duration_secs: 1,
            workspace_root: workspace.path().to_string_lossy().into_owned(),
        }),
    };

    let response = crate::support::daemon_process::send(
        &rightkit_http::Request::post(format!("{}/runtime/soak", daemon.url()))
            .bearer(&daemon.startup.token)
            .json(&request)
            .expect("JSON request body")
            .timeout(Duration::from_secs(30)),
    )
    .expect("POST runtime soak with real worker drive");
    assert!(
        crate::support::daemon_process::is_success(response.status),
        "runtime soak status: {}",
        response.status
    );
    let body: Value = response.json().expect("runtime soak JSON");
    let receipts = body["receipts"]
        .as_array()
        .expect("published receipt manifest");
    assert_eq!(receipts.len(), 1);

    let observation = &receipts[0]["observation"];
    // Four workers, alternating complete/cancel: two of each, and
    // containment must hold (no orphaned descendants, no leaked roots).
    assert_eq!(observation["completedWorkers"].as_u64(), Some(2));
    assert_eq!(observation["cancelledWorkers"].as_u64(), Some(2));
    assert_eq!(observation["ownershipViolations"].as_u64(), Some(0));
}
