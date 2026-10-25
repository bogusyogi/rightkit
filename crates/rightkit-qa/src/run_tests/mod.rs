//! `rightkit-qa run-tests`: compile an app's test binaries in the broker, run them outside it.
//!
//! The broker is for compiling. A test binary that runs minutes inside `rustright cargo test` holds a
//! broker slot for all of them, and everything else queues behind it (Pulse, 2026-10-23: 55 minutes with
//! 15 builds waiting). Real-app tests also cannot run there: the broker's context has no GUI session and a
//! scrubbed environment. This command is the one shared form of what five apps hand-rolled
//! (`scripts/run-ui-tests.sh`, `postright/scripts/run-e2e.sh`, `viewright/scripts/qa-native.sh`,
//! `pulse/scripts/gate.sh` with `run-test-bins.py`):
//!
//! 1. **Compile** with `cargo test --no-run --message-format=json` through the managed client
//!    (`rustright cargo ...`) when it resolves on `PATH`, else plain cargo (a cloud host; `require_broker`
//!    refuses that), or take an existing listing (`--from-cargo-json`). The compile argv passes the same
//!    validator the journeys build uses: compile-only, no `--target-dir`.
//! 2. **Select** the test executables from the `compiler-artifact` records ([`artifacts`](crate::run_tests::artifacts)).
//! 3. **Run** each one outside any broker job, from its package directory as cargo does, with bounded
//!    parallelism, a per-binary and a total deadline, the process tree killed on a deadline, libtest
//!    arguments passed through, and environment and working directory under control (`ExecCtx`).
//! 4. **Record** one JSON receipt, `rightkit.run-tests.receipt/1` ([`receipt`](crate::run_tests::receipt)), in the managed run storage
//!    (`<managed root>/runs/<app>-run-tests/<run id>/`), with every binary's output in files beside it.
//!
//! Exit codes: 0 every binary passed; 1 a binary failed, timed out or could not start, or a run rule
//! failed; 2 the run could not be set up (including: the compile listed no test binary); 3 the compile
//! failed. The whole file set is described in `docs/run-tests.md`.
/// Terminal output that never panics: with stdout or stderr closed (`run-tests | head`), a `println!` would
/// panic inside a worker thread and take the receipt with it. (Under `cfg(test)` the print macros are used,
/// because only they are captured by the test harness.)
macro_rules! say {
    ($($arg:tt)*) => {{
        #[cfg(test)]
        println!($($arg)*);
        #[cfg(not(test))]
        {
            use std::io::Write as _;
            let _ = writeln!(std::io::stdout(), $($arg)*);
        }
    }};
}

macro_rules! shout {
    ($($arg:tt)*) => {{
        #[cfg(test)]
        eprintln!($($arg)*);
        #[cfg(not(test))]
        {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), $($arg)*);
        }
    }};
}

pub mod artifacts;
pub mod cli;
mod exec;
pub mod libtest;
mod lines;
pub mod receipt;

use crate::config::RunTestsConfig;
use crate::journeys::build_record::{parse_request_id, resolve_broker, validate_build_command};
use crate::retention;
use crate::storage::{self, RunOutcome};
use crate::util::{canonical_plain, err, now_iso, Error, Result};
use artifacts::{parse_listing, TestBinary};
use exec::{run_to_files, ProcSpec};
use receipt::{
    judge, BinaryRecord, CompileRecord, HostInfo, Receipt, Rules, SettingsRecord, Status,
    SurvivorRecord, Totals, Verdict, EXIT_COMPILE, EXIT_SETUP, RECEIPT_SCHEMA,
};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Run storage uses `<app>-run-tests` as the app key, so the retention of these runs (newest pass per app
/// and kind, every failure) never evicts the app's journeys receipt.
pub const STORAGE_SUFFIX: &str = "-run-tests";
/// The cargo JSON listing, kept in the run directory.
pub const LISTING_FILE: &str = "listing.jsonl";
/// The compile's stderr (cargo's human output), kept in the run directory.
pub const COMPILE_STDERR_FILE: &str = "compile.stderr.log";
/// Per-binary stdout and stderr files live here.
pub const LOGS_DIR: &str = "logs";
/// A failed binary's output is shown to the terminal this many lines at most.
const FAILURE_TAIL_LINES: usize = 40;
const FAILURE_TAIL_BYTES: u64 = 16 * 1024;
const TMP_ROOT_PREFIX: &str = "rkt-";

/// Which compiler front end builds the test executables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildWith {
    /// `rustright cargo ...` when `rustright` resolves on `PATH`, else plain cargo (never when
    /// `require_broker` is set).
    Auto,
    /// `rustright cargo ...` only; a host without it fails.
    Rustright,
    /// Plain cargo. Refused when `require_broker` is set.
    Cargo,
}

