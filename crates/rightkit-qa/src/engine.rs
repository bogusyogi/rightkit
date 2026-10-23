//! Black-box driving of an app's engine binary: one-shot CLI words and the
//! framed stdio sidecar protocol (newline-delimited JSON through
//! `rightkit-sidecar`). Nothing mocks the engine.
use crate::process::{run_owned, RunOptions, RunnerOwner, Tracker};
use crate::util::{err, Error, Result};
use rightkit_child_process::{OwnedChild, OwnedCommand};
use rightkit_sidecar::{read_json_line, write_json_line, FrameError};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const FRAME_LIMIT: usize = 16 * 1024 * 1024;

/// Start of the error a `Sidecar` request returns when no frame arrives in time.
const TIMEOUT_PREFIX: &str = "sidecar timeout after";
/// Frames [`Sidecar::events`] keeps unless [`Sidecar::set_event_limit`] changes it.
const DEFAULT_EVENT_LIMIT: usize = 2_048;
/// Bytes of engine stderr kept unless [`Sidecar::set_stderr_limit`] changes it.
const DEFAULT_STDERR_LIMIT: usize = 256 * 1024;
/// [`Sidecar::stderr_since`] waits this long, then again while the stderr total is still growing.
const STDERR_SETTLE: Duration = Duration::from_millis(2);
/// Most waits of [`STDERR_SETTLE`] for one read: a hard stop for an engine that never stops writing.
const STDERR_SETTLE_POLLS: u32 = 100;

/// An engine binary and how to launch it without touching real user data.
///
/// Every launch (`cli`, `run`, `start_sidecar`) must name an isolated data directory, or it is
/// refused before the process starts with a [`crate::userdata::UserDataViolation`] (rule
/// `no-isolated-data-dir`). A directory is named when the caller's `CliOptions::data_dir` reaches
/// the engine through `data_dir_env`, or `data_dir_env` is set in the launch environment, or
/// `data_dir_arg` appears in the arguments, and the directory passes
/// [`crate::userdata::check_engine_dirs`]. A `data_dir` on a target that declares neither is not
/// named: the engine would never see it.
#[derive(Debug, Clone)]
pub struct EngineTarget {
    pub binary: PathBuf,
    /// Env applied to every engine process (e.g. `GENRIGHT_NO_KEYCHAIN=1`).
    pub env: Vec<(String, String)>,
    /// Env var that receives the per-scenario data dir (e.g. `GENRIGHT_APP_DATA`).
    pub data_dir_env: Option<String>,
    /// Flag the engine takes its data dir under (e.g. `--data-dir`). Declaring it lets a caller
    /// name the isolated directory in the launch arguments; the harness only judges the value.
    pub data_dir_arg: Option<String>,
    /// Flags that select the real library (e.g. `--use-user-library`). A launch whose arguments
    /// contain one, as `flag` or `flag=value`, is refused. Empty by default; the product declares
    /// its own, this crate hard-codes none.
    pub forbidden_args: Vec<String>,
    /// Set by [`EngineTarget::allow_no_data_dir_for_refusal_probe`]; read it as a warning label.
    pub allow_no_data_dir_for_refusal_probe: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CliOptions {
    pub data_dir: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// Appended as `--input <json>` the way the engine CLI contract expects.
    pub input: Option<Value>,
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct CliResult {
    pub code: Option<i32>,
    pub json: Value,
    pub stdout: String,
    pub stderr: String,
    /// Every JSON object line the engine wrote to stderr (progress events etc.).
    pub events: Vec<Value>,
    pub timed_out: bool,
}

impl CliResult {
    pub fn to_value(&self) -> Value {
        json!({"code": self.code, "json": self.json, "stdout": self.stdout, "stderr": self.stderr, "events": self.events, "timed_out": self.timed_out})
    }
}

impl EngineTarget {
    /// A target with no environment, no data-dir variable or flag, and no deny list. Such a target
    /// cannot be launched until it declares `data_dir_env` or `data_dir_arg` (or the probe
    /// opt-out is used).
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            env: Vec::new(),
            data_dir_env: None,
            data_dir_arg: None,
            forbidden_args: Vec::new(),
            allow_no_data_dir_for_refusal_probe: false,
        }
    }

    /// Env var that receives the data dir (e.g. `GENRIGHT_APP_DATA`).
    pub fn with_data_dir_env(mut self, var: &str) -> Self {
        self.data_dir_env = Some(var.into());
        self
    }

    /// Flag the engine takes its data dir under (e.g. `--data-dir`).
    pub fn with_data_dir_arg(mut self, flag: &str) -> Self {
        self.data_dir_arg = Some(flag.into());
        self
    }

    /// Flags that select the real library; a launch carrying one is refused.
    pub fn forbid_args(mut self, flags: &[&str]) -> Self {
        self.forbidden_args = flags.iter().map(|f| (*f).to_owned()).collect();
        self
    }

    /// Let launches of this target name no data directory, ONLY to assert the engine's own
    /// refusal to run without one. Use it on a dedicated clone of the target, never on the target
    /// that runs real scenarios. A second barrier is still required: on Unix the launch
    /// environment (`CliOptions::env` or the target's `env`) must redirect `HOME` into a QA
    /// directory, and `XDG_DATA_HOME` / `XDG_CONFIG_HOME` too wherever they are set or inherited;
    /// on Windows the Known Folder API ignores the environment, so the launch is refused
    /// whatever the caller passes. See [`crate::userdata::check_engine_launch`].
    pub fn allow_no_data_dir_for_refusal_probe(mut self) -> Self {
        self.allow_no_data_dir_for_refusal_probe = true;
        self
    }

    /// The guard every launch path runs before it spawns anything.
    fn check_launch(&self, args: &[String], opts: &CliOptions) -> Result<()> {
        let env = self.env_for(opts.data_dir.as_deref(), &opts.env);
        crate::userdata::check_engine_launch(&crate::userdata::EngineLaunch {
            binary: &self.binary,
            args,
            data_dir: opts.data_dir.as_deref(),
            data_dir_env: self.data_dir_env.as_deref(),
            data_dir_arg: self.data_dir_arg.as_deref(),
            forbidden_args: &self.forbidden_args,
            env: &env,
            refusal_probe: self.allow_no_data_dir_for_refusal_probe,
        })?;
        Ok(())
    }

    /// Bind this target to one scenario's test-mode root: `RIGHTKIT_UNDER_TEST=1` and
    /// `RIGHTKIT_QA_DATA_DIR=<scenario_dir>`. They go last and replace any earlier value of the same
    /// variable, so the ordinary config env cannot unbind them. A step's own `env` is applied after
    /// this by [`EngineTarget::env_for`] and still wins, so a scenario can set or override either
    /// variable to test a refusal. The scenario directory carries the runner binding
    /// ([`crate::workspace::QaWorkspace::scenario_dir`]); the binding is not proof of the runner.
    ///
    /// A target that opted into [`EngineTarget::allow_no_data_dir_for_refusal_probe`] is left alone:
    /// the probe asserts the engine's own refusal to run without a data directory, and a bound
    /// test-mode root would hand it one.
    pub(crate) fn bind_test_mode(&mut self, scenario_dir: &Path) {
        if self.allow_no_data_dir_for_refusal_probe {
            return;
        }
        let same = |a: &str, b: &str| {
            if cfg!(windows) {
                a.eq_ignore_ascii_case(b)
            } else {
                a == b
            }
        };
        self.env
            .retain(|(k, _)| !same(k, "RIGHTKIT_UNDER_TEST") && !same(k, "RIGHTKIT_QA_DATA_DIR"));
        self.env.push(("RIGHTKIT_UNDER_TEST".into(), "1".into()));
        self.env.push((
            "RIGHTKIT_QA_DATA_DIR".into(),
            scenario_dir.to_string_lossy().into(),
        ));
    }

    /// The environment of one launch: the target's env (with a scenario's test-mode binding), the
    /// data-dir variable, then the caller's own pairs, which win. `cli`, `start_sidecar` and the
    /// launch guard all read it from here.
    pub(crate) fn env_for(
        &self,
        data_dir: Option<&Path>,
        extra: &[(String, String)],
    ) -> Vec<(String, String)> {
        let mut env = self.env.clone();
        if let (Some(var), Some(dir)) = (&self.data_dir_env, data_dir) {
            env.push((var.clone(), dir.to_string_lossy().into()));
        }
        env.extend(extra.iter().cloned());
        env
    }

    pub fn cli(&self, args: &[String], opts: &CliOptions, tracker: &Tracker) -> Result<CliResult> {
        if !self.binary.exists() {
            return err(format!(
                "engine binary not found: {}",
                self.binary.display()
            ));
        }
        self.check_launch(args, opts)?;
        let mut argv = args.to_vec();
        if let Some(input) = &opts.input {
            argv.push("--input".into());
            argv.push(input.to_string());
        }
        let out = run_owned(
            &self.binary.to_string_lossy(),
            &argv,
            &RunOptions {
                env: self.env_for(opts.data_dir.as_deref(), &opts.env),
                timeout: opts.timeout,
                label: format!("engine {}", args.join(" ")),
                ..Default::default()
            },
            tracker,
        )?;
        let json = serde_json::from_str(out.stdout.trim()).unwrap_or(Value::Null);
        let events = out
            .stderr
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with('{'))
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        Ok(CliResult {
            code: out.code,
            json,
            stdout: out.stdout,
            stderr: out.stderr,
            events,
            timed_out: out.timed_out,
        })
    }

    /// `engine run('job status', input)`: words split, non-zero exit or `json.error` fails.
    pub fn run(
        &self,
        words: &str,
        input: Value,
        opts: &CliOptions,
        tracker: &Tracker,
    ) -> Result<Value> {
        let args: Vec<String> = words.split_whitespace().map(String::from).collect();
        let r = self.cli(
            &args,
            &CliOptions {
                input: Some(input),
                ..opts.clone()
            },
            tracker,
        )?;
        if r.code != Some(0) || r.json.get("error").map(|e| !e.is_null()).unwrap_or(false) {
            return err(format!(
                "engine command failed ({:?}): {words}\nstdout: {}\nstderr: {}",
                r.code,
                r.stdout,
                crate::util::tail(&r.stderr, 2000)
            ));
        }
        Ok(r.json)
    }

    pub fn start_sidecar(&self, opts: &CliOptions, tracker: &Tracker) -> Result<Sidecar> {
        Sidecar::start(self, opts, tracker, None)
    }

    /// Like [`EngineTarget::start_sidecar`], and every byte of the engine's stderr is also appended to
    /// `spill` as it arrives (the file is created or truncated first). [`Sidecar::stderr_since`] then
    /// reads a call's stderr from it, so the in-memory limit cannot hide output.
    pub fn start_sidecar_spilling(
        &self,
        opts: &CliOptions,
        tracker: &Tracker,
        spill: &Path,
    ) -> Result<Sidecar> {
        Sidecar::start(self, opts, tracker, Some(spill))
    }
}

/// A running `--stdio` sidecar speaking
/// `{"type":"request","id":N,"method":{"method":"command","params":{"name","input"}}}`
/// and answering `{"type":"response","status":"ok|err","id":N,"result|error":...}`.
pub struct Sidecar {
    child: Option<OwnedChild>,
    stdin: Option<Box<dyn Write + Send>>,
    rx: Receiver<Value>,
    seq: u64,
    pub hello: Value,
    pub pid: u32,
    stderr: Arc<Mutex<StderrBuf>>,
    /// Frames that were neither hello nor the awaited response (events), oldest first. Once the
    /// list reaches twice the event limit (default 2,048, see [`Sidecar::set_event_limit`]) the
    /// oldest are trimmed back to the limit; [`SidecarStatus::events_dropped`] counts them.
    pub events: Vec<Value>,
    /// Frames trimmed from `events` since the sidecar started.
    events_dropped: u64,
    event_limit: usize,
    /// Id of the request being awaited, and whether a frame carrying that id has arrived. Set only
    /// while `request` waits; `push_event` updates it as each frame comes in.
    watch: Option<(u64, bool)>,
    /// Forces the old GenRight `status` frames for every command when true. `false` does not
    /// force jobs frames: the shape then comes from the hello (see [`Sidecar::set_frame_shape`]
    /// to pin the jobs contract against a legacy hello).
    pub legacy_status_frames: bool,
    /// Shape the hello announced (see [`detect_legacy_frames`]); `None` when it names neither.
    detected_legacy: Option<bool>,
    /// Sidecar-level pin set by `set_frame_shape`; beats `legacy_status_frames` and the hello.
    frame_shape: Option<bool>,
    tracker: Tracker,
    /// File holding every byte of engine stderr, when the sidecar was started with one.
    spill: Option<PathBuf>,
}

/// Runtime figures of a running sidecar, computed on call by [`Sidecar::status`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SidecarStatus {
    pub pid: u32,
    /// Requests sent so far (the frame sequence).
    pub requests_sent: u64,
    /// Frames held in [`Sidecar::events`] (neither hello nor an awaited response). Bounded: at most
    /// twice the event limit, default 2,048 frames.
    pub events_buffered: usize,
    /// Frames trimmed from [`Sidecar::events`] since the sidecar started.
    pub events_dropped: u64,
    /// Bytes of engine stderr kept in memory: the last stderr limit's worth (default 256 KiB).
    pub stderr_bytes: usize,
    /// Bytes of engine stderr trimmed from memory since the sidecar started.
    pub stderr_dropped_bytes: u64,
    /// Resident memory, CPU time and I/O counters of the harness process (the sidecar's reader
    /// threads run in it; the engine child is a separate process), or `None` when the host cannot
    /// report them. See `rightkit_child_process::ProcessUsage`.
    pub process: Option<rightkit_child_process::ProcessUsage>,
}

