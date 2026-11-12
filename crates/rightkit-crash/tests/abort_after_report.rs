//! `CrashConfig::abort_after_report`: the process aborts after the report is written.

use rightkit_crash::{install_panic_hook, list_pending, CrashConfig};
use std::path::PathBuf;
use std::process::Command;

const CHILD_ENV: &str = "RK_ABORT_CHILD_DIR";
const ABORT_ENV: &str = "RK_ABORT_CHILD_ABORT";

#[test]
fn child_panics() {
    let Ok(dir) = std::env::var(CHILD_ENV) else {
        return;
    };
    let config = CrashConfig::new("ExampleApp", "1.0.0", PathBuf::from(dir).join("crash"))
        .abort_after_report(std::env::var(ABORT_ENV).is_ok());
    install_panic_hook(config);
    let _ = std::thread::spawn(|| panic!("boom")).join();
    std::process::exit(101);
}

fn run(abort: bool, label: &str) -> (std::process::ExitStatus, usize) {
    let dir = std::env::temp_dir().join(format!("rk-abort-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "child_panics", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, &dir)
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    if abort {
        command.env(ABORT_ENV, "1");
    }
    let status = command.status().unwrap();
    let reports = list_pending(&dir.join("crash")).len();
    let _ = std::fs::remove_dir_all(&dir);
    (status, reports)
}

#[test]
fn abort_is_opt_in_and_follows_the_report() {
    let (status, reports) = run(false, "off");
    assert_eq!(
        status.code(),
        Some(101),
        "default keeps running to the app's own exit"
    );
    assert_eq!(reports, 1);

    let (status, reports) = run(true, "on");
    assert_ne!(
        status.code(),
        Some(101),
        "aborted before the app's exit: {status:?}"
    );
    assert!(!status.success());
    assert_eq!(reports, 1, "the report is on disk before the abort");
}