impl BuildWith {
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "auto" => Ok(BuildWith::Auto),
            "rustright" => Ok(BuildWith::Rustright),
            "cargo" => Ok(BuildWith::Cargo),
            other => err(format!(
                "--build-with {other:?}: expected auto, rustright or cargo"
            )),
        }
    }
}

/// Where each test binary runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CwdMode {
    /// The package directory (the directory of the manifest that owns the target), as cargo does.
    Package,
    /// The app root.
    Workspace,
    /// A directory (relative to the app root unless absolute).
    Path(PathBuf),
}

impl CwdMode {
    pub fn parse(text: &str) -> Self {
        match text.trim() {
            "package" => CwdMode::Package,
            "workspace" => CwdMode::Workspace,
            other => CwdMode::Path(PathBuf::from(other)),
        }
    }

    pub fn as_text(&self) -> String {
        match self {
            CwdMode::Package => "package".to_string(),
            CwdMode::Workspace => "workspace".to_string(),
            CwdMode::Path(p) => p.display().to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListingSource {
    File(PathBuf),
    Stdin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Compile with `cargo test --no-run --message-format=json <cargo_args>`.
    Compile {
        cargo_args: Vec<String>,
        build_with: BuildWith,
    },
    /// Take the executables from an existing cargo JSON listing; nothing is compiled.
    Listing(ListingSource),
}

/// The merged settings of a run (command line over config file over defaults).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub parallel: usize,
    pub binary_timeout: Duration,
    pub total_timeout: Duration,
    pub compile_timeout: Duration,
    pub cwd: CwdMode,
    pub env: BTreeMap<String, String>,
    pub unset_env: Vec<String>,
    pub isolate_tmp: bool,
    pub deny_ignored: bool,
    pub allow_empty: bool,
    pub require_broker: bool,
    pub show_output: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            parallel: RunTestsConfig::DEFAULT_PARALLEL,
            binary_timeout: Duration::from_secs(RunTestsConfig::DEFAULT_BINARY_TIMEOUT_S),
            total_timeout: Duration::from_secs(RunTestsConfig::DEFAULT_TOTAL_TIMEOUT_S),
            compile_timeout: Duration::from_secs(RunTestsConfig::DEFAULT_COMPILE_TIMEOUT_S),
            cwd: CwdMode::Package,
            env: BTreeMap::new(),
            unset_env: RunTestsConfig::DEFAULT_UNSET_ENV
                .iter()
                .map(|s| s.to_string())
                .collect(),
            isolate_tmp: false,
            deny_ignored: false,
            allow_empty: false,
            require_broker: false,
            show_output: false,
        }
    }
}

