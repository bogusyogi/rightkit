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
