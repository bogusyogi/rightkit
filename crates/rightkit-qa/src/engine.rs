//! Black-box driving of an app's engine binary: one-shot CLI words and the
//! framed stdio sidecar protocol (newline-delimited JSON through
//! `rightkit-framed-sidecar`). Nothing mocks the engine.
use crate::process::{run_owned, RunOptions, Tracker};
use crate::util::{err, Error, Result};
use rightkit_framed_sidecar::{read_json_line, write_json_line, FrameError};
use rightkit_process::{OwnedChild, OwnedCommand};
use serde_json::{json, Value};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const FRAME_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct EngineTarget {
    pub binary: PathBuf,
    /// Env applied to every engine process (e.g. `GENRIGHT_NO_KEYCHAIN=1`).
    pub env: Vec<(String, String)>,
    /// Env var that receives the per-scenario data dir (e.g. `GENRIGHT_APP_DATA`).
    pub data_dir_env: Option<String>,
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
    stdin: Option<ChildStdin>,
    rx: Receiver<Value>,
    seq: u64,
    pub hello: Value,
    pub pid: u32,
    stderr: Arc<Mutex<String>>,
    /// Frames that were neither hello nor the awaited response (events).
    pub events: Vec<Value>,
    tracker: Tracker,
}

impl Sidecar {
    fn start(engine: &EngineTarget, opts: &CliOptions, tracker: &Tracker) -> Result<Self> {
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
        let stderr = Arc::new(Mutex::new(String::new()));
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
                        .push_str(&String::from_utf8_lossy(&buf[..n]));
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
        let mut me = Self {
            child: Some(child),
            stdin,
            rx,
            seq: 0,
            hello: Value::Null,
            pid,
            stderr,
            events: vec![],
            tracker: tracker.clone(),
        };
        me.hello = me.await_frame(Duration::from_secs(30), |f| f["type"] == "hello")?;
        Ok(me)
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
                Ok(f) => self.events.push(f),
                Err(RecvTimeoutError::Timeout) => {
                    return err(format!(
                        "sidecar timeout after {}ms; stderr tail: {}",
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

    pub fn stderr_tail(&self) -> String {
        crate::util::tail(&self.stderr.lock().unwrap(), 1500)
    }

    /// Send an arbitrary frame and return the response with the same id.
    pub fn raw(&mut self, mut frame: Value, timeout: Duration) -> Result<Value> {
        self.seq += 1;
        let id = self.seq;
        frame["id"] = json!(id);
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| Error("sidecar stdin closed".into()))?;
        write_json_line(stdin, &frame, FRAME_LIMIT).map_err(|e| Error(e.to_string()))?;
        stdin.flush()?;
        self.await_frame(timeout, |f| f["type"] == "response" && f["id"] == json!(id))
    }

    /// A command; `Ok(result)` for `status: ok`, `Err("code: message")` for `err`.
    pub fn command(&mut self, name: &str, input: Value, timeout: Duration) -> Result<Value> {
        let f = self.raw(json!({"type": "request", "method": {"method": "command", "params": {"name": name, "input": input}}}), timeout)?;
        if f["status"] == "err" {
            return err(format!(
                "{}: {}",
                f["error"]["code"].as_str().unwrap_or("error"),
                f["error"]["message"].as_str().unwrap_or("")
            ));
        }
        Ok(f["result"].clone())
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
                    let _ = child.terminate_tree();
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