impl Settings {
    fn record(&self, test_args: &[String], config: Option<&Path>) -> SettingsRecord {
        SettingsRecord {
            parallel: self.parallel,
            binary_timeout_s: self.binary_timeout.as_secs(),
            total_timeout_s: self.total_timeout.as_secs(),
            compile_timeout_s: self.compile_timeout.as_secs(),
            test_args: test_args.to_vec(),
            cwd: self.cwd.as_text(),
            env_keys: self.env.keys().cloned().collect(),
            unset_env: self.unset_env.clone(),
            isolate_tmp: self.isolate_tmp,
            deny_ignored: self.deny_ignored,
            allow_empty: self.allow_empty,
            require_broker: self.require_broker,
            config: config.map(|p| p.display().to_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// The app root: where cargo runs, and where `cwd = "workspace"` points.
    pub root: PathBuf,
    /// The app key (`genright`). Names the run storage as `<app>-run-tests`.
    pub app: String,
    /// The managed root; `None` means `RIGHTKIT_MANAGED_ROOT` or the platform default.
    pub managed_root: Option<PathBuf>,
    pub run_id: Option<String>,
    pub source: Source,
    /// Libtest arguments passed to every binary (`--skip x`, `--exact`, a filter, `--nocapture`).
    pub test_args: Vec<String>,
    pub settings: Settings,
    /// Also copy the receipt here.
    pub receipt_copy: Option<PathBuf>,
    /// The config file the defaults came from, for the receipt.
    pub config_path: Option<PathBuf>,
}

/// What a run produced.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub exit_code: i32,
    pub receipt_path: PathBuf,
    pub run_dir: PathBuf,
    pub receipt: Receipt,
}

// ------------------------------------------------------------------------------ the compile

/// How the compile is spawned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilePlan {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// True for `rustright cargo ...`.
    pub managed: bool,
    /// Why the route is not the managed one, when it is not.
    pub note: Option<String>,
}

impl CompilePlan {
    pub fn argv(&self) -> Vec<String> {
        std::iter::once(self.program.display().to_string())
            .chain(self.args.iter().cloned())
            .collect()
    }
}

/// Choose and validate the compile command. The words go through
/// [`validate_build_command`] as `rustright cargo test <cargo_args> --no-run`: compile-only, JSON
/// messages, no `--target-dir`, `--out-dir` or `--artifact-dir`. `path_var` is the `PATH` to search for
/// `rustright`; `cargo_program` is the plain cargo to use (the `CARGO` variable, else `cargo`).
pub fn plan_compile(
    cargo_args: &[String],
    build_with: BuildWith,
    require_broker: bool,
    path_var: &std::ffi::OsStr,
    windows: bool,
    cargo_program: Option<&str>,
) -> Result<CompilePlan> {
    let mut words: Vec<String> = vec!["rustright".into(), "cargo".into(), "test".into()];
    words.extend(cargo_args.iter().filter(|a| *a != "--no-run").cloned());
    words.push("--no-run".into());
    let validated = validate_build_command(&words).map_err(|e| Error(format!("run-tests: {e}")))?;
    let managed = |program: PathBuf| CompilePlan {
        program,
        args: validated[1..].to_vec(),
        managed: true,
        note: None,
    };
    let plain = |note: &str| CompilePlan {
        program: PathBuf::from(cargo_program.unwrap_or("cargo")),
        args: validated[2..].to_vec(),
        managed: false,
        note: Some(note.to_string()),
    };
    let broker = resolve_broker("rustright", path_var, windows);
    match build_with {
        BuildWith::Cargo => {
            if require_broker {
                return err("run-tests: plain cargo is refused: require_broker is set, so this host compiles through the broker (`rustright cargo`) only");
            }
            Ok(plain("plain cargo was requested with --build-with cargo"))
        }
        BuildWith::Rustright => Ok(managed(broker.map_err(|e| Error(format!("run-tests: {e}")))?)),
        BuildWith::Auto => match broker {
            Ok(program) => Ok(managed(program)),
            Err(e) if require_broker => err(format!(
                "run-tests: require_broker is set and the broker is not available: {e}"
            )),
            Err(_) => Ok(plain(
                "`rustright` does not resolve on PATH, so plain cargo compiled (no broker on this host)",
            )),
        },
    }
}

// ------------------------------------------------------------------------------ execution

/// What the execution phase needs.
pub(crate) struct ExecCtx<'a> {
    pub root: &'a Path,
    pub logs: &'a Path,
    pub settings: &'a Settings,
    pub test_args: &'a [String],
    /// Parent of each binary's private temp directory (`isolate_tmp`).
    pub tmp_base: PathBuf,
    pub phase_start: Instant,
    pub total_deadline: Instant,
    /// Serialises terminal output so parallel binaries never interleave a line.
    pub print: Mutex<()>,
}

fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn log_stem(index: usize, bin: &TestBinary) -> String {
    let mut name = sanitize(&format!("{}-{}", bin.package, bin.target));
    name.truncate(80);
    format!("{index:03}-{name}")
}

fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// The last `max_bytes` of a file, cut to whole lines.
fn read_tail(path: &Path, max_bytes: u64) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max_bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = file.take(max_bytes).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        text.split_once('\n')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or(text)
    } else {
        text
    }
}

fn copy_to_terminal(path: &Path, to_stderr: bool) {
    let Ok(mut file) = File::open(path) else {
        return;
    };
    let _ = if to_stderr {
        std::io::copy(&mut file, &mut std::io::stderr().lock())
    } else {
        std::io::copy(&mut file, &mut std::io::stdout().lock())
    };
}