/// Frame shape a sidecar's hello announces. rightkit-job-queue' `Dispatcher::hello()` always carries
/// `contract_version`; GenRight's engine hello (`genright-protocol`, `EngineFrame::Hello`) carries
/// `rpc_version` and never `contract_version`. `Some(false)` is the rightkit-job-queue contract,
/// `Some(true)` the legacy GenRight frames, `None` when the hello has neither field.
pub(crate) fn detect_legacy_frames(hello: &Value) -> Option<bool> {
    if hello.get("contract_version").is_some() {
        Some(false)
    } else if hello.get("rpc_version").is_some() {
        Some(true)
    } else {
        None
    }
}

fn command_frame(name: &str, input: Value, legacy: bool) -> Value {
    if legacy {
        json!({"type": "request", "method": {"method": "command", "params": {"name": name, "input": input}}})
    } else {
        json!({"type": "request", "method": name, "params": input})
    }
}

fn command_outcome(f: &Value, legacy: bool) -> Result<Value> {
    let failed = if legacy {
        f["status"] == "err"
    } else {
        f["ok"] != json!(true)
    };
    if failed {
        return err(format!(
            "{}: {}",
            f["error"]["code"].as_str().unwrap_or("error"),
            f["error"]["message"].as_str().unwrap_or("")
        ));
    }
    Ok(f["result"].clone())
}

/// Where the frame shape of a command came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShapeSource {
    /// A per-command `Option`, `set_frame_shape`, or `legacy_status_frames = true`.
    Explicit,
    /// The hello announced it.
    Detected,
    /// The hello named neither shape, so the jobs contract was used by default.
    Fallback,
}

/// A request that got no response: the error, its id, and whether any frame carried that id.
struct Exchange {
    error: Error,
    id: u64,
    answered: bool,
}

/// Engine stderr, kept to its last `limit` bytes. Bytes before `start` were dropped; they stay in
/// `text` until that dead prefix reaches the limit, so each byte moves O(1) times on average and
/// `text` never holds more than about twice the limit.
struct StderrBuf {
    text: String,
    start: usize,
    limit: usize,
    /// Bytes dropped from the front since the sidecar started.
    dropped: u64,
    /// Bytes pushed since the sidecar started, kept or not: the offset a [`Sidecar::stderr_mark`] names.
    written: u64,
    /// Every pushed byte, appended as it arrives, so the memory limit never hides engine output from
    /// a secret scan. Dropped (and `spill_failed` set) when a write fails.
    spill: Option<std::fs::File>,
    spill_failed: bool,
}

impl StderrBuf {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            start: 0,
            limit,
            dropped: 0,
            written: 0,
            spill: None,
            spill_failed: false,
        }
    }

    fn push(&mut self, chunk: &str) {
        self.written += chunk.len() as u64;
        if let Some(file) = self.spill.as_mut() {
            if file.write_all(chunk.as_bytes()).is_err() {
                self.spill = None;
                self.spill_failed = true;
            }
        }
        self.text.push_str(chunk);
        self.trim();
    }

    fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
        self.trim();
    }

    /// Drops the oldest bytes beyond the limit. The cut moves forward to just after a newline when
    /// one sits within an eighth of the limit, so a kept line is whole where possible.
    fn trim(&mut self) {
        if self.text.len() - self.start > self.limit {
            let mut cut = self.text.len() - self.limit;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            let window_end = (cut + self.limit / 8).min(self.text.len());
            if let Some(p) = self.text.as_bytes()[cut..window_end]
                .iter()
                .position(|&b| b == b'\n')
            {
                cut += p + 1;
            }
            self.dropped += (cut - self.start) as u64;
            self.start = cut;
        }
        if self.start >= self.limit {
            self.text.drain(..self.start);
            self.start = 0;
        }
    }

    fn retained(&self) -> &str {
        &self.text[self.start..]
    }

    fn bytes(&self) -> usize {
        self.text.len() - self.start
    }

    /// The last `max` retained bytes, preceded by a marker when earlier output was dropped.
    fn tail(&self, max: usize) -> String {
        let tail = crate::util::tail(self.retained(), max);
        if self.dropped == 0 {
            tail
        } else {
            format!("[{} bytes of earlier stderr dropped]\n{tail}", self.dropped)
        }
    }
}

impl Sidecar {
    fn start(
        engine: &EngineTarget,
        opts: &CliOptions,
        tracker: &Tracker,
        spill: Option<&Path>,
    ) -> Result<Self> {
        engine.check_launch(&["--stdio".to_string()], opts)?;
        let spill_file = match spill {
            Some(path) => Some(std::fs::File::create(path).map_err(|e| {
                Error(format!(
                    "cannot create sidecar stderr file {}: {e}",
                    path.display()
                ))
            })?),
            None => None,
        };
        let mut cmd = Command::new(&engine.binary);
        cmd.arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in engine.env_for(opts.data_dir.as_deref(), &opts.env) {
            cmd.env(k, v);
        }
        let mut owned = OwnedCommand::from_command(cmd);
        owned.windows_hide();
        let mut child = owned.spawn().map_err(|e| {
            Error(format!(
                "failed to start sidecar {}: {e}",
                engine.binary.display()
            ))
        })?;
        let pid = child.id();
        tracker.register_owned(pid, "engine --stdio", RunnerOwner::Sidecar);
        let stdin = child.take_stdin();
        let stdout = child
            .take_stdout()
            .ok_or_else(|| Error("sidecar stdout unavailable".into()))?;
        let mut stderr_pipe = child.take_stderr();
        let stderr = Arc::new(Mutex::new(StderrBuf::new(DEFAULT_STDERR_LIMIT)));
        stderr.lock().unwrap().spill = spill_file;
        let sink = stderr.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            if let Some(p) = stderr_pipe.as_mut() {
                while let Ok(n) = p.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    sink.lock()
                        .unwrap()
                        .push(&String::from_utf8_lossy(&buf[..n]));
                }
            }
        });
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                match read_json_line::<_, Value>(&mut reader, &mut buf, FRAME_LIMIT) {
                    Ok(Some(v)) => {
                        if tx.send(v).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(FrameError::Io(_)) => break,
                    Err(_) => continue, // non-JSON line (log noise): skip, stay on the frame boundary
                }
            }
        });
        Self {
            child: Some(child),
            stdin: stdin.map(|s| Box::new(s) as Box<dyn Write + Send>),
            rx,
            seq: 0,
            hello: Value::Null,
            pid,
            stderr,
            events: vec![],
            events_dropped: 0,
            event_limit: DEFAULT_EVENT_LIMIT,
            watch: None,
            legacy_status_frames: false,
            detected_legacy: None,
            frame_shape: None,
            tracker: tracker.clone(),
            spill: spill.map(Path::to_path_buf),
        }
        .finish_start()
    }

    /// Reads the hello and records the frame shape it announces.
    fn finish_start(mut self) -> Result<Self> {
        self.hello = self.await_frame(Duration::from_secs(30), |f| f["type"] == "hello")?;
        self.detected_legacy = detect_legacy_frames(&self.hello);
        Ok(self)
    }

    fn await_frame(
        &mut self,
        timeout: Duration,
        mut want: impl FnMut(&Value) -> bool,
    ) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left.max(Duration::from_millis(1))) {
                Ok(f) if want(&f) => return Ok(f),
                Ok(f) => self.push_event(f),
                Err(RecvTimeoutError::Timeout) => {
                    return err(format!(
                        "{} {}ms; stderr tail: {}",
                        TIMEOUT_PREFIX,
                        timeout.as_millis(),
                        self.stderr_tail()
                    ))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return err(format!(
                        "sidecar closed its stdout; stderr tail: {}",
                        self.stderr_tail()
                    ))
                }
            }
        }
    }

    /// Buffer counts and process resource usage, read now. No polling thread runs for this.
    pub fn status(&self) -> SidecarStatus {
        let (stderr_bytes, stderr_dropped_bytes) = self
            .stderr
            .lock()
            .map_or((0, 0), |s| (s.bytes(), s.dropped));
        SidecarStatus {
            pid: self.pid,
            requests_sent: self.seq,
            events_buffered: self.events.len(),
            events_dropped: self.events_dropped,
            stderr_bytes,
            stderr_dropped_bytes,
            process: rightkit_child_process::process_usage().ok(),
        }
    }

    /// Keeps the most recent `limit` frames in [`Sidecar::events`] (default 2,048). Frames beyond
    /// the new limit are trimmed now, and the list is trimmed again once it reaches twice the limit.
    pub fn set_event_limit(&mut self, limit: usize) {
        self.event_limit = limit;
        self.drop_oldest_events(limit);
    }

    /// Keeps the last `bytes` of engine stderr in memory (default 256 KiB).
    pub fn set_stderr_limit(&mut self, bytes: usize) {
        if let Ok(mut buf) = self.stderr.lock() {
            buf.set_limit(bytes);
        }
    }

    pub fn stderr_tail(&self) -> String {
        self.stderr.lock().unwrap().tail(1500)
    }

    /// Bytes of engine stderr received so far, including what the memory limit has dropped. Pass it
    /// to [`Sidecar::stderr_since`] to read what the engine writes after this point.
    pub fn stderr_mark(&self) -> u64 {
        self.stderr.lock().map_or(0, |s| s.written)
    }

    /// The engine stderr written since `mark`. The reader thread drains the pipe on its own and the
    /// engine wrote its stderr before it answered, so the total is polled until it stops growing.
    /// With a stderr file ([`EngineTarget::start_sidecar_spilling`]) the text is complete whatever
    /// the memory limit; without one, or after a file write failed, it is the retained tail and
    /// `complete` says whether that covered everything since `mark`.
    pub fn stderr_since(&self, mark: u64) -> StderrSlice {
        let mut now = self.stderr_mark();
        for _ in 0..STDERR_SETTLE_POLLS {
            std::thread::sleep(STDERR_SETTLE);
            let again = self.stderr_mark();
            if again == now {
                break;
            }
            now = again;
        }
        let bytes = now.saturating_sub(mark);
        if bytes == 0 {
            return StderrSlice {
                text: String::new(),
                complete: true,
                bytes: 0,
            };
        }
        let spill_ok = self.stderr.lock().is_ok_and(|s| !s.spill_failed);
        if let (Some(path), true) = (&self.spill, spill_ok) {
            use std::io::{Read, Seek, SeekFrom};
            let read = std::fs::File::open(path).and_then(|mut f| {
                f.seek(SeekFrom::Start(mark))?;
                let mut buf = Vec::new();
                f.take(bytes).read_to_end(&mut buf)?;
                Ok(buf)
            });
            if let Ok(buf) = read {
                if buf.len() as u64 == bytes {
                    return StderrSlice {
                        text: String::from_utf8_lossy(&buf).into_owned(),
                        complete: true,
                        bytes,
                    };
                }
            }
        }
        let buf = self.stderr.lock().unwrap();
        let kept = buf.retained();
        match kept.get(kept.len().saturating_sub(bytes as usize)..) {
            Some(text) if bytes as usize <= kept.len() => StderrSlice {
                text: text.to_string(),
                complete: true,
                bytes,
            },
            _ => StderrSlice {
                text: kept.to_string(),
                complete: false,
                bytes,
            },
        }
    }

    /// One engine command the way the one-shot CLI answers it, so a `cli` step sees the same result
    /// shape from a long-lived engine (`code`, `json`, `stdout`, `stderr`, `events`, `timed_out`).
    ///
    /// - The body is the response's `result`, or `{"error": <the response's error>}`.
    /// - `code` is the CLI's exit code: taken from the response when it carries an integer
    ///   `exit_code` (`exit_code_source` is then `"protocol"`), otherwise ok maps to 0 and an error
    ///   to 1 (`"derived"`; the protocol cannot say 2, the usage code).
    /// - `events` are the event frames since the previous call, each unwrapped to its inner `event`
    ///   as the CLI prints it on stderr.
    /// - `stderr` is the engine's stderr since the previous call; see [`Sidecar::stderr_since`].
    ///
    /// `Ok(CliServed::NotServed)` is the dispatcher's exact refusal of the method (see
    /// [`is_unknown_method_refusal`]); a handler's own error is an answer. An `Err` is a sidecar that
    /// died, closed its pipes or did not answer in time.
    pub fn serve_cli(
        &mut self,
        method: &str,
        input: Option<&Value>,
        timeout: Duration,
    ) -> Result<CliServed> {
        let events_before = self.events.len() as u64 + self.events_dropped;
        let mark = self.stderr_mark();
        let (legacy, _) = self.resolve_shape(None);
        let params = input.cloned().unwrap_or_else(|| json!({}));
        let frame = self
            .request(command_frame(method, params, legacy), timeout)
            .map_err(|x| x.error)?;
        let failed = if legacy {
            frame["status"] == "err"
        } else {
            frame["ok"] != json!(true)
        };
        if failed && is_unknown_method_refusal(&frame, method) {
            return Ok(CliServed::NotServed);
        }
        let new_events =
            (self.events.len() as u64 + self.events_dropped).saturating_sub(events_before) as usize;
        let held = self.events.len();
        let events = self.events[held - new_events.min(held)..]
            .iter()
            .map(|f| f.get("event").cloned().unwrap_or_else(|| f.clone()))
            .collect();
        let stderr = self.stderr_since(mark);
        let json = if failed {
            let error = match frame.get("error") {
                Some(e) if e.is_object() => e.clone(),
                _ => json!({"code": "error", "message": ""}),
            };
            json!({ "error": error })
        } else {
            frame["result"].clone()
        };
        let carried = frame["exit_code"]
            .as_i64()
            .and_then(|c| i32::try_from(c).ok());
        let exit_code_source = if carried.is_some() {
            "protocol"
        } else {
            "derived"
        };
        let code = carried.unwrap_or(i32::from(failed));
        let mut text = stderr.text;
        if !stderr.complete {
            text = format!(
                "[sidecar stderr capped: {} bytes written since the previous call, last {} kept]\n{text}",
                stderr.bytes,
                text.len()
            );
        }
        Ok(CliServed::Answer(Box::new(ServedCli {
            result: CliResult {
                code: Some(code),
                stdout: format!(
                    "{}\n",
                    serde_json::to_string_pretty(&json).unwrap_or_default()
                ),
                json,
                stderr: text,
                events,
                timed_out: false,
            },
            exit_code_source,
            events_truncated: new_events > held,
            stderr_complete: stderr.complete,
        })))
    }

    /// Kill the engine's process tree now, without closing stdin first and without a grace period:
    /// a crash the scenario causes on purpose. Returns the exit code when the process was reaped.
    /// Later requests fail with `sidecar stdin closed`.
    pub fn kill(&mut self) -> Option<i32> {
        self.stdin.take();
        let mut code = None;
        if let Some(mut child) = self.child.take() {
            let (status, note) = crate::process::terminate_bounded(&mut child);
            code = status.and_then(|s| s.code());
            if let Some(note) = note {
                eprintln!("{note}");
            }
        }
        self.tracker.forget(self.pid);
        code
    }

    /// The file holding all of this sidecar's stderr, when it was started with one.
    pub fn stderr_file(&self) -> Option<&Path> {
        self.spill.as_deref()
    }

    /// Buffers a frame that is neither the hello nor an awaited response. A request waiting on
    /// its id is told about the frame here, as it arrives, so trimming never hides it.
    fn push_event(&mut self, frame: Value) {
        if let Some((id, seen)) = self.watch.as_mut() {
            if frame["id"] == json!(*id) {
                *seen = true;
            }
        }
        self.events.push(frame);
        if self.events.len() >= self.event_limit.saturating_mul(2).max(1) {
            self.drop_oldest_events(self.event_limit);
        }
    }

    /// Trims `events` to its most recent `keep` frames, counting what was removed.
    fn drop_oldest_events(&mut self, keep: usize) {
        if self.events.len() > keep {
            let excess = self.events.len() - keep;
            self.events.drain(..excess);
            self.events_dropped += excess as u64;
        }
    }

    /// Send an arbitrary frame and return the response with the same id.
    pub fn raw(&mut self, frame: Value, timeout: Duration) -> Result<Value> {
        self.request(frame, timeout).map_err(|x| x.error)
    }

    /// Writes `frame` with a fresh id and waits for the response with that id. A failure says
    /// whether any frame carrying that id arrived, so a silent engine can be told apart.
    fn request(
        &mut self,
        mut frame: Value,
        timeout: Duration,
    ) -> std::result::Result<Value, Exchange> {
        self.seq += 1;
        let id = self.seq;
        frame["id"] = json!(id);
        let written = match self.stdin.as_mut() {
            Some(stdin) => write_json_line(stdin, &frame, FRAME_LIMIT)
                .map_err(|e| Error(e.to_string()))
                .and_then(|()| stdin.flush().map_err(|e| Error(e.to_string()))),
            None => Err(Error("sidecar stdin closed".into())),
        };
        if let Err(error) = written {
            return Err(Exchange {
                error,
                id,
                answered: false,
            });
        }
        self.watch = Some((id, false));
        let waited = self.await_frame(timeout, |f| f["type"] == "response" && f["id"] == json!(id));
        let answered = self.watch.take().is_some_and(|(_, seen)| seen);
        waited.map_err(|error| Exchange {
            error,
            id,
            answered,
        })
    }

    /// The shape a command is sent in: a per-command `Option`, then the sidecar pin, then the
    /// `legacy_status_frames` field, then what the hello announced, then the jobs contract.
    fn resolve_shape(&self, per_command: Option<bool>) -> (bool, ShapeSource) {
        if let Some(legacy) = per_command.or(self.frame_shape) {
            return (legacy, ShapeSource::Explicit);
        }
        if self.legacy_status_frames {
            return (true, ShapeSource::Explicit);
        }
        match self.detected_legacy {
            Some(legacy) => (legacy, ShapeSource::Detected),
            None => (false, ShapeSource::Fallback),
        }
    }

    /// A command. Default is the rightkit-job-queue contract: `{type:"request", id, method: name,
    /// params: input}` answered by `{type:"response", id, ok, result | error:{code,message}}`.
    /// With `legacy_status_frames` the old GenRight shape is used (`method:{method:"command",
    /// params:{name,input}}`, answered with `status: ok|err`). The shape a hello announces is
    /// used when nothing is set; see `Sidecar::resolve_shape`.
    pub fn command(&mut self, name: &str, input: Value, timeout: Duration) -> Result<Value> {
        self.command_with(name, input, timeout, None)
    }

    pub fn command_with(
        &mut self,
        name: &str,
        input: Value,
        timeout: Duration,
        legacy_status_frames: Option<bool>,
    ) -> Result<Value> {
        let (legacy, source) = self.resolve_shape(legacy_status_frames);
        match self.request(command_frame(name, input, legacy), timeout) {
            Ok(f) => command_outcome(&f, legacy),
            Err(x)
                if source == ShapeSource::Fallback
                    && !x.answered
                    && x.error.0.starts_with(TIMEOUT_PREFIX) =>
            {
                err(format!(
                    "engine did not answer the frame shape that was sent for `{name}`: sent the \
                     rightkit-job-queue contract {{type:\"request\", id, method, params}} and no frame \
                     came back for request id {} within {}ms. The hello carried neither \
                     contract_version nor rpc_version, so the shape was not detected. If this engine \
                     speaks GenRight's legacy frames ({{type:\"request\", id, \
                     method:{{method:\"command\", params:{{name, input}}}}}} answered by status: \
                     ok|err), set legacy_status_frames = true on sidecar_start or on this \
                     sidecar_command. stderr tail: {}",
                    x.id,
                    timeout.as_millis(),
                    self.stderr_tail()
                ))
            }
            Err(x) => Err(x.error),
        }
    }

    /// Pins the frame shape for every command on this sidecar: `Some(true)` the legacy GenRight
    /// frames, `Some(false)` the rightkit-job-queue contract, `None` clears the pin. A per-command
    /// `Option` still wins over the pin.
    pub fn set_frame_shape(&mut self, legacy: Option<bool>) {
        self.frame_shape = legacy;
    }

    pub fn shutdown(&mut self) {
        self.seq += 1;
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = write_json_line(
                stdin,
                &json!({"type":"request","id":self.seq,"method":{"method":"shutdown"}}),
                FRAME_LIMIT,
            );
        }
    }

    /// Close stdin, give the engine a moment to exit, then kill the owned tree.
    /// Returns the exit code if it exited on its own.
    pub fn close(&mut self) -> Option<i32> {
        self.stdin.take();
        let mut code = None;
        if let Some(mut child) = self.child.take() {
            match child.wait_timeout(Duration::from_secs(5)) {
                Ok(Some(s)) => code = s.code(),
                _ => {
                    if let (_, Some(note)) = crate::process::terminate_bounded(&mut child) {
                        eprintln!("{note}");
                    }
                }
            }
        }
        self.tracker.forget(self.pid);
        code
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

// ------------------------------------------------------------------------- engine per scenario

/// Engine stderr read since a [`Sidecar::stderr_mark`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StderrSlice {
    pub text: String,
    /// False when the text is only the last part of what was written (memory limit, no stderr file).
    pub complete: bool,
    /// Bytes written since the mark.
    pub bytes: u64,
}

