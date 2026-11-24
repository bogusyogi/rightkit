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

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const ROLE: &str = "RK_BROWSER_PARENT_DEATH_ROLE";
const TEST_NAME: &str = "a_killed_host_takes_chrome_with_it";

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

#[test]
fn a_killed_host_takes_chrome_with_it() {
    if std::env::var(ROLE).as_deref() == Ok("supervisor") {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");
        if rightkit_browser::find_chrome().is_none() {
            panic!("no Chrome found on this host: the kill-on-parent-exit proof cannot run");
        }
        let session = runtime
            .block_on(rightkit_browser::BrowserSession::launch(
                rightkit_browser::LaunchOptions::default(),
            ))
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

    let exe = std::env::current_exe().expect("test binary");
    let mut supervisor: Child = Command::new(exe)
        .args([
            "--exact",
            TEST_NAME,
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ROLE, "supervisor")
        .stdout(Stdio::piped())
        .spawn()
        .expect("supervisor spawns");

    let stdout = supervisor.stdout.take().expect("piped stdout");
    let (pid_tx, pid_rx) = mpsc::channel::<u32>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(pid) = line
                .strip_prefix("chrome_pid=")
                .and_then(|value| value.trim().parse::<u32>().ok())
            {
                let _ = pid_tx.send(pid);
            }
        }
    });

    let chrome = match pid_rx.recv_timeout(Duration::from_secs(60)) {
        Ok(pid) => pid,
        Err(_) => {
            let _ = supervisor.kill();
            let _ = supervisor.wait();
            panic!("supervisor did not report chrome_pid= within 60 s");
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
