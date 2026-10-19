//! Per-test child process with an isolated suite root, for the suite-touching tests.
//!
//! Why a child process: the build broker never forwards `RIGHTKIT_SUITE_ROOT` (on Windows it does
//! not set `LOCALAPPDATA`; on the Mac it passes the real `HOME`), so a test-mode build refuses the
//! real per-user root and these tests panic. `std::env::set_var` is unsound while libtest runs
//! other threads, so the override is given to a child through `Command::env` instead.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_ENV: &str = "RIGHTKIT_MODELS_TEST_CHILD";
const ROOT_ENV: &str = "RIGHTKIT_SUITE_ROOT";
/// Upper bound for one isolated test child.
const CHILD_LIMIT: Duration = Duration::from_secs(300);

/// Runs `body` with `RIGHTKIT_SUITE_ROOT` set to a fresh directory, in a child process.
///
/// The parent re-runs this test binary filtered to `test_name` (`--exact`, one thread) with the
/// override and `RIGHTKIT_MODELS_TEST_CHILD=<test_name>`. In that child `body` runs in place. The
/// parent requires a successful exit and `1 passed` on stdout, so a filter that matched nothing
/// cannot pass.
pub fn isolated(test_name: &str, body: impl FnOnce()) {
    if std::env::var(CHILD_ENV).as_deref() == Ok(test_name) {
        body();
        return;
    }
    // Cargo's per-target scratch dir lives under the broker-managed target dir (the approved
    // external volume), never the host's default temp on the internal disk.
    let tmp = tempfile::Builder::new()
        .prefix("suite-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .expect("create a temp dir for the isolated suite root");
    let root: PathBuf = tmp.path().join("suite");
    std::fs::create_dir_all(&root).expect("create the isolated suite root");
    let mut child = Command::new(std::env::current_exe().expect("locate the test binary"))
        .args([test_name, "--exact", "--nocapture", "--test-threads=1"])
        .env(ROOT_ENV, &root)
        .env(CHILD_ENV, test_name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the isolated test child");
    // Bounded: a hung child is killed and reported, never waited on forever.
    let deadline = Instant::now() + CHILD_LIMIT;
    while child
        .try_wait()
        .expect("poll the isolated test child")
        .is_none()
    {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("isolated test `{test_name}` exceeded {CHILD_LIMIT:?} and was killed");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child
        .wait_with_output()
        .expect("collect the isolated test child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() || !stdout.contains("1 passed") {
        panic!(
            "isolated test `{test_name}` failed (status {}):\n--- child stdout ---\n{stdout}\n--- child stderr ---\n{stderr}",
            output.status
        );
    }
    assert!(
        root.exists(),
        "isolated suite root was not created: {}",
        root.display()
    );
}
