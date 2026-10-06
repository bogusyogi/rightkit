use crate::report::{sanitize_kind, CrashReport, RawCrash};
use crate::{native, retention, NativeCapture, Retention};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone)]
pub struct CrashConfig {
    pub app_name: String,
    pub app_version: String,
    pub build: Option<String>,
    /// App-owned directory for reports. RightKit never picks a location.
    pub crash_dir: PathBuf,
    pub native_capture: NativeCapture,
    pub retention: Retention,
}

impl CrashConfig {
    pub fn new(
        app_name: impl Into<String>,
        app_version: impl Into<String>,
        crash_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            app_name: app_name.into(),
            app_version: app_version.into(),
            build: None,
            crash_dir: crash_dir.into(),
            native_capture: NativeCapture::Off,
            retention: Retention::default(),
        }
    }
}

static IN_HOOK: AtomicBool = AtomicBool::new(false);

/// Install the panic hook. Chains to the previously installed hook so default
/// stderr output is preserved. Call once, early in `main`. The hook still runs
/// before the abort under `panic = "abort"` (unlike `catch_unwind`).
///
/// The hook is best-effort and must never panic: every step ignores errors.
pub fn install_panic_hook(config: CrashConfig) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // A panic inside the hook (or a second thread panicking while the first
        // is writing) would abort with no report; guard against re-entrancy.
        if !IN_HOOK.swap(true, Ordering::AcqRel) {
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic payload>".to_string());
            let location = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
            let thread = std::thread::current()
                .name()
                .unwrap_or("<unnamed>")
                .to_string();
            let backtrace = std::backtrace::Backtrace::force_capture().to_string();
            let _ = record_report(
                &config,
                "panic",
                &message,
                location.as_deref(),
                &thread,
                &backtrace,
            );
            IN_HOOK.store(false, Ordering::Release);
        }
        previous(info);
    }));
}

/// Write one redacted report (atomically) and optional native capture.
/// Also the entry point for non-panic crashes: a hang watchdog or a supervisor
/// that notices a sidecar died calls this with its own fixed `kind` string.
pub fn record_report(
    config: &CrashConfig,
    kind: &str,
    message: &str,
    location: Option<&str>,
    thread: &str,
    backtrace: &str,
) -> Option<PathBuf> {
    std::fs::create_dir_all(&config.crash_dir).ok()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let kind = sanitize_kind(kind);
    let mut report = CrashReport::redacted(
        RawCrash {
            kind: &kind,
            app_name: &config.app_name,
            app_version: &config.app_version,
            build: config.build.as_deref(),
            thread,
            location,
            message,
            backtrace,
        },
        stamp,
    );
    if config.native_capture == NativeCapture::Auto {
        if let Some(path) = native::capture(&config.crash_dir, &kind, stamp) {
            if let Some(name) = path.file_name() {
                report
                    .native_artifacts
                    .push(name.to_string_lossy().into_owned());
            }
        }
    }
    let path = config
        .crash_dir
        .join(format!("crash_{kind}_{stamp}_{}.json", std::process::id()));
    let bytes = serde_json::to_vec_pretty(&report.to_scrubbed_value()).ok()?;
    // Write-then-rename: a crash while writing never leaves a truncated report
    // that the next launch would mis-parse.
    let temp = path.with_extension("json.partial");
    std::fs::write(&temp, bytes).ok()?;
    std::fs::rename(&temp, &path).ok()?;
    retention::prune(&config.crash_dir, config.retention);
    Some(path)
}