/// What [`Sidecar::serve_cli`] got back.
#[derive(Debug, Clone)]
pub enum CliServed {
    Answer(Box<ServedCli>),
    /// The dispatcher refused the method as unknown ([`is_unknown_method_refusal`]).
    NotServed,
}

/// One command answered by a sidecar in the shape of a one-shot CLI call.
#[derive(Debug, Clone)]
pub struct ServedCli {
    pub result: CliResult,
    /// Where `result.code` came from: `"protocol"` (the response carried `exit_code`) or `"derived"`
    /// (ok is 0, error is 1; the stdio protocol cannot say 2).
    pub exit_code_source: &'static str,
    /// More event frames arrived during the call than the sidecar keeps; `result.events` is the newest part.
    pub events_truncated: bool,
    pub stderr_complete: bool,
}

/// The dispatcher's own refusal of `method`: code `unknown_method` and exactly `unknown method: <method>`.
/// A handler's error, even with that code, names something else (an id, a sub-command) or carries
/// another code, so it is an answer and never a reason to launch a cold process.
pub fn is_unknown_method_refusal(frame: &Value, method: &str) -> bool {
    frame["error"]["code"] == "unknown_method"
        && frame["error"]["message"].as_str() == Some(&format!("unknown method: {method}"))
}

/// Why a scenario may launch an engine process for one command instead of using its sidecar (Sage
/// C3(a)). Each cold declaration names exactly one.
pub const COLD_REASONS: [&str; 7] = [
    "startup",
    "restart",
    "recovery",
    "role-change",
    // The command takes the exclusive store lock: every sidecar of the scenario is closed first.
    "store-lock",
    // The step tests argv or CLI parsing itself (a non-object `--input`, flags, several words).
    "cli-entry",
    // The command is not a method the dispatcher serves (`doctor`, `ping`, ...).
    "non-method",
];

/// `s` as one of [`COLD_REASONS`].
pub fn parse_cold_reason(s: &str) -> Result<&'static str> {
    COLD_REASONS
        .iter()
        .copied()
        .find(|r| *r == s)
        .ok_or_else(|| {
            Error(format!(
                "cold reason `{s}` is not one of: {}",
                COLD_REASONS.join(", ")
            ))
        })
}

/// How an [`EngineSession`] reaches the engine: [`TargetHost`] for a real binary, a fake in tests.
pub trait EngineHost {
    /// Start a `--stdio` sidecar (`stderr_file`: see [`EngineTarget::start_sidecar_spilling`]).
    fn start(&self, opts: &CliOptions, stderr_file: Option<&Path>) -> Result<Sidecar>;
    /// One cold process: the one-shot CLI.
    fn cold(&self, args: &[String], opts: &CliOptions) -> Result<CliResult>;
}

/// An [`EngineTarget`] and the tracker its processes register with.
pub struct TargetHost<'a> {
    pub target: &'a EngineTarget,
    pub tracker: &'a Tracker,
}

impl EngineHost for TargetHost<'_> {
    fn start(&self, opts: &CliOptions, stderr_file: Option<&Path>) -> Result<Sidecar> {
        match stderr_file {
            Some(file) => self.target.start_sidecar_spilling(opts, self.tracker, file),
            None => self.target.start_sidecar(opts, self.tracker),
        }
    }

    fn cold(&self, args: &[String], opts: &CliOptions) -> Result<CliResult> {
        self.target.cli(args, opts, self.tracker)
    }
}

/// The identity of an env set: the step's launch variables, sorted, a later value of a key winning
/// (keys are case-insensitive on Windows). The environment is read once, when the engine starts, so
/// one sidecar can serve only the steps that name the same set; a different role is a different set.
pub fn env_set_id(env: &[(String, String)]) -> String {
    let mut set: BTreeMap<String, &str> = BTreeMap::new();
    for (k, v) in env {
        let key = if cfg!(windows) {
            k.to_ascii_lowercase()
        } else {
            k.clone()
        };
        set.insert(key, v);
    }
    set.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

/// The engine processes one scenario started (ADR-0001 item 5, Sage C3(b)).
///
/// The scenario may hold one sidecar per distinct env set (a role). Starts allowed =
/// distinct env sets that started a sidecar + declared restarts (`restart`, `kill`) + declared cold
/// launches + one reopen for each sidecar a `store-lock` cold launch closed + scenario-started
/// sidecars (`sidecar_start`, each its own declaration). More starts than that fail the scenario
/// as `errored`. A start the scenario did not declare (a cold launch for an unknown method or a
/// non-object input, a plugin `spawn` with `engine: true`) still happens and still counts, is never
/// allowed, and makes the scenario `errored` by itself, whatever allowance is left over. A restart,
/// kill or store-lock closure allows its replacement start only when that start happens.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineStarts {
    /// Sidecars the session started, restarts and reopens included.
    pub sidecar_starts: u32,
    /// One-shot CLI processes, declared or not.
    pub cold_launches: u32,
    /// Sidecars the scenario started itself with `sidecar_start`.
    pub scenario_sidecars: u32,
    /// Engine processes a `start_daemon` step started (the step is its own declaration).
    pub daemon_engines: u32,
    /// Engine processes the step plugin started with `spawn` `engine: true`. Counted, never
    /// allowed: each is also recorded in `undeclared`.
    pub spawned_engines: u32,
    /// Distinct env sets that started a session sidecar.
    pub env_sets: u32,
    /// Replacement starts a `restart` or `kill` step made room for, counted when the replacement
    /// actually starts (a `kill` nobody follows up allows nothing).
    pub declared_restarts: u32,
    /// Cold launches that named a reason.
    pub declared_cold: u32,
    /// Sidecars a `store-lock` cold launch closed that then reopened (counted at the reopen).
    pub store_lock_reopens: u32,
    /// Cold starts the step plugin declared itself (`spawn` `engine: true` with a `cold_reason`),
    /// counted in `declared_cold` too. They are not in the reviewed scenario file, so they are
    /// listed apart in [`EngineStarts::plugin_declared`], each with the plugin step that made it.
    pub plugin_declared_cold: u32,
    pub plugin_declared: Vec<PluginDeclaredStart>,
    /// Starts per env set: `#<n> default`, or `#<n> env <KEYS>` (values never appear), n by first use.
    pub by_env_set: BTreeMap<String, u32>,
    pub cold_reasons: BTreeMap<String, u32>,
    /// Cold launches the scenario did not declare, each with what to declare.
    pub undeclared: Vec<String>,
    /// Sidecars that failed to start, died or did not answer. Any is an `errored` scenario.
    pub crashes: Vec<String>,
}

/// One cold engine start the step plugin declared at run time (not the scenario file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDeclaredStart {
    /// The scenario step number (1-based) the plugin call ran under; `None` outside a step (the
    /// plugin's `setup` or teardown).
    pub step: Option<usize>,
    pub cold_reason: String,
    /// What started, as the spawn named it.
    pub what: String,
}

