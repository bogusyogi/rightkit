//! Native qualification for source-only fixture sessions. Each case owns a real
//! Chrome for Testing process, immutable loopback bundle & throwaway profile.
#![cfg(any(target_os = "macos", windows))]

use rightkit_browser::{
    actions_digest, chrome_for_testing_path, FixtureAction, FixtureAttestation, FixtureBundle,
    FixtureCancellation, FixtureConfinementBackend, FixtureError, FixtureFile, FixtureIntent,
    FixtureOutcomeStatus, FixtureRunFailure, FixtureServer, FixtureSession, FIXTURE_SCHEMA,
};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LEASE_MS: u64 = 30_000;
const MISSING_ACTION_TIMEOUT_MS: u64 = 5_000;
const HARD_LAUNCH_TIMEOUT: Duration = Duration::from_secs(45);
// Fixture cleanup may sequentially wait on process, transport, proxy, server & profile bounds.
// This is a hard-hang guard, not normal control flow.
const HARD_RUN_TIMEOUT: Duration = Duration::from_secs(45);

/// Fixture sessions each own a Chrome process & profile; serialize cases so cleanup evidence
/// belongs to this case alone.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Required CI supplies Chrome for Testing. CI always fails closed when it is absent; local
/// interactive runs may report a clear skip unless assets were explicitly required.
macro_rules! need_chrome {
    ($test:literal) => {
        if let Err(error) = chrome_for_testing_path(None) {
            let assets_required =
                std::env::var_os("RIGHTKIT_REQUIRE_ASSETS").is_some_and(|value| value == "1");
            let github_actions =
                std::env::var_os("GITHUB_ACTIONS").is_some_and(|value| value == "true");
            if assets_required || github_actions {
                panic!("{}: {error}", $test);
            }
            println!("[fixture-session] SKIP {}: {error}", $test);
            return;
        }
    };
}

fn files() -> Vec<FixtureFile> {
    vec![
        FixtureFile {
            path: "/index.html".into(),
            content_type: "text/html; charset=utf-8".into(),
            body: br#"<!doctype html>
<meta charset="utf-8"><title>RightKit fixture</title>
<link rel="icon" href="data:,">
<input id="value" aria-label="value"><button id="commit">Commit</button>
<p id="result">pending</p><script src="/fixture.js"></script>"#
                .to_vec(),
        },
        // Fixture CSP permits only source scripts. Keep behavior outside HTML so this
        // qualification exercises that immutable-bundle rule too.
        FixtureFile {
            path: "/fixture.js".into(),
            content_type: "application/javascript; charset=utf-8".into(),
            body: br#"document.getElementById('commit').addEventListener('click', () => {
  document.getElementById('result').innerText = document.getElementById('value').value;
});"#
                .to_vec(),
        },
    ]
}

async fn launch(probe_id: &str, actions: Vec<FixtureAction>) -> FixtureSession {
    assert!(actions.len() <= 8, "fixture action cap");
    let files = files();
    let digest = FixtureBundle::source_digest(&files).expect("fixture source digest");
    let bundle = FixtureBundle::verify("native-fixture", &digest, files).expect("verified bundle");
    let server = FixtureServer::prepare(bundle)
        .await
        .expect("owned fixture server");
    let exact_url = server.url("/index.html").expect("fixture URL");
    let intent = FixtureIntent {
        schema: FIXTURE_SCHEMA.into(),
        fixture_id: "native-fixture".into(),
        fixture_source_digest: digest,
        probe_id: probe_id.into(),
        actions_digest: actions_digest(&actions).expect("action digest"),
        exact_url,
        actions,
        expires_after_ms: LEASE_MS,
    };
    tokio::time::timeout(HARD_LAUNCH_TIMEOUT, FixtureSession::launch(server, intent))
        .await
        .expect("fixture launch hard-hang guard")
        .expect("native fixture launch")
}

fn assert_platform_backend(attestation: &FixtureAttestation) {
    #[cfg(target_os = "macos")]
    assert_eq!(
        attestation.confinement_backend,
        FixtureConfinementBackend::MacosSeatbeltProxy,
        "attestation must name active macOS fixture backend"
    );
    #[cfg(windows)]
    assert_eq!(
        attestation.confinement_backend,
        FixtureConfinementBackend::WindowsAppcontainerPipeFulfillment,
        "attestation must name active Windows fixture backend"
    );
}

