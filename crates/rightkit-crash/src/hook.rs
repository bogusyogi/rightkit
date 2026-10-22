use crate::report::{
    redact_known, redact_known_value, sanitize_kind, CrashReport, RawCrash, SecretList,
};
use crate::{native, retention, NativeCapture, Retention};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

static FILE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Process-wide sequence number that keeps artifact file names unique when several land within the
/// same second.
pub(crate) fn next_file_seq() -> u64 {
    FILE_SEQ.fetch_add(1, Ordering::Relaxed)
}

/// How long a report waits for its sink. A sink that blocks (for example on a lock the panicking
/// thread holds) stops being waited for after this, and the report path continues.
const SINK_TIMEOUT: Duration = Duration::from_secs(2);

/// Host callback that receives each already-redacted report, as the value that would be written.
type ReportCallback = Arc<dyn Fn(serde_json::Value) + Send + Sync + 'static>;

/// Newtype so `CrashConfig` can keep deriving `Debug` and `Clone`.
#[derive(Clone)]
struct ReportSink(ReportCallback);

impl fmt::Debug for ReportSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ReportSink")
    }
}

/// The process boundary a host installs crash capture in. Every required process of an app (the
/// webview window host, the engine, each supervised helper) installs through the same owner API
/// and its reports say which boundary they came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessKind {
    /// The window/webview host process, including renderer errors it forwards.
    Window,
    /// The app engine process.
    Engine,
    /// A supervised helper or sidecar process.
    Helper,
}

impl ProcessKind {
    /// Fixed code written to the report's `process_kind` field.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Engine => "engine",
            Self::Helper => "helper",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CrashConfig {
    pub app_name: String,
    pub app_version: String,
    pub build: Option<String>,
    /// App-owned directory for reports. RightKit never picks a location.
    pub crash_dir: PathBuf,
    pub native_capture: NativeCapture,
    pub retention: Retention,
    /// Optional host sink for each redacted report (see [`CrashConfig::with_sink`]).
    sink: Option<ReportSink>,
    /// Whether a report file is written to `crash_dir` (default `true`; see [`CrashConfig::write_report_file`]).
    write_file: bool,
    /// Whether the panic hook captures a backtrace (default `true`; see [`CrashConfig::capture_backtrace`]).
    capture_backtrace: bool,
    /// Declared process boundary (see [`CrashConfig::process_kind`]).
    process_kind: Option<ProcessKind>,
    /// Known secrets removed from every report and native text artifact (see
    /// [`CrashConfig::redact_values`]).
    redact_values: SecretList,
    /// Whether the panic hook hands the panic to the previously installed hook (default: only in
    /// builds with debug assertions; see [`CrashConfig::chain_default_hook`]).
    chain_default_hook: bool,
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
            sink: None,
            write_file: true,
            capture_backtrace: true,
            process_kind: None,
            redact_values: SecretList::default(),
            chain_default_hook: cfg!(debug_assertions),
        }
    }

    /// Add secret strings (API keys, database keys, tokens the app holds) that are removed from
    /// every report and from the macOS native sample text, replaced with `[redacted]`. This is in
    /// addition to the built-in pattern scrub, and catches formats no pattern knows. Empty strings
    /// are ignored. Same contract as `rightkit-http`'s `ClientConfig::redact_values`. A Windows
    /// minidump is binary and is not scrubbed (see the README). The values are never printed by
    /// `Debug`.
    pub fn redact_values(mut self, values: Vec<String>) -> Self {
        self.redact_values.extend(values);
        self
    }

    /// Choose what the panic hook does after recording a report. `true` passes the panic to the
    /// previous hook, so the standard message (which quotes the panic payload) still reaches
    /// stderr. `false` drops that output: the hook writes one fixed line that names the app and
    /// points at the crash report, and the panic payload is not printed. The default is `false`
    /// in release builds (a panic payload can carry user content into a log the app does not
    /// control) and `true` in builds with debug assertions, so development keeps the full message.
    pub fn chain_default_hook(mut self, enabled: bool) -> Self {
        self.chain_default_hook = enabled;
        self
    }

    /// The known secrets set with [`CrashConfig::redact_values`], for the hang watchdog's log tail.
    pub(crate) fn secrets(&self) -> &[String] {
        self.redact_values.as_slice()
    }

    /// Whether files are written to `crash_dir` (see [`CrashConfig::write_report_file`]).
    pub(crate) fn writes_files(&self) -> bool {
        self.write_file
    }

    /// Declare which process boundary this configuration belongs to; every report it records
    /// carries the code in `process_kind`.
    pub fn process_kind(mut self, kind: ProcessKind) -> Self {
        self.process_kind = Some(kind);
        self
    }

    /// Hand each redacted report to `sink` as a JSON value: the same value that would be written to
    /// `crash_dir`. A host uses this to write reports into its own log. The sink runs inside the
    /// panic hook, under the re-entrancy guard, on a short-lived thread that the report path waits
    /// for at most two seconds (if that thread cannot be spawned, the sink runs inline on the
    /// panicking thread instead, unbounded). The file record, when enabled, is written before the
    /// sink runs.
    ///
    /// With `panic = "unwind"` a sink that panics is contained by its thread: the panic is reported
    /// through the previous hook, and the next panic is still recorded. With `panic = "abort"` a
    /// sink panic aborts the process as any panic does; the file record, if enabled, is already on
    /// disk by then, but a sink-only configuration has no other record. The sink should not take
    /// locks the panicking code might hold.
    pub fn with_sink(mut self, sink: impl Fn(serde_json::Value) + Send + Sync + 'static) -> Self {
        self.sink = Some(ReportSink(Arc::new(sink)));
        self
    }

    /// This configuration without its sink (used by the hang watchdog, which reports through
    /// `HangOptions::on_hang` instead).
    pub(crate) fn without_sink(mut self) -> Self {
        self.sink = None;
        self
    }

    /// Choose whether reports are also written to `crash_dir` (default `true`). With `false` and no
    /// sink, nothing is recorded and [`record_report`] returns `None`.
    pub fn write_report_file(mut self, enabled: bool) -> Self {
        self.write_file = enabled;
        self
    }

    /// Choose whether the panic hook captures a backtrace (default `true`). Capturing and
    /// symbolizing one runs before the sink and is not bounded by the sink's two-second wait, so a
    /// host that only forwards the message to its own log can turn it off; the report's
    /// `backtrace` field is then empty. Reports recorded through [`record_report`] carry the
    /// backtrace the caller passes, regardless of this setting.
    pub fn capture_backtrace(mut self, enabled: bool) -> Self {
        self.capture_backtrace = enabled;
        self
    }
}