/// How a binary's end is classified. A binary that exited 0 but whose own summary reports failed
/// tests did not pass: a custom harness that returns 0 over a red summary is a false clean.
pub(crate) fn classify(
    timed_out: bool,
    code: Option<i32>,
    summary: Option<&libtest::LibtestSummary>,
) -> (Status, Option<String>) {
    if timed_out {
        return (Status::TimedOut, None);
    }
    if code != Some(0) {
        return (Status::Failed, None);
    }
    match summary {
        Some(s) if s.failed > 0 => (
            Status::Failed,
            Some(format!(
                "exited 0 but its summary reports {} failed test(s)",
                s.failed
            )),
        ),
        _ => (Status::Passed, None),
    }
}

/// `exit=0`, `exit=101`, `exit=timeout`, `exit=signal-9`, `exit=error` or `exit=not-run`.
fn exit_word(rec: &BinaryRecord) -> String {
    match rec.status {
        Status::TimedOut => "timeout".to_string(),
        Status::Error => "error".to_string(),
        Status::NotRun => "not-run".to_string(),
        _ => match (rec.exit, rec.signal) {
            (Some(code), _) => code.to_string(),
            (None, Some(sig)) => format!("signal-{sig}"),
            (None, None) => "unknown".to_string(),
        },
    }
}

/// The one-line summary printed for each binary:
/// `run-tests: fx::ui exit=0 passed=3 failed=0 ignored=1 12.3s`.
pub fn summary_line(rec: &BinaryRecord) -> String {
    let mut line = format!("run-tests: {} exit={}", rec.label(), exit_word(rec));
    if let Some(s) = &rec.summary {
        line.push_str(&format!(
            " passed={} failed={} ignored={}",
            s.passed, s.failed, s.ignored
        ));
    }
    if let Some(ms) = rec.duration_ms {
        line.push_str(&format!(" {:.1}s", ms as f64 / 1000.0));
    }
    line
}