#[cfg(target_os = "macos")]
fn process_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .expect("macOS process query")
        .success()
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}"
            ),
        ])
        .status()
        .expect("Windows process query")
        .success()
}

/// Read process tables without passing `marker` to a host command, then filter in Rust. Chrome
/// includes its unique temporary profile path in root & child command lines.
fn pids_for_profile(marker: &str) -> Vec<u32> {
    #[cfg(target_os = "macos")]
    let output = Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .expect("macOS process-table query");
    #[cfg(windows)]
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-CimInstance Win32_Process | ForEach-Object { [string]$_.ProcessId + ' ' + $_.CommandLine }",
        ])
        .output()
        .expect("Windows process-table query");
    assert!(
        output.status.success(),
        "process-table query failed: {}",
        output.status
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(marker))
        .filter_map(|line| line.split_whitespace().next()?.parse().ok())
        .collect()
}

async fn wait_profile_processes_gone(marker: &str) -> Vec<u32> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let pids = pids_for_profile(marker);
        if pids.is_empty() || tokio::time::Instant::now() >= deadline {
            return pids;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn assert_terminal(attestation: &FixtureAttestation, label: &str) {
    assert_platform_backend(attestation);
    assert!(
        attestation.output_cleanup.browser_stopped
            && attestation.output_cleanup.transport_stopped
            && attestation.output_cleanup.proxy_stopped
            && attestation.output_cleanup.server_stopped
            && attestation.output_cleanup.profile_removed,
        "{label}: incomplete cleanup evidence: {:?}",
        attestation.output_cleanup
    );
    assert!(
        !Path::new(&attestation.profile_identity).exists(),
        "{label}: fixture profile remains at {}",
        attestation.profile_identity
    );

    let endpoint: SocketAddr = attestation
        .server_endpoint
        .parse()
        .expect("attested endpoint");
    assert!(
        TcpStream::connect_timeout(&endpoint, Duration::from_millis(300)).is_err(),
        "{label}: fixture endpoint still accepts connections: {endpoint}"
    );
    let rebind_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let rebound = loop {
        match TcpListener::bind(endpoint) {
            Ok(listener) => break listener,
            Err(_) if tokio::time::Instant::now() < rebind_deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("{label}: fixture endpoint did not rebind: {error}"),
        }
    };
    drop(rebound);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while process_alive(attestation.browser_pid) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !process_alive(attestation.browser_pid),
        "{label}: owned Chrome pid {} remains alive",
        attestation.browser_pid
    );
    let profile_processes = wait_profile_processes_gone(&attestation.profile_identity).await;
    assert!(
        profile_processes.is_empty(),
        "{label}: fixture profile marker remains in Chrome process command lines: {profile_processes:?}"
    );
}

