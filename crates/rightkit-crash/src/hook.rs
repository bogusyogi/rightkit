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
    /// Whether the panic hook aborts the process after recording (default `false`; see
    /// [`CrashConfig::abort_after_report`]).
    abort_after_report: bool,
    /// macOS `sample` duration in seconds (default 1; see [`CrashConfig::sample_secs`]).
    sample_secs: u8,
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
            chain_default_hook: false,
            abort_after_report: false,
            sample_secs: crate::native::DEFAULT_SAMPLE_SECS,
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

    /// Abort the process (`std::process::abort`) once the panic hook has recorded its report and
    /// run any chained hook. Default `false`: the hook returns and the panic unwinds (or aborts)
    /// as the build's panic strategy says. Set it for an app that must never continue past a
    /// panic caught by a foreign frame, with the report already on disk.
    pub fn abort_after_report(mut self, enabled: bool) -> Self {
        self.abort_after_report = enabled;
        self
    }

    /// How long the macOS native sample runs, in seconds, for the panic hook, the hang watchdog
    /// and `capture_native_for_pid`. Default 1; held to 1 to 3, because the capture is abandoned
    /// after 6 s. Ignored where there is no sample (Windows takes a minidump).
    pub fn sample_secs(mut self, secs: u8) -> Self {
        self.sample_secs = crate::native::clamp_sample_secs(secs);
        self
    }

    pub(crate) fn sample_duration(&self) -> u8 {
        self.sample_secs
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

    /// Keep any sink already set and run `extra` after it on each report (used by
    /// `init_diagnostics` to also write the report to the log).
    #[cfg(feature = "diagnostics")]
    pub(crate) fn then_sink(
        mut self,
        extra: impl Fn(&serde_json::Value) + Send + Sync + 'static,
    ) -> Self {
        let previous = self.sink.take();
        self.sink = Some(ReportSink(Arc::new(move |value: serde_json::Value| {
            if let Some(previous) = &previous {
                (previous.0)(value.clone());
            }
            extra(&value);
        })));
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
    std::thread::Builder::new()
        .name("rightkit-crash-sink".into())
        .spawn(work)
        .is_ok()
}

thread_local! {
    /// Nesting depth of live [`CaughtPanicScope`] guards on this thread (#794).
    static CAUGHT_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// RAII marker for panics the app catches on the current thread (`catch_unwind`, a join
/// handle's `JoinError` on a blocking thread). While the guard lives, the panic hook on this
/// thread records no crash report and writes no crash notice. Hold it only around the
/// `catch_unwind` call on the same thread; it does not follow a task across `.await` points.
#[derive(Debug)]
#[must_use = "the scope ends when the guard is dropped"]
pub struct CaughtPanicScope {
    _private: (),
}

impl CaughtPanicScope {
    /// Enter a caught-panic scope on the current thread.
    pub fn enter() -> Self {
        CAUGHT_DEPTH.with(|d| d.set(d.get() + 1));
        Self { _private: () }
    }
}

impl Drop for CaughtPanicScope {
    fn drop(&mut self) {
        CAUGHT_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// True while a [`CaughtPanicScope`] is live on this thread.
fn caught_scope_active() -> bool {
    CAUGHT_DEPTH.with(|d| d.get() > 0)
}

thread_local! {
    /// True while this thread is recording a panic (or running a sink for one). Per thread, so
    /// concurrent panics on different threads are each recorded, while a panic inside the hook on
    /// the same thread cannot recurse. `const` init and no destructor: access is safe in a hook.
    static IN_HOOK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runtime switch for [`record_report`]; reports are recorded unless this is `false`.
static RECORDING: AtomicBool = AtomicBool::new(true);

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
    let abort = config.abort_after_report;
    let app_name = config.app_name.clone();
    std::panic::set_hook(Box::new(move |info| {
        if caught_scope_active() {
            // The app catches this panic on this thread (#794): it is isolated work, not a crash.
            if chain {
                previous(info);
            }
            return;
        }
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
        if abort {
            std::process::abort();
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
        config.sample_secs,
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
            .map(|d| native::capture_detailed(d, &kind, stamp, secrets, config.sample_secs))
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