impl PluginDeclaredStart {
    pub fn to_value(&self) -> Value {
        json!({
            "declared_by": "plugin",
            "step": self.step,
            "cold_reason": self.cold_reason,
            "what": self.what,
        })
    }
}

/// Every engine start that happened: sidecars, cold launches, scenario sidecars, daemons and
/// plugin spawns. Shared by [`EngineStarts::total`] and the receipt validator (`journeys::receipt`),
/// so the two cannot disagree on what a start is.
pub fn engine_start_total(
    sidecar_starts: u32,
    cold_launches: u32,
    scenario_sidecars: u32,
    daemon_engines: u32,
    spawned_engines: u32,
) -> u32 {
    sidecar_starts + cold_launches + scenario_sidecars + daemon_engines + spawned_engines
}

/// Starts the scenario may make: one per env set, plus declared restarts, declared cold launches,
/// store-lock reopens, scenario sidecars and daemons. Shared by [`EngineStarts::allowed`] and the
/// receipt validator, so the rule has one definition.
pub fn engine_start_allowance(
    env_sets: u32,
    declared_restarts: u32,
    declared_cold: u32,
    store_lock_reopens: u32,
    scenario_sidecars: u32,
    daemon_engines: u32,
) -> u32 {
    env_sets
        + declared_restarts
        + declared_cold
        + store_lock_reopens
        + scenario_sidecars
        + daemon_engines
}

impl EngineStarts {
    pub fn total(&self) -> u32 {
        engine_start_total(
            self.sidecar_starts,
            self.cold_launches,
            self.scenario_sidecars,
            self.daemon_engines,
            self.spawned_engines,
        )
    }

    pub fn allowed(&self) -> u32 {
        engine_start_allowance(
            self.env_sets,
            self.declared_restarts,
            self.declared_cold,
            self.store_lock_reopens,
            self.scenario_sidecars,
            self.daemon_engines,
        )
    }

    /// `Some(..)` when the scenario started more than it declared, or started anything it did not
    /// declare at all. An undeclared start is a violation by itself: unused allowance elsewhere
    /// (a declared restart whose replacement never started) cannot cover it.
    pub fn violation(&self) -> Option<String> {
        let (total, allowed) = (self.total(), self.allowed());
        if total <= allowed && self.undeclared.is_empty() {
            return None;
        }
        let undeclared = if self.undeclared.is_empty() {
            String::new()
        } else {
            format!(
                "; undeclared engine launches: {}",
                self.undeclared.join(" | ")
            )
        };
        let head = if total > allowed {
            format!("engine starts {total} > allowed {allowed}")
        } else {
            format!("undeclared engine launch (engine starts {total}, allowed {allowed})")
        };
        Some(format!(
            "{head} (env sets {} + declared restarts {} + declared cold {} + store-lock reopens {} + scenario sidecars {} + daemons {}; started {} sidecar(s), {} cold launch(es), {} plugin-spawned engine(s){undeclared})",
            self.env_sets,
            self.declared_restarts,
            self.declared_cold,
            self.store_lock_reopens,
            self.scenario_sidecars,
            self.daemon_engines,
            self.sidecar_starts,
            self.cold_launches,
            self.spawned_engines
        ))
    }

    /// Every reason the scenario is `errored` on account of its engines: crashes, then the count check.
    pub fn errored(&self) -> Vec<String> {
        let mut out = self.crashes.clone();
        out.extend(self.violation());
        out
    }

    pub fn to_value(&self) -> Value {
        json!({
            "engine_starts": self.total(),
            "allowed": self.allowed(),
            "sidecar_starts": self.sidecar_starts,
            "cold_launches": self.cold_launches,
            "scenario_sidecars": self.scenario_sidecars,
            "daemon_engines": self.daemon_engines,
            "spawned_engines": self.spawned_engines,
            "env_sets": self.env_sets,
            "declared_restarts": self.declared_restarts,
            "declared_cold": self.declared_cold,
            "store_lock_reopens": self.store_lock_reopens,
            "plugin_declared_cold": self.plugin_declared_cold,
            "plugin_declared": self
                .plugin_declared
                .iter()
                .map(PluginDeclaredStart::to_value)
                .collect::<Vec<_>>(),
            "by_env_set": self.by_env_set,
            "cold_reasons": self.cold_reasons,
            "undeclared": self.undeclared,
            "crashes": self.crashes,
        })
    }
}

/// How a call was served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Sidecar,
    Cold,
}

/// Where a `cli` call goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The scenario's sidecar for the call's env set. A call it cannot serve goes cold, undeclared.
    Session,
    /// A cold process, declared with this reason.
    Cold(&'static str),
}

/// One `cli` call: the words, the launch options (`env` picks the sidecar) and a label for messages.
pub struct CliCall<'a> {
    pub args: &'a [String],
    pub opts: &'a CliOptions,
    pub label: &'a str,
}

/// A call's result and how it was reached.
#[derive(Debug, Clone)]
pub struct Served {
    pub result: CliResult,
    pub via: Via,
    /// `"process"` for a cold call; see [`ServedCli::exit_code_source`] for a sidecar.
    pub exit_code_source: &'static str,
    pub events_truncated: bool,
    pub stderr_complete: bool,
    pub env_set: String,
    pub cold_reason: Option<&'static str>,
}

impl Served {
    /// The step result: the CLI shape plus `via`, `env_set`, `exit_code_source` and, when they apply,
    /// `cold_reason`, `stderr_complete: false` and `events_truncated: true`.
    pub fn to_value(&self) -> Value {
        let mut v = self.result.to_value();
        v["via"] = json!(match self.via {
            Via::Sidecar => "sidecar",
            Via::Cold => "cold",
        });
        v["env_set"] = json!(self.env_set);
        v["exit_code_source"] = json!(self.exit_code_source);
        if let Some(reason) = self.cold_reason {
            v["cold_reason"] = json!(reason);
        }
        if !self.stderr_complete {
            v["stderr_complete"] = json!(false);
        }
        if self.events_truncated {
            v["events_truncated"] = json!(true);
        }
        v
    }
}

/// The verdict of one parity check.
#[derive(Debug, Clone)]
pub struct ParityReport {
    pub command: String,
    /// Empty when cold and in-session agree.
    pub differences: Vec<String>,
    /// Where the in-session exit code came from (see [`ServedCli::exit_code_source`]).
    pub exit_code_source: &'static str,
    /// False when the stdio protocol cannot carry the exit code: only body and error class are compared.
    pub exit_code_compared: bool,
    pub cold: CliResult,
    pub session: CliResult,
}

impl ParityReport {
    pub fn equal(&self) -> bool {
        self.differences.is_empty()
    }

    pub fn to_value(&self) -> Value {
        let side =
            |r: &CliResult| json!({"code": r.code, "json": r.json, "events": r.events.len()});
        json!({
            "command": self.command,
            "equal": self.equal(),
            "differences": self.differences,
            "compares": if self.exit_code_compared {
                "json body, error class, exit code"
            } else {
                "json body and error class only: the stdio protocol does not carry the CLI exit code"
            },
            "exit_code_compared": self.exit_code_compared,
            "exit_code_source": self.exit_code_source,
            "cold": side(&self.cold),
            "session": side(&self.session),
        })
    }
}

/// The error class of a CLI body: `error.code` (or the string `error`), `None` for an ok body.
fn error_class(body: &Value) -> Option<String> {
    match body.get("error") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(e) => Some(e["code"].as_str().unwrap_or("error").to_string()),
    }
}

/// Differences between a cold result and an in-session one (Sage C3(c)): the JSON body, the error
/// class when both failed, and the exit code. The exit code is compared only when
/// `session_exit_code_source` is `"protocol"`; a derived code is invented from ok/error and proves
/// nothing, so it is left out and [`ParityReport::exit_code_compared`] says so.
pub fn compare_parity(
    cold: &CliResult,
    session: &CliResult,
    session_exit_code_source: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    match (error_class(&cold.json), error_class(&session.json)) {
        (None, None) => {
            if cold.json != session.json {
                out.push(format!(
                    "JSON body differs: cold {} vs in-session {}",
                    crate::util::tail(&cold.json.to_string(), 400),
                    crate::util::tail(&session.json.to_string(), 400)
                ));
            }
        }
        (Some(a), Some(b)) => {
            if a != b {
                out.push(format!(
                    "error class differs: cold `{a}` vs in-session `{b}`"
                ));
            }
        }
        (a, b) => out.push(format!(
            "outcome differs: cold {} vs in-session {}",
            a.map_or("ok".to_string(), |c| format!("error `{c}`")),
            b.map_or("ok".to_string(), |c| format!("error `{c}`"))
        )),
    }
    if session_exit_code_source == "protocol" && cold.code != session.code {
        out.push(format!(
            "exit code differs: cold {:?} vs in-session {:?}",
            cold.code, session.code
        ));
    }
    out
}

/// Why `call` cannot be answered by a sidecar at all, or `None`.
fn session_ineligible(call: &CliCall) -> Option<&'static str> {
    let [word] = call.args else {
        return Some("not exactly one method word");
    };
    if word.starts_with('-') {
        return Some("a flag, not a method");
    }
    match &call.opts.input {
        Some(i) if !i.is_object() && !i.is_null() => {
            Some("input is not a JSON object (the CLI answers 2)")
        }
        _ => None,
    }
}

struct EnvSet {
    label: String,
    sidecar_started: bool,
}

/// Why the next start of an env set is a replacement the scenario made room for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Replace {
    /// A `restart` or `kill` step closed the set's sidecar.
    Restart,
    /// A `store-lock` cold launch closed it.
    StoreLockReopen,
}

/// One scenario's engine: a `--stdio` sidecar per distinct env set, started on first use, and the
/// counter of every engine process the scenario started. The scenario owns it for its whole run and
/// stops it (see [`EngineSession::close_all`]) before survivors are recorded.
pub struct EngineSession {
    slots: BTreeMap<String, Sidecar>,
    /// Env sets whose next start is an allowed replacement (counted when it starts).
    pending: BTreeMap<String, Replace>,
    sets: BTreeMap<String, EnvSet>,
    stderr_dir: Option<PathBuf>,
    stderr_files: Vec<PathBuf>,
    starts: EngineStarts,
}

impl EngineSession {
    /// `stderr_dir`: where each sidecar keeps all of its stderr (`engine-sidecar-NN.stderr.log`).
    pub fn new(stderr_dir: Option<PathBuf>) -> Self {
        Self {
            slots: BTreeMap::new(),
            pending: BTreeMap::new(),
            sets: BTreeMap::new(),
            stderr_dir,
            stderr_files: Vec::new(),
            starts: EngineStarts::default(),
        }
    }

    pub fn starts(&self) -> &EngineStarts {
        &self.starts
    }

    /// The stderr files of every sidecar started so far; complete once [`EngineSession::close_all`] ran.
    pub fn stderr_files(&self) -> &[PathBuf] {
        &self.stderr_files
    }

    /// Sidecars running now.
    pub fn live_sidecars(&self) -> usize {
        self.slots.len()
    }

    /// The set's label, assigned on first sight.
    fn label(&mut self, id: &str, env: &[(String, String)]) -> String {
        let next = self.sets.len() + 1;
        self.sets
            .entry(id.to_string())
            .or_insert_with(|| {
                let mut keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
                keys.sort_unstable();
                keys.dedup();
                EnvSet {
                    label: if keys.is_empty() {
                        format!("#{next} default")
                    } else {
                        format!("#{next} env {}", keys.join(","))
                    },
                    sidecar_started: false,
                }
            })
            .label
            .clone()
    }

    fn count_start(&mut self, label: &str) {
        *self.starts.by_env_set.entry(label.to_string()).or_default() += 1;
    }

    /// Start the sidecar for env set `id` (counted even when the start fails).
    fn open(
        &mut self,
        host: &dyn EngineHost,
        opts: &CliOptions,
        id: &str,
        label: &str,
    ) -> Result<()> {
        self.starts.sidecar_starts += 1;
        self.count_start(label);
        // A replacement the scenario made room for is allowed now that it starts, not before.
        match self.pending.remove(id) {
            Some(Replace::Restart) => self.starts.declared_restarts += 1,
            Some(Replace::StoreLockReopen) => self.starts.store_lock_reopens += 1,
            None => {}
        }
        if let Some(set) = self.sets.get_mut(id) {
            if !set.sidecar_started {
                set.sidecar_started = true;
                self.starts.env_sets += 1;
            }
        }
        let file = self.stderr_dir.as_ref().map(|d| {
            d.join(format!(
                "engine-sidecar-{:02}.stderr.log",
                self.starts.sidecar_starts
            ))
        });
        let start = CliOptions {
            input: None,
            ..opts.clone()
        };
        let started = host.start(&start, file.as_deref());
        if let Some(f) = file.filter(|f| f.exists()) {
            self.stderr_files.push(f);
        }
        match started {
            Ok(sidecar) => {
                self.slots.insert(id.to_string(), sidecar);
                Ok(())
            }
            Err(e) => {
                self.starts.crashes.push(format!(
                    "engine sidecar for env set {label} failed to start: {}",
                    e.0
                ));
                Err(e)
            }
        }
    }