fn missing_action() -> FixtureAction {
    FixtureAction::AssertVisible {
        selector: "#never-created".into(),
        timeout: Some(MISSING_ACTION_TIMEOUT_MS),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fixture_session_passes_with_native_backend_and_terminal_cleanup() {
    need_chrome!("fixture_session_passes_with_native_backend_and_terminal_cleanup");
    let _serial = SERIAL.lock().await;
    let actions = vec![
        FixtureAction::Click {
            selector: "#value".into(),
        },
        FixtureAction::Type {
            text: "native fixture".into(),
        },
        FixtureAction::Click {
            selector: "#commit".into(),
        },
        FixtureAction::AssertText {
            selector: "#result".into(),
            text: "native fixture".into(),
        },
    ];
    let session = launch("native-pass", actions).await;
    let launched = session.attestation().clone();
    assert_platform_backend(&launched);
    assert!(Path::new(&launched.profile_identity).is_dir());

    let outcome = tokio::time::timeout(HARD_RUN_TIMEOUT, session.run())
        .await
        .expect("pass run hard-hang guard")
        .expect("fixture run passes");
    println!(
        "[fixture-session] pass pid={} endpoint={} profile={} actions={} cleanup={:?}",
        outcome.attestation.browser_pid,
        outcome.attestation.server_endpoint,
        outcome.attestation.profile_identity,
        outcome.completed_actions,
        outcome.attestation.output_cleanup
    );
    assert_eq!(outcome.status, FixtureOutcomeStatus::Passed);
    assert_eq!(outcome.completed_actions, 4);
    assert!(outcome.cleanup_complete);
    assert_terminal(&outcome.attestation, "pass").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fixture_session_cancels_during_missing_element_action_and_cleans_up() {
    need_chrome!("fixture_session_cancels_during_missing_element_action_and_cleans_up");
    let _serial = SERIAL.lock().await;
    let session = launch("native-cancel", vec![missing_action()]).await;
    let cancellation = FixtureCancellation::new();
    let signal = cancellation.clone();
    let canceller = tokio::spawn(async move {
        // Launch is complete. `#never-created` cannot complete this action, so this signal
        // arrives during its visibility wait rather than before `run_with_cancel` begins.
        tokio::time::sleep(Duration::from_millis(250)).await;
        signal.cancel();
    });
    let failure = tokio::time::timeout(HARD_RUN_TIMEOUT, session.run_with_cancel(cancellation))
        .await
        .expect("cancel run hard-hang guard")
        .expect_err("cancellation must stop missing-element action");
    canceller.await.expect("cancellation task");
    println!(
        "[fixture-session] cancel cause={} pid={} endpoint={} profile={} actions={} cleanup={:?}",
        failure.cause,
        failure.attestation.browser_pid,
        failure.attestation.server_endpoint,
        failure.attestation.profile_identity,
        failure.completed_actions,
        failure.attestation.output_cleanup
    );
    assert!(matches!(failure.cause, FixtureError::Cancelled(_)));
    assert_eq!(failure.completed_actions, 0);
    assert!(failure.cleanup_complete, "{failure:?}");
    assert!(failure.cleanup_error.is_none(), "{failure:?}");
    assert_terminal(&failure.attestation, "cancel").await;
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after Unix epoch")
        .as_millis()
}

#[tokio::test(flavor = "multi_thread")]
async fn fixture_session_deadline_expires_during_missing_element_action_and_cleans_up() {
    need_chrome!("fixture_session_deadline_expires_during_missing_element_action_and_cleans_up");
    let _serial = SERIAL.lock().await;
    let session = launch("native-deadline", vec![missing_action()]).await;
    let expires_at = session.attestation().expires_at_unix_ms;
    let remaining = expires_at.saturating_sub(unix_ms());
    assert!(
        remaining > 5_000,
        "launch consumed fixture lease: {remaining}ms remain"
    );

    // Use almost all 30 s lease only after launch has an attested native backend. The missing
    // element action has a 5 s bound, leaving it active when fixture lease expires.
    let target_remaining_ms = 3_500u128;
    tokio::time::sleep(Duration::from_millis(
        remaining
            .saturating_sub(target_remaining_ms)
            .try_into()
            .unwrap(),
    ))
    .await;
    let before_run = expires_at.saturating_sub(unix_ms());
    assert!(
        (1_000..=5_000).contains(&before_run),
        "deadline action did not begin within remaining lease: {before_run}ms"
    );
    let failure: FixtureRunFailure = tokio::time::timeout(
        HARD_RUN_TIMEOUT,
        session.run_with_cancel(FixtureCancellation::new()),
    )
    .await
    .expect("deadline run hard-hang guard")
    .expect_err("fixture lease must expire during missing-element action");
    println!(
        "[fixture-session] deadline cause={} pid={} endpoint={} profile={} actions={} cleanup={:?}",
        failure.cause,
        failure.attestation.browser_pid,
        failure.attestation.server_endpoint,
        failure.attestation.profile_identity,
        failure.completed_actions,
        failure.attestation.output_cleanup
    );
    assert!(matches!(failure.cause, FixtureError::Expired(_)));
    assert_eq!(failure.completed_actions, 0);
    assert!(failure.cleanup_complete, "{failure:?}");
    assert!(failure.cleanup_error.is_none(), "{failure:?}");
    assert_terminal(&failure.attestation, "deadline").await;
}