fn run_one(ctx: &ExecCtx, index: usize, bin: &TestBinary) -> BinaryRecord {
    let settings = ctx.settings;
    let cwd = match &settings.cwd {
        CwdMode::Package if !bin.manifest_dir.as_os_str().is_empty() => bin.manifest_dir.clone(),
        CwdMode::Package | CwdMode::Workspace => ctx.root.to_path_buf(),
        CwdMode::Path(p) if p.is_absolute() => p.clone(),
        CwdMode::Path(p) => ctx.root.join(p),
    };
    let exe = if bin.executable.is_absolute() {
        bin.executable.clone()
    } else {
        ctx.root.join(&bin.executable)
    };
    let mut rec = BinaryRecord::pending(index, bin, ctx.test_args, &cwd);
    rec.binary = exe.display().to_string();
    let now = Instant::now();
    if now >= ctx.total_deadline {
        rec.error = Some(format!(
            "the total deadline ({}s) passed before it started",
            settings.total_timeout.as_secs()
        ));
        let _g = ctx.print.lock().unwrap();
        say!("{}", summary_line(&rec));
        return rec;
    }
    let remaining = ctx.total_deadline - now;
    let (timeout, kind) = if remaining < settings.binary_timeout {
        (remaining, "total")
    } else {
        (settings.binary_timeout, "binary")
    };
    rec.timeout_s = Some(timeout.as_secs_f64().ceil() as u64);
    rec.timeout_kind = Some(kind.to_string());
    if !exe.is_file() {
        rec.status = Status::Error;
        rec.error = Some(
            "the test executable is missing (was the broker's target directory cleaned since the compile?)"
                .to_string(),
        );
        let _g = ctx.print.lock().unwrap();
        say!("{}", summary_line(&rec));
        shout!(
            "run-tests: {}: {}",
            rec.label(),
            rec.error.as_deref().unwrap_or("")
        );
        return rec;
    }
    rec.sha256 = rightkit_fs::digest::sha256_file(&exe).ok();
    let stem = log_stem(index, bin);
    let stdout = ctx.logs.join(format!("{stem}.stdout.log"));
    let stderr = ctx.logs.join(format!("{stem}.stderr.log"));
    rec.stdout = Some(stdout.display().to_string());
    rec.stderr = Some(stderr.display().to_string());

    // The variables cargo gives a test process that tests read at run time, then the isolated temp
    // directory, then the caller's own: the caller wins.
    let mut env: Vec<(String, String)> = vec![("CARGO_PKG_NAME".into(), bin.package.clone())];
    if !bin.manifest_dir.as_os_str().is_empty() {
        env.push((
            "CARGO_MANIFEST_DIR".into(),
            bin.manifest_dir.display().to_string(),
        ));
    }
    let tmpdir = if settings.isolate_tmp {
        let dir = ctx.tmp_base.join(index.to_string());
        match std::fs::create_dir_all(&dir) {
            Ok(()) => {
                let text = dir.display().to_string();
                if cfg!(windows) {
                    env.push(("TEMP".into(), text.clone()));
                    env.push(("TMP".into(), text.clone()));
                } else {
                    env.push(("TMPDIR".into(), text.clone()));
                }
                rec.tmpdir = Some(text);
                Some(dir)
            }
            Err(e) => {
                rec.status = Status::Error;
                rec.error = Some(format!(
                    "cannot create the temp directory {}: {e}",
                    dir.display()
                ));
                let _g = ctx.print.lock().unwrap();
                say!("{}", summary_line(&rec));
                return rec;
            }
        }
    } else {
        None
    };
    env.extend(settings.env.iter().map(|(k, v)| (k.clone(), v.clone())));

    {
        let _g = ctx.print.lock().unwrap();
        say!("     Running {} ({})", bin.label(), exe.display());
    }
    rec.started_after_ms = Some(ctx.phase_start.elapsed().as_millis() as u64);
    let spec = ProcSpec {
        program: exe,
        args: ctx.test_args.to_vec(),
        cwd,
        env,
        unset_env: settings.unset_env.clone(),
        stdout: stdout.clone(),
        stderr: stderr.clone(),
        timeout,
        label: bin.label(),
    };
    match run_to_files(&spec) {
        Err(e) => {
            rec.status = Status::Error;
            rec.error = Some(e.0);
        }
        Ok(o) => {
            rec.exit = o.code;
            rec.signal = o.signal;
            rec.timed_out = o.timed_out;
            rec.duration_ms = Some(o.elapsed.as_millis() as u64);
            rec.survivors = o
                .survivors
                .iter()
                .map(|s| SurvivorRecord {
                    pid: s.pid,
                    name: s.name.clone(),
                })
                .collect();
            if let Ok(file) = File::open(&stdout) {
                if let Ok(parsed) = libtest::parse_stream(BufReader::new(file)) {
                    rec.summary = parsed.summary;
                    rec.failed_tests = parsed.failed_tests;
                }
            }
            let (status, note) = classify(o.timed_out, o.code, rec.summary.as_ref());
            rec.status = status;
            rec.error = note;
        }
    }
    rec.stdout_bytes = std::fs::metadata(&stdout).ok().map(|m| m.len());
    rec.stderr_bytes = std::fs::metadata(&stderr).ok().map(|m| m.len());
    if let Some(dir) = tmpdir {
        let _ = std::fs::remove_dir_all(dir);
    }
    report_binary(ctx, &rec, &stdout, &stderr);
    rec
}

/// Print the binary's summary line, and for a binary that did not pass the failed tests and the end of
/// its output. `show_output` prints everything.
fn report_binary(ctx: &ExecCtx, rec: &BinaryRecord, stdout: &Path, stderr: &Path) {
    let _g = ctx.print.lock().unwrap();
    say!("{}", summary_line(rec));
    if ctx.settings.show_output {
        copy_to_terminal(stdout, false);
        copy_to_terminal(stderr, true);
    }
    if rec.passed() {
        for s in &rec.survivors {
            shout!(
                "run-tests: {}: left pid {} running ({}); it was killed",
                rec.label(),
                s.pid,
                s.name
            );
        }
        return;
    }
    if let Some(note) = &rec.error {
        shout!("run-tests: {}: {note}", rec.label());
    }
    for name in rec.failed_tests.iter().take(20) {
        shout!("run-tests: {}: FAILED {name}", rec.label());
    }
    if !ctx.settings.show_output {
        for (what, path) in [("stdout", stdout), ("stderr", stderr)] {
            let tail = tail_lines(&read_tail(path, FAILURE_TAIL_BYTES), FAILURE_TAIL_LINES);
            if !tail.trim().is_empty() {
                shout!(
                    "--- {} {what} (last lines; full log: {}) ---\n{tail}",
                    rec.label(),
                    path.display()
                );
            }
        }
    }
}