/// Run `sink` on its own thread and wait for it, bounded by [`SINK_TIMEOUT`]. A panic in the sink
/// ends that thread only (the sink thread holds the re-entrancy guard, so the panic hook skips the
/// nested capture and chains to the previous hook), so under unwinding the caller always returns.
/// If the thread cannot be spawned the sink runs inline on the calling thread, which already holds
/// the guard, so the record is not lost.
fn run_sink(sink: &ReportSink, value: serde_json::Value) {
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let callback = Arc::clone(&sink.0);
    // The closure owns the value until it runs; recover it when the spawn fails.
    let slot = Arc::new(std::sync::Mutex::new(Some(value)));
    let thread_slot = Arc::clone(&slot);
    let spawned = spawn_sink_thread(move || {
        let _guard = ReentryGuard::enter();
        let taken = thread_slot.lock().ok().and_then(|mut v| v.take());
        if let Some(value) = taken {
            callback(value);
        }
        let _ = done_tx.send(());
    });
    if spawned {
        // Disconnected (the sink panicked and dropped the sender) returns at once.
        let _ = done_rx.recv_timeout(SINK_TIMEOUT);
    } else {
        let taken = slot.lock().ok().and_then(|mut v| v.take());
        if let Some(value) = taken {
            (sink.0)(value);
        }
    }
}

/// Spawn the sink thread; false when the OS refused (or, in tests, when the seam says so).
fn spawn_sink_thread(work: impl FnOnce() + Send + 'static) -> bool {
    #[cfg(test)]
    if FAIL_NEXT_SPAWN.swap(false, Ordering::SeqCst) {
        return false;
    }
    std::thread::Builder::new()
        .name("rightkit-crash-sink".into())
        .spawn(work)
        .is_ok()
}

