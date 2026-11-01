//! The `membrane` binary: one program, subcommands per C4. The host app starts `serve` and owns it;
//! every other command is a one-shot run that leaves nothing running.

use rightkit_crash::{install_panic_hook, CrashConfig, NativeCapture, Retention};

/// Reports kept and the directory cap, as the host app hub and CLI keep them.
const KEEP_REPORTS: usize = 10;
const KEEP_BYTES: u64 = 1024 * 1024;

/// Local redacted panic reports under `<Membrane state>/crash/`; nothing is uploaded and no native
/// dump is taken. The directory is created by the first panic.
fn install_crash_reports() {
    let dir = rightkit_context_runtime::paths::state_root().join("crash");
    let mut config = CrashConfig::new("membrane", env!("CARGO_PKG_VERSION"), dir);
    config.native_capture = NativeCapture::Off;
    config.retention = Retention {
        max_native_dumps: 0,
        max_reports: KEEP_REPORTS,
        max_dir_bytes: KEEP_BYTES,
    };
    install_panic_hook(config);
}

fn main() {
    install_crash_reports();
    std::process::exit(rightkit_context::dispatch::run(std::env::args_os()));
}
