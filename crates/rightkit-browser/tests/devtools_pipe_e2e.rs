//! End to end: a real Chrome launched by `BrowserSession::launch` is driven over the DevTools
//! pipe only. The launch itself is the readiness proof (it returns only after Chrome answered
//! `Browser.getVersion` over the pipe); this test then round-trips a page script, checks Chrome's
//! command line carries the pipe switch and no debugging port switch, checks the Chrome process
//! listens on no TCP port, and checks that a graceful stop leaves no Chrome behind.
//!
//! It skips with a message when Chrome for Testing is not installed on the host, like
//! `kill_on_parent_exit_e2e.rs`.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::process::Command;
use std::time::{Duration, Instant};

const TEST_NAME: &str = "chrome_answers_cdp_over_the_devtools_pipe_and_opens_no_port";

/// True while the process exists and is not a zombie.
fn alive(pid: u32) -> bool {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .expect("ps runs");
    let stat = String::from_utf8_lossy(&output.stdout).trim().to_string();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// The full command line of a live process.
fn command_line(pid: u32) -> String {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[tokio::test]
async fn chrome_answers_cdp_over_the_devtools_pipe_and_opens_no_port() {
    if let Err(error) = rightkit_browser::chrome_for_testing_path(None) {
        println!(
            "skipping {TEST_NAME}: Chrome for Testing is not installed on this host, so the \
             DevTools pipe proof cannot run. Set RIGHTKIT_CHROME_FOR_TESTING to a Chrome for \
             Testing executable, or run `rightkit-qa-chrome install`. Detail: {error}"
        );
        return;
    }

    let session =
        rightkit_browser::BrowserSession::launch(rightkit_browser::LaunchOptions::default())
            .await
            .unwrap_or_else(|error| panic!("launch over the DevTools pipe failed: {error}"));
    let pid = session
        .process_id()
        .expect("launched session reports its Chrome pid");

    // A page script runs in Chrome and its value comes back over the pipe.
    let page = session
        .new_page("about:blank")
        .await
        .expect("a page opens over the pipe");
    let value = page.eval("6 * 7").await.expect("script result comes back");
    assert_eq!(value.as_f64(), Some(42.0), "script value was {value}");

    // Chrome was started with the pipe switch and without any debugging port switch.
    let argv = command_line(pid);
    assert!(
        argv.contains("--remote-debugging-pipe"),
        "Chrome (pid {pid}) must run with the DevTools pipe switch: {argv}"
    );
    assert!(
        !argv.contains("remote-debugging-port"),
        "Chrome (pid {pid}) must not be given a debugging port: {argv}"
    );

    // The browser process itself listens on no TCP port.
    match Command::new("lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), "-iTCP", "-sTCP:LISTEN"])
        .output()
    {
        Ok(output) => {
            let listing = String::from_utf8_lossy(&output.stdout).trim().to_string();
            assert!(
                listing.is_empty(),
                "Chrome (pid {pid}) must open no listening TCP socket, lsof reported:\n{listing}"
            );
        }
        Err(error) => println!("lsof is not available here, listener check skipped: {error}"),
    }

    session.shutdown().await;
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(pid) {
        assert!(
            Instant::now() < deadline,
            "Chrome (pid {pid}) is still running 10 s after a graceful shutdown"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