/// Run every binary, `settings.parallel` at a time, and return one record per binary in listing order.
pub(crate) fn execute(ctx: &ExecCtx, bins: &[TestBinary]) -> Vec<BinaryRecord> {
    let n = bins.len();
    let workers = ctx.settings.parallel.clamp(1, n.max(1));
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<BinaryRecord>>> = Mutex::new((0..n).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= n {
                    break;
                }
                let rec = run_one(ctx, i, &bins[i]);
                results.lock().unwrap()[i] = Some(rec);
            });
        }
    });
    results
        .into_inner()
        .unwrap()
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            r.unwrap_or_else(|| {
                let mut rec = BinaryRecord::pending(i, &bins[i], ctx.test_args, ctx.root);
                rec.error = Some("a worker stopped before it ran this binary".to_string());
                rec
            })
        })
        .collect()
}

// ------------------------------------------------------------------------------ the run

struct Driven {
    compile: CompileRecord,
    binaries: Vec<BinaryRecord>,
    verdict: Verdict,
}

fn display_command(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.contains(' ') {
                format!("{a:?}")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The compile: spawn the plan, listing to `listing.jsonl`, stderr to `compile.stderr.log`. `Ok(true)`
/// when the compile exited 0 in time.
fn compile(
    plan: &CompilePlan,
    opts: &Options,
    root: &Path,
    run_dir: &Path,
    d: &mut Driven,
) -> Result<bool> {
    let stderr_path = run_dir.join(COMPILE_STDERR_FILE);
    let spec = ProcSpec {
        program: plan.program.clone(),
        args: plan.args.clone(),
        cwd: root.to_path_buf(),
        env: vec![],
        unset_env: vec![],
        stdout: run_dir.join(LISTING_FILE),
        stderr: stderr_path.clone(),
        timeout: opts.settings.compile_timeout,
        label: "compile".to_string(),
    };
    d.compile.managed = plan.managed;
    d.compile.program = Some(plan.program.display().to_string());
    d.compile.argv = plan.argv();
    d.compile.note = plan.note.clone();
    say!(
        "run-tests: compiling ({}): {}",
        if plan.managed {
            "broker"
        } else {
            "plain cargo"
        },
        display_command(&plan.argv())
    );
    if let Some(note) = &plan.note {
        shout!("run-tests: note: {note}");
    }
    let outcome = run_to_files(&spec)?;
    d.compile.exit = outcome.code;
    d.compile.timed_out = outcome.timed_out;
    d.compile.duration_ms = Some(outcome.elapsed.as_millis() as u64);
    if plan.managed {
        let head = read_head(&stderr_path, 256 * 1024);
        d.compile.request_id = parse_request_id(&head).ok();
    }
    if !outcome.success() {
        let tail = tail_lines(
            &read_tail(&stderr_path, FAILURE_TAIL_BYTES * 4),
            FAILURE_TAIL_LINES * 2,
        );
        if !tail.trim().is_empty() {
            shout!(
                "--- compile stderr (last lines; full log: {}) ---\n{tail}",
                stderr_path.display()
            );
        }
    } else if opts.settings.show_output {
        copy_to_terminal(&stderr_path, true);
    }
    Ok(outcome.success())
}

fn read_head(path: &Path, max_bytes: u64) -> String {
    let Ok(file) = File::open(path) else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = file.take(max_bytes).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn drive(
    opts: &Options,
    root: &Path,
    run_dir: &Path,
    plan: Option<&CompilePlan>,
    d: &mut Driven,
) -> Result<()> {
    let listing_path = run_dir.join(LISTING_FILE);
    let from_listing = matches!(opts.source, Source::Listing(_));
    match (&opts.source, plan) {
        (Source::Compile { .. }, Some(plan)) => {
            if !compile(plan, opts, root, run_dir, d)? {
                let how = if d.compile.timed_out {
                    format!(
                        "the compile timed out after {}s",
                        opts.settings.compile_timeout.as_secs()
                    )
                } else {
                    format!(
                        "the compile failed (exit {})",
                        d.compile.exit.map_or("none".to_string(), |c| c.to_string())
                    )
                };
                d.verdict = Verdict::fail(EXIT_COMPILE, how);
                d.compile.listing = Some(listing_path.display().to_string());
                return Ok(());
            }
        }
        (Source::Listing(source), _) => {
            match source {
                ListingSource::File(path) => {
                    std::fs::copy(path, &listing_path).map_err(|e| {
                        Error(format!("cannot read the listing {}: {e}", path.display()))
                    })?;
                }
                ListingSource::Stdin => {
                    let mut out = File::create(&listing_path)?;
                    std::io::copy(&mut std::io::stdin().lock(), &mut out)
                        .map_err(|e| Error(format!("cannot read the listing from stdin: {e}")))?;
                }
            }
            d.compile.argv = vec!["--from-cargo-json".to_string()];
        }
        (Source::Compile { .. }, None) => return err("run-tests: internal: no compile plan"),
    }
    d.compile.listing = Some(listing_path.display().to_string());
    d.compile.listing_sha256 = rightkit_fs::digest::sha256_file(&listing_path).ok();
    let listing = parse_listing(BufReader::new(File::open(&listing_path)?))?;
    d.compile.build_finished = listing.build_finished;
    d.compile.test_binaries = listing.binaries.len();
    match listing.build_finished {
        Some(true) => {}
        Some(false) => {
            d.verdict = Verdict::fail(
                EXIT_COMPILE,
                "cargo's build-finished record says the build failed",
            );
            return Ok(());
        }
        None if from_listing => {
            d.verdict = Verdict::fail(
                EXIT_SETUP,
                "the listing has no build-finished record: it is not a complete compile (cut short, or filtered)",
            );
            return Ok(());
        }
        None => {
            d.verdict = Verdict::fail(
                EXIT_COMPILE,
                "the compile exited 0 without a build-finished record in its listing",
            );
            return Ok(());
        }
    }
    if listing.binaries.is_empty() {
        d.verdict = Verdict::fail(
            EXIT_SETUP,
            format!(
                "the compile listed no test executable ({} artifact record(s), none of the test profile with an executable)",
                listing.artifacts
            ),
        );
        return Ok(());
    }

    let logs = run_dir.join(LOGS_DIR);
    std::fs::create_dir_all(&logs)?;
    let token = {
        let mut b = [0u8; 4];
        getrandom::fill(&mut b).map_err(|e| Error(format!("random source unavailable: {e}")))?;
        rightkit_fs::digest::hex(&b)
    };
    let s = &opts.settings;
    let phase_start = Instant::now();
    let ctx = ExecCtx {
        root,
        logs: &logs,
        settings: s,
        test_args: &opts.test_args,
        tmp_base: std::env::temp_dir().join(format!("{TMP_ROOT_PREFIX}{token}")),
        phase_start,
        total_deadline: phase_start + s.total_timeout,
        print: Mutex::new(()),
    };
    say!(
        "run-tests: {} test binar{} (parallel {}, {}s per binary, {}s total)",
        listing.binaries.len(),
        if listing.binaries.len() == 1 {
            "y"
        } else {
            "ies"
        },
        s.parallel.min(listing.binaries.len()).max(1),
        s.binary_timeout.as_secs(),
        s.total_timeout.as_secs()
    );
    d.binaries = execute(&ctx, &listing.binaries);
    if s.isolate_tmp {
        let _ = std::fs::remove_dir(&ctx.tmp_base);
    }
    d.verdict = judge(
        &d.binaries,
        Rules {
            deny_ignored: s.deny_ignored,
            allow_empty: s.allow_empty,
        },
    );
    Ok(())
}

/// Run the command. An `Err` means nothing was started (a bad argument, an unavailable broker, a run
/// directory that could not be made); every other outcome, a failed compile included, is an
/// [`Outcome`] with a receipt on disk and `exit_code` to exit with.
pub fn run(opts: &Options) -> Result<Outcome> {
    let root = canonical_plain(&opts.root).map_err(|e| {
        Error(format!(
            "run-tests: cannot resolve the app root {}: {e}",
            opts.root.display()
        ))
    })?;
    if !root.is_dir() {
        return err(format!("run-tests: {} is not a directory", root.display()));
    }
    let storage_app = format!("{}{STORAGE_SUFFIX}", opts.app);
    storage::safe_key(&storage_app, "app")?;
    let s = &opts.settings;
    // The compile is planned before anything is created: a host that must use the broker and has none
    // fails here, with no run directory left behind.
    let plan = match &opts.source {
        Source::Compile {
            cargo_args,
            build_with,
        } => {
            let path_var = std::env::var_os("PATH").unwrap_or_default();
            let cargo = std::env::var("CARGO").ok().filter(|c| !c.trim().is_empty());
            Some(plan_compile(
                cargo_args,
                *build_with,
                s.require_broker,
                &path_var,
                cfg!(windows),
                cargo.as_deref(),
            )?)
        }
        Source::Listing(_) => None,
    };
    let lease = s.compile_timeout + s.total_timeout + Duration::from_secs(600);
    let run = storage::create(
        opts.managed_root.as_deref(),
        &storage_app,
        opts.run_id.as_deref(),
        lease,
    )?;
    let started = Instant::now();
    let started_at = now_iso();
    say!(
        "run-tests: run {} ({})",
        run.marker.run_id,
        run.root.display()
    );

    let source_word = match (&opts.source, &plan) {
        (Source::Listing(_), _) => "listing",
        (_, Some(p)) if p.managed => "broker",
        _ => "cargo",
    };
    let mut driven = Driven {
        compile: CompileRecord::empty(source_word),
        binaries: vec![],
        verdict: Verdict::pass(),
    };
    if let Err(e) = drive(opts, &root, &run.root, plan.as_ref(), &mut driven) {
        driven.verdict = Verdict::fail(EXIT_SETUP, e.0);
    }

    let receipt = Receipt {
        schema: RECEIPT_SCHEMA.to_string(),
        run_id: run.marker.run_id.clone(),
        app: opts.app.clone(),
        started_at,
        finished_at: now_iso(),
        duration_ms: started.elapsed().as_millis() as u64,
        root: root.display().to_string(),
        host: HostInfo::current(),
        compile: driven.compile,
        settings: s.record(&opts.test_args, opts.config_path.as_deref()),
        totals: Totals::of(&driven.binaries),
        binaries: driven.binaries,
        verdict: driven.verdict,
    };
    let receipt_path = receipt::write(&run.root, &receipt)?;
    if let Some(copy) = &opts.receipt_copy {
        if let Some(parent) = copy.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&receipt_path, copy).map_err(|e| {
            Error(format!(
                "cannot copy the receipt to {}: {e}",
                copy.display()
            ))
        })?;
    }
    // The run is closed with its outcome, then the ordinary retention applies: every failed run is kept,
    // the newest passing run is kept, older passes go.
    let outcome = if receipt.verdict.passed {
        RunOutcome::Passed
    } else {
        RunOutcome::Failed
    };
    if let Err(e) = storage::set_run_outcome(&run.root, outcome)
        .and_then(|()| storage::finish(&run).map(|_| ()))
    {
        shout!("run-tests: cannot close the run: {e}");
    }
    if let Some(managed) = retention::managed_root_of_run(&run.root) {
        let _ = retention::prune_runs_after_job(&managed, &storage_app);
    }
    print_verdict(&receipt, &receipt_path);
    Ok(Outcome {
        exit_code: receipt.verdict.exit_code,
        receipt_path,
        run_dir: run.root,
        receipt,
    })
}

fn print_verdict(receipt: &Receipt, path: &Path) {
    let t = &receipt.totals;
    if t.binaries > 0 {
        say!(
            "run-tests: {} test binar{}: {} passed, {} failed, {} timed out, {} could not run, {} not run (tests: {} passed, {} failed, {} ignored)",
            t.binaries,
            if t.binaries == 1 { "y" } else { "ies" },
            t.passed,
            t.failed,
            t.timed_out,
            t.errors,
            t.not_run,
            t.tests.passed,
            t.tests.failed,
            t.tests.ignored
        );
    }
    if receipt.verdict.passed {
        say!("run-tests: passed");
    } else {
        for reason in &receipt.verdict.reasons {
            shout!("run-tests: FAILED: {reason}");
        }
        shout!("run-tests: exit {}", receipt.verdict.exit_code);
    }
    say!("run-tests: receipt {}", path.display());
}

#[cfg(test)]
mod tests;
