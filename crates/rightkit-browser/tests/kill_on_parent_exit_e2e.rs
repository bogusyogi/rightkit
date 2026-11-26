//! End to end: a `BrowserSession` launched by a host that is then SIGKILLed takes Chrome and its
//! whole process group with it (CodeRight #470). Chrome is tied by `kill_on_parent_exit`, which on
//! Linux is `PR_SET_PDEATHSIG` keyed to the spawning thread, so the spawn runs on the dedicated
//! launcher thread. The proof also waits past tokio's 10 s blocking-thread keep-alive to show that
//! a thread exiting does not take Chrome down.
//!
//! The test binary runs itself a second time as the supervisor (`RK_BROWSER_PARENT_DEATH_ROLE`).
//! The supervisor launches Chrome, prints its pid and waits. The test checks Chrome is alive, kills
//! the supervisor with SIGKILL (no cleanup runs), and expects Chrome and its group to be gone.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const ROLE: &str = "RK_BROWSER_PARENT_DEATH_ROLE";
const TEST_NAME: &str = "a_killed_host_takes_chrome_with_it";

/// The Chrome for Testing the host names: the executable in `RIGHTKIT_CHROME_FOR_TESTING`, else
/// the one in the managed root `RIGHTKIT_CHROME_FOR_TESTING_ROOT` when it holds an install. The
/// crate reads these names only with its `host-env` feature, so the test reads them itself and
/// passes the result as the explicit executable: the proof runs the same in every feature round.
fn host_chrome() -> Option<std::path::PathBuf> {
    let non_empty = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
    };
    non_empty("RIGHTKIT_CHROME_FOR_TESTING").or_else(|| {
        non_empty("RIGHTKIT_CHROME_FOR_TESTING_ROOT")
            .map(|root| rightkit_browser::ManagedRoot::at(root).executable())
            .filter(|executable| executable.exists())
    })
}

/// Chrome for Testing as the launches below resolve it.
fn resolve_chrome() -> rightkit_browser::Result<std::path::PathBuf> {
    rightkit_browser::chrome_for_testing_path(host_chrome().as_deref())
}

/// Default launch options with the host's Chrome for Testing as the explicit executable.
#[allow(clippy::field_reassign_with_default)]
fn launch_options() -> rightkit_browser::LaunchOptions {
    let mut options = rightkit_browser::LaunchOptions::default();
    options.chrome_path = host_chrome();
    options
}

/// True while the process exists and is not a zombie.
fn alive(pid: u32) -> bool {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .expect("ps runs");
    let stat = String::from_utf8_lossy(&output.stdout).trim().to_string();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// True while any process still belongs to the process group led by `pgid`.
fn group_members_remain(pgid: u32) -> bool {
    let output = Command::new("ps")
        .args(["-A", "-o", "pgid="])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .any(|group| group == pgid)
}

/// True when Chrome for Testing resolves the way the supervisor's default launch resolves it.
/// Otherwise prints the reason on stdout and returns false, so the test skips.
fn chrome_for_testing_or_skip() -> bool {
    match resolve_chrome() {
        Ok(_) => true,
        Err(error) => {
            println!(
                "skipping {TEST_NAME}: Chrome for Testing is not installed on this host, so the \
                 kill-on-parent-exit proof cannot run. Set RIGHTKIT_CHROME_FOR_TESTING to a \
                 Chrome for Testing executable, or run `rightkit-qa-chrome install`. \
                 Detail: {error}"
            );
            false
        }
    }
}

#[test]
fn a_killed_host_takes_chrome_with_it() {
    if std::env::var(ROLE).as_deref() == Ok("supervisor") {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");
        // The same resolver the launch below uses. `find_chrome` would also accept a
        // regular Google Chrome, which `LaunchOptions::default()` never launches, so checking it
        // here would refuse a host that has Chrome for Testing but no regular Chrome.
        if let Err(error) = resolve_chrome() {
            panic!("Chrome for Testing is not available here, so the proof cannot run: {error}");
        }
        let session = runtime
            .block_on(rightkit_browser::BrowserSession::launch(launch_options()))
            .unwrap_or_else(|error| panic!("supervisor could not launch Chrome: {error}"));
        let pid = session
            .process_id()
            .expect("launched session reports its Chrome pid");
        println!("chrome_pid={pid}");
        std::io::stdout().flush().expect("flush stdout");
        // Held without cleanup: the test SIGKILLs this process, so Drop never runs.
        std::thread::sleep(Duration::from_secs(120));
        drop(session);
        return;
    }

    if !chrome_for_testing_or_skip() {
        return;
    }

    let exe = std::env::current_exe().expect("test binary");
    let mut supervisor: Child = Command::new(exe)
        .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
        .env(ROLE, "supervisor")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("supervisor spawns");

    let stderr = supervisor.stderr.take().expect("piped stderr");
    let (stderr_tx, stderr_rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        // Drains the pipe for the whole run so the supervisor never blocks on a full pipe.
        let mut text = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut text);
        let _ = stderr_tx.send(text);
    });

    let stdout = supervisor.stdout.take().expect("piped stdout");
    let (pid_tx, pid_rx) = mpsc::channel::<u32>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            // With one test thread libtest prints "test <name> ... " with no newline before the
            // test's own output, so the marker is not at the start of its line.
            if let Some(pid) = line
                .split_once("chrome_pid=")
                .and_then(|(_, value)| value.trim().parse::<u32>().ok())
            {
                let _ = pid_tx.send(pid);
            }
        }
    });

    let chrome = match pid_rx.recv_timeout(Duration::from_secs(60)) {
        Ok(pid) => pid,
        Err(_) => {
            // A supervisor that exits closes its stdout at once, so this arm returns without
            // waiting out the 60 s bound; the bound applies only to a live supervisor.
            let _ = supervisor.kill();
            let status = supervisor.wait();
            let stderr_text = stderr_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|_| "(stderr still open: a child holds the pipe)".into());
            panic!(
                "supervisor did not report chrome_pid= (exit status {status:?}); \
                 supervisor stderr:\n{stderr_text}"
            );
        }
    };

    assert!(
        alive(chrome),
        "Chrome (pid {chrome}) must be alive while the supervisor runs"
    );

    // Longer than tokio's 10 s blocking-thread keep-alive: the thread that spawned Chrome would
    // have exited by now if the launch had run on a pooled blocking thread.
    std::thread::sleep(Duration::from_secs(12));
    assert!(
        alive(chrome),
        "Chrome (pid {chrome}) must survive after the spawning thread would have exited"
    );

    supervisor.kill().expect("SIGKILL the supervisor");
    supervisor.wait().expect("reap the supervisor");

    // The watchdog TERMs the group, then KILLs it 2 s later. Allow 10 s in total.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if !alive(chrome) && !group_members_remain(chrome) {
            break;
        }
        if Instant::now() >= deadline {
            panic!(
                "Chrome (pid {chrome}) or its process group survived the SIGKILL of its host \
                 for more than 10 s"
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