    /// One call on the sidecar for the call's env set, started on first use. `Ok(None)`: the engine's
    /// dispatcher refuses the method. An `Err` is an engine that died or went silent, recorded as a crash.
    fn session_call(&mut self, host: &dyn EngineHost, call: &CliCall) -> Result<Option<Served>> {
        let id = env_set_id(&call.opts.env);
        let label = self.label(&id, &call.opts.env);
        if !self.slots.contains_key(&id) {
            self.open(host, call.opts, &id, &label)?;
        }
        let timeout = call.opts.timeout.unwrap_or(Duration::from_secs(120));
        let answer = {
            let sidecar = self
                .slots
                .get_mut(&id)
                .ok_or_else(|| Error("engine sidecar vanished".into()))?;
            sidecar.serve_cli(&call.args[0], call.opts.input.as_ref(), timeout)
        };
        match answer {
            Ok(CliServed::Answer(a)) => {
                let a = *a;
                Ok(Some(Served {
                    result: a.result,
                    via: Via::Sidecar,
                    exit_code_source: a.exit_code_source,
                    events_truncated: a.events_truncated,
                    stderr_complete: a.stderr_complete,
                    env_set: label,
                    cold_reason: None,
                }))
            }
            Ok(CliServed::NotServed) => Ok(None),
            Err(e) => {
                // A sidecar that stopped answering is gone for good: it is not reused or retried.
                if let Some(mut dead) = self.slots.remove(&id) {
                    dead.kill();
                }
                let note = format!(
                    "{}: engine sidecar for env set {label} died or did not answer `{}`: {}",
                    call.label, call.args[0], e.0
                );
                self.starts.crashes.push(note.clone());
                Err(Error(note))
            }
        }
    }

    /// Close every sidecar (stdin closed, a moment to exit, then the tree is killed). Returns the env
    /// sets that were closed.
    fn close_slots(&mut self) -> Vec<String> {
        let mut closed = Vec::new();
        for (id, mut sidecar) in std::mem::take(&mut self.slots) {
            sidecar.close();
            closed.push(id);
        }
        closed
    }

    /// Stop everything the session started. Call it before survivors are read from the tracker.
    pub fn close_all(&mut self) {
        self.close_slots();
    }

    /// A cold process for the call. `declared` is the reason the scenario named; `None` is a launch the
    /// runner chose (the sidecar could not serve it), kept as an undeclared start with `note`.
    fn cold_launch(
        &mut self,
        host: &dyn EngineHost,
        call: &CliCall,
        declared: Option<&'static str>,
        note: Option<String>,
    ) -> Result<Served> {
        let id = env_set_id(&call.opts.env);
        let label = self.label(&id, &call.opts.env);
        if declared == Some("store-lock") {
            // The exclusive store lock refuses any other engine process on the store: the scenario's
            // sidecars go first, and the declaration covers one reopen of each.
            // each may reopen once, and the reopen is allowed only if it happens.
            for closed in self.close_slots() {
                self.pending
                    .entry(closed)
                    .or_insert(Replace::StoreLockReopen);
            }
        }
        self.starts.cold_launches += 1;
        self.count_start(&label);
        match declared {
            Some(reason) => {
                self.starts.declared_cold += 1;
                *self
                    .starts
                    .cold_reasons
                    .entry(reason.to_string())
                    .or_default() += 1;
            }
            None => self.starts.undeclared.extend(note),
        }
        let result = host.cold(call.args, call.opts)?;
        Ok(Served {
            result,
            via: Via::Cold,
            exit_code_source: "process",
            events_truncated: false,
            stderr_complete: true,
            env_set: label,
            cold_reason: declared,
        })
    }

    /// One `cli` call. [`Route::Session`] goes to the sidecar for the call's env set, and goes cold
    /// (undeclared) only when it cannot be a sidecar call at all (see `session_ineligible`) or when the
    /// dispatcher's exact unknown-method refusal came back. [`Route::Cold`] is a declared cold launch.
    pub fn cli(&mut self, host: &dyn EngineHost, call: &CliCall, route: Route) -> Result<Served> {
        let declare = "declare lifecycle = \"cold\" with cold_reason";
        match route {
            Route::Cold(reason) => self.cold_launch(host, call, Some(reason), None),
            Route::Session => {
                if let Some(why) = session_ineligible(call) {
                    let note = format!("{}: {why}; {declare} = \"cli-entry\"", call.label);
                    return self.cold_launch(host, call, None, Some(note));
                }
                match self.session_call(host, call)? {
                    Some(served) => Ok(served),
                    None => {
                        let note = format!(
                            "{}: the dispatcher refuses `{}` as an unknown method; {declare} = \"non-method\"",
                            call.label, call.args[0]
                        );
                        self.cold_launch(host, call, None, Some(note))
                    }
                }
            }
        }
    }

    /// Close the env set's sidecar if one runs and open a new one now. Declared: it allows one start,
    /// counted when the replacement starts, and only when there was a sidecar (or a killed or
    /// store-lock-closed one) to replace; a restart of a set that never ran is just its first start.
    pub fn restart(&mut self, host: &dyn EngineHost, opts: &CliOptions) -> Result<Value> {
        let id = env_set_id(&opts.env);
        let label = self.label(&id, &opts.env);
        let closed = self.slots.remove(&id).map(|mut s| (s.pid, s.close()));
        if closed.is_some() {
            self.pending.insert(id.clone(), Replace::Restart);
        }
        self.open(host, opts, &id, &label)?;
        let fresh = self
            .slots
            .get(&id)
            .ok_or_else(|| Error("engine sidecar vanished".into()))?;
        Ok(json!({
            "env_set": label,
            "closed_pid": closed.map(|(pid, _)| pid),
            "closed_exit_code": closed.and_then(|(_, code)| code),
            "pid": fresh.pid,
            "hello": fresh.hello,
        }))
    }

    /// Kill the env set's sidecar (no grace period). The engine is expected to recover on the next call
    /// to that set, which starts its replacement. Declared, so it adds one allowed start.
    pub fn kill(&mut self, opts: &CliOptions) -> Result<Value> {
        let id = env_set_id(&opts.env);
        let label = self.label(&id, &opts.env);
        let Some(mut sidecar) = self.slots.remove(&id) else {
            return err(format!(
                "kill: no sidecar is running for env set {label}; a cli step on it starts one"
            ));
        };
        let pid = sidecar.pid;
        let code = sidecar.kill();
        // Allowed when the replacement starts (the next call on this set), not now.
        self.pending.insert(id, Replace::Restart);
        Ok(json!({
            "env_set": label,
            "killed_pid": pid,
            "exit_code": code,
            "next": "the next cli step on this env set starts the replacement engine",
        }))
    }

    /// A sidecar the scenario started itself (`sidecar_start`): counted, and its own declaration.
    pub fn note_scenario_sidecar(&mut self) {
        self.starts.scenario_sidecars += 1;
    }

    /// An engine process a `start_daemon` step started (counted when the process started, even if it
    /// then failed to become ready). The step is its own declaration, so it is also allowed.
    pub fn note_daemon_engine(&mut self) {
        self.starts.daemon_engines += 1;
    }

    /// An engine process the step plugin started with `spawn` `engine: true` (`what` names the child).
    /// With `cold_reason` (one of [`COLD_REASONS`], already validated by [`parse_cold_reason`]) it is a
    /// declared cold start, counted and allowed exactly as a declared `cold_cli` is. Without one it is
    /// counted, never allowed, and recorded as undeclared, so the scenario is `errored`.
    /// `step` is the scenario step the plugin call ran under (`None` outside a step); a declared
    /// start is recorded under it as a plugin declaration, apart from the scenario's own.
    pub fn note_spawned_engine(
        &mut self,
        what: &str,
        cold_reason: Option<&'static str>,
        step: Option<usize>,
    ) {
        let Some(reason) = cold_reason else {
            self.starts.spawned_engines += 1;
            self.starts.undeclared.push(format!(
                "{what}: the step plugin started the engine with `spawn` `engine: true` and no `cold_reason`, which no scenario declaration covers"
            ));
            return;
        };
        if reason == "store-lock" {
            // As for a declared store-lock cold launch: the scenario's sidecars go first, and the
            // declaration covers one reopen of each, allowed when it happens.
            for closed in self.close_slots() {
                self.pending
                    .entry(closed)
                    .or_insert(Replace::StoreLockReopen);
            }
        }
        self.starts.cold_launches += 1;
        self.starts.declared_cold += 1;
        self.starts.plugin_declared_cold += 1;
        self.starts.plugin_declared.push(PluginDeclaredStart {
            step,
            cold_reason: reason.to_string(),
            what: what.to_string(),
        });
        *self
            .starts
            .cold_reasons
            .entry(reason.to_string())
            .or_default() += 1;
    }

    /// A plugin `spawn` of the engine that the runner refused because its `cold_reason` was not valid
    /// (nothing was started). Recorded as an engine failure, so the scenario is `errored`.
    pub fn note_engine_refusal(&mut self, what: &str) {
        self.starts.crashes.push(what.to_string());
    }

    /// Run one served method cold and in-session and compare them (Sage C3(c), see [`compare_parity`]).
    /// The cold launch is declared as `parity`. Use it on a read-only or idempotent command: it runs twice.
    /// It cannot cover a `store-lock` command, which refuses to run beside the sidecar. Errors when the
    /// dispatcher does not serve the method (there is nothing in-session to compare).
    pub fn parity(&mut self, host: &dyn EngineHost, call: &CliCall) -> Result<ParityReport> {
        if let Some(why) = session_ineligible(call) {
            return err(format!(
                "{}: parity needs one served method: {why}",
                call.label
            ));
        }
        let cold = self.cold_launch(host, call, Some("parity"), None)?;
        let Some(session) = self.session_call(host, call)? else {
            return err(format!(
                "{}: the dispatcher does not serve `{}`, so there is no in-session answer to compare",
                call.label, call.args[0]
            ));
        };
        let differences = compare_parity(&cold.result, &session.result, session.exit_code_source);
        Ok(ParityReport {
            command: call.args.join(" "),
            differences,
            exit_code_source: session.exit_code_source,
            exit_code_compared: session.exit_code_source == "protocol",
            cold: cold.result,
            session: session.result,
        })
    }
}

/// Build the engine through the managed wrapper and return the executable it
/// produced (from cargo's `compiler-artifact` message), never a stale copy.
pub fn build_engine(
    cwd: &Path,
    command: &[String],
    package: &str,
    env: &[(String, String)],
    tracker: &Tracker,
) -> Result<PathBuf> {
    let Some((program, rest)) = command.split_first() else {
        return err("engine.build is empty");
    };
    let mut args = rest.to_vec();
    if !args.iter().any(|a| a.starts_with("--message-format")) {
        args.push("--message-format=json-render-diagnostics".into());
    }
    let out = run_owned(
        program,
        &args,
        &RunOptions {
            cwd: Some(cwd.to_path_buf()),
            env: env.to_vec(),
            timeout: Some(Duration::from_secs(45 * 60)),
            label: "engine build".into(),
            ..Default::default()
        },
        tracker,
    )?;
    if out.code != Some(0) {
        return err(format!(
            "engine build failed ({:?}):\n{}",
            out.code,
            crate::util::tail(&out.stderr, 4000)
        ));
    }
    out.stdout
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == package)
        .filter_map(|m| m["executable"].as_str().map(PathBuf::from))
        .next_back()
        .ok_or_else(|| {
            Error(format!(
                "build produced no executable for package {package}"
            ))
        })
}

#[cfg(test)]
mod command_shape_tests {
    use super::*;

    #[test]
    fn jobs_contract_is_the_default_frame() {
        let f = command_frame("scan", json!({"a": 1}), false);
        assert_eq!(
            f,
            json!({"type":"request","method":"scan","params":{"a":1}})
        );
        let ok = json!({"type":"response","id":1,"ok":true,"result":{"n":2}});
        assert_eq!(command_outcome(&ok, false).unwrap(), json!({"n":2}));
        let bad =
            json!({"type":"response","id":1,"ok":false,"error":{"code":"nope","message":"m"}});
        assert_eq!(command_outcome(&bad, false).unwrap_err().0, "nope: m");
        // A response with no `ok` is not success.
        assert!(command_outcome(&json!({"type":"response","id":1}), false).is_err());
    }

    #[test]
    fn legacy_status_frames_keep_the_old_shape() {
        let f = command_frame("scan", json!({"a": 1}), true);
        assert_eq!(
            f,
            json!({"type":"request","method":{"method":"command","params":{"name":"scan","input":{"a":1}}}})
        );
        let ok = json!({"type":"response","id":1,"status":"ok","result":{"n":2}});
        assert_eq!(command_outcome(&ok, true).unwrap(), json!({"n":2}));
        let bad =
            json!({"type":"response","id":1,"status":"err","error":{"code":"c","message":"m"}});
        assert_eq!(command_outcome(&bad, true).unwrap_err().0, "c: m");
    }
}

#[cfg(test)]
mod frame_shape_tests {
    use super::*;
    use std::sync::mpsc::Sender;

    /// Bytes the sidecar wrote to its stdin, shared with the test.
    #[derive(Clone, Default)]
    struct Wire(Arc<Mutex<Vec<u8>>>);

    impl Write for Wire {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Wire {
        fn sent(&self) -> Vec<Value> {
            let bytes = self.0.lock().unwrap().clone();
            String::from_utf8(bytes)
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }
    }

    /// An in-memory sidecar. `frames` are the engine's output in order: the hello first, then
    /// any answers. The returned sender keeps the reader open, so silence is a timeout, not a
    /// closed stream.
    fn sidecar(frames: Vec<Value>) -> (Sidecar, Wire, Sender<Value>) {
        let (tx, rx) = channel();
        for f in frames {
            tx.send(f).unwrap();
        }
        let wire = Wire::default();
        let s = Sidecar {
            child: None,
            stdin: Some(Box::new(wire.clone())),
            rx,
            seq: 0,
            hello: Value::Null,
            pid: 0,
            stderr: Arc::new(Mutex::new(StderrBuf::new(DEFAULT_STDERR_LIMIT))),
            events: vec![],
            events_dropped: 0,
            event_limit: DEFAULT_EVENT_LIMIT,
            watch: None,
            legacy_status_frames: false,
            detected_legacy: None,
            frame_shape: None,
            tracker: Tracker::new(),
            spill: None,
        }
        .finish_start()
        .unwrap();
        (s, wire, tx)
    }