thread_local! {
    /// True while this thread is recording a panic (or running a sink for one). Per thread, so
    /// concurrent panics on different threads are each recorded, while a panic inside the hook on
    /// the same thread cannot recurse. `const` init and no destructor: access is safe in a hook.
    static IN_HOOK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test seam: the next sink thread spawn reports failure.
#[cfg(test)]
static FAIL_NEXT_SPAWN: AtomicBool = AtomicBool::new(false);
/// The statics here are process-wide; every test that records or toggles recording takes this.
#[cfg(test)]
pub(crate) static TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// Runtime switch for [`record_report`]; reports are recorded unless this is `false`.
static RECORDING: AtomicBool = AtomicBool::new(true);
/// Test seam: the next report write panics once, so tests can prove the guard resets.
#[cfg(test)]
pub(crate) static FAIL_NEXT_WRITE: AtomicBool = AtomicBool::new(false);

/// Holds this thread's re-entrancy flag for one capture. Dropping it (normally or while unwinding) clears
/// the flag. A caller that finds the flag already set gets `None` and records nothing.
struct ReentryGuard;

impl ReentryGuard {
    fn enter() -> Option<Self> {
        IN_HOOK
            .try_with(|flag| (!flag.replace(true)).then_some(Self))
            .ok()
            .flatten()
    }
}

impl Drop for ReentryGuard {
    fn drop(&mut self) {
        let _ = IN_HOOK.try_with(|flag| flag.set(false));
    }
}

/// Turn panic recording on or off at runtime, after [`install_panic_hook`]. The previously
/// installed hook still runs either way. Recording is on by default.
pub fn set_recording_enabled(enabled: bool) {
    RECORDING.store(enabled, Ordering::Release);
}

/// Whether [`record_report`] currently writes reports.
pub fn recording_enabled() -> bool {
    RECORDING.load(Ordering::Acquire)
}

/// Install the panic hook. In release builds it prints one fixed line instead of chaining to the
/// previous hook, so the panic payload never reaches stderr; builds with debug assertions chain
/// (see [`CrashConfig::chain_default_hook`]). Call once, early in `main`. The hook still runs
/// before the abort under `panic = "abort"` (unlike `catch_unwind`).
///
/// The hook is best-effort and must never panic: every step ignores errors. Note that the
/// standard library aborts the process when a panic hook itself panics, so this guarantee rests
/// on the report path not panicking; the guard only makes sure that a failure which unwinds
/// (for example from a caller that catches it) leaves recording enabled for the next panic.
pub fn install_panic_hook(config: CrashConfig) {
    let previous = std::panic::take_hook();
    let chain = config.chain_default_hook;
    let app_name = config.app_name.clone();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        let recorded = capture_panic(&config, &message, location.as_deref()).is_some();
        if chain {
            previous(info);
        } else {
            eprintln!("{}", non_chaining_notice(&app_name, recorded));
        }
    }));
}

/// The stderr line a non-chaining hook prints after a panic. The success wording is used only
/// when the report was actually recorded.
fn non_chaining_notice(app_name: &str, recorded: bool) -> String {
    if recorded {
        format!("{app_name} stopped after an internal error. A crash report was recorded.")
    } else {
        format!("{app_name} stopped after an internal error. A crash report could not be recorded.")
    }
}

/// Install the panic hook for one process boundary: the same as setting
/// [`CrashConfig::process_kind`] and calling [`install_panic_hook`]. Each required process
/// (window host, engine, every supervised helper) calls this once, early in its `main`.
pub fn install_process_hook(kind: ProcessKind, config: CrashConfig) {
    install_panic_hook(config.process_kind(kind));
}

/// Record that a supervised helper died. Called by the supervising process, which still has the
/// restart context the dead helper cannot report. `exit` is a short fixed description such as an
/// exit code or signal name; `restart_count` is how many restarts the supervisor has done.
/// Returns what [`record_report`] returns; a helper that never got to install a hook is still
/// covered because the supervisor's own config records the report.
pub fn record_helper_exit(
    config: &CrashConfig,
    helper: &str,
    exit: &str,
    restart_count: u32,
) -> Option<PathBuf> {
    let mut config = config.clone();
    config.process_kind = Some(ProcessKind::Helper);
    let message = format!("helper {helper} exited: {exit}; restarts so far: {restart_count}");
    record_report(&config, "sidecar_crash", &message, None, "supervisor", "")
}

