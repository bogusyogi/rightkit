//! Black-box driving of an app's engine binary: one-shot CLI words and the
//! framed stdio sidecar protocol (newline-delimited JSON through
//! `rightkit-sidecar`). Nothing mocks the engine.
use crate::process::{run_owned, RunOptions, Tracker};
use crate::util::{err, Error, Result};
use rightkit_child_process::{OwnedChild, OwnedCommand};
use rightkit_sidecar::{read_json_line, write_json_line, FrameError};
use serde_json::{json, Value};
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

    fn env_for(
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
        Sidecar::start(self, opts, tracker)
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
}

impl StderrBuf {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            start: 0,
            limit,
            dropped: 0,
        }
    }

    fn push(&mut self, chunk: &str) {
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
    fn start(engine: &EngineTarget, opts: &CliOptions, tracker: &Tracker) -> Result<Self> {
        engine.check_launch(&["--stdio".to_string()], opts)?;
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
        tracker.register(pid, "engine --stdio");
        let stdin = child.take_stdin();
        let stdout = child
            .take_stdout()
            .ok_or_else(|| Error("sidecar stdout unavailable".into()))?;
        let mut stderr_pipe = child.take_stderr();
        let stderr = Arc::new(Mutex::new(StderrBuf::new(DEFAULT_STDERR_LIMIT)));
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
    /// used when nothing is set; see [`Sidecar::resolve_shape`].
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