    fn legacy_hello() -> Value {
        json!({"type": "hello", "rpc_version": 1, "build": "0.1.0", "pid": 7})
    }
    fn jobs_hello() -> Value {
        json!({"type": "hello", "engine": "demo", "version": "1.0.0", "contract_version": 1})
    }
    /// A hello that names neither field.
    fn bare_hello() -> Value {
        json!({"type": "hello", "engine": "demo"})
    }
    fn legacy_ok(id: u64) -> Value {
        json!({"type": "response", "status": "ok", "id": id, "result": {"n": 2}})
    }
    fn jobs_ok(id: u64) -> Value {
        json!({"type": "response", "id": id, "ok": true, "result": {"n": 2}})
    }
    fn input() -> Value {
        json!({"a": 1})
    }
    fn legacy_frame(id: u64) -> Value {
        json!({"type": "request", "id": id,
               "method": {"method": "command", "params": {"name": "scan", "input": {"a": 1}}}})
    }
    fn jobs_frame(id: u64) -> Value {
        json!({"type": "request", "id": id, "method": "scan", "params": {"a": 1}})
    }
    const WAIT: Duration = Duration::from_secs(5);
    const SILENT: Duration = Duration::from_millis(50);

    #[test]
    fn detection_reads_the_hello_fields() {
        assert_eq!(detect_legacy_frames(&legacy_hello()), Some(true));
        assert_eq!(detect_legacy_frames(&jobs_hello()), Some(false));
        assert_eq!(detect_legacy_frames(&bare_hello()), None);
    }

    #[test]
    fn legacy_hello_selects_legacy_frames_without_a_flag() {
        let (mut s, wire, _tx) = sidecar(vec![legacy_hello(), legacy_ok(1)]);
        assert_eq!(s.command("scan", input(), WAIT).unwrap(), json!({"n": 2}));
        assert_eq!(wire.sent(), vec![legacy_frame(1)]);
    }

    #[test]
    fn jobs_hello_selects_contract_frames() {
        let (mut s, wire, _tx) = sidecar(vec![jobs_hello(), jobs_ok(1)]);
        assert_eq!(s.command("scan", input(), WAIT).unwrap(), json!({"n": 2}));
        assert_eq!(wire.sent(), vec![jobs_frame(1)]);
    }

    #[test]
    fn per_command_flag_beats_a_jobs_hello_both_ways() {
        let (mut s, wire, _tx) = sidecar(vec![jobs_hello(), legacy_ok(1), jobs_ok(2)]);
        s.command_with("scan", input(), WAIT, Some(true)).unwrap();
        s.command_with("scan", input(), WAIT, Some(false)).unwrap();
        assert_eq!(wire.sent(), vec![legacy_frame(1), jobs_frame(2)]);
    }

    #[test]
    fn per_command_flag_beats_a_legacy_hello() {
        let (mut s, wire, _tx) = sidecar(vec![legacy_hello(), jobs_ok(1)]);
        s.command_with("scan", input(), WAIT, Some(false)).unwrap();
        assert_eq!(wire.sent(), vec![jobs_frame(1)]);
    }

    #[test]
    fn sidecar_field_and_pin_beat_the_hello() {
        let (mut s, wire, _tx) = sidecar(vec![jobs_hello(), legacy_ok(1), jobs_ok(2)]);
        s.legacy_status_frames = true;
        s.command("scan", input(), WAIT).unwrap();
        s.set_frame_shape(Some(false));
        s.command("scan", input(), WAIT).unwrap();
        assert_eq!(wire.sent(), vec![legacy_frame(1), jobs_frame(2)]);
    }

    #[test]
    fn sidecar_pin_beats_a_legacy_hello() {
        let (mut s, wire, _tx) = sidecar(vec![legacy_hello(), jobs_ok(1)]);
        s.set_frame_shape(Some(false));
        s.command("scan", input(), WAIT).unwrap();
        assert_eq!(wire.sent(), vec![jobs_frame(1)]);
    }

    #[test]
    fn ambiguous_hello_and_silence_names_the_shape_and_the_switch() {
        let (mut s, wire, _tx) = sidecar(vec![bare_hello()]);
        let e = s.command("scan", input(), SILENT).unwrap_err().0;
        assert!(
            e.contains("did not answer the frame shape that was sent"),
            "{e}"
        );
        assert!(e.contains("{type:\"request\", id, method, params}"), "{e}");
        assert!(e.contains("legacy_status_frames"), "{e}");
        assert!(e.contains("`scan`"), "{e}");
        assert_eq!(wire.sent(), vec![jobs_frame(1)]);
    }

    #[test]
    fn ambiguous_hello_with_a_frame_for_the_id_keeps_the_plain_timeout() {
        let event = json!({"type": "event", "id": 1, "event": "progress"});
        let (mut s, _wire, _tx) = sidecar(vec![bare_hello(), event]);
        let e = s.command("scan", input(), SILENT).unwrap_err().0;
        assert!(e.starts_with("sidecar timeout"), "{e}");
        assert!(!e.contains("did not answer"), "{e}");
    }

    #[test]
    fn explicit_shape_and_silence_keep_the_plain_timeout() {
        let (mut s, _wire, _tx) = sidecar(vec![bare_hello()]);
        let e = s
            .command_with("scan", input(), SILENT, Some(false))
            .unwrap_err()
            .0;
        assert!(e.starts_with("sidecar timeout"), "{e}");
        assert!(!e.contains("did not answer"), "{e}");
    }

    /// `count` event frames; none answers request 1.
    fn events(count: usize) -> Vec<Value> {
        (0..count)
            .map(|n| json!({"type": "event", "id": 0, "n": n}))
            .collect()
    }

    #[test]
    fn a_full_ring_keeps_the_newest_events_and_the_waiter_still_gets_its_answer() {
        let mut frames = vec![bare_hello()];
        frames.extend(events(10_000));
        frames.push(jobs_ok(1));
        let (mut s, _wire, _tx) = sidecar(frames);
        let result = s.command("scan", input(), WAIT).unwrap();
        assert_eq!(result, json!({"n": 2}));
        let status = s.status();
        // Every event is either held or counted as dropped.
        assert_eq!(s.events.len() as u64 + status.events_dropped, 10_000);
        assert!(
            (DEFAULT_EVENT_LIMIT..2 * DEFAULT_EVENT_LIMIT).contains(&s.events.len()),
            "held {}",
            s.events.len()
        );
        assert_eq!(s.events.last().unwrap()["n"], json!(9_999));
        assert_eq!(status.events_buffered, s.events.len());
    }

    #[test]
    fn the_event_limit_is_configurable_and_trims_at_once() {
        let mut frames = vec![bare_hello()];
        frames.extend(events(50));
        let (mut s, _wire, _tx) = sidecar(frames);
        assert!(s.command("scan", input(), SILENT).is_err());
        assert_eq!(s.events.len(), 50);
        s.set_event_limit(5);
        assert_eq!(s.events.len(), 5);
        assert_eq!(s.events[0]["n"], json!(45));
        assert_eq!(s.status().events_dropped, 45);
    }

    #[test]
    fn an_answer_seen_before_the_ring_trimmed_it_is_still_reported_as_answered() {
        let early = json!({"type": "event", "id": 1, "event": "progress"});
        let mut frames = vec![bare_hello(), early];
        frames.extend(events(3 * DEFAULT_EVENT_LIMIT));
        let (mut s, _wire, _tx) = sidecar(frames);
        let brief = Duration::from_millis(500);
        let e = s.command("scan", input(), brief).unwrap_err().0;
        assert!(e.starts_with("sidecar timeout"), "{e}");
        assert!(!e.contains("did not answer"), "{e}");
        assert!(s.status().events_dropped > 0);
        assert!(s.events.iter().all(|f| f["id"] != json!(1)));
    }

    #[test]
    fn stderr_keeps_its_last_bytes_on_a_line_boundary_and_says_what_it_dropped() {
        let mut buf = StderrBuf::new(21);
        buf.push(&format!("{}\n{}", "a".repeat(10), "b".repeat(20)));
        // The cut lands inside the first line; the newline just after it moves the cut past that line.
        assert_eq!(buf.retained(), "b".repeat(20));
        assert_eq!(buf.dropped, 11);
        assert_eq!(buf.bytes(), 20);
        assert!(buf
            .tail(1500)
            .starts_with("[11 bytes of earlier stderr dropped]\n"));
    }

    #[test]
    fn stderr_stays_within_its_limit_however_much_is_written() {
        const LIMIT: usize = 4096;
        let mut buf = StderrBuf::new(LIMIT);
        let mut written = 0u64;
        for n in 0..20_000 {
            let line = format!("progress {n:08}\n");
            written += line.len() as u64;
            buf.push(&line);
            assert!(buf.bytes() <= LIMIT);
            assert!(buf.text.len() < 2 * LIMIT, "dead prefix was not compacted");
        }
        assert_eq!(buf.dropped + buf.bytes() as u64, written);
        assert!(buf.retained().starts_with("progress "));
        assert!(buf.retained().ends_with("progress 00019999\n"));
    }

    #[test]
    fn stderr_cuts_never_split_a_character() {
        let mut buf = StderrBuf::new(7);
        buf.push(&"é".repeat(40));
        assert!(buf.retained().chars().all(|c| c == 'é'));
        assert!(buf.bytes() <= 7);
    }
}

#[cfg(test)]
mod launch_guard_tests {
    use super::*;
    use crate::userdata::Rule;

    fn target() -> EngineTarget {
        EngineTarget::new("/nonexistent/engine")
            .with_data_dir_env("APP_DATA")
            .forbid_args(&["--use-user-library"])
    }

    fn refusal(r: Result<()>) -> String {
        r.unwrap_err().0
    }

    #[test]
    fn a_launch_without_a_data_dir_is_refused_for_cli_and_sidecar_alike() {
        let t = target();
        let words = vec!["doctor".to_string()];
        let e = refusal(t.check_launch(&words, &CliOptions::default()));
        assert!(e.contains(&Rule::NoIsolatedDataDir.to_string()), "{e}");
        assert!(e.contains("/nonexistent/engine"), "{e}");
        let e = refusal(t.check_launch(&["--stdio".to_string()], &CliOptions::default()));
        assert!(e.contains(&Rule::NoIsolatedDataDir.to_string()), "{e}");
    }

    #[test]
    fn a_data_dir_reaches_the_engine_only_through_a_declared_variable() {
        let dir = crate::storage::app_temp_dir().join("rkqa-engine-guard-never-created");
        let opts = CliOptions {
            data_dir: Some(dir),
            ..Default::default()
        };
        let words = vec!["doctor".to_string()];
        target().check_launch(&words, &opts).unwrap();
        let mut undeclared = target();
        undeclared.data_dir_env = None;
        let e = refusal(undeclared.check_launch(&words, &opts));
        assert!(e.contains(&Rule::NoIsolatedDataDir.to_string()), "{e}");
    }

    #[test]
    fn a_forbidden_flag_is_refused_whatever_the_data_dir() {
        let dir = crate::storage::app_temp_dir().join("rkqa-engine-guard-never-created");
        let opts = CliOptions {
            data_dir: Some(dir),
            ..Default::default()
        };
        let words = vec!["doctor".to_string(), "--use-user-library".to_string()];
        let e = refusal(target().check_launch(&words, &opts));
        assert!(e.contains(&Rule::ForbiddenArgument.to_string()), "{e}");
        // The target's deny list is its own: a bare target forbids nothing.
        let mut bare = target();
        bare.forbidden_args.clear();
        bare.check_launch(&words, &opts).unwrap();
    }

    #[test]
    fn the_probe_label_alone_never_opens_a_launch() {
        let t = target().allow_no_data_dir_for_refusal_probe();
        assert!(t.allow_no_data_dir_for_refusal_probe);
        // No HOME redirect in the launch environment: still refused (on Windows always).
        let e = refusal(t.check_launch(&["doctor".to_string()], &CliOptions::default()));
        assert!(e.contains(&Rule::ProbeUnavailable.to_string()), "{e}");
    }
}

/// The engine-per-scenario counter and routing against a fixture engine that lives in the test
/// process: a scripted sidecar (frames answered as its stdin is written) and a one-shot CLI that
/// answers from the same table, so cold and in-session results can be compared.
#[cfg(test)]
mod session_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::Sender;

    #[derive(Clone)]
    struct Reply {
        ok: bool,
        result: Value,
        code: &'static str,
        message: String,
        exit_code: Option<i64>,
        events: Vec<Value>,
        stderr: String,
    }

    impl Reply {
        fn ok(result: Value) -> Self {
            Self {
                ok: true,
                result,
                code: "",
                message: String::new(),
                exit_code: None,
                events: vec![],
                stderr: String::new(),
            }
        }
        fn fail(code: &'static str, message: &str) -> Self {
            Self {
                ok: false,
                result: Value::Null,
                code,
                message: message.into(),
                exit_code: None,
                events: vec![],
                stderr: String::new(),
            }
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Sidecar,
        Cold,
    }

    /// `(mode, method, params, launch env) -> reply`; `None` is an engine process that dies unanswered.
    type Engine =
        Arc<dyn Fn(Mode, &str, &Value, &[(String, String)]) -> Option<Reply> + Send + Sync>;