/// Where reports go when `crash_dir` cannot be created or written to: a private (`0700` on Unix)
/// per-app folder in the OS temp directory. An app that uses the fallback lists it at next launch
/// with `list_pending(&fallback_crash_dir(app_name))`. Reports there are redacted and owner-only
/// like any other; the folder is temporary storage the OS may clear.
pub fn fallback_crash_dir(app_name: &str) -> PathBuf {
    let name: String = app_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(48)
        .collect();
    let name = if name.is_empty() {
        "app".to_string()
    } else {
        name
    };
    std::env::temp_dir().join(format!("rightkit-crash-{name}"))
}

/// True when `dir` exists (created if needed) and a file can be created in it.
fn dir_is_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".rightkit_probe_{}", std::process::id()));
    let ok = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe)
        .is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// The directory a report is written to: `crash_dir` when it is usable, else the private
/// [`fallback_crash_dir`] (announced on stderr as one `rightkit-crash:` line), else `None`.
/// The fallback is refused when it is a symlink or not ours (the private-dir hardening fails).
pub(crate) fn resolve_crash_dir(config: &CrashConfig) -> Option<PathBuf> {
    if dir_is_writable(&config.crash_dir) {
        return Some(config.crash_dir.clone());
    }
    let fallback = fallback_crash_dir(&config.app_name);
    if rightkit_fs::ensure_private_dir(&fallback).is_ok() && dir_is_writable(&fallback) {
        let _ = writeln!(
            std::io::stderr(),
            "rightkit-crash: crash_dir unavailable; using the private fallback folder"
        );
        return Some(fallback);
    }
    let _ = writeln!(
        std::io::stderr(),
        "rightkit-crash: crash_dir and fallback folder unavailable; no report file written"
    );
    None
}

/// Native sample (macOS) or minidump (Windows) of another process, `pid`, that the caller names:
/// a supervised helper that hangs or misbehaves while still alive. Call it before killing the
/// helper; a process that already exited cannot be sampled. It runs the same tools as
/// `capture_native` into `config.crash_dir` (or the fallback folder), under the same
/// single-flight guard, and the macOS sample text goes through the same scrub-before-publish path
/// (pattern scrub plus [`CrashConfig::redact_values`]). A Windows minidump stays unscrubbed and
/// local, as for the panic hook. Returns `None` when recording is off, `pid` is zero, the
/// platform has no capture, or the capture failed. Not subject to
/// [`CrashConfig::native_capture`], which governs only the automatic capture. Older artifacts are
/// pruned under `config.retention`.
pub fn capture_native_for_pid(config: &CrashConfig, kind: &str, pid: u32) -> Option<PathBuf> {
    if !recording_enabled() || pid == 0 {
        return None;
    }
    let dir = resolve_crash_dir(config)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let outcome = native::capture_pid_detailed(
        &dir,
        &sanitize_kind(kind),
        stamp,
        pid,
        config.redact_values.as_slice(),
    );
    if outcome.artifact.is_some() {
        retention::prune(&dir, config.retention);
    }
    outcome.artifact
}

/// Record one panic report, unless this thread is already inside a capture.
fn capture_panic(config: &CrashConfig, message: &str, location: Option<&str>) -> Option<PathBuf> {
    let _guard = ReentryGuard::enter()?;
    let thread = std::thread::current()
        .name()
        .unwrap_or("<unnamed>")
        .to_string();
    let backtrace = if config.capture_backtrace {
        std::backtrace::Backtrace::force_capture().to_string()
    } else {
        String::new()
    };
    record_report(config, "panic", message, location, &thread, &backtrace)
}

