//! Sidecar-side crash capture — the ASR engine's own postmortem.
//!
//! The shell (`heardright_next`) can only dump ITS OWN process when it notices
//! the sidecar died; by then the sidecar is gone and undumpable from outside.
//! So the engine installs its own panic hook through `rightkit-crash`, writing
//! a redacted, content-free report and a native dump of itself into the SAME
//! `app_data/crash` directory the shell exports and prunes. Owner filenames use
//! the shell's retention prefixes (`crash_` / `dump_` / `sample_`), so
//! `rightkit_crash::prune` (called from `crash_dump::write_dump`) manages engine artifacts too.
//!
//! The release profile is `panic = "unwind"` (the RightKit standard), so a panic on a worker thread
//! would kill only that thread and leave a live engine with dead dictation. The hook therefore
//! ends the process itself after the report is written; the shell's on-demand restart and
//! crash-dump observer then handle it exactly as they did under `panic = "abort"`.

use std::path::PathBuf;

use rightkit_crash::{CrashConfig, NativeCapture, ProcessKind};

static CRASH_IDENTITY: std::sync::OnceLock<(&'static str, &'static str)> =
    std::sync::OnceLock::new();

/// Name the product and version crash reports carry. Call before [`install_panic_hook`];
/// unconfigured reports carry the data folder name and this crate's version. Returns false when
/// already set.
pub fn configure_crash_identity(product: &'static str, version: &'static str) -> bool {
    CRASH_IDENTITY.set((product, version)).is_ok()
}

fn crash_identity() -> (&'static str, &'static str) {
    CRASH_IDENTITY.get().copied().unwrap_or_else(|| {
        (
            rightkit_dictation_core::app_paths::app_data_dir_name(),
            env!("CARGO_PKG_VERSION"),
        )
    })
}

fn crash_dir() -> PathBuf {
    crate::settings::app_data_root().join("crash")
}

/// Install the sidecar panic hook. Records the report, then aborts the process so the shell
/// restarts the engine. Call once, early in `main`.
pub fn install_panic_hook() {
    rightkit_crash::set_dump_helper("rightkit-dump-helper.exe");
    install_at(crash_dir(), NativeCapture::Auto);
}

fn install_at(dir: PathBuf, native_capture: NativeCapture) {
    let mut config =
        CrashConfig::new(crash_identity().0, crash_identity().1, dir).with_sink(|report| {
            let field = |name: &str| report.get(name).and_then(|v| v.as_str()).unwrap_or("");
            tracing::error!(
                "sidecar panic thread={} loc={} msg={:?}",
                field("thread"),
                field("location"),
                field("message")
            );
        });
    config.native_capture = native_capture;
    rightkit_crash::install_process_hook(ProcessKind::Engine, config);
    abort_after_hook();
}

/// Wrap the installed hook so the process ends once the report and native capture have run.
/// A panic that unwinds out of a worker thread would otherwise leave the engine alive and silent.
fn abort_after_hook() {
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        report(info);
        std::process::abort();
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const CHILD_ENV: &str = "RIGHTKIT_CRASH_CAPTURE_TEST_CHILD";

    /// Runs inside the re-executed child: installs the hook, panics on a spawned thread, and would
    /// otherwise exit cleanly after five seconds.
    #[test]
    fn child_panics_on_worker_thread() {
        let Ok(dir) = std::env::var(CHILD_ENV) else {
            return;
        };
        install_at(PathBuf::from(dir), NativeCapture::Off);
        let _ = std::thread::Builder::new()
            .name("test-worker".into())
            .spawn(|| panic!("worker failed"))
            .unwrap()
            .join();
        std::thread::sleep(Duration::from_secs(5));
        std::process::exit(0);
    }

    #[test]
    fn a_worker_thread_panic_writes_a_report_and_aborts_the_process() {
        let dir = std::env::temp_dir().join(format!("hr-crash-capture-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let started = Instant::now();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "crash_capture::tests::child_panics_on_worker_thread",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, &dir)
            .env("RUST_BACKTRACE", "0")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("spawn child");
        assert!(
            !status.success(),
            "the child must end abnormally, got {status:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "the child lived through the panic"
        );
        let reports = std::fs::read_dir(&dir)
            .expect("crash dir")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("crash_"))
            .count();
        assert!(reports >= 1, "no crash_ report was written");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