    /// The fixture engine's command table.
    fn table() -> Engine {
        Arc::new(
            |mode: Mode, method: &str, params: &Value, env: &[(String, String)]| {
                let role = env
                    .iter()
                    .find(|(k, _)| k == "ROLE")
                    .map_or("none", |(_, v)| v.as_str());
                match method {
                    "job.status" => Some(Reply::ok(json!({"role": role, "echo": params}))),
                    "job.progress" => {
                        let mut r = Reply::ok(json!({"done": true}));
                        r.events = vec![json!({"progress": 1}), json!({"progress": 2})];
                        r.stderr = "canary-7f3a engine log line\n".into();
                        Some(r)
                    }
                    // Handler errors: other codes, or the dispatcher's code on text naming something else.
                    "job.lookup" => Some(Reply::fail("unknown_template", "unknown template: t")),
                    "job.nested" => {
                        Some(Reply::fail("unknown_method", "unknown method: job.inner"))
                    }
                    "store.restore" => Some(Reply::ok(json!({"restored": true}))),
                    "job.crash" => None,
                    "job.differs" => Some(Reply::ok(
                        json!({"mode": if mode == Mode::Cold { "cold" } else { "session" }}),
                    )),
                    "job.errdiffers" => Some(Reply::fail(
                        if mode == Mode::Cold {
                            "bad_input"
                        } else {
                            "conflict"
                        },
                        "x",
                    )),
                    "job.exit" => {
                        let mut r = Reply::fail("nope", "n");
                        r.exit_code = Some(if mode == Mode::Cold { 1 } else { 3 });
                        Some(r)
                    }
                    // The dispatcher's refusal; the one-shot CLI turns it into its usage error, exit 2.
                    other => {
                        let mut r =
                            Reply::fail("unknown_method", &format!("unknown method: {other}"));
                        if mode == Mode::Cold {
                            r.exit_code = Some(2);
                        }
                        Some(r)
                    }
                }
            },
        )
    }

    /// Stdin of a scripted sidecar: each complete request line is answered at once.
    struct ScriptedWire {
        buf: Vec<u8>,
        tx: Option<Sender<Value>>,
        stderr: Arc<Mutex<StderrBuf>>,
        engine: Engine,
        env: Vec<(String, String)>,
        live: Arc<AtomicUsize>,
    }

    impl Drop for ScriptedWire {
        fn drop(&mut self) {
            self.live.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl ScriptedWire {
        fn respond(&mut self, req: &Value) {
            let id = req["id"].clone();
            let method = req["method"].as_str().unwrap_or("").to_string();
            match (self.engine)(Mode::Sidecar, &method, &req["params"], &self.env) {
                // The process died: its stdout closes.
                None => self.tx = None,
                Some(r) => {
                    self.stderr.lock().unwrap().push(&r.stderr);
                    let Some(tx) = self.tx.as_ref() else { return };
                    for e in &r.events {
                        tx.send(json!({"type": "event", "id": id, "event": e}))
                            .unwrap();
                    }
                    let mut frame = if r.ok {
                        json!({"type": "response", "id": id, "ok": true, "result": r.result})
                    } else {
                        json!({"type": "response", "id": id, "ok": false,
                               "error": {"code": r.code, "message": r.message}})
                    };
                    if let Some(c) = r.exit_code {
                        frame["exit_code"] = json!(c);
                    }
                    tx.send(frame).unwrap();
                }
            }
        }
    }

    impl Write for ScriptedWire {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.buf.extend_from_slice(data);
            while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=pos).collect();
                let req: Value = serde_json::from_slice(&line).unwrap();
                self.respond(&req);
            }
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct FakeHost {
        engine: Engine,
        /// Sidecars alive now (a scripted wire counts itself out when it is dropped).
        live: Arc<AtomicUsize>,
        sidecar_starts: AtomicUsize,
        cold_calls: AtomicUsize,
        /// `live` as each cold call began.
        live_at_cold: Mutex<Vec<usize>>,
        stderr_limit: usize,
    }

    impl FakeHost {
        fn new() -> Self {
            Self {
                engine: table(),
                live: Arc::new(AtomicUsize::new(0)),
                sidecar_starts: AtomicUsize::new(0),
                cold_calls: AtomicUsize::new(0),
                live_at_cold: Mutex::new(vec![]),
                stderr_limit: DEFAULT_STDERR_LIMIT,
            }
        }
    }

    impl EngineHost for FakeHost {
        fn start(&self, opts: &CliOptions, stderr_file: Option<&Path>) -> Result<Sidecar> {
            self.sidecar_starts.fetch_add(1, Ordering::SeqCst);
            self.live.fetch_add(1, Ordering::SeqCst);
            let (tx, rx) = channel();
            tx.send(
                json!({"type": "hello", "engine": "fake", "version": "1", "contract_version": 1}),
            )
            .unwrap();
            let stderr = Arc::new(Mutex::new(StderrBuf::new(self.stderr_limit)));
            if let Some(p) = stderr_file {
                stderr.lock().unwrap().spill = Some(std::fs::File::create(p).unwrap());
            }
            let wire = ScriptedWire {
                buf: vec![],
                tx: Some(tx),
                stderr: stderr.clone(),
                engine: self.engine.clone(),
                env: opts.env.clone(),
                live: self.live.clone(),
            };
            Sidecar {
                child: None,
                stdin: Some(Box::new(wire)),
                rx,
                seq: 0,
                hello: Value::Null,
                pid: 0,
                stderr,
                events: vec![],
                events_dropped: 0,
                event_limit: DEFAULT_EVENT_LIMIT,
                watch: None,
                legacy_status_frames: false,
                detected_legacy: None,
                frame_shape: None,
                tracker: Tracker::new(),
                spill: stderr_file.map(Path::to_path_buf),
            }
            .finish_start()
        }

        fn cold(&self, args: &[String], opts: &CliOptions) -> Result<CliResult> {
            self.cold_calls.fetch_add(1, Ordering::SeqCst);
            self.live_at_cold
                .lock()
                .unwrap()
                .push(self.live.load(Ordering::SeqCst));
            let input = opts.input.clone().unwrap_or_else(|| json!({}));
            let r = (self.engine)(Mode::Cold, &args.join(" "), &input, &opts.env)
                .ok_or_else(|| Error("cold engine died".into()))?;
            let code = r
                .exit_code
                .map_or(i32::from(!r.ok), |c| i32::try_from(c).unwrap());
            let json = if r.ok {
                r.result.clone()
            } else {
                json!({"error": {"code": r.code, "message": r.message}})
            };
            // The one-shot CLI prints each event as a JSON line on stderr after the engine's own lines.
            let mut stderr = r.stderr.clone();
            for e in &r.events {
                stderr.push_str(&format!("{e}\n"));
            }
            Ok(CliResult {
                code: Some(code),
                stdout: format!("{}\n", serde_json::to_string_pretty(&json).unwrap()),
                json,
                stderr,
                events: r.events.clone(),
                timed_out: false,
            })
        }
    }

    fn opts(env: &[(&str, &str)], input: Option<Value>) -> CliOptions {
        CliOptions {
            data_dir: None,
            env: env
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            input,
            timeout: Some(Duration::from_secs(5)),
        }
    }

    fn run(
        s: &mut EngineSession,
        host: &FakeHost,
        cmd: &str,
        env: &[(&str, &str)],
        route: Route,
    ) -> Result<Served> {
        let args = vec![cmd.to_string()];
        let o = opts(env, None);
        s.cli(
            host,
            &CliCall {
                args: &args,
                opts: &o,
                label: "step 1",
            },
            route,
        )
    }

    fn status(s: &mut EngineSession, host: &FakeHost, env: &[(&str, &str)]) -> Served {
        run(s, host, "job.status", env, Route::Session).unwrap()
    }

    #[test]
    fn starts_equal_env_sets_plus_declared_restarts_and_pass() {
        let host = FakeHost::new();
        let mut s = EngineSession::new(None);
        for _ in 0..3 {
            assert_eq!(status(&mut s, &host, &[]).via, Via::Sidecar);
        }
        // A role is a second env set: its own sidecar, no restart needed.
        for _ in 0..3 {
            let served = status(&mut s, &host, &[("ROLE", "daemon")]);
            assert_eq!(served.result.json["role"], json!("daemon"));
        }
        s.restart(&host, &opts(&[("ROLE", "daemon")], None))
            .unwrap();
        status(&mut s, &host, &[("ROLE", "daemon")]);
        let st = s.starts();
        assert_eq!(st.env_sets, 2);
        assert_eq!(st.declared_restarts, 1);
        assert_eq!((st.sidecar_starts, st.cold_launches), (3, 0));
        assert_eq!((st.total(), st.allowed()), (3, 3));
        assert_eq!(st.violation(), None);
        assert!(st.errored().is_empty());
        assert_eq!(host.sidecar_starts.load(Ordering::SeqCst), 3);
        assert_eq!(host.cold_calls.load(Ordering::SeqCst), 0);
        // Per env set, labelled by first use, never by value.
        let per: Vec<(&str, u32)> = st
            .by_env_set
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect();
        assert_eq!(per, vec![("#1 default", 1), ("#2 env ROLE", 2)]);
        assert_eq!(st.to_value()["engine_starts"], json!(3));
    }

    #[test]
    fn one_undeclared_start_more_fails_the_scenario() {
        let host = FakeHost::new();
        let mut s = EngineSession::new(None);
        status(&mut s, &host, &[]);
        // A non-object input is served cold by the runner; the scenario never declared it.
        let args = vec!["job.status".to_string()];
        let o = opts(&[], Some(json!("text")));
        let served = s
            .cli(
                &host,
                &CliCall {
                    args: &args,
                    opts: &o,
                    label: "step 4 `job.status`",
                },
                Route::Session,
            )
            .unwrap();
        assert_eq!(served.via, Via::Cold);
        let st = s.starts();
        assert_eq!((st.total(), st.allowed()), (2, 1));
        let v = st.violation().expect("2 starts, 1 allowed");
        assert!(v.starts_with("engine starts 2 > allowed 1"), "{v}");
        assert!(v.contains("step 4") && v.contains("cli-entry"), "{v}");
        assert_eq!(st.errored(), vec![v]);

        // The same call declared with its reason is within the allowance.
        let host = FakeHost::new();
        let mut s = EngineSession::new(None);
        status(&mut s, &host, &[]);
        let served = s
            .cli(
                &host,
                &CliCall {
                    args: &args,
                    opts: &o,
                    label: "step 4",
                },
                Route::Cold("cli-entry"),
            )
            .unwrap();
        assert_eq!(served.cold_reason, Some("cli-entry"));
        assert_eq!((s.starts().total(), s.starts().allowed()), (2, 2));
        assert_eq!(s.starts().violation(), None);
        assert_eq!(s.starts().cold_reasons["cli-entry"], 1);
    }

    #[test]
    fn only_the_dispatchers_exact_refusal_goes_cold() {
        let host = FakeHost::new();
        let mut s = EngineSession::new(None);
        // A handler's unknown_* error, and the dispatcher's code on text about something else, are answers.
        for method in ["job.lookup", "job.nested"] {
            let served = run(&mut s, &host, method, &[], Route::Session).unwrap();
            assert_eq!(served.via, Via::Sidecar, "{method}");
            assert_eq!(served.result.code, Some(1));
            assert!(served.result.json["error"]["code"].is_string());
        }
        assert_eq!(host.cold_calls.load(Ordering::SeqCst), 0);
        assert!(s.starts().undeclared.is_empty());
        // The refusal of the method asked for is answered by the one-shot CLI (its usage error, 2).
        let served = run(&mut s, &host, "doctor", &[], Route::Session).unwrap();
        assert_eq!(served.via, Via::Cold);
        assert_eq!(served.result.code, Some(2));
        assert_eq!(served.exit_code_source, "process");
        assert_eq!(host.cold_calls.load(Ordering::SeqCst), 1);
        let st = s.starts();
        assert_eq!(st.undeclared.len(), 1);
        assert!(
            st.undeclared[0].contains("non-method"),
            "{:?}",
            st.undeclared
        );
        assert!(
            st.violation().is_some(),
            "an undeclared cold launch is not allowed"
        );
    }

    #[test]
    fn refusal_matches_the_method_asked_for_and_nothing_else() {
        let refusal = json!({"type":"response","ok":false,"error":{"code":"unknown_method","message":"unknown method: doctor"}});
        assert!(is_unknown_method_refusal(&refusal, "doctor"));
        assert!(!is_unknown_method_refusal(&refusal, "job.inner"));
        let other_code =
            json!({"ok":false,"error":{"code":"unknown_job","message":"unknown method: doctor"}});
        assert!(!is_unknown_method_refusal(&other_code, "doctor"));
        assert!(!is_unknown_method_refusal(
            &json!({"ok":true,"result":{}}),
            "doctor"
        ));
    }

    #[test]
    fn events_are_unwrapped_and_stderr_since_the_last_call_is_appended() {
        let host = FakeHost::new();
        let mut s = EngineSession::new(None);
        let session = run(&mut s, &host, "job.progress", &[], Route::Session).unwrap();
        let cold = run(&mut s, &host, "job.progress", &[], Route::Cold("startup")).unwrap();
        // The inner event objects, as the one-shot CLI prints them, not the frames.
        assert_eq!(
            session.result.events,
            vec![json!({"progress": 1}), json!({"progress": 2})]
        );
        assert_eq!(session.result.events, cold.result.events);
        // The engine's own stderr reaches the step, so a secret scan sees it.
        assert!(
            session.result.stderr.contains("canary-7f3a"),
            "{}",
            session.result.stderr
        );
        assert!(session.stderr_complete);
        assert_eq!(session.exit_code_source, "derived");
        // A call that writes nothing gets nothing of the previous call's stderr or events.
        let quiet = status(&mut s, &host, &[]);
        assert_eq!(quiet.result.stderr, "");
        assert!(quiet.result.events.is_empty());
        let again = run(&mut s, &host, "job.progress", &[], Route::Session).unwrap();
        assert_eq!(again.result.events.len(), 2);
        assert_eq!(again.result.stderr.matches("canary-7f3a").count(), 1);
        let v = session.to_value();
        assert_eq!(v["via"], json!("sidecar"));
        assert_eq!(v["exit_code_source"], json!("derived"));
    }