/// Write one redacted report (atomically) and optional native capture.
/// Also the entry point for non-panic crashes: a hang watchdog or a supervisor
/// that notices a sidecar died calls this with its own fixed `kind` string.
///
/// Returns `None` when recording is switched off (see [`set_recording_enabled`]) or the report
/// could not be written. The file is owner-only: mode `0600` on Unix, and on Windows a protected
/// DACL granting only the current user. It is written to a temp file and renamed into place, so a
/// destination that is a symlink is replaced, never written through.
pub fn record_report(
    config: &CrashConfig,
    kind: &str,
    message: &str,
    location: Option<&str>,
    thread: &str,
    backtrace: &str,
) -> Option<PathBuf> {
    if !recording_enabled() {
        return None;
    }
    if !config.write_file && config.sink.is_none() {
        return None;
    }
    // Only create the directory when something will be written there.
    let dir = if config.write_file || config.native_capture == NativeCapture::Auto {
        resolve_crash_dir(config)
    } else {
        None
    };
    #[cfg(test)]
    if FAIL_NEXT_WRITE.swap(false, Ordering::SeqCst) {
        panic!("injected report writer failure");
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let kind = sanitize_kind(kind);
    // Known secrets first, on the raw text, so truncation cannot leave half of one behind.
    let secrets = config.redact_values.as_slice();
    let message = redact_known(message, secrets);
    let backtrace = redact_known(backtrace, secrets);
    let thread = redact_known(thread, secrets);
    let location = location.map(|l| redact_known(l, secrets));
    let mut report = CrashReport::redacted(
        RawCrash {
            kind: &kind,
            app_name: &config.app_name,
            app_version: &config.app_version,
            build: config.build.as_deref(),
            thread: &thread,
            location: location.as_deref(),
            message: &message,
            backtrace: &backtrace,
            process_kind: config.process_kind.map(ProcessKind::as_str),
        },
        stamp,
    );
    if config.native_capture == NativeCapture::Auto {
        let outcome = dir
            .as_deref()
            .map(|d| native::capture_detailed(d, &kind, stamp, secrets))
            .unwrap_or_default();
        if let Some(path) = outcome.artifact {
            if let Some(name) = path.file_name() {
                report
                    .native_artifacts
                    .push(name.to_string_lossy().into_owned());
            }
        }
        report.native_capture_note = outcome.note;
    }
    let mut value = report.to_scrubbed_value();
    redact_known_value(&mut value, secrets);
    // File first: under `panic = "abort"` a panicking sink aborts the process, and the file record
    // must already be on disk by then. A failed file never skips the sink.
    let written = match (&dir, config.write_file) {
        (Some(dir), true) => {
            let path = dir.join(format!(
                "crash_{kind}_{stamp}_{}_{}.json",
                std::process::id(),
                next_file_seq()
            ));
            persist_report(config, dir, &path, &value)
        }
        _ => None,
    };
    if let Some(sink) = &config.sink {
        run_sink(sink, value);
    }
    written
}

/// Write the report file atomically and prune old reports.
fn persist_report(
    config: &CrashConfig,
    dir: &Path,
    path: &Path,
    value: &serde_json::Value,
) -> Option<PathBuf> {
    let bytes = serde_json::to_vec_pretty(value).ok()?;
    // Owner-only and write-then-rename: a crash while writing never leaves a truncated report
    // that the next launch would mis-parse, and no other local user can read the report.
    let options = rightkit_fs::AtomicWriteOptions {
        private: true,
        ..rightkit_fs::AtomicWriteOptions::default()
    };
    rightkit_fs::atomic_write_with_options(path, options, |file| file.write_all(&bytes)).ok()?;
    retention::prune(dir, config.retention);
    Some(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::TEST_SERIAL as SERIAL;
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::Arc;

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "rightkit-crash-hook-{label}-{}-{nonce}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn reports_in(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect()
    }

    #[test]
    fn a_report_writer_panic_does_not_disable_later_reports() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = temp_dir("writer-panic");
        let config = CrashConfig::new("test-app", "0.0.0", &dir);

        FAIL_NEXT_WRITE.store(true, Ordering::SeqCst);
        let first = catch_unwind(AssertUnwindSafe(|| {
            capture_panic(&config, "first panic", None)
        }));
        assert!(first.is_err(), "the injected writer failure must unwind");
        assert!(
            !IN_HOOK.with(|f| f.get()),
            "the re-entrancy flag must be cleared on unwind"
        );

        let second = capture_panic(&config, "second panic", None);
        assert!(second.is_some(), "a later panic must still record a report");
        assert_eq!(
            reports_in(&dir).len(),
            1,
            "exactly the second report is on disk"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_nested_capture_while_one_is_running_records_nothing() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = temp_dir("nested");
        let config = CrashConfig::new("test-app", "0.0.0", &dir);
        let outer = ReentryGuard::enter().expect("first guard");
        assert!(capture_panic(&config, "inner", None).is_none());
        drop(outer);
        assert!(capture_panic(&config, "after", None).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recording_can_be_switched_off_and_back_on_at_runtime() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = temp_dir("switch");
        let config = CrashConfig::new("test-app", "0.0.0", &dir);
        assert!(recording_enabled(), "recording is on by default");

        set_recording_enabled(false);
        assert!(record_report(&config, "panic", "off", None, "t", "bt").is_none());
        assert!(capture_panic(&config, "off", None).is_none());
        assert!(reports_in(&dir).is_empty(), "nothing is written while off");

        set_recording_enabled(true);
        assert!(record_report(&config, "panic", "on", None, "t", "bt").is_some());
        assert_eq!(reports_in(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn reports_are_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = temp_dir("mode");
        let config = CrashConfig::new("test-app", "0.0.0", &dir);
        let path = record_report(&config, "panic", "mode", None, "t", "bt").expect("written");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_reports_in_the_same_second_are_two_files() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = temp_dir("same-second");
        let config = CrashConfig::new("test-app", "0.0.0", &dir);
        let first = record_report(&config, "hang", "one", None, "t", "").expect("first");
        let second = record_report(&config, "hang", "one", None, "t", "").expect("second");
        assert_ne!(
            first, second,
            "the second report must not replace the first"
        );
        assert_eq!(reports_in(&dir).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sink_receives_the_same_redacted_value_that_is_written() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = temp_dir("sink-value");
        let received = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&received);
        let config = CrashConfig::new("test-app", "0.0.0", &dir)
            .with_sink(move |value| seen.lock().unwrap().push(value));

        let path = record_report(&config, "panic", "boom", None, "t", "bt").expect("file written");
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let sunk = received.lock().unwrap();
        assert_eq!(sunk.len(), 1, "the sink runs once per report");
        assert_eq!(sunk[0], written, "sink value equals the written report");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_panicking_sink_is_contained_and_the_next_panic_is_still_recorded() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = temp_dir("sink-panic");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let config = CrashConfig::new("test-app", "0.0.0", &dir)
            .write_report_file(false)
            .with_sink(move |_| {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("injected sink failure");
                }
            });

        // The first report's sink panics on its own thread; the call still returns.
        let first = capture_panic(&config, "first", None);
        assert!(
            first.is_none(),
            "no file is written when the file option is off"
        );
        assert!(!IN_HOOK.with(|f| f.get()), "the guard is released");

        capture_panic(&config, "second", None);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "the second panic reached the sink after the first sink panicked"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_option_off_writes_no_file_but_the_sink_still_runs() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = temp_dir("sink-only");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let config = CrashConfig::new("test-app", "0.0.0", &dir)
            .write_report_file(false)
            .with_sink(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            });
        assert!(record_report(&config, "panic", "no file", None, "t", "bt").is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(reports_in(&dir).is_empty(), "no file is written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_sink_thread_spawn_runs_the_sink_inline() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = temp_dir("spawn-fail");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let config = CrashConfig::new("test-app", "0.0.0", &dir)
            .write_report_file(false)
            .with_sink(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            });
        FAIL_NEXT_SPAWN.store(true, Ordering::SeqCst);
        capture_panic(&config, "spawn fails", None);
        assert!(!FAIL_NEXT_SPAWN.load(Ordering::SeqCst), "seam was consumed");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the sink ran inline");
        assert!(!IN_HOOK.with(|f| f.get()), "the guard is released");
    }

    #[test]
    fn sink_only_mode_creates_no_directory() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = temp_dir("no-dir");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let config = CrashConfig::new("test-app", "0.0.0", &dir)
            .write_report_file(false)
            .with_sink(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            });
        record_report(&config, "panic", "m", None, "t", "bt");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!dir.exists(), "crash_dir must not be created");
    }

    #[test]
    fn capture_backtrace_off_leaves_the_backtrace_empty() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let received = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&received);
        let off = CrashConfig::new("test-app", "0.0.0", temp_dir("bt-off"))
            .write_report_file(false)
            .capture_backtrace(false)
            .with_sink(move |value| seen.lock().unwrap().push(value));
        capture_panic(&off, "no bt", None);
        let seen = Arc::clone(&received);
        let on = CrashConfig::new("test-app", "0.0.0", temp_dir("bt-on"))
            .write_report_file(false)
            .with_sink(move |value| seen.lock().unwrap().push(value));
        capture_panic(&on, "bt", None);
        let got = received.lock().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0]["backtrace"], "");
        assert_ne!(got[1]["backtrace"], "", "default still captures");
    }

    #[test]
    fn the_file_is_on_disk_before_the_sink_runs() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = temp_dir("order");
        let count_at_sink = Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
        let slot = Arc::clone(&count_at_sink);
        let watch = dir.clone();
        let config = CrashConfig::new("test-app", "0.0.0", &dir)
            .with_sink(move |_| slot.store(reports_in(&watch).len(), Ordering::SeqCst));
        record_report(&config, "panic", "m", None, "t", "bt").expect("written");
        assert_eq!(count_at_sink.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_panic_on_another_thread_during_a_slow_sink_is_still_recorded() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let slow_dir = temp_dir("slow");
        let other_dir = temp_dir("other");
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let entered_tx = std::sync::Mutex::new(entered_tx);
        let release_rx = std::sync::Mutex::new(release_rx);
        let slow = CrashConfig::new("test-app", "0.0.0", &slow_dir)
            .write_report_file(false)
            .with_sink(move |_| {
                let _ = entered_tx.lock().unwrap().send(());
                let _ = release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10));
            });
        let first = std::thread::spawn(move || capture_panic(&slow, "slow", None));
        entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("sink entered");
        let other = CrashConfig::new("test-app", "0.0.0", &other_dir);
        let recorded = capture_panic(&other, "concurrent", None);
        let _ = release_tx.send(());
        first.join().expect("first thread");
        assert!(recorded.is_some(), "the concurrent panic was recorded");
        assert_eq!(reports_in(&other_dir).len(), 1);
        let _ = std::fs::remove_dir_all(&other_dir);
    }

    #[test]
    fn a_declared_process_kind_is_in_the_report_and_helper_exit_records_restart_context() {
        let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("process-kind");
        let config = CrashConfig::new("App", "1", &dir).process_kind(ProcessKind::Window);
        let path = record_report(&config, "window_error", "renderer failed", None, "main", "")
            .expect("report");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(value["process_kind"], "window");

        let supervisor = CrashConfig::new("App", "1", &dir).process_kind(ProcessKind::Engine);
        let path = record_helper_exit(&supervisor, "indexer", "signal 11", 3).expect("report");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(value["process_kind"], "helper");
        assert_eq!(value["kind"], "sidecar_crash");
        assert!(value["message"]
            .as_str()
            .unwrap()
            .contains("restarts so far: 3"));

        let plain = CrashConfig::new("App", "1", &dir);
        let path = record_report(&plain, "panic", "x", None, "main", "").expect("report");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert!(value.get("process_kind").is_none());
    }

    const KEY_A: &str = "zq-live-4F9x7TnoPatternMatches";
    const KEY_B: &str = "db-key-0xFEEDFACE-1234";

    #[test]
    fn known_secrets_are_scrubbed_from_every_field_file_and_sink() {
        let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("secrets");
        let received = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&received);
        let config = CrashConfig::new("App", "1", &dir)
            .redact_values(vec![KEY_A.into(), String::new(), KEY_B.into()])
            .with_sink(move |value| seen.lock().unwrap().push(value));
        assert!(
            !format!("{config:?}").contains(KEY_A),
            "Debug must not print secrets"
        );
        let path = record_report(
            &config,
            "panic",
            &format!("auth failed with {KEY_A} and {KEY_B}"),
            Some(&format!("src/{KEY_B}.rs:1:1")),
            &format!("worker-{KEY_A}"),
            &format!("frame key={KEY_A}\nframe db={KEY_B}"),
        )
        .expect("report");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(KEY_A) && !text.contains(KEY_B), "{text}");
        assert!(text.contains("[redacted]"));
        let sunk = received.lock().unwrap();
        let sunk = serde_json::to_string(&sunk[0]).unwrap();
        assert!(!sunk.contains(KEY_A) && !sunk.contains(KEY_B), "{sunk}");

        // Common key formats are scrubbed with no configuration at all.
        let plain = CrashConfig::new("App", "1", &dir);
        // Built at runtime so no credential-shaped literal ships in the package (the publish secret
        // scan refuses one). The AWS value is AWS's documentation example; the token is fake.
        let aws_key = ["AKIA", "IOSFODNN7EXAMPLE"].concat();
        let api_token = ["sk-ant-", "test-not-a-real-key-000000000000"].concat();
        let message =
            format!("aws {aws_key} hex 0123456789abcdef0123456789abcdef01234567 token {api_token}");
        let path = record_report(&plain, "panic", &message, None, "t", "").expect("report");
        let text = std::fs::read_to_string(&path).unwrap();
        for raw in [
            aws_key.as_str(),
            "0123456789abcdef0123456789abcdef01234567",
            api_token.as_str(),
        ] {
            assert!(!text.contains(raw), "{raw} leaked: {text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unusable_crash_dir_falls_back_to_a_private_temp_folder() {
        let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let base = temp_dir("fallback");
        std::fs::create_dir_all(&base).unwrap();
        // A regular file where the parent directory should be: crash_dir cannot be created.
        let blocker = base.join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        let app = format!("RkCrashFallbackTest{}", std::process::id());
        let config = CrashConfig::new(&app, "1", blocker.join("crash"));
        let fallback = fallback_crash_dir(&app);
        let _ = std::fs::remove_dir_all(&fallback);

        let path = record_report(&config, "panic", "boom", None, "t", "").expect("fallback report");
        assert_eq!(path.parent(), Some(fallback.as_path()));
        assert!(!blocker.join("crash").exists());
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["message"], "boom");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&fallback).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "the fallback folder is owner-only");
        }
        assert_eq!(crate::list_pending(&fallback).len(), 1);
        let _ = std::fs::remove_dir_all(&fallback);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_usable_crash_dir_is_not_replaced_by_the_fallback() {
        let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("no-fallback");
        let app = format!("RkCrashNoFallbackTest{}", std::process::id());
        let config = CrashConfig::new(&app, "1", &dir);
        let path = record_report(&config, "panic", "boom", None, "t", "").expect("report");
        assert_eq!(path.parent(), Some(dir.as_path()));
        assert!(!fallback_crash_dir(&app).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn child_capture_refuses_pid_zero_and_respects_the_recording_switch() {
        let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("child-zero");
        let config = CrashConfig::new("App", "1", &dir);
        assert!(capture_native_for_pid(&config, "hang", 0).is_none());
        set_recording_enabled(false);
        assert!(capture_native_for_pid(&config, "hang", 1234).is_none());
        set_recording_enabled(true);
        assert!(
            !dir.exists(),
            "nothing is created when the capture is refused"
        );
    }

    /// Environment-dependent: `sample` may be refused for a child under some sandboxes, so a
    /// `None` is tolerated; a kept sample must name the child and be scrubbed of known secrets.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_named_child_is_sampled_through_the_scrubbing_path_on_macos() {
        let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("child-sample");
        let config = CrashConfig::new("App", "1", &dir).redact_values(vec!["sleep".into()]);
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("8")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        let sample = capture_native_for_pid(&config, "helper hang", pid);
        let _ = child.kill();
        let _ = child.wait();
        if let Some(path) = sample {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.starts_with("sample_helperhang_"), "{name}");
            // sample_<kind>_<stamp>_<pid>_<seq>.txt (the sequence keeps same-second names apart).
            let stem = name.strip_suffix(".txt").expect("txt sample");
            let (rest, seq) = stem.rsplit_once('_').expect("sequence suffix");
            assert!(seq.parse::<u64>().is_ok(), "{name}");
            assert!(rest.ends_with(&format!("_{pid}")), "{name}");
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(!text.contains("sleep"), "known secret survived: {text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chaining_defaults_to_the_build_profile() {
        let config = CrashConfig::new("App", "1.0.0", std::env::temp_dir());
        assert_eq!(config.chain_default_hook, cfg!(debug_assertions));
        assert!(config.clone().chain_default_hook(true).chain_default_hook);
        assert!(!config.chain_default_hook(false).chain_default_hook);
    }

    #[test]
    fn non_chaining_notice_claims_a_report_only_when_one_was_recorded() {
        let recorded = non_chaining_notice("Demo", true);
        assert!(
            recorded.ends_with("A crash report was recorded."),
            "{recorded}"
        );
        assert!(!recorded.contains("could not"), "{recorded}");

        let failed = non_chaining_notice("Demo", false);
        assert!(
            failed.ends_with("A crash report could not be recorded."),
            "{failed}"
        );
        assert!(!failed.contains("was recorded"), "{failed}");
    }
}