    #[test]
    fn a_capped_stderr_is_marked_and_a_stderr_file_keeps_everything() {
        let mut host = FakeHost::new();
        host.stderr_limit = 8;
        let mut s = EngineSession::new(None);
        let capped = run(&mut s, &host, "job.progress", &[], Route::Session).unwrap();
        assert!(!capped.stderr_complete);
        assert!(
            capped.result.stderr.starts_with("[sidecar stderr capped"),
            "{}",
            capped.result.stderr
        );
        assert_eq!(capped.to_value()["stderr_complete"], json!(false));

        let dir = std::env::temp_dir().join(format!("rkqa-session-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = EngineSession::new(Some(dir.clone()));
        let full = run(&mut s, &host, "job.progress", &[], Route::Session).unwrap();
        assert!(full.stderr_complete, "{}", full.result.stderr);
        assert_eq!(full.result.stderr, "canary-7f3a engine log line\n");
        s.close_all();
        let files = s.stderr_files().to_vec();
        assert_eq!(files.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&files[0]).unwrap(),
            "canary-7f3a engine log line\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_store_lock_cold_call_runs_with_no_sidecar_and_covers_their_reopen() {
        let host = FakeHost::new();
        let mut s = EngineSession::new(None);
        status(&mut s, &host, &[]);
        status(&mut s, &host, &[("ROLE", "daemon")]);
        assert_eq!(s.live_sidecars(), 2);
        let served = run(
            &mut s,
            &host,
            "store.restore",
            &[],
            Route::Cold("store-lock"),
        )
        .unwrap();
        assert_eq!(served.via, Via::Cold);
        // The exclusive lock command saw no other engine process of the scenario.
        assert_eq!(*host.live_at_cold.lock().unwrap(), vec![0]);
        assert_eq!(s.live_sidecars(), 0);
        // The next call reopens a sidecar; that start is covered by the declaration.
        status(&mut s, &host, &[]);
        let st = s.starts();
        // Only the default set reopened; the ROLE set's reopen is not allowed until it happens.
        assert_eq!(st.store_lock_reopens, 1);
        assert_eq!((st.sidecar_starts, st.cold_launches), (3, 1));
        assert_eq!((st.total(), st.allowed()), (4, 4));
        assert_eq!(st.violation(), None);
        // A different cold reason does not close anything.
        let mut s = EngineSession::new(None);
        status(&mut s, &host, &[]);
        run(&mut s, &host, "job.status", &[], Route::Cold("recovery")).unwrap();
        assert_eq!(s.live_sidecars(), 1);
    }

    #[test]
    fn a_kill_is_declared_and_the_next_call_recovers_on_a_new_engine() {
        let host = FakeHost::new();
        let mut s = EngineSession::new(None);
        assert!(s.kill(&opts(&[], None)).is_err(), "nothing to kill yet");
        status(&mut s, &host, &[]);
        let killed = s.kill(&opts(&[], None)).unwrap();
        assert_eq!(killed["env_set"], json!("#1 default"));
        assert_eq!(host.live.load(Ordering::SeqCst), 0);
        assert_eq!(s.live_sidecars(), 0);
        assert_eq!(status(&mut s, &host, &[]).via, Via::Sidecar);
        let st = s.starts();
        assert_eq!((st.total(), st.allowed()), (2, 2));
        assert_eq!(st.violation(), None);
        assert_eq!(host.sidecar_starts.load(Ordering::SeqCst), 2);
    }

    /// Oracle item 4: a kill whose replacement never starts allows nothing, so an undeclared cold
    /// launch after it is errored, not absorbed by the unused allowance.
    #[test]
    fn a_kill_then_an_undeclared_cold_launch_is_errored() {
        let host = FakeHost::new();
        let mut s = EngineSession::new(None);
        status(&mut s, &host, &[]);
        s.kill(&opts(&[], None)).unwrap();
        assert_eq!(
            s.starts().declared_restarts,
            0,
            "no replacement started yet"
        );
        // A non-object input is served cold by the runner; the scenario never declared it.
        let args = vec!["job.status".to_string()];
        let o = opts(&[], Some(json!("text")));
        let served = s
            .cli(
                &host,
                &CliCall {
                    args: &args,
                    opts: &o,
                    label: "step 3",
                },
                Route::Session,
            )
            .unwrap();
        assert_eq!(served.via, Via::Cold);
        let st = s.starts();
        assert_eq!(st.undeclared.len(), 1);
        assert!(st.violation().is_some());
        assert!(!st.errored().is_empty());
    }

    /// A cold start the plugin declared is recorded apart, as the plugin's, with its step; the
    /// scenario's own declarations never appear in that list.
    #[test]
    fn a_plugin_declared_cold_start_is_recorded_with_declared_by_plugin_and_its_step() {
        let host = FakeHost::new();
        let mut s = EngineSession::new(None);
        status(&mut s, &host, &[]);
        s.note_spawned_engine("spawn `eng` (pid 9)", Some("startup"), Some(4));
        let st = s.starts();
        assert_eq!(st.plugin_declared_cold, 1);
        assert_eq!(st.declared_cold, 1);
        assert_eq!(st.violation(), None);
        let v = st.to_value();
        assert_eq!(v["plugin_declared_cold"], json!(1));
        let list = v["plugin_declared"].as_array().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["declared_by"], json!("plugin"));
        assert_eq!(list[0]["step"], json!(4));
        assert_eq!(list[0]["cold_reason"], json!("startup"));
        assert_eq!(v["cold_reasons"]["startup"], json!(1));

        // A session with no plugin declaration lists none.
        let mut own = EngineSession::new(None);
        status(&mut own, &host, &[]);
        let own_v = own.starts().to_value();
        assert_eq!(own_v["plugin_declared_cold"], json!(0));
        assert!(own_v["plugin_declared"].as_array().unwrap().is_empty());
    }

    /// An undeclared launch is errored by itself, even when the counts alone would pass.
    #[test]
    fn an_undeclared_launch_is_errored_even_when_unused_allowance_covers_its_count() {
        let st = EngineStarts {
            sidecar_starts: 1,
            cold_launches: 1,
            env_sets: 1,
            declared_restarts: 1,
            undeclared: vec!["step 2: cold".to_string()],
            ..EngineStarts::default()
        };
        assert!(st.total() <= st.allowed(), "the counts alone would pass");
        let v = st.violation().expect("undeclared launch is a violation");
        assert!(v.starts_with("undeclared engine launch"), "{v}");
        assert!(v.contains("step 2: cold"), "{v}");
        assert_eq!(st.errored(), vec![v]);
    }

    /// A restart of a set that never ran is its first start: no allowance appears from nowhere.
    #[test]
    fn a_restart_of_a_set_that_never_ran_allows_nothing_extra() {
        let host = FakeHost::new();
        let mut s = EngineSession::new(None);
        s.restart(&host, &opts(&[], None)).unwrap();
        let st = s.starts();
        assert_eq!((st.total(), st.allowed()), (1, 1));
        assert_eq!(st.declared_restarts, 0);
        // A restart of a running set is allowed once its replacement starts, which is at once.
        s.restart(&host, &opts(&[], None)).unwrap();
        let st = s.starts();
        assert_eq!((st.total(), st.allowed()), (2, 2));
        assert_eq!(st.declared_restarts, 1);
        assert_eq!(st.violation(), None);
    }

    /// Oracle item 4: engines started through a plugin `spawn` (`engine: true`) count, are never
    /// allowed, and make the scenario errored; `start_daemon` counts and is its own declaration.
    #[test]
    fn plugin_spawned_engines_count_and_error_and_daemons_count_and_pass() {
        let host = FakeHost::new();
        let mut s = EngineSession::new(None);
        status(&mut s, &host, &[]);
        for n in 1..=3 {
            s.note_spawned_engine(&format!("spawn `eng-{n}`"), None, Some(n));
        }
        let st = s.starts();
        assert!(st.total() >= 4, "total {}", st.total());
        assert_eq!(st.spawned_engines, 3);
        assert_eq!((st.total(), st.allowed()), (4, 1));
        let v = st.violation().expect("three undeclared spawns");
        assert!(v.starts_with("engine starts 4 > allowed 1"), "{v}");
        assert!(v.contains("eng-3"), "{v}");
        assert!(!st.errored().is_empty());
        assert_eq!(st.to_value()["spawned_engines"], json!(3));

        let mut s = EngineSession::new(None);
        s.note_daemon_engine();
        let st = s.starts();
        assert_eq!((st.total(), st.allowed()), (1, 1));
        assert_eq!(st.violation(), None);
    }

    #[test]
    fn a_sidecar_that_dies_mid_call_is_errored_and_not_reused() {
        let host = FakeHost::new();
        let mut s = EngineSession::new(None);
        status(&mut s, &host, &[]);
        let e = run(&mut s, &host, "job.crash", &[], Route::Session)
            .unwrap_err()
            .0;
        assert!(e.contains("died or did not answer `job.crash`"), "{e}");
        assert_eq!(s.live_sidecars(), 0);
        assert_eq!(s.starts().crashes.len(), 1);
        assert!(!s.starts().errored().is_empty());
        // The next call starts a fresh sidecar, counted: 2 starts for 1 env set, so the count check fails too.
        status(&mut s, &host, &[]);
        assert!(s.starts().violation().is_some());
    }

    #[test]
    fn parity_passes_when_cold_and_in_session_agree() {
        let host = FakeHost::new();
        let mut s = EngineSession::new(None);
        let args = vec!["job.status".to_string()];
        let o = opts(&[], Some(json!({"a": 1})));
        let call = CliCall {
            args: &args,
            opts: &o,
            label: "step 2",
        };
        let report = s.parity(&host, &call).unwrap();
        assert!(report.equal(), "{:?}", report.differences);
        // The stdio protocol here carries no exit code: only body and error class are compared, and said so.
        assert_eq!(report.exit_code_source, "derived");
        assert!(!report.exit_code_compared);
        assert!(report.to_value()["compares"]
            .as_str()
            .unwrap()
            .contains("only"));
        assert_eq!(s.starts().cold_reasons["parity"], 1);
        assert_eq!(s.starts().violation(), None);
    }

    #[test]
    fn parity_fails_on_a_differing_body_error_class_or_protocol_exit_code() {
        let host = FakeHost::new();
        let mut s = EngineSession::new(None);
        let check = |s: &mut EngineSession, cmd: &str| {
            let args = vec![cmd.to_string()];
            let o = opts(&[], None);
            s.parity(
                &host,
                &CliCall {
                    args: &args,
                    opts: &o,
                    label: "step 2",
                },
            )
            .unwrap()
        };
        let body = check(&mut s, "job.differs");
        assert!(!body.equal());
        assert!(
            body.differences[0].starts_with("JSON body differs"),
            "{:?}",
            body.differences
        );
        let class = check(&mut s, "job.errdiffers");
        assert!(
            class.differences[0].contains("error class differs"),
            "{:?}",
            class.differences
        );
        // The response carries an exit code, so it is compared, and 1 vs 3 differs.
        let exit = check(&mut s, "job.exit");
        assert_eq!(exit.exit_code_source, "protocol");
        assert!(exit.exit_code_compared);
        assert!(
            exit.differences
                .iter()
                .any(|d| d.contains("exit code differs")),
            "{:?}",
            exit.differences
        );
        // A method the dispatcher refuses has no in-session answer: nothing to compare.
        let args = vec!["doctor".to_string()];
        let o = opts(&[], None);
        let e = s
            .parity(
                &host,
                &CliCall {
                    args: &args,
                    opts: &o,
                    label: "step 3",
                },
            )
            .unwrap_err()
            .0;
        assert!(e.contains("does not serve `doctor`"), "{e}");
    }

    #[test]
    fn compare_parity_reads_body_class_and_exit_code() {
        let result = |code: i32, json: Value| CliResult {
            code: Some(code),
            json,
            stdout: String::new(),
            stderr: String::new(),
            events: vec![],
            timed_out: false,
        };
        let ok = result(0, json!({"a": 1}));
        assert!(compare_parity(&ok, &ok.clone(), "derived").is_empty());
        let other = result(0, json!({"a": 2}));
        assert_eq!(compare_parity(&ok, &other, "derived").len(), 1);
        let bad = result(1, json!({"error": {"code": "x", "message": "m"}}));
        let bad_other_text = result(1, json!({"error": {"code": "x", "message": "other"}}));
        assert!(
            compare_parity(&bad, &bad_other_text, "derived").is_empty(),
            "class is the code, not the text"
        );
        let mixed = compare_parity(&ok, &bad, "derived");
        assert!(mixed[0].starts_with("outcome differs"), "{mixed:?}");
        // The exit code only counts when the protocol carried it.
        let exit2 = result(2, json!({"a": 1}));
        assert!(compare_parity(&ok, &exit2, "derived").is_empty());
        assert_eq!(compare_parity(&ok, &exit2, "protocol").len(), 1);
    }

    #[test]
    fn env_sets_ignore_order_and_a_later_value_wins() {
        let a = vec![
            ("B".to_string(), "2".to_string()),
            ("A".to_string(), "1".to_string()),
        ];
        let b = vec![
            ("A".to_string(), "0".to_string()),
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "2".to_string()),
        ];
        assert_eq!(env_set_id(&a), env_set_id(&b));
        assert_ne!(
            env_set_id(&a),
            env_set_id(&[("A".to_string(), "1".to_string())])
        );
        assert_eq!(env_set_id(&[]), "");
    }

    #[test]
    fn cold_reasons_are_the_closed_set_of_c3a() {
        for r in [
            "startup",
            "restart",
            "recovery",
            "role-change",
            "store-lock",
            "cli-entry",
            "non-method",
        ] {
            assert_eq!(parse_cold_reason(r).unwrap(), r);
        }
        let e = parse_cold_reason("because").unwrap_err().0;
        assert!(e.contains("store-lock") && e.contains("cli-entry"), "{e}");
    }

    #[test]
    fn scenario_sidecars_are_counted_and_each_is_its_own_declaration() {
        let mut s = EngineSession::new(None);
        s.note_scenario_sidecar();
        s.note_scenario_sidecar();
        let st = s.starts();
        assert_eq!((st.total(), st.allowed()), (2, 2));
        assert_eq!(st.violation(), None);
    }
}
